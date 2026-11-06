//! Judges the redeemer evaluations the evaluator and each EvalTx version give a record against the ledger's.

use std::collections::BTreeMap;

use dolos_core::{ChainError, DomainError};
use ledger_oracle_judge::{
    cbor::Purpose,
    context::{self, ContextTally, Listing, PallasContext, PallasContexts},
    eval::{
        self, named_in_message, wire_purpose, Answer, DolosAnswer, EvalTally, Layer, Level,
        Malformed, Refusal, RowKey, RowVerdict, WireEval, WireFee,
    },
    record::{LedgerRedeemers, Record, RecordId, Units},
};
use pallas::codec::minicbor;
use pallas::ledger::primitives::dijkstra::Language;
use pallas::ledger::traverse::MultiEraRedeemerTag as RedeemerTag;
use pallas::ledger::validate::phase2::{
    tx::{MultiEraTxEvalResult, RedeemerArguments},
    Builtin,
};
use tonic::Request;

use super::{purpose, tag_number, validate::Group};

/// The evaluator's answer for one transaction, with the builtins each redeemer called and what an EvalTx response says in its place.
struct Evaluator {
    answer: Result<DolosAnswer, Malformed>,
    calls: BTreeMap<RowKey, Vec<Builtin>>,
    /// The refusal and the message an EvalTx response renders it to.
    refusal: Option<(String, Refusal)>,
    /// The message each EvalTx version that cannot represent a result gives in place of its report.
    unrepresentable: BTreeMap<Layer, String>,
}

/// The message the EvalTx version `layer` gives in place of a report of `results` when it cannot represent one of them, naming the first.
fn unrepresentable(
    layer: Layer,
    results: &[(MultiEraTxEvalResult, Vec<Builtin>)],
) -> Option<String> {
    results.iter().find_map(|(r, _)| {
        let redeemer = format!("{:?}[{}]", r.tag, r.index);
        let guarding = matches!(r.tag, RedeemerTag::Guarding);
        let what = match (layer, r.sub_transaction) {
            (Layer::Pallas, _) => None,
            (_, Some(sub)) => Some(format!("{redeemer} of sub transaction {sub}")),
            (Layer::V1beta | Layer::V1alpha, None) if guarding => Some(redeemer),
            (Layer::V1 | Layer::V1beta | Layer::V1alpha, None) => None,
        };
        what.map(|what| format!("RPC {layer} cannot represent redeemer {what}"))
    })
}

/// The refusal the known lists name an evaluation error by.
fn refusal(e: &ChainError) -> Refusal {
    Refusal::new(&match e {
        ChainError::Phase2EvaluationUnsupported(unsupported) => unsupported.0.to_string(),
        other => other.to_string(),
    })
}

/// The evaluator's answer from its estimate of one transaction.
fn evaluator(estimate: Result<Vec<(MultiEraTxEvalResult, Vec<Builtin>)>, ChainError>) -> Evaluator {
    let results = match estimate {
        Ok(results) => results,
        Err(e) => {
            let refusal = refusal(&e);
            let text = format!("{:#?}", DomainError::from(e));
            return Evaluator {
                answer: Ok(DolosAnswer::Refused(refusal.clone())),
                calls: BTreeMap::new(),
                refusal: Some((text, refusal)),
                unrepresentable: BTreeMap::new(),
            };
        }
    };
    let key = |r: &MultiEraTxEvalResult| RowKey {
        level: r.sub_transaction.map_or(Level::Top, Level::Sub),
        purpose: purpose(r),
    };
    let rows = results.iter().map(|(r, _)| {
        let key = key(r);
        let answer = match r.success {
            true => Answer::Ran(Units {
                mem: r.units.mem,
                steps: r.units.steps,
            }),
            false => Answer::Failed,
        };
        (key, answer)
    });
    let mut calls: BTreeMap<RowKey, Vec<Builtin>> = BTreeMap::new();
    for (r, called) in &results {
        calls.entry(key(r)).or_default().extend(called);
    }
    let unrepresentable = Layer::ALL
        .into_iter()
        .filter_map(|layer| Some((layer, unrepresentable(layer, &results)?)))
        .collect();
    Evaluator {
        answer: eval::evaluated(rows).map(DolosAnswer::Evaluated),
        calls,
        refusal: None,
        unrepresentable,
    }
}

/// The fee an EvalTx response gives, from the oneof of its big integer.
macro_rules! wire_fee {
    ($version:ident, $fee:expr) => {{
        use pallas::interop::utxorpc::$version::spec::cardano::big_int::BigInt;
        match $fee.and_then(|fee| fee.big_int) {
            None => WireFee::Unset,
            Some(BigInt::Int(n)) => WireFee::Int(n),
            Some(other) => WireFee::Other(format!("{other:?}")),
        }
    }};
}

/// The EvalTx response of one u5c version for a transaction against the group's domain, its errors read by the given closure.
macro_rules! wire_eval {
    ($version:ident, $group:expr, $tx:expr, $error:expr) => {{
        use pallas::interop::utxorpc::$version::spec::submit::{
            any_chain_eval::Chain, any_chain_tx, submit_service_server::SubmitService as _,
            AnyChainTx, EvalTxRequest,
        };
        let tx = AnyChainTx {
            r#type: Some(any_chain_tx::Type::Raw($tx.to_vec().into())),
        };
        let service =
            crate::serve::grpc::$version::submit::SubmitServiceImpl::new($group.domain().clone());
        let response = service
            .eval_tx(Request::new(EvalTxRequest { tx: Some(tx) }))
            .await
            .unwrap_or_else(|e| panic!("EvalTx {} {e}", stringify!($version)))
            .into_inner();
        let report = response.report.and_then(|r| r.chain);
        let Chain::Cardano(report) = report.expect("EvalTx answers with a Cardano report");
        let redeemers = report.redeemers.iter().map(|r| {
            let units = r.ex_units.as_ref().map(|u| Units {
                mem: u.memory,
                steps: u.steps,
            });
            (wire_purpose(r.purpose, r.index), units)
        });
        WireEval {
            redeemers: redeemers.collect(),
            errors: report.errors.iter().map($error).collect(),
            total: report.ex_units.map(|u| Units {
                mem: u.memory,
                steps: u.steps,
            }),
            fee: wire_fee!($version, report.fee),
        }
    }};
}

/// The response of each EvalTx version for a transaction against the group's domain.
async fn wire_evals(group: &Group, tx: &[u8]) -> [(Layer, WireEval); 3] {
    let named = |purpose: Option<i32>, index: Option<u32>, msg: &String| {
        let purpose = purpose.zip(index).map(|(p, i)| wire_purpose(p, i));
        (purpose, msg.clone())
    };
    let v1 = wire_eval!(v1, group, tx, |e| named(e.purpose, e.index, &e.msg));
    let v1beta = wire_eval!(v1beta, group, tx, |e| named(e.purpose, e.index, &e.msg));
    let v1alpha = wire_eval!(v1alpha, group, tx, |e| (
        named_in_message(&e.msg),
        e.msg.clone()
    ));
    [
        (Layer::V1, v1),
        (Layer::V1beta, v1beta),
        (Layer::V1alpha, v1alpha),
    ]
}

/// Judges the evaluator's and each EvalTx version's answer for one record against the ledger's rows and the rows the known list gives it, and returns the builtins called by each redeemer the evaluator ran in the ledger's units.
pub(super) async fn judge_record(
    group: &Group,
    id: RecordId,
    record: &Record,
    listed: &BTreeMap<RowKey, Refusal>,
    tally: &mut EvalTally,
) -> Vec<Builtin> {
    let tx = &record.tx.0;
    let wires = wire_evals(group, tx).await;
    for (layer, wire) in &wires {
        tally.add_total(*layer, id, wire);
        tally.add_fee(*layer, id, record.min_fee, &wire.fee);
    }
    let LedgerRedeemers::Read(redeemers) = &record.redeemers else {
        tally.undecoded += 1;
        return Vec::new();
    };
    let ledger = eval::ledger_rows(redeemers);
    tally.collisions += eval::collisions(&ledger);
    let evaluator = evaluator(group.estimate(tx));
    let refusal = evaluator
        .refusal
        .as_ref()
        .map(|(text, r)| (text.as_str(), r));
    for (layer, wire) in wires {
        let unrepresentable = evaluator.unrepresentable.get(&layer);
        let answer = wire.answer(refusal, unrepresentable.map(String::as_str));
        tally.add_answer(layer, id, &ledger, listed, answer);
    }
    let verdicts = tally.add_answer(Layer::Pallas, id, &ledger, listed, evaluator.answer);
    let ran = verdicts.iter().filter_map(|(key, verdict)| match verdict {
        RowVerdict::Agree(Answer::Ran(_)) => evaluator.calls.get(key),
        _ => None,
    });
    ran.flatten().copied().collect()
}

/// The contexts the evaluator builds for one transaction, keyed by row.
fn pallas_contexts(arguments: Result<Vec<RedeemerArguments>, ChainError>) -> PallasContexts {
    let arguments = match arguments {
        Ok(arguments) => arguments,
        Err(e) => return PallasContexts::Refused(refusal(&e)),
    };
    let rows = arguments.into_iter().map(|r| {
        let key = RowKey {
            level: r.sub_transaction.map_or(Level::Top, Level::Sub),
            purpose: Purpose {
                tag: tag_number(&r.tag),
                index: r.index.into(),
            },
        };
        let language = match r.language {
            Language::PlutusV1 => context::Language::V1,
            Language::PlutusV2 => context::Language::V2,
            Language::PlutusV3 => context::Language::V3,
            Language::PlutusV4 => context::Language::V4,
        };
        let args = r.args.iter().map(|arg| {
            minicbor::to_vec(arg).unwrap_or_else(|e| panic!("encoding a script argument {e}"))
        });
        let args = args.collect();
        (key, PallasContext { language, args })
    });
    PallasContexts::built(rows).unwrap_or_else(|key| panic!("the evaluator built {key} twice"))
}

/// Judges the arguments the evaluator applies each redeemer's script of one record to against the ledger's and the listing the known list gives each row.
pub(super) fn judge_context(
    group: &Group,
    id: RecordId,
    record: &Record,
    listed: &BTreeMap<RowKey, Listing>,
    tally: &mut ContextTally,
) {
    let ledger = context::ledger_contexts(record);
    let pallas = pallas_contexts(group.arguments(&record.tx.0));
    tally.add(id, &context::judge_tx(&ledger, &pallas, listed));
}
