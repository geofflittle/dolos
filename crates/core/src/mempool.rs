use super::*;
use crate::TagDimension;

pub use pallas::ledger::validate::phase2::EvalReport;

use futures_core::Stream;
use std::pin::Pin;
use tracing::{debug, warn};

#[derive(Debug)]
pub struct MempoolTx {
    pub hash: TxHash,
    pub payload: EraCbor,
    pub stage: MempoolTxStage,
    pub confirmations: u32,
    pub non_confirmations: u32,
    pub confirmed_at: Option<ChainPoint>,

    // this might be empty if the tx is cloned
    pub report: Option<EvalReport>,
}

impl PartialEq for MempoolTx {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
    }
}

impl Eq for MempoolTx {}

impl Clone for MempoolTx {
    fn clone(&self) -> Self {
        Self {
            hash: self.hash,
            payload: self.payload.clone(),
            stage: self.stage.clone(),
            confirmations: self.confirmations,
            non_confirmations: self.non_confirmations,
            confirmed_at: self.confirmed_at.clone(),
            report: None,
        }
    }
}

impl MempoolTx {
    pub fn new(hash: TxHash, payload: EraCbor, report: EvalReport) -> Self {
        Self {
            hash,
            payload,
            stage: MempoolTxStage::Pending,
            confirmations: 0,
            non_confirmations: 0,
            confirmed_at: None,
            report: Some(report),
        }
    }

    pub fn confirm(&mut self, point: &ChainPoint) {
        self.stage = MempoolTxStage::Confirmed;
        self.confirmations += 1;
        self.non_confirmations = 0;
        if self.confirmed_at.is_none() {
            self.confirmed_at = Some(point.clone());
        }
    }

    pub fn retry(&mut self) {
        self.stage = MempoolTxStage::Pending;
        self.confirmations = 0;
        self.non_confirmations = 0;
        self.confirmed_at = None;
    }

    pub fn mark_stale(&mut self) {
        self.non_confirmations += 1;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MempoolTxStage {
    Pending,
    Propagated,
    Acknowledged,
    Confirmed,
    Finalized,
    Dropped,
    RolledBack,
    Unknown,
}

#[derive(Clone)]
pub struct MempoolEvent {
    pub tx: MempoolTx,
}

pub struct TxStatus {
    pub stage: MempoolTxStage,
    pub confirmations: u32,
    pub non_confirmations: u32,
    pub confirmed_at: Option<ChainPoint>,
}

pub struct MempoolPage {
    pub items: Vec<MempoolTx>,
    pub next_cursor: Option<u64>,
}

#[derive(Debug, Error)]
pub enum MempoolError {
    #[error("internal error: {0}")]
    Internal(#[from] Box<dyn std::error::Error + Send + Sync>),

    #[error("traverse error: {0}")]
    TraverseError(#[from] pallas::ledger::traverse::Error),

    #[error("decode error: {0}")]
    DecodeError(#[from] pallas::codec::minicbor::decode::Error),

    #[error(transparent)]
    StateError(#[from] StateError),

    #[error("plutus not supported")]
    PlutusNotSupported,

    #[error("invalid tx: {0}")]
    InvalidTx(String),

    #[error("pparams not available")]
    PParamsNotAvailable,

    #[error("duplicate tx")]
    DuplicateTx,
}

/// Durable storage for the transaction submission lifecycle.
///
/// A `MempoolStore` tracks transactions through a linear state machine with
/// one rollback edge:
///
/// ```text
///   Pending → Propagated → Acknowledged → Confirmed → Finalized
///                                             ↓
///                                          Pending   (rollback via `confirm(unseen)`)
/// ```
///
/// - **Pending** — received but not yet submitted to a peer.
/// - **Propagated** — sent to a peer, awaiting acknowledgement.
/// - **Acknowledged** — the peer accepted the transaction.
/// - **Confirmed** — observed on-chain; confirmation counter tracks depth.
/// - **Finalized** — enough confirmations accumulated; no longer actively
///   tracked.
///
/// The "inflight" umbrella covers all post-pending, pre-finalized stages
/// (Propagated, Acknowledged, Confirmed).
///
/// Implementors must be thread-safe (`Clone + Send + Sync`) and persist state
/// durably so that a restart does not lose in-progress transactions.
pub trait MempoolStore: Clone + Send + Sync + 'static {
    type Stream: futures_core::Stream<Item = Result<MempoolEvent, MempoolError>>
        + Unpin
        + Send
        + Sync;

    /// Append a transaction to the pending queue.
    ///
    /// Implementors must persist the transaction and emit a `Pending` event
    /// via [`subscribe`](Self::subscribe). Returns
    /// [`DuplicateTx`](MempoolError::DuplicateTx) if a transaction with the
    /// same hash is already pending.
    fn receive(&self, tx: MempoolTx) -> Result<(), MempoolError>;

    /// Returns `true` if there is at least one transaction in the pending
    /// queue.
    fn has_pending(&self) -> bool;

    /// Returns all transactions from the pending queue in the order they were
    /// first admitted. Does not remove them.
    fn peek_pending(&self) -> Vec<MempoolTx>;

    /// Moves matching transactions from pending to the inflight table
    /// (initial sub-stage: `Propagated`).
    ///
    /// Implementors must remove them from the pending queue and begin tracking
    /// them as inflight. Hashes not found in pending are silently ignored.
    fn mark_inflight(&self, hashes: &[TxHash]) -> Result<(), MempoolError>;

    /// Transitions `Propagated` transactions to `Acknowledged`.
    ///
    /// Only transitions transactions currently in `Propagated`; other stages
    /// are silently skipped.
    fn mark_acknowledged(&self, hashes: &[TxHash]) -> Result<(), MempoolError>;

    /// Returns the transaction if it is currently in any inflight sub-stage
    /// (Propagated, Acknowledged, or Confirmed), `None` otherwise.
    ///
    /// Used to retrieve the payload for re-submission checks.
    fn find_inflight(&self, tx_hash: &TxHash) -> Option<MempoolTx>;

    /// Returns all inflight transactions (Propagated, Acknowledged,
    /// or Confirmed) in arbitrary order. Does not remove them.
    fn peek_inflight(&self) -> Vec<MempoolTx>;

    /// Called after each chain-sync event.
    ///
    /// For `seen_txs`, inflight or pending: transitions to `Confirmed` and increments the
    /// confirmation counter. For `unseen_txs` (rollback): moves the
    /// transaction back to the pending queue, at the place it was first
    /// admitted, so it can be re-submitted.
    /// Hashes not currently tracked are silently ignored.
    ///
    /// `point` is the block's chain point. On first confirmation, it is
    /// stored as `confirmed_at`. On rollback (unseen), it is cleared.
    ///
    /// After processing seen/unseen, transactions with
    /// `confirmations >= finalize_threshold` are moved to the finalized log
    /// as `Finalized`, and transactions with
    /// `non_confirmations >= drop_threshold` are moved to the finalized log
    /// as `Dropped`.
    fn confirm(
        &self,
        point: &ChainPoint,
        seen_txs: &[TxHash],
        unseen_txs: &[TxHash],
        finalize_threshold: u32,
        drop_threshold: u32,
    ) -> Result<(), MempoolError>;

    /// Returns the full status of a transaction including stage,
    /// confirmation count, and the chain point where it was first confirmed.
    fn check_status(&self, tx_hash: &TxHash) -> TxStatus;

    /// Moves matching pending transactions to the finalized log as `Dropped`, ignoring hashes not pending.
    fn drop_pending(&self, hashes: &[TxHash]) -> Result<(), MempoolError>;

    /// Paginate through finalized transactions.
    ///
    /// `cursor` is a sequence number assigned at finalization time. Returns a
    /// [`MempoolPage`] with the next cursor (`None` if exhausted).
    fn dump_finalized(&self, cursor: u64, limit: usize) -> MempoolPage;

    /// Returns a stream of lifecycle events.
    ///
    /// Implementors should emit an event for every stage transition.
    fn subscribe(&self) -> Self::Stream;
}

// ---------------------------------------------------------------------------
// Streams
// ---------------------------------------------------------------------------

pub struct UpdateFilter<M: MempoolStore> {
    inner: M::Stream,
    subjects: HashSet<TxHash>,
}

impl<M: MempoolStore> UpdateFilter<M> {
    pub fn new(inner: M::Stream, subjects: HashSet<TxHash>) -> Self {
        Self { inner, subjects }
    }
}

impl<M: MempoolStore> Stream for UpdateFilter<M> {
    type Item = MempoolEvent;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        loop {
            match Pin::new(&mut self.inner).poll_next(cx) {
                std::task::Poll::Ready(None) => return std::task::Poll::Ready(None),
                std::task::Poll::Ready(Some(Ok(x))) => {
                    if self.subjects.contains(&x.tx.hash) {
                        return std::task::Poll::Ready(Some(x));
                    }
                    // Non-matching item: continue polling the inner stream
                }
                std::task::Poll::Ready(Some(Err(_))) => return std::task::Poll::Ready(None),
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Mempool-aware UTxO store
// ---------------------------------------------------------------------------

pub struct MempoolAwareUtxoStore<'a, D: Domain> {
    inner: &'a D::State,
    mempool: &'a D::Mempool,
    pending: Pending,
}

/// The pending transactions a mempool aware view counts.
enum Pending {
    /// The pending queue of the mempool, read at each use.
    Queue,
    /// The given transactions, in place of the pending queue.
    Only(Vec<MempoolTx>),
}

impl<D: Domain> MempoolAwareUtxoStore<'_, D> {
    /// The pending transactions this view counts.
    pub fn pending(&self) -> Vec<MempoolTx> {
        match &self.pending {
            Pending::Queue => self.mempool.peek_pending(),
            Pending::Only(txs) => txs.clone(),
        }
    }

    fn unconfirmed(&self) -> Vec<MempoolTx> {
        let mut all_txs = self.pending();
        all_txs.extend(self.mempool.peek_inflight());
        all_txs
    }
}

/// Each sub transaction the transaction lists, then the transaction, in the order the ledger applies them.
fn applied<'t>(tx: &'t MultiEraTx<'t>) -> Vec<MultiEraTx<'t>> {
    let mut txs = tx.sub_transactions();
    txs.push(tx.clone());
    txs
}

fn scan_mempool_utxos<F>(predicate: F, all_txs: Vec<MempoolTx>) -> HashSet<TxoRef>
where
    F: Fn(&MultiEraOutput<'_>) -> bool,
{
    let mut refs = HashSet::new();

    for mtx in all_txs {
        let era_cbor = &mtx.payload;
        let Some(top) = MultiEraTx::try_from(era_cbor).ok() else {
            continue;
        };

        for tx in applied(&top) {
            debug!(tx = %tx.hash(), "scanning mempool tx");

            for (idx, inflight) in tx.produces() {
                if predicate(&inflight) {
                    let txoref = TxoRef::from((tx.hash(), idx as u32));
                    debug!(txoref = %txoref, "mempool utxo matches predicate");
                    refs.insert(txoref);
                }
            }
        }
    }

    refs
}

fn exclude_inflight_stxis(refs: &mut HashSet<TxoRef>, all_txs: Vec<MempoolTx>) {
    debug!("excluding inflight stxis");

    for mtx in all_txs {
        let era_cbor = &mtx.payload;
        let Some(top) = MultiEraTx::try_from(era_cbor).ok() else {
            warn!("invalid inflight tx");
            continue;
        };

        for tx in applied(&top) {
            debug!(tx = %tx.hash(), "checking inflight tx");

            for locked in tx.consumes() {
                let txoref = TxoRef::from(&locked);
                if refs.remove(&txoref) {
                    debug!(txoref = %txoref, "excluded stxi");
                }
            }
        }
    }
}

fn select_mempool_utxos(refs: &mut HashSet<TxoRef>, all_txs: Vec<MempoolTx>) -> UtxoMap {
    let mut map = HashMap::new();

    for mtx in all_txs {
        let era_cbor = &mtx.payload;
        let Some(top) = MultiEraTx::try_from(era_cbor).ok() else {
            continue;
        };

        for tx in applied(&top) {
            debug!(tx = %tx.hash(), "checking mempool tx");

            for (idx, inflight) in tx.produces() {
                let txoref = TxoRef::from((tx.hash(), idx as u32));
                debug!(txoref = %txoref, "checking mempool utxo");

                if refs.contains(&txoref) {
                    let era_cbor = EraCbor::from(inflight);
                    debug!(txoref = %txoref, "selected utxo available inmempool tx");
                    refs.remove(&txoref);
                    map.insert(txoref, Arc::new(era_cbor));
                }
            }
        }
    }

    map
}

impl<'a, D: Domain> MempoolAwareUtxoStore<'a, D> {
    pub fn new(inner: &'a D::State, mempool: &'a D::Mempool) -> Self {
        Self {
            inner,
            mempool,
            pending: Pending::Queue,
        }
    }

    /// A view that counts `pending` in place of the mempool's pending queue.
    pub fn with_pending(
        inner: &'a D::State,
        mempool: &'a D::Mempool,
        pending: Vec<MempoolTx>,
    ) -> Self {
        Self {
            inner,
            mempool,
            pending: Pending::Only(pending),
        }
    }

    pub fn state(&self) -> &D::State {
        self.inner
    }

    pub fn mempool(&self) -> &D::Mempool {
        self.mempool
    }

    /// Get UTxOs by a tag dimension and key, merging results from both the
    /// state store's tags and the mempool.
    ///
    /// The `predicate` is used to filter mempool UTxOs that match the query
    /// criteria.
    pub fn get_utxos_by_tag<F>(
        &self,
        dimension: TagDimension,
        key: &[u8],
        predicate: F,
    ) -> Result<UtxoSet, StateError>
    where
        F: Fn(&MultiEraOutput<'_>) -> bool,
    {
        let from_mempool = scan_mempool_utxos(predicate, self.unconfirmed());

        let mut utxos = self.inner.utxos_by_tag(dimension, key)?;

        utxos.extend(from_mempool);

        exclude_inflight_stxis(&mut utxos, self.unconfirmed());

        Ok(utxos)
    }

    pub fn get_utxos(&self, mut refs: HashSet<TxoRef>) -> Result<UtxoMap, StateError> {
        exclude_inflight_stxis(&mut refs, self.unconfirmed());

        let from_mempool = select_mempool_utxos(&mut refs, self.unconfirmed());

        let mut utxos = self.inner.get_utxos(Vec::from_iter(refs))?;

        utxos.extend(from_mempool);

        Ok(utxos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use dolos_testing::streams::{noop_waker, ScriptedStream};
    use dolos_testing::tx_sequence_to_hash;

    type MockStream = ScriptedStream<Result<MempoolEvent, MempoolError>>;

    fn test_hash(n: u8) -> TxHash {
        tx_sequence_to_hash(n as u64)
    }

    fn test_event(hash: TxHash) -> MempoolEvent {
        MempoolEvent {
            tx: MempoolTx::new(hash, EraCbor(7, vec![0x80]), vec![]),
        }
    }

    // MockStore must live here (not in dolos-testing) to implement the local
    // MempoolStore trait and avoid the two-copies-of-dolos-core problem.
    #[derive(Clone)]
    struct MockStore;

    impl MempoolStore for MockStore {
        type Stream = MockStream;

        fn receive(&self, _tx: MempoolTx) -> Result<(), MempoolError> {
            Ok(())
        }

        fn has_pending(&self) -> bool {
            false
        }

        fn peek_pending(&self) -> Vec<MempoolTx> {
            vec![]
        }

        fn mark_inflight(&self, _hashes: &[TxHash]) -> Result<(), MempoolError> {
            Ok(())
        }

        fn mark_acknowledged(&self, _hashes: &[TxHash]) -> Result<(), MempoolError> {
            Ok(())
        }

        fn find_inflight(&self, _tx_hash: &TxHash) -> Option<MempoolTx> {
            None
        }

        fn peek_inflight(&self) -> Vec<MempoolTx> {
            vec![]
        }

        fn confirm(
            &self,
            _point: &ChainPoint,
            _seen: &[TxHash],
            _unseen: &[TxHash],
            _finalize_threshold: u32,
            _drop_threshold: u32,
        ) -> Result<(), MempoolError> {
            Ok(())
        }

        fn drop_pending(&self, _hashes: &[TxHash]) -> Result<(), MempoolError> {
            Ok(())
        }

        fn check_status(&self, _hash: &TxHash) -> TxStatus {
            TxStatus {
                stage: MempoolTxStage::Unknown,
                confirmations: 0,
                non_confirmations: 0,
                confirmed_at: None,
            }
        }

        fn dump_finalized(&self, _cursor: u64, _limit: usize) -> MempoolPage {
            MempoolPage {
                items: vec![],
                next_cursor: None,
            }
        }

        fn subscribe(&self) -> Self::Stream {
            ScriptedStream::empty()
        }
    }

    /// Drive the filter stream to completion, collecting all emitted events.
    fn collect_sync(mut filter: UpdateFilter<MockStore>) -> Vec<MempoolEvent> {
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut out = Vec::new();
        loop {
            match Pin::new(&mut filter).poll_next(&mut cx) {
                Poll::Ready(Some(ev)) => out.push(ev),
                Poll::Ready(None) => break,
                Poll::Pending => break,
            }
        }
        out
    }

    /// Helper that returns the next single poll result from the filter.
    fn poll_once(
        filter: &mut UpdateFilter<MockStore>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<MempoolEvent>> {
        Pin::new(filter).poll_next(cx)
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[test]
    fn yields_matching_events() {
        let h1 = test_hash(1);
        let h2 = test_hash(2);

        let inner = MockStream::new(vec![
            Poll::Ready(Some(Ok(test_event(h1)))),
            Poll::Ready(Some(Ok(test_event(h2)))),
            Poll::Ready(None),
        ]);

        let subjects = HashSet::from([h1, h2]);
        let filter = UpdateFilter::<MockStore>::new(inner, subjects);
        let events = collect_sync(filter);

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].tx.hash, h1);
        assert_eq!(events[1].tx.hash, h2);
    }

    #[test]
    fn filters_out_non_matching_events() {
        let h1 = test_hash(1);
        let h2 = test_hash(2);
        let h3 = test_hash(3);

        let inner = MockStream::new(vec![
            Poll::Ready(Some(Ok(test_event(h1)))),
            Poll::Ready(Some(Ok(test_event(h2)))),
            Poll::Ready(Some(Ok(test_event(h3)))),
            Poll::Ready(None),
        ]);

        let subjects = HashSet::from([h1, h3]);
        let filter = UpdateFilter::<MockStore>::new(inner, subjects);
        let events = collect_sync(filter);

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].tx.hash, h1);
        assert_eq!(events[1].tx.hash, h3);
    }

    #[test]
    fn skips_non_matching_and_reaches_pending() {
        // The filter must skip non-matching Ready items and propagate Pending
        // only when the inner stream itself returns Pending.
        let h1 = test_hash(1);
        let h_other = test_hash(99);

        let inner = MockStream::new(vec![
            Poll::Ready(Some(Ok(test_event(h_other)))),
            Poll::Ready(Some(Ok(test_event(h_other)))),
            Poll::Pending,
        ]);

        let subjects = HashSet::from([h1]);
        let mut filter = UpdateFilter::<MockStore>::new(inner, subjects);
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        // Should skip both non-matching items and return Pending from the inner stream.
        let result = poll_once(&mut filter, &mut cx);
        assert!(result.is_pending());
    }

    #[test]
    fn skips_non_matching_then_yields_matching() {
        let h_skip = test_hash(10);
        let h_want = test_hash(20);

        let inner = MockStream::new(vec![
            Poll::Ready(Some(Ok(test_event(h_skip)))),
            Poll::Ready(Some(Ok(test_event(h_skip)))),
            Poll::Ready(Some(Ok(test_event(h_want)))),
            Poll::Ready(None),
        ]);

        let subjects = HashSet::from([h_want]);
        let mut filter = UpdateFilter::<MockStore>::new(inner, subjects);
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        // Should skip two non-matching and return the matching one.
        match poll_once(&mut filter, &mut cx) {
            Poll::Ready(Some(ev)) => assert_eq!(ev.tx.hash, h_want),
            Poll::Ready(None) => panic!("expected Ready(Some), got Ready(None)"),
            Poll::Pending => panic!("expected Ready(Some), got Pending"),
        }
    }

    #[test]
    fn error_terminates_stream() {
        let h1 = test_hash(1);
        let err = MempoolError::Internal("test".into());

        let inner = MockStream::new(vec![
            Poll::Ready(Some(Ok(test_event(h1)))),
            Poll::Ready(Some(Err(err))),
            // items after error should never be reached
            Poll::Ready(Some(Ok(test_event(h1)))),
        ]);

        let subjects = HashSet::from([h1]);
        let filter = UpdateFilter::<MockStore>::new(inner, subjects);
        let events = collect_sync(filter);

        // First event yielded, then error terminates the stream.
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tx.hash, h1);
    }

    #[test]
    fn empty_stream_returns_none() {
        let inner = MockStream::new(vec![Poll::Ready(None)]);

        let subjects = HashSet::from([test_hash(1)]);
        let mut filter = UpdateFilter::<MockStore>::new(inner, subjects);
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        assert!(matches!(poll_once(&mut filter, &mut cx), Poll::Ready(None)));
    }

    #[test]
    fn empty_subjects_yields_nothing() {
        let h1 = test_hash(1);

        let inner = MockStream::new(vec![
            Poll::Ready(Some(Ok(test_event(h1)))),
            Poll::Ready(Some(Ok(test_event(h1)))),
            Poll::Ready(None),
        ]);

        let subjects = HashSet::new();
        let filter = UpdateFilter::<MockStore>::new(inner, subjects);
        let events = collect_sync(filter);

        assert!(events.is_empty());
    }

    #[test]
    fn pending_propagated_immediately() {
        let inner = MockStream::new(vec![Poll::Pending]);

        let subjects = HashSet::from([test_hash(1)]);
        let mut filter = UpdateFilter::<MockStore>::new(inner, subjects);
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        assert!(poll_once(&mut filter, &mut cx).is_pending());
    }

    #[test]
    fn interleaved_pending_and_events() {
        let h1 = test_hash(1);
        let h2 = test_hash(2);

        let inner = MockStream::new(vec![
            Poll::Ready(Some(Ok(test_event(h1)))),
            Poll::Pending,
            Poll::Ready(Some(Ok(test_event(h2)))),
            Poll::Ready(None),
        ]);

        let subjects = HashSet::from([h1, h2]);
        let mut filter = UpdateFilter::<MockStore>::new(inner, subjects);
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);

        // First poll: yields h1.
        match poll_once(&mut filter, &mut cx) {
            Poll::Ready(Some(ev)) => assert_eq!(ev.tx.hash, h1),
            Poll::Ready(None) => panic!("expected Ready(Some(h1)), got Ready(None)"),
            Poll::Pending => panic!("expected Ready(Some(h1)), got Pending"),
        }

        // Second poll: inner returns Pending.
        assert!(poll_once(&mut filter, &mut cx).is_pending());

        // Third poll: yields h2.
        match poll_once(&mut filter, &mut cx) {
            Poll::Ready(Some(ev)) => assert_eq!(ev.tx.hash, h2),
            Poll::Ready(None) => panic!("expected Ready(Some(h2)), got Ready(None)"),
            Poll::Pending => panic!("expected Ready(Some(h2)), got Pending"),
        }

        // Fourth poll: stream done.
        assert!(matches!(poll_once(&mut filter, &mut cx), Poll::Ready(None)));
    }

    // The transaction bd1b7336 of the block at slot 854292 lists the sub
    // transaction ca42eb12, which spends d2fa6568#0 and makes one output.
    const PARENT: &str = "bd1b7336a5a62005a2576658e64990fab684a641139095afc16865c371e637b4";
    const SUB: &str = "ca42eb1227f150670a8a8c7c82468ff10dd726b2aa08ac209a8b8b5530fb28c7";
    const SUB_SPENDS: &str = "d2fa6568066113f78eefdc89025b419a8e68db15eef70e9a8cbea3b4ed87d8b6";

    fn txo(hash: &str, index: u32) -> TxoRef {
        TxoRef(hash.parse().unwrap(), index)
    }

    /// The transaction that lists a sub transaction, as a pending entry.
    fn pending_with_sub() -> MempoolTx {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../test_data/musashi-w36/ranking-sub-transaction.block");
        let text = std::fs::read_to_string(&path).unwrap();
        let cbor = hex::decode(text.trim()).unwrap();
        let block = pallas::ledger::traverse::MultiEraBlock::decode(&cbor).unwrap();
        let tx = block
            .txs()
            .into_iter()
            .find(|tx| tx.hash() == PARENT.parse::<TxHash>().unwrap())
            .unwrap();
        MempoolTx::new(tx.hash(), EraCbor(tx.era().into(), tx.encode()), vec![])
    }

    /// The output reference one past the last output of the listing transaction.
    fn past_parent_outputs(mtx: &MempoolTx) -> TxoRef {
        let tx = MultiEraTx::try_from(&mtx.payload).unwrap();
        txo(PARENT, tx.outputs().len() as u32)
    }

    #[test]
    fn a_sub_transaction_output_is_not_selected_under_the_listing_hash() {
        let mtx = pending_with_sub();
        let past = past_parent_outputs(&mtx);
        let mut refs = HashSet::from([past.clone()]);

        let selected = select_mempool_utxos(&mut refs, vec![mtx]);

        assert!(selected.is_empty());
        assert_eq!(refs, HashSet::from([past]));
    }

    #[test]
    fn a_sub_transaction_output_is_selected_under_its_own_hash() {
        let mtx = pending_with_sub();
        let mut refs = HashSet::from([txo(SUB, 0)]);

        let selected = select_mempool_utxos(&mut refs, vec![mtx]);

        assert_eq!(
            selected.keys().cloned().collect::<HashSet<_>>(),
            HashSet::from([txo(SUB, 0)])
        );
        assert!(refs.is_empty());
    }

    #[test]
    fn an_input_no_pending_transaction_spends_is_kept() {
        let mtx = pending_with_sub();
        let mut refs = HashSet::from([txo(PARENT, 0)]);

        exclude_inflight_stxis(&mut refs, vec![mtx]);

        assert_eq!(refs, HashSet::from([txo(PARENT, 0)]));
    }

    #[test]
    fn an_input_a_pending_sub_transaction_spends_is_excluded() {
        let mtx = pending_with_sub();
        let mut refs = HashSet::from([txo(SUB_SPENDS, 0)]);

        exclude_inflight_stxis(&mut refs, vec![mtx]);

        assert!(refs.is_empty());
    }

    #[test]
    fn a_tag_scan_finds_a_sub_transaction_output_under_its_own_hash() {
        let mtx = pending_with_sub();
        let past = past_parent_outputs(&mtx);

        let found = scan_mempool_utxos(|_| true, vec![mtx]);

        assert!(found.contains(&txo(SUB, 0)));
        assert!(!found.contains(&past));
    }
}
