// Both RPC versions run these assertions against the same immutable capture.
use super::*;
use dolos_cardano::{
    model::{EpochState, PParamValue},
    SingletonEntity,
};
use dolos_testing::toy_domain::ToyDomain;
use pallas::{
    codec::minicbor,
    ledger::{
        primitives::dijkstra as n,
        traverse::{Era, MultiEraTx},
    },
};
use u5c::submit::submit_service_server::SubmitService;

#[allow(dead_code)]
mod capture {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/musashi_submission.rs"
    ));
}

fn request(bytes: &[u8]) -> Request<EvalTxRequest> {
    Request::new(EvalTxRequest {
        tx: Some(AnyChainTx {
            r#type: Some(any_chain_tx::Type::Raw(bytes.to_vec().into())),
        }),
    })
}

async fn evaluate<D: Domain + ChainReads>(domain: &D, bytes: &[u8]) -> u5c::cardano::TxEval {
    let response = SubmitServiceImpl::new(domain.clone())
        .eval_tx(request(bytes))
        .await
        .unwrap()
        .into_inner();
    let Chain::Cardano(report) = response.report.unwrap().chain.unwrap();
    report
}

fn exact(report: &u5c::cardano::TxEval) {
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let units = report.ex_units.as_ref().unwrap();
    assert_eq!((units.memory, units.steps), (18485, 4805428));
    assert_eq!(report.redeemers.len(), 1);
    let r = &report.redeemers[0];
    assert_eq!(
        (r.purpose, r.index),
        (u5c::cardano::RedeemerPurpose::Cert as i32, 0)
    );
    assert_eq!(r.ex_units, report.ex_units);
}

// Synthetic witness mutations only; the original body is preserved exactly.
fn derivative(bytes: &[u8], unsigned: bool, budget: Option<n::ExUnits>) -> Vec<u8> {
    let decoded = MultiEraTx::decode_for_era(Era::Dijkstra, bytes).unwrap();
    let mut tx = decoded.as_dijkstra().unwrap().clone();
    if unsigned {
        tx.transaction_witness_set.vkeywitness = None;
    }
    if let Some(budget) = budget {
        for r in tx
            .transaction_witness_set
            .redeemer
            .as_mut()
            .unwrap()
            .0
            .values_mut()
        {
            r.ex_units = budget;
        }
    }
    let encoded = minicbor::to_vec(tx.to_mempool_transaction()).unwrap();
    assert_eq!(
        MultiEraTx::decode_for_era(Era::Dijkstra, &encoded)
            .unwrap()
            .hash(),
        decoded.hash()
    );
    encoded
}

#[tokio::test]
async fn estimation_rpc_captured_registration() {
    let (domain, bytes) = capture::fixture(true);
    exact(&evaluate(&domain, &bytes).await);
    assert!(!domain.mempool().has_pending());
}

#[tokio::test]
async fn estimation_rpc_unsigned() {
    let (domain, bytes) = capture::fixture(true);
    let unsigned = derivative(&bytes, true, None);
    exact(&evaluate(&domain, &unsigned).await);
    let error = domain
        .validate_tx(&domain.read_chain(), &unsigned)
        .unwrap_err();
    assert!(
        format!("{error:?}").contains("VKWitnessMissing"),
        "{error:?}"
    );
    assert!(!domain.mempool().has_pending());
}

#[tokio::test]
async fn estimation_rpc_placeholder_budgets() {
    let (domain, bytes) = capture::fixture(true);
    for budget in [
        n::ExUnits { mem: 0, steps: 0 },
        n::ExUnits {
            mem: 18484,
            steps: 4805428,
        },
        n::ExUnits {
            mem: 18485,
            steps: 4805427,
        },
        n::ExUnits {
            mem: u64::MAX,
            steps: u64::MAX,
        },
    ] {
        let changed = derivative(&bytes, false, Some(budget));
        exact(&evaluate(&domain, &changed).await);
    }
    assert!(!domain.mempool().has_pending());
}

#[tokio::test]
async fn estimation_rpc_unsigned_zero_budget() {
    let (domain, bytes) = capture::fixture(true);
    let changed = derivative(&bytes, true, Some(n::ExUnits { mem: 0, steps: 0 }));
    let before = changed.clone();
    exact(&evaluate(&domain, &changed).await);
    assert_eq!(changed, before);
    assert!(!domain.mempool().has_pending());
}

#[tokio::test]
async fn estimation_rpc_exhaustion_retains_failure_and_units() {
    let (domain, bytes) = capture::fixture(true);
    let mut epoch = dolos_cardano::load_epoch::<ToyDomain>(domain.state()).unwrap();
    epoch
        .pparams
        .unwrap_live_mut()
        .set(PParamValue::MaxTxExUnits(n::ExUnits { mem: 0, steps: 0 }));
    let writer = domain.state().start_writer().unwrap();
    writer
        .write_entity_typed(&EpochState::singleton_key(), &epoch)
        .unwrap();
    writer.commit().unwrap();
    let report = evaluate(&domain, &bytes).await;
    assert_eq!(report.redeemers.len(), 1, "{report:?}");
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.msg.contains("Out of budget")),
        "{report:?}"
    );
    assert_eq!(
        report.redeemers[0].purpose,
        u5c::cardano::RedeemerPurpose::Cert as i32
    );
    let units = report.redeemers[0].ex_units.as_ref().unwrap();
    assert!(units.memory > 0 && units.steps > 0);
    assert!(!domain.mempool().has_pending());
}

#[tokio::test]
async fn estimation_rpc_missing_input_and_malformed_bytes() {
    let (domain, bytes) = capture::fixture(true);
    let decoded = MultiEraTx::decode_for_era(Era::Dijkstra, &bytes).unwrap();
    let refs = decoded.inputs().iter().map(TxoRef::from).collect();
    let delta = UtxoSetDelta {
        consumed_utxo: domain.state().get_utxos(refs).unwrap(),
        ..Default::default()
    };
    let writer = domain.state().start_writer().unwrap();
    writer.apply_utxoset(&delta).unwrap();
    writer.commit().unwrap();
    for bytes in [bytes, vec![0xff]] {
        let report = evaluate(&domain, &bytes).await;
        assert!(!report.errors.is_empty());
        assert!(report.redeemers.is_empty());
    }
    assert!(!domain.mempool().has_pending());
}

#[tokio::test]
async fn estimation_rpc_request_errors_and_submission_validation() {
    let (domain, bytes) = capture::fixture(true);
    let service = SubmitServiceImpl::new(domain.clone());
    for tx in [None, Some(AnyChainTx { r#type: None })] {
        let error = service
            .eval_tx(Request::new(EvalTxRequest { tx }))
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
    }
    let unsigned = derivative(&bytes, true, None);
    let submit = |bytes: Vec<u8>| {
        Request::new(SubmitTxRequest {
            tx: Some(AnyChainTx {
                r#type: Some(any_chain_tx::Type::Raw(bytes.into())),
            }),
        })
    };
    let error = service.submit_tx(submit(unsigned)).await.unwrap_err();
    assert!(
        error.message().contains("vkey witness is missing"),
        "{error}"
    );
    assert!(!domain.mempool().has_pending());
    service.submit_tx(submit(bytes.clone())).await.unwrap();
    assert_eq!(domain.mempool().peek_pending()[0].payload().1, bytes);
}

#[tokio::test]
async fn estimation_rpc_earlier_conway() {
    use pallas::codec::utils::KeepRaw;
    use pallas::ledger::primitives::conway::{LegacyTransactionOutput, TransactionOutput};
    use std::sync::Arc;
    let bytes = capture::bytes("conway-control.hex");
    let tx = MultiEraTx::decode_for_era(Era::Conway, &bytes).unwrap();
    // Reconstructed input from Pallas's successful_mainnet_tx, as in the existing
    // Dolos submission regression. This is not a captured Conway state snapshot.
    let output = TransactionOutput::Legacy(KeepRaw::from(LegacyTransactionOutput {
        address: hex::decode("015c5c318d01f729e205c95eb1b02d623dd10e78ea58f72d0c13f892b2e8904edc699e2f0ce7b72be7cec991df651a222e2ae9244eb5975cba").unwrap().into(),
        amount: pallas::ledger::primitives::alonzo::Value::Coin(20_000_000),
        datum_hash: None,
    }));
    let mut delta = UtxoSetDelta::default();
    delta.produced_utxo.insert(
        TxoRef::from(&tx.inputs()[0]),
        Arc::new(EraCbor(7, minicbor::to_vec(output).unwrap())),
    );
    let mut genesis = dolos_cardano::include::mainnet::load();
    genesis.force_protocol = Some(9);
    let domain = ToyDomain::new_with_genesis(Arc::new(genesis), Some(delta), None);
    let writer = domain.state().start_writer().unwrap();
    writer.set_cursor(ChainPoint::Slot(137806612)).unwrap();
    writer.commit().unwrap();
    let report = evaluate(&domain, &bytes).await;
    assert!(report.errors.is_empty(), "{report:?}");
    assert!(report.redeemers.is_empty());
    assert_eq!(report.ex_units, Some(ExUnits::default()));
    assert!(!domain.mempool().has_pending());
    assert_eq!(
        domain
            .receive_tx("conway-regression", &domain.read_chain(), &bytes)
            .unwrap(),
        tx.hash()
    );
}

#[tokio::test]
async fn estimation_rpc_resolves_pending_and_inflight_outputs_without_mutation() {
    use pallas::ledger::traverse::MultiEraBlock;
    for inflight in [false, true] {
        let (inner, bytes) = capture::fixture(true);
        let domain = RpcDomain::new(inner);
        let producer_block = capture::bytes("1401345.block");
        let block = MultiEraBlock::decode(&producer_block).unwrap();
        let producer = block.txs().remove(0);
        let decoded = MultiEraTx::decode_for_era(Era::Dijkstra, &bytes).unwrap();
        let refs = decoded.requires().iter().map(TxoRef::from).collect();
        let inputs = domain.state().get_utxos(refs).unwrap();
        let writer = domain.state().start_writer().unwrap();
        writer
            .apply_utxoset(&UtxoSetDelta {
                consumed_utxo: inputs,
                ..Default::default()
            })
            .unwrap();
        writer.commit().unwrap();
        // Synthetic mempool prestate: original captured producer, placed directly
        // in the store to isolate lookup. This does not validate that producer.
        let payload =
            minicbor::to_vec(producer.as_dijkstra().unwrap().to_mempool_transaction()).unwrap();
        domain
            .mempool()
            .receive(MempoolTx::new(
                producer.hash(),
                EraCbor(8, payload.clone()),
                vec![],
            ))
            .unwrap();
        if inflight {
            domain.mempool().mark_inflight(&[producer.hash()]).unwrap();
        }
        assert_eq!(
            domain.mempool().peek_pending().len(),
            usize::from(!inflight)
        );
        assert_eq!(
            domain.mempool().peek_inflight().len(),
            usize::from(inflight)
        );
        let cursor = domain.state().read_cursor().unwrap();
        let unsigned = derivative(&bytes, true, Some(n::ExUnits { mem: 0, steps: 0 }));
        exact(&evaluate(&domain, &unsigned).await);
        assert_eq!(domain.state().read_cursor().unwrap(), cursor);
        let mut remaining = domain.mempool().peek_pending();
        remaining.extend(domain.mempool().peek_inflight());
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].payload().1, payload);
    }
}

// Use a production mempool: ToyDomain intentionally ignores inflight transitions.
#[derive(Clone)]
struct RpcDomain {
    inner: ToyDomain,
    mempool: dolos_core::builtin::EphemeralMempool,
}
impl RpcDomain {
    fn new(inner: ToyDomain) -> Self {
        Self {
            inner,
            mempool: dolos_core::builtin::EphemeralMempool::new(),
        }
    }
}
impl Domain for RpcDomain {
    type Entity = <ToyDomain as Domain>::Entity;
    type EntityDelta = <ToyDomain as Domain>::EntityDelta;
    type Chain = <ToyDomain as Domain>::Chain;
    type WorkUnit = <ToyDomain as Domain>::WorkUnit;
    type Wal = <ToyDomain as Domain>::Wal;
    type State = <ToyDomain as Domain>::State;
    type Archive = <ToyDomain as Domain>::Archive;
    type TipSubscription = <ToyDomain as Domain>::TipSubscription;
    type Mempool = dolos_core::builtin::EphemeralMempool;
    fn storage_config(&self) -> &config::StorageConfig {
        self.inner.storage_config()
    }
    fn sync_config(&self) -> &config::SyncConfig {
        self.inner.sync_config()
    }
    fn genesis(&self) -> std::sync::Arc<Genesis> {
        self.inner.genesis()
    }
    fn read_chain(&self) -> std::sync::RwLockReadGuard<'_, Self::Chain> {
        self.inner.read_chain()
    }
    fn write_chain(&self) -> std::sync::RwLockWriteGuard<'_, Self::Chain> {
        self.inner.write_chain()
    }
    fn wal(&self) -> &Self::Wal {
        self.inner.wal()
    }
    fn state(&self) -> &Self::State {
        self.inner.state()
    }
    fn archive(&self) -> &Self::Archive {
        self.inner.archive()
    }
    fn mempool(&self) -> &Self::Mempool {
        &self.mempool
    }
    fn watch_tip(&self, from: Option<ChainPoint>) -> Result<Self::TipSubscription, DomainError> {
        self.inner.watch_tip(from)
    }
    fn notify_tip(&self, tip: TipEvent) {
        self.inner.notify_tip(tip);
    }
}
impl pallas::interop::utxorpc::LedgerContext for RpcDomain {
    fn get_utxos(
        &self,
        refs: &[pallas::interop::utxorpc::TxoRef],
    ) -> Option<pallas::interop::utxorpc::UtxoMap> {
        self.inner.get_utxos(refs)
    }
    fn get_slot_timestamp(&self, slot: u64) -> Option<u64> {
        self.inner.get_slot_timestamp(slot)
    }
}

#[tokio::test]
async fn estimation_rpc_submission_still_enforces_phase_two() {
    use pallas::{
        crypto::{hash::Hasher, key::ed25519::SecretKey},
        ledger::{
            addresses::{Network, ShelleyAddress, ShelleyDelegationPart, ShelleyPaymentPart},
            primitives::conway::LanguageViews,
        },
    };
    use std::{collections::BTreeMap, sync::Arc};
    let (domain, bytes) = capture::fixture(true);
    let decoded = MultiEraTx::decode_for_era(Era::Dijkstra, &bytes).unwrap();
    let mut tx = decoded.as_dijkstra().unwrap().clone();
    // Synthetic owner and funding address; never a network signing key. Keep
    // the captured script/parameters and balance, and recompute integrity and
    // signature so an insufficient budget must reach phase two.
    let key = SecretKey::from([93; 32]);
    let owner = Hasher::<224>::hash(key.public_key().as_ref());
    let address = ShelleyAddress::new(
        Network::Testnet,
        ShelleyPaymentPart::Key(owner),
        ShelleyDelegationPart::Null,
    )
    .to_vec();
    let inputs: Vec<_> = decoded.inputs().iter().map(TxoRef::from).collect();
    assert_eq!(inputs.len(), 1);
    let mut delta = UtxoSetDelta::default();
    for (reference, raw) in domain.state().get_utxos(inputs).unwrap() {
        let mut output: n::TransactionOutput = minicbor::decode(raw.cbor()).unwrap();
        match &mut output {
            n::TransactionOutput::PostAlonzo(output) => output.address = address.clone().into(),
            n::TransactionOutput::Legacy(output) => output.address = address.clone().into(),
        }
        delta.produced_utxo.insert(
            reference,
            Arc::new(EraCbor(8, minicbor::to_vec(output).unwrap())),
        );
    }
    let writer = domain.state().start_writer().unwrap();
    writer.apply_utxoset(&delta).unwrap();
    writer.commit().unwrap();
    // Explicit guards describe the required signatories independently of witnesses.
    if tx.transaction_body.guards.is_some() {
        tx.transaction_body.guards =
            Some(n::Guards::AddrKeyhashes(vec![owner].try_into().unwrap()));
    }
    let pp = dolos_cardano::load_effective_pparams::<ToyDomain>(domain.state()).unwrap();
    let seal = |tx: &mut n::BlockTransaction<'_>, budget| {
        for r in tx
            .transaction_witness_set
            .redeemer
            .as_mut()
            .unwrap()
            .0
            .values_mut()
        {
            r.ex_units = budget;
        }
        let mut integrity =
            minicbor::to_vec(tx.transaction_witness_set.redeemer.as_ref().unwrap()).unwrap();
        if let Some(datums) = &tx.transaction_witness_set.plutus_data {
            integrity.extend(minicbor::to_vec(datums).unwrap());
        }
        integrity.extend(
            minicbor::to_vec(LanguageViews(BTreeMap::from([(
                2,
                pp.cost_models_plutus_v3().unwrap(),
            )])))
            .unwrap(),
        );
        tx.transaction_body.script_data_hash = Some(Hasher::<256>::hash(&integrity));
        let hash = Hasher::<256>::hash(&minicbor::to_vec(&tx.transaction_body).unwrap());
        tx.transaction_witness_set.vkeywitness = n::NonEmptySet::from_vec(vec![n::VKeyWitness {
            vkey: key.public_key().as_ref().to_vec().into(),
            signature: key.sign(hash).as_ref().to_vec().into(),
        }]);
        minicbor::to_vec(tx.to_mempool_transaction()).unwrap()
    };
    let signed = seal(
        &mut tx,
        n::ExUnits {
            mem: 18485,
            steps: 4805428,
        },
    );
    domain.validate_tx(&domain.read_chain(), &signed).unwrap();
    let insufficient = seal(&mut tx, n::ExUnits { mem: 0, steps: 0 });
    let error = domain
        .validate_tx(&domain.read_chain(), &insufficient)
        .unwrap_err();
    assert!(
        matches!(
            error,
            DomainError::ChainError(ChainError::Phase2ValidationRejected(_))
        ),
        "{error:?}"
    );
    let service = SubmitServiceImpl::new(domain.clone());
    let error = service
        .submit_tx(Request::new(SubmitTxRequest {
            tx: Some(AnyChainTx {
                r#type: Some(any_chain_tx::Type::Raw(insufficient.clone().into())),
            }),
        }))
        .await
        .unwrap_err();
    assert!(error.message().contains("phase-2"), "{error}");
    exact(&evaluate(&domain, &insufficient).await);
    assert!(!domain.mempool().has_pending());
}

#[tokio::test]
async fn every_handler_reads_the_stores_on_the_blocking_pool() {
    use crate::serve::grpc::read_counting::ReadCountingDomain;

    let (toy, bytes) = capture::fixture(true);
    let domain = ReadCountingDomain::new(toy);
    let service = SubmitServiceImpl::new(domain.clone());
    let mut reads = Vec::new();

    domain.take();
    service.eval_tx(request(&bytes)).await.unwrap();
    reads.push(("eval_tx", domain.take()));

    let hash = service
        .submit_tx(Request::new(SubmitTxRequest {
            tx: Some(AnyChainTx {
                r#type: Some(any_chain_tx::Type::Raw(bytes.into())),
            }),
        }))
        .await
        .unwrap()
        .into_inner()
        .r#ref;
    reads.push(("submit_tx", domain.take()));

    service
        .wait_for_tx(Request::new(WaitForTxRequest { r#ref: vec![hash] }))
        .await
        .unwrap();
    reads.push(("wait_for_tx", domain.take()));

    assert_eq!(
        reads,
        [
            ("eval_tx", (0, true)),
            ("submit_tx", (0, true)),
            ("wait_for_tx", (0, true)),
        ]
    );
}

/// Two mempool transactions, the first minting `alpha` and the second `beta`.
fn minting_txs() -> Vec<MempoolTx> {
    let (blocks, _, _) = dolos_testing::synthetic::build_synthetic_blocks(
        dolos_testing::synthetic::SyntheticBlockConfig {
            block_count: 2,
            txs_per_block: 1,
            asset_names_by_block: vec!["alpha".into(), "beta".into()],
            ..Default::default()
        },
    );
    blocks
        .iter()
        .map(|raw| {
            let block = pallas::ledger::traverse::MultiEraBlock::decode(raw).unwrap();
            let tx = block.txs().remove(0);
            MempoolTx::new(tx.hash(), EraCbor(tx.era().into(), tx.encode()), vec![])
        })
        .collect()
}

/// A predicate that admits a transaction minting the synthetic asset `name`.
// The asset pattern fields are optional at v1 and v1beta and plain at v1alpha.
#[allow(clippy::useless_conversion)]
fn mints(name: &str) -> TxPredicate {
    TxPredicate {
        r#match: Some(AnyChainTxPattern {
            chain: Some(any_chain_tx_pattern::Chain::Cardano(
                u5c::cardano::TxPattern {
                    mints_asset: Some(u5c::cardano::AssetPattern {
                        policy_id: bytes::Bytes::from(vec![1u8; 28]).into(),
                        asset_name: bytes::Bytes::from(name.as_bytes().to_vec()).into(),
                    }),
                    ..Default::default()
                },
            )),
        }),
        ..Default::default()
    }
}

fn watch_request(predicate: Option<TxPredicate>, paths: &[&str]) -> WatchMempoolRequest {
    let mut request = WatchMempoolRequest {
        predicate,
        field_mask: Some(Default::default()),
    };
    request.field_mask.as_mut().unwrap().paths = paths.iter().map(|p| p.to_string()).collect();
    if paths.is_empty() {
        request.field_mask = None;
    }
    request
}

/// The answers WatchMempool gives while the mempool receives `txs`, with the
/// reads each made on the runtime thread and whether any ran elsewhere.
async fn watched(
    request: WatchMempoolRequest,
    txs: &[MempoolTx],
) -> (Vec<Result<TxInMempool, Status>>, (usize, bool)) {
    use crate::serve::grpc::read_counting::ReadCountingDomain;

    let domain = ReadCountingDomain::new(ToyDomain::new(None, None));
    let updates = futures_util::stream::iter(
        txs.iter()
            .cloned()
            .map(|tx| Ok(MempoolEvent { tx }))
            .collect::<Vec<_>>(),
    );
    let answers = watch_mempool_answers(domain.clone(), updates, request)
        .map(|answer| answer.map(|x| x.tx.expect("every answer holds its transaction")))
        .collect::<Vec<_>>()
        .await;
    (answers, domain.take())
}

fn refs(answers: &[Result<TxInMempool, Status>]) -> Vec<Vec<u8>> {
    answers
        .iter()
        .map(|x| x.as_ref().unwrap().r#ref.to_vec())
        .collect()
}

#[tokio::test]
async fn watch_mempool_answers_only_the_transactions_its_predicate_admits() {
    let txs = minting_txs();
    let [alpha, beta] = [0, 1].map(|i| txs[i].hash().to_vec());
    let not_alpha = TxPredicate {
        not: vec![mints("alpha")],
        ..Default::default()
    };

    for (predicate, expected) in [
        (None, vec![alpha.clone(), beta.clone()]),
        (Some(mints("beta")), vec![beta.clone()]),
        (Some(mints("alpha")), vec![alpha.clone()]),
        (Some(not_alpha), vec![beta.clone()]),
        (Some(mints("gamma")), vec![]),
    ] {
        let (answers, _) = watched(watch_request(predicate.clone(), &[]), &txs).await;
        assert_eq!(refs(&answers), expected, "{predicate:?}");
    }
}

#[tokio::test]
async fn watch_mempool_answers_hold_what_the_mask_names() {
    let txs = minting_txs();
    let payload = |i: usize| txs[i].payload().cbor().to_vec();
    let parsed_hash = |x: &TxInMempool| {
        x.parsed_state
            .as_ref()
            .map(|tx_in_mempool::ParsedState::Cardano(tx)| tx_hash(tx))
    };

    let (unmasked, _) = watched(watch_request(None, &[]), &txs).await;
    assert_eq!(unmasked.len(), 2);
    for (i, answer) in unmasked.iter().enumerate() {
        let answer = answer.as_ref().unwrap();
        assert_eq!(answer.r#ref.to_vec(), txs[i].hash().to_vec());
        assert_eq!(answer.native_bytes.to_vec(), payload(i));
        assert_eq!(answer.stage, Stage::Mempool as i32);
        assert_eq!(parsed_hash(answer), Some(txs[i].hash().to_vec()));
    }

    let only = |answer: &TxInMempool| {
        (
            !answer.r#ref.is_empty(),
            !answer.native_bytes.is_empty(),
            answer.stage != 0,
            answer.parsed_state.is_some(),
        )
    };
    for (paths, parts) in [
        (&["tx.native_bytes"][..], (false, true, false, false)),
        (&["tx.cardano.hash"][..], (false, false, false, true)),
        (&["stage"][..], (false, false, true, false)),
        (&["tx.ref", "tx.stage"][..], (true, false, true, false)),
        (&["tx"][..], (true, true, true, true)),
    ] {
        let (answers, _) = watched(watch_request(None, paths), &txs).await;
        assert_eq!(answers.len(), 2, "{paths:?}");
        for answer in &answers {
            assert_eq!(only(answer.as_ref().unwrap()), parts, "{paths:?}");
        }
    }

    let (filtered, _) = watched(watch_request(Some(mints("beta")), &["tx.ref"]), &txs).await;
    assert_eq!(refs(&filtered), vec![txs[1].hash().to_vec()]);
    assert_eq!(
        only(filtered[0].as_ref().unwrap()),
        (true, false, false, false)
    );
}

#[tokio::test]
async fn watch_mempool_reads_the_stores_on_the_blocking_pool() {
    let txs = minting_txs();

    let (_, reads) = watched(watch_request(Some(mints("beta")), &["tx.ref"]), &txs).await;
    assert_eq!(reads, (0, true));

    let (_, reads) = watched(watch_request(None, &["tx.native_bytes"]), &txs).await;
    assert_eq!(reads, (0, false));
}

#[tokio::test]
async fn watch_mempool_reports_a_payload_that_does_not_decode() {
    let undecodable = MempoolTx::new(Hash::new([7u8; 32]), EraCbor(7, vec![0xff]), vec![]);

    let (bytes_only, _) = watched(
        watch_request(None, &["tx.native_bytes"]),
        std::slice::from_ref(&undecodable),
    )
    .await;
    assert_eq!(
        bytes_only[0].as_ref().unwrap().native_bytes.to_vec(),
        vec![0xff]
    );

    for request in [
        watch_request(None, &[]),
        watch_request(Some(mints("beta")), &["tx.ref"]),
    ] {
        let (answers, _) = watched(request, std::slice::from_ref(&undecodable)).await;
        assert_eq!(answers.len(), 1);
        assert_eq!(
            answers[0].as_ref().unwrap_err().code(),
            tonic::Code::Internal
        );
    }
}
