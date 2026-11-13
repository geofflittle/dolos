use super::*;
use crate::TagDimension;

pub use pallas::ledger::validate::phase2::MultiEraEvalReport;

use futures_core::Stream;
use pallas::codec::minicbor::decode;
use self_cell::self_cell;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::OnceLock;
use tracing::warn;

/// A transaction decoded from a payload, none when the payload does not decode.
type TxDecode<'a> = Option<MultiEraTx<'a>>;

self_cell!(
    /// A payload and its decode.
    struct DecodedTx {
        owner: Arc<EraCbor>,

        #[not_covariant]
        dependent: TxDecode,
    }

    impl {Debug}
);

#[derive(Debug)]
pub struct MempoolTx {
    hash: TxHash,
    payload: Arc<EraCbor>,
    pub stage: MempoolTxStage,
    pub confirmations: u32,
    pub non_confirmations: u32,
    pub confirmed_at: Option<ChainPoint>,

    // this might be empty if the tx is cloned
    pub report: Option<MultiEraEvalReport>,

    /// The decode of the payload, made at the first read and shared by every
    /// clone.
    decoded: Arc<OnceLock<DecodedTx>>,

    /// The effects of the payload, decoded at the first read and shared by
    /// every clone.
    effects: Arc<OnceLock<Option<TxEffects>>>,
}

/// The outputs a transaction and the sub transactions it lists make, and the
/// inputs they spend.
#[derive(Debug, Default)]
pub struct TxEffects {
    produced: Vec<(TxoRef, EraCbor)>,
    consumed: Vec<TxoRef>,
}

impl TxEffects {
    /// The effects of a transaction.
    fn of(top: &MultiEraTx<'_>) -> Self {
        let mut effects = Self::default();
        let Ok(()) = for_each_applied_tx(top, |tx| {
            let hash = tx.hash();
            for (idx, output) in tx.produces() {
                let txoref = TxoRef::from((hash, idx as u32));
                effects.produced.push((txoref, EraCbor::from(output)));
            }
            effects
                .consumed
                .extend(tx.consumes().iter().map(TxoRef::from));
            Ok::<_, Infallible>(())
        });
        effects
    }
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
            decoded: self.decoded.clone(),
            effects: self.effects.clone(),
        }
    }
}

impl MempoolTx {
    pub fn new(hash: TxHash, payload: EraCbor, report: MultiEraEvalReport) -> Self {
        Self {
            hash,
            payload: Arc::new(payload),
            stage: MempoolTxStage::Pending,
            confirmations: 0,
            non_confirmations: 0,
            confirmed_at: None,
            report: Some(report),
            decoded: Arc::default(),
            effects: Arc::default(),
        }
    }

    /// A transaction read back from storage at a stage, with no evaluation
    /// report.
    pub fn restored(
        hash: TxHash,
        payload: EraCbor,
        stage: MempoolTxStage,
        confirmations: u32,
        non_confirmations: u32,
        confirmed_at: Option<ChainPoint>,
    ) -> Self {
        Self {
            hash,
            payload: Arc::new(payload),
            stage,
            confirmations,
            non_confirmations,
            confirmed_at,
            report: None,
            decoded: Arc::default(),
            effects: Arc::default(),
        }
    }

    /// The hash the transaction was admitted under.
    pub fn hash(&self) -> TxHash {
        self.hash
    }

    /// The encoded transaction.
    pub fn payload(&self) -> &EraCbor {
        &self.payload
    }

    /// The encoded transaction, taken from the entry.
    pub fn into_payload(self) -> EraCbor {
        Arc::unwrap_or_clone(self.payload)
    }

    /// The result of `f` on the decoded transaction, decoded at the first read
    /// and shared by every clone.
    pub fn with_tx<R>(
        &self,
        f: impl for<'a> FnOnce(Result<&MultiEraTx<'a>, decode::Error>) -> R,
    ) -> R {
        self.decoded
            .get_or_init(|| {
                DecodedTx::new(self.payload.clone(), |payload| {
                    MultiEraTx::try_from(&**payload).ok()
                })
            })
            .with_dependent(|_, decoded| match decoded {
                Some(tx) => f(Ok(tx)),
                // A payload that does not decode keeps no decode, so its error
                // is read from the payload again.
                None => match MultiEraTx::try_from(&*self.payload) {
                    Ok(tx) => f(Ok(&tx)),
                    Err(error) => f(Err(error)),
                },
            })
    }

    /// The outputs the transaction makes and the inputs it spends, none when
    /// the payload does not decode.
    pub fn effects(&self) -> Option<&TxEffects> {
        self.effects
            .get_or_init(|| self.with_tx(|tx| tx.ok().map(TxEffects::of)))
            .as_ref()
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

    #[error("mempool full: {0}")]
    Full(PassedBound),
}

/// The most transactions, and the most payload bytes, a mempool holds before it
/// refuses a submit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MempoolBound {
    pub max_txs: usize,
    pub max_bytes: usize,
}

impl MempoolBound {
    /// Two endorser blocks of transactions at a `maxEndorserBlockTxsSize` of
    /// 2000000 bytes.
    pub const DEFAULT_MAX_BYTES: usize = 4_000_000;

    /// The default byte bound over transactions of 200 bytes.
    pub const DEFAULT_MAX_TXS: usize = 20_000;

    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl Default for MempoolBound {
    fn default() -> Self {
        Self {
            max_txs: Self::DEFAULT_MAX_TXS,
            max_bytes: Self::DEFAULT_MAX_BYTES,
        }
    }
}

/// The limit of a [`MempoolBound`] that holding one more transaction would pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PassedBound {
    #[error("would hold {would_hold} transactions, past the bound of {bound}")]
    Txs { bound: usize, would_hold: usize },

    #[error("would hold {would_hold} payload bytes, past the bound of {bound}")]
    Bytes { bound: usize, would_hold: usize },
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
    /// same hash is already pending, and [`Full`](MempoolError::Full) if
    /// holding it would pass the store's [`MempoolBound`].
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
    /// or Confirmed) in the order they were first admitted. Does not remove
    /// them.
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

    /// Runs `f` over the index of the transactions pending or inflight.
    fn read_unconfirmed<R>(&self, f: impl FnOnce(&UnconfirmedIndex) -> R) -> R;

    /// The lock that orders the validation and admission of submissions to this
    /// mempool.
    fn submit_lock(&self) -> &std::sync::Mutex<()>;

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
}

/// The outputs a set of mempool transactions make and the inputs they spend,
/// each counted once per transaction.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UnconfirmedIndex {
    produced: HashMap<TxoRef, (usize, Arc<EraCbor>)>,
    consumed: HashMap<TxoRef, usize>,
}

impl UnconfirmedIndex {
    /// The index of the given transactions.
    pub fn of<'a>(txs: impl IntoIterator<Item = &'a MempoolTx>) -> Self {
        let mut index = Self::default();
        for tx in txs {
            index.insert(tx);
        }
        index
    }

    /// Counts the outputs `tx` makes and the inputs it spends.
    pub fn insert(&mut self, tx: &MempoolTx) {
        let Some(effects) = tx.effects() else {
            warn!(tx.hash = %tx.hash(), "mempool tx does not decode");
            return;
        };
        for (txoref, era_cbor) in &effects.produced {
            self.produced
                .entry(txoref.clone())
                .or_insert_with(|| (0, Arc::new(era_cbor.clone())))
                .0 += 1;
        }
        for txoref in &effects.consumed {
            *self.consumed.entry(txoref.clone()).or_default() += 1;
        }
    }

    /// Uncounts the outputs `tx` makes and the inputs it spends.
    pub fn remove(&mut self, tx: &MempoolTx) {
        let Some(effects) = tx.effects() else {
            return;
        };
        for (txoref, _) in &effects.produced {
            if let std::collections::hash_map::Entry::Occupied(mut entry) =
                self.produced.entry(txoref.clone())
            {
                entry.get_mut().0 -= 1;
                if entry.get().0 == 0 {
                    entry.remove();
                }
            }
        }
        for txoref in &effects.consumed {
            if let std::collections::hash_map::Entry::Occupied(mut entry) =
                self.consumed.entry(txoref.clone())
            {
                *entry.get_mut() -= 1;
                if *entry.get() == 0 {
                    entry.remove();
                }
            }
        }
    }

    /// Removes from `refs` each input a transaction of the index spends.
    pub fn exclude_consumed(&self, refs: &mut HashSet<TxoRef>) {
        refs.retain(|txoref| !self.consumed.contains_key(txoref));
    }

    /// Removes from `refs` and returns each output a transaction of the index
    /// makes.
    pub fn take_produced(&self, refs: &mut HashSet<TxoRef>) -> UtxoMap {
        let mut taken = HashMap::new();
        refs.retain(|txoref| match self.produced.get(txoref) {
            Some((_, era_cbor)) => {
                taken.insert(txoref.clone(), era_cbor.clone());
                false
            }
            None => true,
        });
        taken
    }

    /// The outputs the transactions of the index make that `predicate` admits.
    pub fn scan_produced(
        &self,
        predicate: impl Fn(&MultiEraOutput<'_>) -> bool,
    ) -> HashSet<TxoRef> {
        self.produced
            .iter()
            .filter(|(_, (_, era_cbor))| {
                MultiEraOutput::try_from(&**era_cbor).is_ok_and(|output| predicate(&output))
            })
            .map(|(txoref, _)| txoref.clone())
            .collect()
    }
}

/// The transactions a mempool holds pending or inflight, each with a value the
/// store keeps beside it, and the index of their effects.
pub struct UnconfirmedTxs<T> {
    txs: HashMap<TxHash, (T, MempoolTx)>,
    index: UnconfirmedIndex,
    bound: MempoolBound,
    bytes: usize,
}

impl<T> Default for UnconfirmedTxs<T> {
    fn default() -> Self {
        Self::bounded(MempoolBound::default())
    }
}

/// The payload bytes a transaction counts against a [`MempoolBound`].
fn bound_bytes(tx: &MempoolTx) -> usize {
    tx.payload().cbor().len()
}

impl<T> UnconfirmedTxs<T> {
    pub fn bounded(bound: MempoolBound) -> Self {
        Self {
            txs: HashMap::new(),
            index: UnconfirmedIndex::default(),
            bound,
            bytes: 0,
        }
    }

    /// Refuses `tx` when holding it beside the transactions held would pass
    /// the bound.
    pub fn room_for(&self, tx: &MempoolTx) -> Result<(), MempoolError> {
        let txs = self.txs.len() + 1;
        if txs > self.bound.max_txs {
            return Err(MempoolError::Full(PassedBound::Txs {
                bound: self.bound.max_txs,
                would_hold: txs,
            }));
        }
        let bytes = self.bytes + bound_bytes(tx);
        if bytes > self.bound.max_bytes {
            return Err(MempoolError::Full(PassedBound::Bytes {
                bound: self.bound.max_bytes,
                would_hold: bytes,
            }));
        }
        Ok(())
    }

    /// Holds `tx` with `value`, replacing any transaction held under its hash.
    pub fn insert(&mut self, value: T, tx: MempoolTx) {
        self.index.insert(&tx);
        self.bytes += bound_bytes(&tx);
        if let Some((_, replaced)) = self.txs.insert(tx.hash(), (value, tx)) {
            self.index.remove(&replaced);
            self.bytes -= bound_bytes(&replaced);
        }
    }

    /// Takes the transaction out, with its effects.
    pub fn remove(&mut self, hash: &TxHash) -> Option<(T, MempoolTx)> {
        let (value, tx) = self.txs.remove(hash)?;
        self.index.remove(&tx);
        self.bytes -= bound_bytes(&tx);
        Some((value, tx))
    }

    pub fn get(&self, hash: &TxHash) -> Option<&(T, MempoolTx)> {
        self.txs.get(hash)
    }

    /// The transaction, for a change of stage or counters.
    pub fn get_mut(&mut self, hash: &TxHash) -> Option<&mut MempoolTx> {
        self.txs.get_mut(hash).map(|(_, tx)| tx)
    }

    pub fn contains(&self, hash: &TxHash) -> bool {
        self.txs.contains_key(hash)
    }

    pub fn values(&self) -> impl Iterator<Item = &(T, MempoolTx)> {
        self.txs.values()
    }

    pub fn len(&self) -> usize {
        self.txs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.txs.is_empty()
    }

    /// The effects of every transaction held.
    pub fn index(&self) -> &UnconfirmedIndex {
        &self.index
    }
}

/// Runs `f` over the index of the transactions a view counting `pending` treats
/// as unconfirmed.
fn with_unconfirmed<M: MempoolStore, R>(
    mempool: &M,
    pending: &Pending,
    f: impl FnOnce(&UnconfirmedIndex) -> R,
) -> R {
    match pending {
        Pending::Queue => mempool.read_unconfirmed(f),
        Pending::Only(txs) => f(&UnconfirmedIndex::of(
            txs.iter().chain(&mempool.peek_inflight()),
        )),
    }
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
        let mut utxos = self.inner.utxos_by_tag(dimension, key)?;

        with_unconfirmed(self.mempool, &self.pending, |index| {
            utxos.extend(index.scan_produced(predicate));
            index.exclude_consumed(&mut utxos);
        });

        Ok(utxos)
    }

    pub fn get_utxos(&self, mut refs: HashSet<TxoRef>) -> Result<UtxoMap, StateError> {
        let from_mempool = with_unconfirmed(self.mempool, &self.pending, |index| {
            index.exclude_consumed(&mut refs);
            index.take_produced(&mut refs)
        });

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

        fn read_unconfirmed<R>(&self, f: impl FnOnce(&UnconfirmedIndex) -> R) -> R {
            f(&UnconfirmedIndex::default())
        }

        fn submit_lock(&self) -> &std::sync::Mutex<()> {
            static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
            &LOCK
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

    // The fixture's transaction lists one sub transaction that spends one
    // input and makes one output.
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
        let tx = MultiEraTx::try_from(mtx.payload()).unwrap();
        txo(PARENT, tx.outputs().len() as u32)
    }

    #[test]
    fn a_sub_transaction_output_is_not_selected_under_the_listing_hash() {
        let mtx = pending_with_sub();
        let past = past_parent_outputs(&mtx);
        let mut refs = HashSet::from([past.clone()]);

        let selected = UnconfirmedIndex::of([&mtx]).take_produced(&mut refs);

        assert!(selected.is_empty());
        assert_eq!(refs, HashSet::from([past]));
    }

    #[test]
    fn a_sub_transaction_output_is_selected_under_its_own_hash() {
        let mtx = pending_with_sub();
        let mut refs = HashSet::from([txo(SUB, 0)]);

        let selected = UnconfirmedIndex::of([&mtx]).take_produced(&mut refs);

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

        UnconfirmedIndex::of([&mtx]).exclude_consumed(&mut refs);

        assert_eq!(refs, HashSet::from([txo(PARENT, 0)]));
    }

    #[test]
    fn an_input_a_pending_sub_transaction_spends_is_excluded() {
        let mtx = pending_with_sub();
        let mut refs = HashSet::from([txo(SUB_SPENDS, 0)]);

        UnconfirmedIndex::of([&mtx]).exclude_consumed(&mut refs);

        assert!(refs.is_empty());
    }

    #[test]
    fn a_tag_scan_finds_a_sub_transaction_output_under_its_own_hash() {
        let mtx = pending_with_sub();
        let past = past_parent_outputs(&mtx);

        let found = UnconfirmedIndex::of([&mtx]).scan_produced(|_| true);

        assert!(found.contains(&txo(SUB, 0)));
        assert!(!found.contains(&past));
    }

    /// Bytes that decode as no transaction.
    fn undecodable() -> EraCbor {
        EraCbor(7, vec![0x80])
    }

    #[test]
    fn an_entry_whose_payload_does_not_decode_makes_and_spends_nothing() {
        let mtx = MempoolTx::new(PARENT.parse().unwrap(), undecodable(), vec![]);
        let mut refs = HashSet::from([txo(SUB, 0), txo(SUB_SPENDS, 0)]);

        let index = UnconfirmedIndex::of([&mtx]);
        index.exclude_consumed(&mut refs);
        let selected = index.take_produced(&mut refs);

        assert!(selected.is_empty());
        assert_eq!(refs, HashSet::from([txo(SUB, 0), txo(SUB_SPENDS, 0)]));
    }

    #[test]
    fn a_read_after_the_first_decodes_no_payload() {
        let mut stored = pending_with_sub();
        let mut first = HashSet::from([txo(SUB, 0)]);
        UnconfirmedIndex::of([&stored.clone()]).take_produced(&mut first);
        // These bytes do not decode, so a read that still finds the output
        // answers from the effects of the first read.
        stored.payload = Arc::new(undecodable());
        let mut refs = HashSet::from([txo(SUB, 0), txo(SUB_SPENDS, 0)]);

        let index = UnconfirmedIndex::of([&stored]);
        index.exclude_consumed(&mut refs);
        let selected = index.take_produced(&mut refs);

        assert_eq!(
            selected.keys().cloned().collect::<HashSet<_>>(),
            HashSet::from([txo(SUB, 0)])
        );
        assert!(refs.is_empty());
    }

    #[test]
    fn removing_a_transaction_uncounts_only_its_own_effects() {
        let mtx = pending_with_sub();
        let mut index = UnconfirmedIndex::of([&mtx, &mtx]);

        index.remove(&mtx);

        assert_eq!(index, UnconfirmedIndex::of([&mtx]));
        index.remove(&mtx);
        assert_eq!(index, UnconfirmedIndex::default());
    }

    #[test]
    fn a_payload_that_does_not_decode_reads_as_its_decode_error() {
        let mtx = MempoolTx::new(PARENT.parse().unwrap(), undecodable(), vec![]);
        let direct = MultiEraTx::try_from(&undecodable()).unwrap_err();

        let read = mtx.clone().with_tx(|tx| tx.map(|_| ()).unwrap_err());

        assert_eq!(read.to_string(), direct.to_string());
        assert_eq!(
            mtx.with_tx(|tx| tx.map(|_| ()).unwrap_err()).to_string(),
            direct.to_string()
        );
    }

    /// The address of the decode a transaction reads.
    fn decode_address(mtx: &MempoolTx) -> usize {
        mtx.with_tx(|tx| std::ptr::from_ref(tx.unwrap()) as usize)
    }

    #[test]
    fn a_clone_reads_the_decode_its_original_made() {
        let admitted = pending_with_sub();
        let read = admitted.clone();

        let first = decode_address(&admitted);
        let again = decode_address(&read);

        assert_eq!(
            admitted.with_tx(|tx| tx.unwrap().hash()),
            PARENT.parse::<TxHash>().unwrap()
        );
        assert_eq!(first, again, "the clone decoded the payload again");
    }

    /// A mempool that counts the transactions its peeks hand out.
    #[derive(Clone, Default)]
    struct Counting {
        inner: crate::builtin::EphemeralMempool,
        walked: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Counting {
        fn walked(&self) -> usize {
            self.walked.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn count(&self, txs: Vec<MempoolTx>) -> Vec<MempoolTx> {
            self.walked
                .fetch_add(txs.len(), std::sync::atomic::Ordering::SeqCst);
            txs
        }
    }

    impl MempoolStore for Counting {
        type Stream = crate::builtin::EphemeralMempoolStream;

        fn receive(&self, tx: MempoolTx) -> Result<(), MempoolError> {
            self.inner.receive(tx)
        }

        fn has_pending(&self) -> bool {
            self.inner.has_pending()
        }

        fn peek_pending(&self) -> Vec<MempoolTx> {
            self.count(self.inner.peek_pending())
        }

        fn mark_inflight(&self, hashes: &[TxHash]) -> Result<(), MempoolError> {
            self.inner.mark_inflight(hashes)
        }

        fn mark_acknowledged(&self, hashes: &[TxHash]) -> Result<(), MempoolError> {
            self.inner.mark_acknowledged(hashes)
        }

        fn find_inflight(&self, tx_hash: &TxHash) -> Option<MempoolTx> {
            self.inner.find_inflight(tx_hash)
        }

        fn peek_inflight(&self) -> Vec<MempoolTx> {
            self.count(self.inner.peek_inflight())
        }

        fn confirm(
            &self,
            point: &ChainPoint,
            seen_txs: &[TxHash],
            unseen_txs: &[TxHash],
            finalize_threshold: u32,
            drop_threshold: u32,
        ) -> Result<(), MempoolError> {
            self.inner.confirm(
                point,
                seen_txs,
                unseen_txs,
                finalize_threshold,
                drop_threshold,
            )
        }

        fn check_status(&self, tx_hash: &TxHash) -> TxStatus {
            self.inner.check_status(tx_hash)
        }

        fn drop_pending(&self, hashes: &[TxHash]) -> Result<(), MempoolError> {
            self.inner.drop_pending(hashes)
        }

        fn dump_finalized(&self, cursor: u64, limit: usize) -> MempoolPage {
            self.inner.dump_finalized(cursor, limit)
        }

        fn read_unconfirmed<R>(&self, f: impl FnOnce(&UnconfirmedIndex) -> R) -> R {
            self.inner.read_unconfirmed(f)
        }

        fn submit_lock(&self) -> &std::sync::Mutex<()> {
            self.inner.submit_lock()
        }

        fn subscribe(&self) -> Self::Stream {
            self.inner.subscribe()
        }
    }

    /// A mempool holding the transaction that lists a sub transaction, one in
    /// flight, and `filler` more that decode as no transaction.
    fn crowded(filler: u8) -> Counting {
        let store = Counting::default();
        store.receive(pending_with_sub()).unwrap();
        let flying = MempoolTx::new(test_hash(0), undecodable(), vec![]);
        store.receive(flying.clone()).unwrap();
        store.mark_inflight(&[flying.hash()]).unwrap();
        for n in 1..=filler {
            store
                .receive(MempoolTx::new(test_hash(n), undecodable(), vec![]))
                .unwrap();
        }
        store
    }

    #[test]
    fn a_view_of_given_pending_transactions_ignores_the_queue() {
        let store = crowded(3);
        let mut refs = HashSet::from([txo(SUB, 0), txo(SUB_SPENDS, 0)]);

        let selected = with_unconfirmed(&store, &Pending::Only(Vec::new()), |index| {
            index.exclude_consumed(&mut refs);
            index.take_produced(&mut refs)
        });

        assert!(selected.is_empty());
        assert_eq!(refs, HashSet::from([txo(SUB, 0), txo(SUB_SPENDS, 0)]));
    }

    #[test]
    fn a_lookup_over_the_queue_walks_no_transaction() {
        let store = crowded(40);
        let mut refs = HashSet::from([txo(SUB, 0), txo(SUB_SPENDS, 0)]);

        let selected = with_unconfirmed(&store, &Pending::Queue, |index| {
            index.exclude_consumed(&mut refs);
            index.take_produced(&mut refs)
        });

        assert_eq!(
            selected.keys().cloned().collect::<HashSet<_>>(),
            HashSet::from([txo(SUB, 0)])
        );
        assert!(refs.is_empty());
        assert_eq!(store.walked(), 0, "transactions walked for one lookup");
    }
}
