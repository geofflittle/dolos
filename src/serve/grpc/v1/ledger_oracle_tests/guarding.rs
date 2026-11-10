//! Guard execution through Dolos admission, evaluation, estimation and RPC v1.
//! Corpus bytes are unchanged; mutations and evidence limits:
//! test_data/musashi-guard-callers/.
use super::*;
use dolos_core::{ChainError, MempoolAwareUtxoStore};
use pallas::interop::utxorpc::v1::spec as rpc;
use pallas::ledger::primitives::dijkstra as n;
use rpc::submit::submit_service_server::SubmitService as _;
use tonic::Request;

pub(super) struct Fixture {
    _seed: seed::Seeded,
    pub(super) group: validate::Group,
    pub(super) tx: Vec<u8>,
    expected: Option<ledger_oracle_judge::record::Units>,
}
pub(super) fn fixture(number: u32) -> Fixture {
    static CORPUS: std::sync::OnceLock<Corpus> = std::sync::OnceLock::new();
    let corpus = CORPUS.get_or_init(|| load(CorpusKind::Ic));
    let run = corpus
        .kept()
        .find(|run| run.records().iter().any(|r| r.n == number))
        .expect("kept record");
    let record = run.records().iter().find(|r| r.n == number).unwrap();
    let mut pre: BTreeMap<_, _> = run.seed().iter().cloned().map(|e| (e.input, e)).collect();
    for prior in run.records().iter().take_while(|r| r.n != number) {
        if let LedgerOutcome::Accepted {
            consumed, produced, ..
        } = &prior.outcome
        {
            for input in consumed {
                pre.remove(input);
            }
            for output in produced {
                pre.insert(output.input, output.clone());
            }
        }
    }
    let expected = ledger_oracle_judge::eval::ledger_rows(record)
        .values()
        .find_map(|a| match a {
            ledger_oracle_judge::eval::Answer::Ran(u) => Some(*u),
            _ => None,
        });
    let seeded = seed::seed(
        &seed::genesis(run.era()),
        run.epoch().unwrap(),
        &params::seed_values(run.pparams()).unwrap(),
        &pre.into_values().collect::<Vec<_>>(),
        &record.accounts_pre,
        record,
    );
    let group = validate::group(&seeded.domain, record.slot, Relay::Held);
    Fixture {
        _seed: seeded,
        group,
        tx: record.tx.0.clone(),
        expected,
    }
}
pub(super) fn reports(
    f: &Fixture,
    raw: &[u8],
) -> (
    Result<Vec<TxEvalResult>, ChainError>,
    Result<Vec<TxEvalResult>, ChainError>,
) {
    let d = f.group.domain();
    let u = MempoolAwareUtxoStore::<DomainAdapter>::new(d.state(), d.mempool());
    (
        dolos_cardano::validate::evaluate_tx(raw, &u, &d.genesis),
        dolos_cardano::validate::estimate_tx(raw, &u, &d.genesis),
    )
}
pub(super) async fn rpc(f: &Fixture, raw: &[u8]) -> rpc::cardano::TxEval {
    let service = crate::serve::grpc::v1::submit::SubmitServiceImpl::new(f.group.domain().clone());
    let response = service
        .eval_tx(Request::new(rpc::submit::EvalTxRequest {
            tx: Some(rpc::submit::AnyChainTx {
                r#type: Some(rpc::submit::any_chain_tx::Type::Raw(raw.to_vec().into())),
            }),
        }))
        .await
        .unwrap()
        .into_inner();
    let rpc::submit::any_chain_eval::Chain::Cardano(report) =
        response.report.unwrap().chain.unwrap();
    report
}
fn assert_guard(report: &[TxEvalResult], want: ledger_oracle_judge::record::Units) {
    assert_eq!(report.len(), 1, "{report:?}");
    let r = &report[0];
    assert_eq!(
        (r.sub_transaction, r.tag, r.index),
        (None, RedeemerTag::Guarding, 0)
    );
    assert!(r.success, "{r:?}");
    assert_eq!((r.units.mem, r.units.steps), (want.mem, want.steps));
}
fn mutation(f: &Fixture, change: impl FnOnce(&mut n::MempoolTransaction<'_>)) -> Vec<u8> {
    let mut tx: n::MempoolTransaction = minicbor::decode(&f.tx).unwrap();
    change(&mut tx);
    minicbor::to_vec(tx).unwrap()
}
#[tokio::test]
async fn original_guards_reach_admission_evaluation_estimation_rpc() {
    for n in [900, 1344] {
        let f = fixture(n);
        let want = f.expected.unwrap();
        let (evaluated, estimated) = reports(&f, &f.tx);
        assert_guard(&evaluated.unwrap(), want);
        assert_guard(&estimated.unwrap(), want);
        let called = f.group.estimate(&f.tx).unwrap();
        assert!(!called[0].1.is_empty());
        let wire = rpc(&f, &f.tx).await;
        assert!(wire.errors.is_empty(), "{wire:?}");
        assert_eq!(wire.redeemers.len(), 1);
        assert_eq!(
            (wire.redeemers[0].purpose, wire.redeemers[0].index),
            (rpc::cardano::RedeemerPurpose::Guarding as i32, 0)
        );
        let units = wire.redeemers[0].ex_units.as_ref().unwrap();
        assert_eq!((units.memory, units.steps), (want.mem, want.steps));
        assert!(f.group.check(&f.tx).is_ok());
        assert!(f.group.receive(&f.tx).is_ok());
        println!("guard caller original ic#{n}: admission/evaluation/estimation/calls/RPC PASS mem={} cpu={}",want.mem,want.steps);
    }
}
#[tokio::test]
async fn missing_guard_redeemer_is_not_silent() {
    let f = fixture(900);
    let raw = mutation(&f, |tx| tx.transaction_witness_set.redeemer = None);
    let original: n::MempoolTransaction = minicbor::decode(&f.tx).unwrap();
    let changed: n::MempoolTransaction = minicbor::decode(&raw).unwrap();
    assert_eq!(
        original.transaction_body.raw_cbor(),
        changed.transaction_body.raw_cbor()
    );
    let (evaluated, estimated) = reports(&f, &raw);
    let calls = f.group.estimate(&raw);
    let wire = rpc(&f, &raw).await;
    println!("guard caller missing redeemer: eval={evaluated:?} estimate={estimated:?} calls={calls:?} rpc={wire:?}");
    let admission = f.group.check(&raw);
    println!("guard caller missing redeemer admission: {admission:?}");
    assert!(admission.is_err(), "admission must reject");
    assert!(
        evaluated.is_err() && estimated.is_err() && calls.is_err() && !wire.errors.is_empty(),
        "missing guard execution must be reported"
    );
}
#[tokio::test]
async fn missing_script_is_reported() {
    let f = fixture(900);
    let raw = mutation(&f, |tx| tx.transaction_witness_set.plutus_v4_script = None);
    let (evaluated, estimated) = reports(&f, &raw);
    let wire = rpc(&f, &raw).await;
    assert!(
        format!("{evaluated:?}").contains("missing required script"),
        "{evaluated:?}"
    );
    assert!(estimated.is_err() && !wire.errors.is_empty());
    assert!(f.group.check(&raw).is_err());
}
#[tokio::test]
async fn wrong_guard_index_is_reported() {
    let f = fixture(900);
    let raw = mutation(&f, |tx| {
        let original = tx.transaction_witness_set.redeemer.as_ref().unwrap();
        let value = original.values().next().unwrap().clone();
        tx.transaction_witness_set.redeemer = Some(
            n::Redeemers(BTreeMap::from([(
                n::RedeemersKey {
                    tag: n::RedeemerTag::Guarding,
                    index: 1,
                },
                value,
            )]))
            .into(),
        );
    });
    let (evaluated, estimated) = reports(&f, &raw);
    let wire = rpc(&f, &raw).await;
    assert!(
        format!("{evaluated:?}").contains("extraneous redeemer"),
        "{evaluated:?}"
    );
    assert!(estimated.is_err() && !wire.errors.is_empty());
    assert!(f.group.check(&raw).is_err());
}
#[tokio::test]
async fn declared_budget_and_estimation_are_distinct() {
    let f = fixture(900);
    let raw = mutation(&f, |tx| {
        let mut r = tx
            .transaction_witness_set
            .redeemer
            .as_ref()
            .unwrap()
            .0
            .clone();
        for value in r.values_mut() {
            value.ex_units = n::ExUnits { mem: 1, steps: 1 };
        }
        tx.transaction_witness_set.redeemer = Some(n::Redeemers(r).into());
    });
    let (evaluated, estimated) = reports(&f, &raw);
    let evaluated = evaluated.unwrap();
    assert!(!evaluated[0].success, "{evaluated:?}");
    assert_guard(&estimated.unwrap(), f.expected.unwrap());
    assert!(rpc(&f, &raw).await.errors.is_empty());
    assert!(f.group.check(&raw).is_err());
}
#[tokio::test]
async fn original_bad_guard_datum_is_rejected() {
    let f = fixture(1330);
    let result = f.group.check(&f.tx);
    assert!(
        format!("{result:?}").contains("DijkstraMalformedGuardDatum"),
        "{result:?}"
    );
    // Context evaluation and phase-one datum-shape checks have different
    // responsibilities.
    assert_guard(&reports(&f, &f.tx).0.unwrap(), f.expected.unwrap());
}

#[tokio::test]
async fn reference_input_provides_guard_code() {
    use ledger_oracle_judge::record::{Bytes, UtxoEntry};
    use pallas::codec::utils::CborWrap;
    let f = fixture(900);
    let mut tx: n::MempoolTransaction = minicbor::decode(&f.tx).unwrap();
    let script = tx
        .transaction_witness_set
        .plutus_v4_script
        .as_ref()
        .unwrap()[0]
        .clone();
    let output = n::TransactionOutput::PostAlonzo(
        n::PostAlonzoTransactionOutput {
            address: [vec![0x60], vec![0x55; 28]].concat().into(),
            value: n::Value::Coin(5_000_000),
            datum_option: None,
            script_ref: Some(CborWrap(n::ScriptRef::PlutusV4Script(script))),
        }
        .into(),
    );
    let entry = UtxoEntry {
        input: TxIn {
            tx: [0x77; 32],
            ix: 0,
        },
        output: Bytes(minicbor::to_vec(&output).unwrap()),
    };
    f.group.seed(&[&entry]);
    tx.transaction_body.reference_inputs = Some(
        vec![n::TransactionInput {
            transaction_id: [0x77; 32].into(),
            index: 0,
        }]
        .try_into()
        .unwrap(),
    );
    tx.transaction_witness_set.plutus_v4_script = None;
    let raw = minicbor::to_vec(tx).unwrap();
    // Body/UTxO mutation is synthetic and unsigned; this proves evaluation, not
    // admission.
    let (evaluated, estimated) = reports(&f, &raw);
    let evaluated = evaluated.unwrap();
    let estimated = estimated.unwrap();
    println!("guard caller reference guard: eval={evaluated:?} estimate={estimated:?}");
    assert_eq!(evaluated.len(), 1);
    assert!(evaluated[0].success);
    assert!(estimated[0].success);
    assert_eq!(
        (evaluated[0].tag, evaluated[0].index),
        (RedeemerTag::Guarding, 0)
    );
    assert_eq!(evaluated[0].units, estimated[0].units);
    let wire = rpc(&f, &raw).await;
    assert!(wire.errors.is_empty(), "{wire:?}");
    assert_eq!(wire.redeemers[0].purpose, 7);
    assert!(!f.group.estimate(&raw).unwrap()[0].1.is_empty());
}
#[tokio::test]
async fn failing_guard_is_reported_by_rpc() {
    let f = fixture(900);
    let raw = mutation(&f, |tx| {
        // Known synthetic explicit-error V4 program; no signing or admission
        // claim.
        let script = n::PlutusScript::<4>(hex::decode("450101002601").unwrap().into());
        tx.transaction_body.guards = Some(n::Guards::Credentials(
            vec![n::StakeCredential::ScriptHash(script.compute_hash())]
                .try_into()
                .unwrap(),
        ));
        tx.transaction_witness_set.plutus_v4_script = Some(vec![script].try_into().unwrap());
    });
    let (evaluated, estimated) = reports(&f, &raw);
    assert!(!evaluated.unwrap()[0].success);
    assert!(!estimated.unwrap()[0].success);
    let wire = rpc(&f, &raw).await;
    assert_eq!(wire.errors.len(), 1);
    assert_eq!(wire.errors[0].purpose, Some(7));
    assert_eq!(wire.errors[0].index, Some(0));
    assert!(wire.errors[0].msg.contains("Guarding[0]"));
}
#[tokio::test]
async fn nonzero_guard_index_is_preserved() {
    let f = fixture(900);
    let raw = mutation(&f, |tx| {
        let n::Guards::Credentials(old) = tx.transaction_body.guards.as_ref().unwrap() else {
            panic!()
        };
        let mut guards = vec![n::StakeCredential::AddrKeyhash([0x66; 28].into())];
        guards.extend(old.iter().cloned());
        tx.transaction_body.guards = Some(n::Guards::Credentials(guards.try_into().unwrap()));
        let value = tx
            .transaction_witness_set
            .redeemer
            .as_ref()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .clone();
        tx.transaction_witness_set.redeemer = Some(
            n::Redeemers(BTreeMap::from([(
                n::RedeemersKey {
                    tag: n::RedeemerTag::Guarding,
                    index: 1,
                },
                value,
            )]))
            .into(),
        );
    });
    // Synthetic extra key credential; evaluation only, no signature/admission
    // claim.
    let (evaluated, estimated) = reports(&f, &raw);
    let evaluated = evaluated.unwrap();
    let estimated = estimated.unwrap();
    // Adding a credential changes context cost; the original exact budget need
    // not suffice.
    assert!(estimated[0].success, "{estimated:?}");
    assert_eq!(
        (estimated[0].tag, estimated[0].index),
        (RedeemerTag::Guarding, 1)
    );
    assert_eq!(
        (evaluated[0].tag, evaluated[0].index),
        (RedeemerTag::Guarding, 1)
    );
    let wire = rpc(&f, &raw).await;
    assert!(wire.errors.is_empty(), "{wire:?}");
    assert_eq!((wire.redeemers[0].purpose, wire.redeemers[0].index), (7, 1));
}
#[tokio::test]
async fn missing_reference_input_is_reported() {
    let f = fixture(900);
    let raw = mutation(&f, |tx| {
        tx.transaction_body.reference_inputs = Some(
            vec![n::TransactionInput {
                transaction_id: [0x78; 32].into(),
                index: 0,
            }]
            .try_into()
            .unwrap(),
        );
    });
    let (evaluated, estimated) = reports(&f, &raw);
    assert!(
        format!("{evaluated:?}").contains("resolved Input not found"),
        "{evaluated:?}"
    );
    assert!(estimated.is_err());
    assert!(!rpc(&f, &raw).await.errors.is_empty());
}

#[tokio::test]
async fn missing_child_guard_redeemer_is_not_silent() {
    let f = fixture(900);
    let raw = mutation(&f, |tx| {
        let mut sub: n::SubTransaction =
            minicbor::decode(&[0x83, 0xa2, 0x00, 0x80, 0x01, 0x80, 0xa0, 0xf6]).unwrap();
        sub.sub_transaction_body.guards = tx.transaction_body.guards.clone();
        tx.transaction_body.guards = None;
        tx.transaction_body.sub_transactions = Some(vec![sub].try_into().unwrap());
        tx.transaction_witness_set.redeemer = None;
    });
    // Synthetic child body: evaluation only. The guard script remains in the
    // parent witness set.
    let (evaluated, estimated) = reports(&f, &raw);
    let calls = f.group.estimate(&raw);
    let wire = rpc(&f, &raw).await;
    for result in [&evaluated, &estimated] {
        let error = format!(
            "{:?}",
            result.as_ref().expect_err("missing child guard redeemer")
        );
        assert!(error.contains("Guarding[0]"), "{error}");
    }
    assert!(calls.is_err());
    assert!(!wire.errors.is_empty(), "{wire:?}");
    assert!(wire.errors[0].msg.contains("Guarding[0]"));
}

#[tokio::test]
async fn missing_guard_script_and_redeemer_are_reported() {
    let f = fixture(900);
    let raw = mutation(&f, |tx| {
        tx.transaction_witness_set.redeemer = None;
        tx.transaction_witness_set.plutus_v4_script = None;
    });
    let (evaluated, estimated) = reports(&f, &raw);
    for result in [&evaluated, &estimated] {
        let error = format!("{:?}", result.as_ref().expect_err("missing guard script"));
        assert!(error.contains("missing required script"), "{error}");
    }
    assert!(f.group.estimate(&raw).is_err());
    let wire = rpc(&f, &raw).await;
    assert!(!wire.errors.is_empty(), "{wire:?}");
    assert!(wire.errors[0].msg.contains("missing required script"));
}

#[tokio::test]
async fn native_and_key_only_transactions_need_no_plutus_redeemers() {
    // Original accepted corpus records: native guards, a required native guard
    // with null data, and a key-only transaction.
    for number in [855, 869, 899] {
        let f = fixture(number);
        let (evaluated, estimated) = reports(&f, &f.tx);
        assert!(evaluated.unwrap().is_empty());
        assert!(estimated.unwrap().is_empty());
        assert!(f.group.estimate(&f.tx).unwrap().is_empty());
        let wire = rpc(&f, &f.tx).await;
        assert!(wire.errors.is_empty(), "record {number}: {wire:?}");
        assert!(wire.redeemers.is_empty());
        assert_eq!(wire.ex_units, Some(rpc::cardano::ExUnits::default()));
        assert!(f.group.check(&f.tx).is_ok());
    }
}
