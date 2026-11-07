//! Judges the redeemer evaluations the evaluator and each EvalTx version give a record against the ledger's.

use std::collections::BTreeMap;

use dolos_core::{ChainError, DomainError};
use ledger_oracle_judge::{
    cbor::Purpose,
    context::{self, ContextTally, Listing, PallasContext, PallasContexts},
    eval::{
        self, named_in_message, wire_purpose, Answer, DolosAnswer, EvalTally, Layer, Level,
        Malformed, Refusal, RowKey, RowVerdict, TxDemand, WireEval, WireFee,
    },
    record::{LedgerOutcome, LedgerRedeemers, Phase2, Record, RecordId, Units},
};
use pallas::codec::minicbor;
use pallas::ledger::primitives::dijkstra::{Language, MempoolTransaction};
use pallas::ledger::primitives::ExUnits;
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

/// Each EvalTx version's answer from its response, named by the evaluator's refusal and the message each version gives for what it cannot represent.
fn wire_answers(
    wires: &[(Layer, WireEval)],
    evaluator: &Evaluator,
) -> Vec<(Layer, Result<DolosAnswer, Malformed>)> {
    let refusal = evaluator
        .refusal
        .as_ref()
        .map(|(text, r)| (text.as_str(), r));
    let answers = wires.iter().map(|(layer, wire)| {
        let unrepresentable = evaluator.unrepresentable.get(layer);
        let answer = wire.answer(refusal, unrepresentable.map(String::as_str));
        (*layer, answer)
    });
    answers.collect()
}

/// The transaction a builder evaluates before signing, derived from one signed record with its top level key witnesses removed and its top level redeemer budgets zero.
struct Draft {
    signed: RecordId,
    bytes: Vec<u8>,
}

impl Draft {
    /// The draft of a signed Dijkstra transaction, or why its bytes derive none.
    fn of(signed: RecordId, tx: &[u8]) -> Result<Self, String> {
        let mut draft: MempoolTransaction = minicbor::decode(tx).map_err(|e| e.to_string())?;
        let witnesses = &mut draft.transaction_witness_set;
        witnesses.vkeywitness = None;
        if let Some(redeemers) = witnesses.redeemer.as_mut() {
            for value in redeemers.0.values_mut() {
                value.ex_units = ExUnits { mem: 0, steps: 0 };
            }
        }
        let bytes = minicbor::to_vec(&draft).map_err(|e| e.to_string())?;
        Ok(Draft { signed, bytes })
    }
}

/// Judges the evaluator's and each EvalTx version's answer for a draft against the signed record's ledger rows.
async fn judge_draft(
    group: &Group,
    draft: &Draft,
    ledger: &BTreeMap<RowKey, Answer>,
    listed: &BTreeMap<RowKey, Refusal>,
    tally: &mut EvalTally,
) {
    let wires = wire_evals(group, &draft.bytes).await;
    let evaluator = evaluator(group.estimate(&draft.bytes));
    for (layer, answer) in wire_answers(&wires, &evaluator) {
        tally.draft.add(layer, draft.signed, ledger, listed, answer);
    }
    let pallas = evaluator.answer;
    tally
        .draft
        .add(Layer::Pallas, draft.signed, ledger, listed, pallas);
}

/// Judges the evaluator's and each EvalTx version's answer for one record against the ledger's rows, the rows the known list gives it and, when the ledger evaluated none, its outcome's demand and the answer the transaction known list gives it, and returns the builtins called by each redeemer the evaluator ran in the ledger's units.
pub(super) async fn judge_record(
    group: &Group,
    id: RecordId,
    record: &Record,
    listed: &BTreeMap<RowKey, Refusal>,
    tx_listed: Option<&str>,
    tally: &mut EvalTally,
) -> Vec<Builtin> {
    let tx = &record.tx.0;
    let wires = wire_evals(group, tx).await;
    for (layer, wire) in &wires {
        if wire.total_agrees().is_none() {
            let msg: Vec<&str> = wire.errors.iter().map(|(_, m)| m.as_str()).collect();
            let msg: String = msg.join(" | ").chars().take(200).collect();
            eprintln!("WIRE-REFUSES {layer} {id} {msg}");
        }
        tally.add_total(*layer, id, wire);
        tally.add_fee(*layer, id, record.min_fee, &wire.fee);
    }
    if record.redeemers == LedgerRedeemers::Undecoded {
        tally.undecoded += 1;
        return Vec::new();
    }
    let ledger = eval::ledger_rows(record);
    tally.collisions += eval::collisions(&ledger);
    let evaluator = evaluator(group.estimate(tx));
    let demand = ledger.is_empty().then(|| TxDemand::of(&record.outcome));
    let mut judge_tx = |layer: Layer, answer: &Result<DolosAnswer, Malformed>| {
        if let Some(demand) = demand {
            let verdict = eval::tx_verdict(demand, answer, tx_listed);
            tally.tx.add(layer, id, &verdict);
        }
    };
    let answers = wire_answers(&wires, &evaluator);
    for (layer, answer) in &answers {
        judge_tx(*layer, answer);
    }
    judge_tx(Layer::Pallas, &evaluator.answer);
    for (layer, answer) in answers {
        tally.add_answer(layer, id, &ledger, listed, answer);
    }
    if let (LedgerOutcome::Accepted { .. }, Phase2::Valid, false) =
        (&record.outcome, record.flag(), ledger.is_empty())
    {
        match Draft::of(id, tx) {
            Ok(draft) => judge_draft(group, &draft, &ledger, listed, tally).await,
            Err(why) => tally.draft.underived.push(format!("{id} {why}")),
        }
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

#[test]
fn a_draft_keeps_the_signed_body_and_drops_its_key_witnesses_and_budgets() {
    use ledger_oracle_judge::record::CorpusKind;

    let corpus = super::load(CorpusKind::Ic);
    let records = corpus.kept().flat_map(|run| run.records());
    fn decode(tx: &[u8]) -> MempoolTransaction<'_> {
        minicbor::decode(tx).unwrap()
    }
    let signed = records
        .filter(|r| matches!(r.outcome, LedgerOutcome::Accepted { .. }))
        .find(|r| {
            let tx = decode(&r.tx.0);
            let witnesses = &tx.transaction_witness_set;
            witnesses.vkeywitness.is_some() && witnesses.redeemer.is_some()
        })
        .expect("an accepted record signs and carries redeemers");
    let id = RecordId {
        corpus: CorpusKind::Ic,
        n: signed.n,
    };
    let draft = Draft::of(id, &signed.tx.0).unwrap();
    assert_eq!(draft.signed, id);
    let (signed, draft) = (decode(&signed.tx.0), decode(&draft.bytes));
    assert_eq!(
        signed.transaction_body.raw_cbor(),
        draft.transaction_body.raw_cbor()
    );
    assert_eq!(draft.transaction_witness_set.vkeywitness, None);
    let budgets = |tx: &MempoolTransaction| -> Vec<(u64, u64)> {
        let redeemers = tx.transaction_witness_set.redeemer.as_ref().unwrap();
        redeemers
            .values()
            .map(|v| (v.ex_units.mem, v.ex_units.steps))
            .collect()
    };
    assert!(budgets(&signed).iter().any(|b| *b != (0, 0)));
    assert!(budgets(&draft).iter().all(|b| *b == (0, 0)));
    assert_eq!(budgets(&signed).len(), budgets(&draft).len());
    assert!(Draft::of(id, &[0x80]).is_err());
}
