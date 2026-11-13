use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use pallas::codec::minicbor::{self, Decode, Encode};
use redb::{ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use thiserror::Error;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tracing::{debug, warn};

use dolos_core::{
    config::RedbMempoolConfig, ChainPoint, EraCbor, MempoolBound, MempoolError, MempoolEvent,
    MempoolPage, MempoolStore, MempoolTx, MempoolTxStage, TxHash, TxStatus, UnconfirmedIndex,
    UnconfirmedTxs,
};

// ── Error newtype (mirrors wal/mod.rs pattern) ──────────────────────────

#[derive(Debug, Error)]
#[error(transparent)]
struct RedbMempoolError(#[from] MempoolError);

impl From<redb::Error> for RedbMempoolError {
    fn from(value: redb::Error) -> Self {
        Self(MempoolError::Internal(Box::new(value)))
    }
}

impl From<RedbMempoolError> for MempoolError {
    fn from(value: RedbMempoolError) -> Self {
        value.0
    }
}

impl From<redb::DatabaseError> for RedbMempoolError {
    fn from(value: redb::DatabaseError) -> Self {
        Self(MempoolError::Internal(Box::new(redb::Error::from(value))))
    }
}

impl From<redb::TableError> for RedbMempoolError {
    fn from(value: redb::TableError) -> Self {
        Self(MempoolError::Internal(Box::new(redb::Error::from(value))))
    }
}

impl From<redb::CommitError> for RedbMempoolError {
    fn from(value: redb::CommitError) -> Self {
        Self(MempoolError::Internal(Box::new(redb::Error::from(value))))
    }
}

impl From<redb::StorageError> for RedbMempoolError {
    fn from(value: redb::StorageError) -> Self {
        Self(MempoolError::Internal(Box::new(redb::Error::from(value))))
    }
}

impl From<redb::TransactionError> for RedbMempoolError {
    fn from(value: redb::TransactionError) -> Self {
        Self(MempoolError::Internal(Box::new(redb::Error::from(value))))
    }
}

const DEFAULT_CACHE_SIZE_MB: usize = 32;

// ── Layer 1: redb key/value types ───────────────────────────────────────

/// Composite key for pending table: `[8-byte seq BE ++ 32-byte tx_hash]`.
#[derive(Debug)]
struct DbPendingKey([u8; 40]);

impl DbPendingKey {
    fn new(seq: u64, hash: &TxHash) -> Self {
        let mut key = [0u8; 40];
        key[..8].copy_from_slice(&seq.to_be_bytes());
        key[8..].copy_from_slice(hash.as_ref());
        Self(key)
    }

    fn seq(&self) -> u64 {
        u64::from_be_bytes(self.0[..8].try_into().unwrap())
    }

    fn hash(&self) -> TxHash {
        let mut h = [0u8; 32];
        h.copy_from_slice(&self.0[8..40]);
        TxHash::from(h)
    }
}

impl redb::Value for DbPendingKey {
    type SelfType<'a>
        = Self
    where
        Self: 'a;
    type AsBytes<'a>
        = &'a [u8; 40]
    where
        Self: 'a;

    fn fixed_width() -> Option<usize> {
        Some(40)
    }

    fn from_bytes<'a>(data: &'a [u8]) -> Self::SelfType<'a>
    where
        Self: 'a,
    {
        let inner = <[u8; 40]>::try_from(data).unwrap();
        Self(inner)
    }

    fn as_bytes<'a, 'b: 'a>(value: &'a Self::SelfType<'b>) -> Self::AsBytes<'a>
    where
        Self: 'b,
    {
        &value.0
    }

    fn type_name() -> redb::TypeName {
        redb::TypeName::new("mempool_pending_key")
    }
}

impl redb::Key for DbPendingKey {
    fn compare(data1: &[u8], data2: &[u8]) -> std::cmp::Ordering {
        data1.cmp(data2)
    }
}

/// 32-byte tx hash key for the inflight table.
#[derive(Debug)]
struct DbTxHash([u8; 32]);

impl DbTxHash {
    fn from_tx_hash(hash: &TxHash) -> Self {
        let mut inner = [0u8; 32];
        inner.copy_from_slice(hash.as_ref());
        Self(inner)
    }

    fn to_tx_hash(&self) -> TxHash {
        TxHash::from(self.0)
    }
}

impl redb::Value for DbTxHash {
    type SelfType<'a>
        = Self
    where
        Self: 'a;
    type AsBytes<'a>
        = &'a [u8; 32]
    where
        Self: 'a;

    fn fixed_width() -> Option<usize> {
        Some(32)
    }

    fn from_bytes<'a>(data: &'a [u8]) -> Self::SelfType<'a>
    where
        Self: 'a,
    {
        let inner = <[u8; 32]>::try_from(data).unwrap();
        Self(inner)
    }

    fn as_bytes<'a, 'b: 'a>(value: &'a Self::SelfType<'b>) -> Self::AsBytes<'a>
    where
        Self: 'b,
    {
        &value.0
    }

    fn type_name() -> redb::TypeName {
        redb::TypeName::new("mempool_tx_hash")
    }
}

impl redb::Key for DbTxHash {
    fn compare(data1: &[u8], data2: &[u8]) -> std::cmp::Ordering {
        data1.cmp(data2)
    }
}

/// Newtype wrapping `EraCbor` for the pending table value (foreign type).
#[derive(Debug)]
struct DbEraCbor(EraCbor);

impl redb::Value for DbEraCbor {
    type SelfType<'a>
        = Self
    where
        Self: 'a;
    type AsBytes<'a>
        = Vec<u8>
    where
        Self: 'a;

    fn fixed_width() -> Option<usize> {
        None
    }

    fn from_bytes<'a>(data: &'a [u8]) -> Self::SelfType<'a>
    where
        Self: 'a,
    {
        Self(minicbor::decode(data).unwrap())
    }

    fn as_bytes<'a, 'b: 'a>(value: &'a Self::SelfType<'b>) -> Self::AsBytes<'a>
    where
        Self: 'b,
    {
        minicbor::to_vec(&value.0).unwrap()
    }

    fn type_name() -> redb::TypeName {
        redb::TypeName::new("mempool_era_cbor")
    }
}

// ── Inflight record ─────────────────────────────────────────────────────

#[derive(Clone, Debug, Encode, Decode, PartialEq)]
enum InflightStage {
    #[n(0)]
    Propagated,
    #[n(1)]
    Acknowledged,
    #[n(2)]
    Confirmed,
}

#[derive(Clone, Debug, Encode, Decode)]
struct InflightRecord {
    #[n(0)]
    stage: InflightStage,
    #[n(1)]
    confirmations: u32,
    #[n(2)]
    payload: EraCbor,
    #[cbor(n(3), with = "minicbor::bytes")]
    confirmed_at: Option<Vec<u8>>,
    #[n(4)]
    non_confirmations: u32,
    #[n(5)]
    seq: Option<u64>,
}

impl redb::Value for InflightRecord {
    type SelfType<'a>
        = Self
    where
        Self: 'a;
    type AsBytes<'a>
        = Vec<u8>
    where
        Self: 'a;

    fn fixed_width() -> Option<usize> {
        None
    }

    fn from_bytes<'a>(data: &'a [u8]) -> Self::SelfType<'a>
    where
        Self: 'a,
    {
        minicbor::decode(data).unwrap()
    }

    fn as_bytes<'a, 'b: 'a>(value: &'a Self::SelfType<'b>) -> Self::AsBytes<'a>
    where
        Self: 'b,
    {
        minicbor::to_vec(value).unwrap()
    }

    fn type_name() -> redb::TypeName {
        redb::TypeName::new("mempool_inflight_record")
    }
}

/// Entry stored in finalized log table for pagination.
#[derive(Debug, Encode, Decode)]
struct FinalizedEntry {
    #[cbor(n(0), with = "minicbor::bytes")]
    hash: Vec<u8>,
    #[n(1)]
    confirmations: u32,
    #[cbor(n(2), with = "minicbor::bytes")]
    confirmed_at: Option<Vec<u8>>,
    #[n(3)]
    payload: Option<EraCbor>,
    #[n(4)]
    dropped: Option<bool>,
}

impl redb::Value for FinalizedEntry {
    type SelfType<'a>
        = Self
    where
        Self: 'a;
    type AsBytes<'a>
        = Vec<u8>
    where
        Self: 'a;

    fn fixed_width() -> Option<usize> {
        None
    }

    fn from_bytes<'a>(data: &'a [u8]) -> Self::SelfType<'a>
    where
        Self: 'a,
    {
        minicbor::decode(data).unwrap()
    }

    fn as_bytes<'a, 'b: 'a>(value: &'a Self::SelfType<'b>) -> Self::AsBytes<'a>
    where
        Self: 'b,
    {
        minicbor::to_vec(value).unwrap()
    }

    fn type_name() -> redb::TypeName {
        redb::TypeName::new("mempool_finalized_log_entry")
    }
}

impl FinalizedEntry {
    fn into_mempool_tx(self) -> MempoolTx {
        let mut hash_bytes = [0u8; 32];
        hash_bytes.copy_from_slice(&self.hash);
        let stage = if self.dropped.unwrap_or(false) {
            MempoolTxStage::Dropped
        } else {
            MempoolTxStage::Finalized
        };
        MempoolTx::restored(
            TxHash::from(hash_bytes),
            self.payload.unwrap_or(EraCbor(0, vec![])),
            stage,
            self.confirmations,
            0,
            self.confirmed_at
                .map(|b| ChainPoint::from_bytes(b[..].try_into().unwrap())),
        )
    }
}

impl InflightRecord {
    fn new(payload: EraCbor, seq: u64) -> Self {
        Self {
            stage: InflightStage::Propagated,
            confirmations: 0,
            payload,
            confirmed_at: None,
            non_confirmations: 0,
            seq: Some(seq),
        }
    }

    fn acknowledge(&mut self) -> bool {
        if self.stage == InflightStage::Propagated {
            self.stage = InflightStage::Acknowledged;
            true
        } else {
            false
        }
    }

    fn confirm(&mut self, point: &ChainPoint) {
        self.stage = InflightStage::Confirmed;
        self.confirmations += 1;
        self.non_confirmations = 0;
        if self.confirmed_at.is_none() {
            self.confirmed_at = Some(point.clone().into_bytes().to_vec());
        }
    }

    fn retry(&mut self) {
        self.confirmed_at = None;
    }

    fn mark_stale(&mut self) {
        self.non_confirmations += 1;
    }

    fn is_finalizable(&self, threshold: u32) -> bool {
        self.stage == InflightStage::Confirmed && self.confirmations >= threshold
    }

    fn is_droppable(&self, threshold: u32) -> bool {
        self.non_confirmations >= threshold
    }

    fn to_tx_status(&self) -> TxStatus {
        let stage = match self.stage {
            InflightStage::Propagated => MempoolTxStage::Propagated,
            InflightStage::Acknowledged => MempoolTxStage::Acknowledged,
            InflightStage::Confirmed => MempoolTxStage::Confirmed,
        };
        TxStatus {
            stage,
            confirmations: self.confirmations,
            non_confirmations: self.non_confirmations,
            confirmed_at: self
                .confirmed_at
                .as_ref()
                .map(|b| ChainPoint::from_bytes(b[..].try_into().unwrap())),
        }
    }

    fn into_finalized_entry(self, hash: TxHash) -> FinalizedEntry {
        FinalizedEntry {
            hash: hash.to_vec(),
            confirmations: self.confirmations,
            confirmed_at: self.confirmed_at,
            payload: Some(self.payload),
            dropped: Some(false),
        }
    }

    fn into_dropped_entry(self, hash: TxHash) -> FinalizedEntry {
        FinalizedEntry {
            hash: hash.to_vec(),
            confirmations: self.confirmations,
            confirmed_at: self.confirmed_at,
            payload: Some(self.payload),
            dropped: Some(true),
        }
    }

    fn to_mempool_tx(&self, hash: TxHash) -> MempoolTx {
        let stage = match self.stage {
            InflightStage::Propagated => MempoolTxStage::Propagated,
            InflightStage::Acknowledged => MempoolTxStage::Acknowledged,
            InflightStage::Confirmed => MempoolTxStage::Confirmed,
        };
        MempoolTx::restored(
            hash,
            self.payload.clone(),
            stage,
            self.confirmations,
            self.non_confirmations,
            self.confirmed_at
                .as_ref()
                .map(|b| ChainPoint::from_bytes(b[..].try_into().unwrap())),
        )
    }
}

// ── Layer 2: table wrapper structs ──────────────────────────────────────

struct PendingTable;

impl PendingTable {
    const DEF: TableDefinition<'static, DbPendingKey, DbEraCbor> = TableDefinition::new("pending");

    fn initialize(wx: &redb::WriteTransaction) -> Result<(), RedbMempoolError> {
        wx.open_table(Self::DEF)?;
        Ok(())
    }

    fn has_any(rx: &redb::ReadTransaction) -> Result<bool, RedbMempoolError> {
        let table = rx.open_table(Self::DEF)?;
        Ok(table.len()? > 0)
    }

    fn peek(rx: &redb::ReadTransaction) -> Result<Vec<MempoolTx>, RedbMempoolError> {
        let table = rx.open_table(Self::DEF)?;
        let iter = table.iter()?;
        let mut result = Vec::new();
        for entry in iter {
            let entry = entry?;
            let key = entry.0.value();
            let payload = entry.1.value().0;
            result.push(MempoolTx::restored(
                key.hash(),
                payload,
                MempoolTxStage::Pending,
                0,
                0,
                None,
            ));
        }
        Ok(result)
    }

    fn contains(wx: &redb::WriteTransaction, tx_hash: &TxHash) -> Result<bool, RedbMempoolError> {
        let table = wx.open_table(Self::DEF)?;
        for entry in table.iter()? {
            let entry = entry?;
            if entry.0.value().hash() == *tx_hash {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn contains_hash(
        rx: &redb::ReadTransaction,
        tx_hash: &TxHash,
    ) -> Result<bool, RedbMempoolError> {
        let table = rx.open_table(Self::DEF)?;
        for entry in table.iter()? {
            let entry = entry?;
            if entry.0.value().hash() == *tx_hash {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn next_seq(wx: &redb::WriteTransaction) -> Result<u64, RedbMempoolError> {
        let table = wx.open_table(Self::DEF)?;
        let pending = table.last()?.map(|entry| entry.0.value().seq());
        let inflight = InflightTable::collect_all(wx)?;
        let held = inflight.iter().filter_map(|(_, record)| record.seq);
        Ok(pending
            .into_iter()
            .chain(held)
            .max()
            .map_or(0, |seq| seq + 1))
    }

    fn insert(
        wx: &redb::WriteTransaction,
        seq: u64,
        hash: &TxHash,
        payload: &EraCbor,
    ) -> Result<(), RedbMempoolError> {
        let mut table = wx.open_table(Self::DEF)?;
        let key = DbPendingKey::new(seq, hash);
        table.insert(key, DbEraCbor(payload.clone()))?;
        Ok(())
    }

    fn drain_by_hashes(
        wx: &redb::WriteTransaction,
        hashes: &HashSet<TxHash>,
    ) -> Result<Vec<(u64, TxHash, EraCbor)>, RedbMempoolError> {
        let mut table = wx.open_table(Self::DEF)?;
        let extracted = table.extract_if(|key, _value| hashes.contains(&key.hash()))?;
        extracted
            .map(|entry| {
                let (key, value) = entry?;
                let key = key.value();
                Ok((key.seq(), key.hash(), value.value().0))
            })
            .collect()
    }
}

struct InflightTable;

impl InflightTable {
    const DEF: TableDefinition<'static, DbTxHash, InflightRecord> =
        TableDefinition::new("inflight");

    fn initialize(wx: &redb::WriteTransaction) -> Result<(), RedbMempoolError> {
        wx.open_table(Self::DEF)?;
        Ok(())
    }

    fn get(
        rx: &redb::ReadTransaction,
        hash: &TxHash,
    ) -> Result<Option<InflightRecord>, RedbMempoolError> {
        let table = rx.open_table(Self::DEF)?;
        let key = DbTxHash::from_tx_hash(hash);
        let result = table.get(key)?.map(|e| e.value());
        Ok(result)
    }

    fn read(
        wx: &redb::WriteTransaction,
        hash: &TxHash,
    ) -> Result<Option<InflightRecord>, RedbMempoolError> {
        let table = wx.open_table(Self::DEF)?;
        let key = DbTxHash::from_tx_hash(hash);
        let result = table.get(key)?.map(|e| e.value());
        Ok(result)
    }

    fn write(
        wx: &redb::WriteTransaction,
        hash: &TxHash,
        record: &InflightRecord,
    ) -> Result<(), RedbMempoolError> {
        let mut table = wx.open_table(Self::DEF)?;
        table.insert(DbTxHash::from_tx_hash(hash), record.clone())?;
        Ok(())
    }

    fn remove(wx: &redb::WriteTransaction, hash: &TxHash) -> Result<(), RedbMempoolError> {
        let mut table = wx.open_table(Self::DEF)?;
        table.remove(DbTxHash::from_tx_hash(hash))?;
        Ok(())
    }

    fn collect_all(
        wx: &redb::WriteTransaction,
    ) -> Result<Vec<(TxHash, InflightRecord)>, RedbMempoolError> {
        let table = wx.open_table(Self::DEF)?;
        let mut entries = Vec::new();
        for entry in table.iter()? {
            let entry = entry?;
            entries.push((entry.0.value().to_tx_hash(), entry.1.value()));
        }
        Ok(entries)
    }

    fn peek(rx: &redb::ReadTransaction) -> Result<Vec<MempoolTx>, RedbMempoolError> {
        let table = rx.open_table(Self::DEF)?;
        let mut records = Vec::new();
        for entry in table.iter()? {
            let entry = entry?;
            records.push((entry.0.value().to_tx_hash(), entry.1.value()));
        }
        records.sort_by_key(|(_, record)| record.seq);
        Ok(records
            .into_iter()
            .map(|(hash, record)| record.to_mempool_tx(hash))
            .collect())
    }
}

struct FinalizedTable;

impl FinalizedTable {
    const DEF: TableDefinition<'static, u64, FinalizedEntry> = TableDefinition::new("finalized");

    fn initialize(wx: &redb::WriteTransaction) -> Result<(), RedbMempoolError> {
        wx.open_table(Self::DEF)?;
        Ok(())
    }

    fn append(wx: &redb::WriteTransaction, entry: FinalizedEntry) -> Result<(), RedbMempoolError> {
        let mut table = wx.open_table(Self::DEF)?;
        let seq = match table.last()? {
            Some(e) => e.0.value() + 1,
            None => 0,
        };
        table.insert(seq, entry)?;
        Ok(())
    }

    fn paginate(
        rx: &redb::ReadTransaction,
        cursor: u64,
        limit: usize,
    ) -> Result<MempoolPage, RedbMempoolError> {
        let table = rx.open_table(Self::DEF)?;
        let iter = table.range(cursor..)?;
        let mut items = Vec::with_capacity(limit);
        let mut last_seq = None;

        for entry in iter {
            if items.len() >= limit {
                break;
            }
            let entry = entry?;
            let seq = entry.0.value();
            let log_entry = entry.1.value();
            items.push(log_entry.into_mempool_tx());
            last_seq = Some(seq);
        }

        let next_cursor = if items.len() >= limit {
            last_seq.map(|s| s + 1)
        } else {
            None
        };

        Ok(MempoolPage { items, next_cursor })
    }
}

// ── Layer 3: RedbMempool orchestration ──────────────────────────────────

#[derive(Clone)]
pub struct RedbMempool {
    db: Arc<redb::Database>,
    /// The transactions the pending and inflight tables hold, for their
    /// decodes and the index of their effects.
    unconfirmed: Arc<RwLock<UnconfirmedTxs<()>>>,
    updates: broadcast::Sender<MempoolEvent>,
    submit: Arc<std::sync::Mutex<()>>,
}

pub struct RedbMempoolStream {
    inner: BroadcastStream<MempoolEvent>,
}

impl futures_core::Stream for RedbMempoolStream {
    type Item = Result<MempoolEvent, MempoolError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use futures_util::StreamExt;

        match self.inner.poll_next_unpin(cx) {
            std::task::Poll::Ready(Some(x)) => match x {
                Ok(x) => std::task::Poll::Ready(Some(Ok(x))),
                Err(err) => {
                    std::task::Poll::Ready(Some(Err(MempoolError::Internal(Box::new(err)))))
                }
            },
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

impl RedbMempool {
    pub fn open(
        path: impl AsRef<std::path::Path>,
        config: &RedbMempoolConfig,
        bound: MempoolBound,
    ) -> Result<Self, MempoolError> {
        let db = redb::Database::builder()
            .set_repair_callback(|x| {
                warn!(progress = x.progress() * 100f64, "mempool db is repairing")
            })
            .set_cache_size(1024 * 1024 * config.cache.unwrap_or(DEFAULT_CACHE_SIZE_MB))
            .create(path)
            .map_err(|e| MempoolError::Internal(Box::new(e)))?;

        Self::from_db(db, bound)
    }

    pub fn in_memory() -> Result<Self, MempoolError> {
        let db = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .map_err(|e| MempoolError::Internal(Box::new(e)))?;

        Self::from_db(db, MempoolBound::default())
    }

    fn from_db(db: redb::Database, bound: MempoolBound) -> Result<Self, MempoolError> {
        let (updates, _) = broadcast::channel(16);

        let out = Self {
            db: Arc::new(db),
            unconfirmed: Arc::new(RwLock::new(UnconfirmedTxs::bounded(bound))),
            updates,
            submit: Arc::default(),
        };

        out.ensure_initialized()?;
        out.load_unconfirmed()?;
        Ok(out)
    }

    fn ensure_initialized(&self) -> Result<(), RedbMempoolError> {
        let wx = self.db.begin_write()?;
        PendingTable::initialize(&wx)?;
        InflightTable::initialize(&wx)?;
        FinalizedTable::initialize(&wx)?;
        wx.commit()?;
        Ok(())
    }

    /// Holds every transaction the pending and inflight tables hold.
    fn load_unconfirmed(&self) -> Result<(), RedbMempoolError> {
        let rx = self.db.begin_read()?;
        let mut unconfirmed = self.unconfirmed.write().unwrap();
        for tx in PendingTable::peek(&rx)?
            .into_iter()
            .chain(InflightTable::peek(&rx)?)
        {
            unconfirmed.insert((), tx);
        }
        Ok(())
    }

    /// Each of `txs` with the payload and decode of the transaction held under
    /// its hash.
    fn sharing_decodes(&self, txs: Vec<MempoolTx>) -> Vec<MempoolTx> {
        let unconfirmed = self.unconfirmed.read().unwrap();
        txs.into_iter()
            .map(|tx| match unconfirmed.get(&tx.hash()) {
                Some((_, held)) => {
                    let mut shared = held.clone();
                    shared.stage = tx.stage;
                    shared.confirmations = tx.confirmations;
                    shared.non_confirmations = tx.non_confirmations;
                    shared.confirmed_at = tx.confirmed_at;
                    shared
                }
                None => tx,
            })
            .collect()
    }

    fn notify(&self, tx: MempoolTx) {
        if self.updates.send(MempoolEvent { tx }).is_err() {
            debug!("no mempool update receivers");
        }
    }

    /// Commits `f` and stops holding each transaction it finalized or dropped.
    fn with_write_tx<F>(&self, f: F) -> Result<(), RedbMempoolError>
    where
        F: FnOnce(&redb::WriteTransaction) -> Result<Vec<MempoolTx>, RedbMempoolError>,
    {
        let mut unconfirmed = self.unconfirmed.write().unwrap();
        let wx = self.db.begin_write()?;
        let events = f(&wx)?;
        wx.commit()?;
        for tx in &events {
            if matches!(
                tx.stage,
                MempoolTxStage::Finalized | MempoolTxStage::Dropped
            ) {
                unconfirmed.remove(&tx.hash());
            }
        }
        drop(unconfirmed);

        for tx in events {
            self.notify(tx);
        }

        Ok(())
    }

    fn receive_inner(&self, tx: MempoolTx) -> Result<(), RedbMempoolError> {
        let mut unconfirmed = self.unconfirmed.write().unwrap();
        let wx = self.db.begin_write()?;
        if PendingTable::contains(&wx, &tx.hash())? {
            return Err(MempoolError::DuplicateTx.into());
        }
        if InflightTable::read(&wx, &tx.hash())?.is_some() {
            return Err(MempoolError::DuplicateTx.into());
        }
        unconfirmed.room_for(&tx)?;
        let seq = PendingTable::next_seq(&wx)?;
        PendingTable::insert(&wx, seq, &tx.hash(), tx.payload())?;
        wx.commit()?;
        unconfirmed.insert((), tx.clone());
        drop(unconfirmed);
        self.notify(tx);
        Ok(())
    }
}

impl MempoolStore for RedbMempool {
    type Stream = RedbMempoolStream;

    fn receive(&self, tx: MempoolTx) -> Result<(), MempoolError> {
        debug!(tx.hash = %tx.hash(),"tx received");
        self.receive_inner(tx)?;
        Ok(())
    }

    fn has_pending(&self) -> bool {
        let rx = match self.db.begin_read() {
            Ok(rx) => rx,
            Err(_) => return false,
        };
        PendingTable::has_any(&rx).unwrap_or(false)
    }

    fn peek_pending(&self) -> Vec<MempoolTx> {
        let rx = match self.db.begin_read() {
            Ok(rx) => rx,
            Err(_) => return vec![],
        };
        self.sharing_decodes(PendingTable::peek(&rx).unwrap_or_default())
    }

    fn mark_inflight(&self, hashes: &[TxHash]) -> Result<(), MempoolError> {
        let hash_set: HashSet<TxHash> = hashes.iter().copied().collect();

        self.with_write_tx(|wx| {
            let drained = PendingTable::drain_by_hashes(wx, &hash_set)?;
            let mut events = Vec::new();
            for (seq, hash, payload) in drained {
                let record = InflightRecord::new(payload, seq);
                let tx = record.to_mempool_tx(hash);
                InflightTable::write(wx, &hash, &record)?;
                debug!(tx.hash = %tx.hash(),"tx inflight");
                events.push(tx);
            }
            Ok(events)
        })?;

        Ok(())
    }

    fn mark_acknowledged(&self, hashes: &[TxHash]) -> Result<(), MempoolError> {
        self.with_write_tx(|wx| {
            let mut events = Vec::new();
            for hash in hashes {
                if let Some(mut record) = InflightTable::read(wx, hash)? {
                    if record.acknowledge() {
                        let tx = record.to_mempool_tx(*hash);
                        InflightTable::write(wx, hash, &record)?;
                        debug!(tx.hash = %tx.hash(),"tx acknowledged");
                        events.push(tx);
                    }
                }
            }
            Ok(events)
        })?;

        Ok(())
    }

    fn find_inflight(&self, tx_hash: &TxHash) -> Option<MempoolTx> {
        let rx = self.db.begin_read().ok()?;
        let record = InflightTable::get(&rx, tx_hash).ok()??;
        self.sharing_decodes(vec![record.to_mempool_tx(*tx_hash)])
            .pop()
    }

    fn peek_inflight(&self) -> Vec<MempoolTx> {
        let rx = match self.db.begin_read() {
            Ok(rx) => rx,
            Err(_) => return vec![],
        };
        self.sharing_decodes(InflightTable::peek(&rx).unwrap_or_default())
    }

    fn confirm(
        &self,
        point: &ChainPoint,
        seen_txs: &[TxHash],
        unseen_txs: &[TxHash],
        finalize_threshold: u32,
        drop_threshold: u32,
    ) -> Result<(), MempoolError> {
        let point = point.clone();
        self.with_write_tx(|wx| {
            let seen_set: HashSet<TxHash> = seen_txs.iter().copied().collect();
            let unseen_set: HashSet<TxHash> = unseen_txs.iter().copied().collect();
            // A pending transaction the block holds is confirmed.
            for (seq, hash, payload) in PendingTable::drain_by_hashes(wx, &seen_set)? {
                InflightTable::write(wx, &hash, &InflightRecord::new(payload, seq))?;
            }
            let entries = InflightTable::collect_all(wx)?;
            let mut events = Vec::new();

            for (tx_hash, mut record) in entries {
                if seen_set.contains(&tx_hash) {
                    record.confirm(&point);
                    // Check if this tx is now finalizable
                    if record.is_finalizable(finalize_threshold) {
                        let mut tx = record.to_mempool_tx(tx_hash);
                        let log_entry = record.into_finalized_entry(tx_hash);
                        InflightTable::remove(wx, &tx_hash)?;
                        FinalizedTable::append(wx, log_entry)?;
                        tx.stage = MempoolTxStage::Finalized;
                        debug!(tx.hash = %tx.hash(),"tx finalized");
                        events.push(tx);
                    } else {
                        InflightTable::write(wx, &tx_hash, &record)?;
                        let tx = record.to_mempool_tx(tx_hash);
                        debug!(tx.hash = %tx.hash(),"tx confirmed");
                        events.push(tx);
                    }
                } else if unseen_set.contains(&tx_hash) {
                    record.retry();
                    InflightTable::remove(wx, &tx_hash)?;
                    let seq = match record.seq {
                        Some(seq) => seq,
                        None => PendingTable::next_seq(wx)?,
                    };
                    PendingTable::insert(wx, seq, &tx_hash, &record.payload)?;
                    let mut tx = record.to_mempool_tx(tx_hash);
                    tx.retry();
                    debug!(tx.hash = %tx.hash(),"retry tx");
                    events.push(tx);
                } else if record.stage == InflightStage::Confirmed {
                    // Already confirmed on-chain; treat subsequent blocks as
                    // additional confirmations (increasing depth).
                    record.confirm(&point);
                    if record.is_finalizable(finalize_threshold) {
                        let mut tx = record.to_mempool_tx(tx_hash);
                        let log_entry = record.into_finalized_entry(tx_hash);
                        InflightTable::remove(wx, &tx_hash)?;
                        FinalizedTable::append(wx, log_entry)?;
                        tx.stage = MempoolTxStage::Finalized;
                        debug!(tx.hash = %tx.hash(),"tx finalized");
                        events.push(tx);
                    } else {
                        InflightTable::write(wx, &tx_hash, &record)?;
                        let tx = record.to_mempool_tx(tx_hash);
                        events.push(tx);
                    }
                } else {
                    record.mark_stale();
                    // Check if this tx should be dropped
                    if record.is_droppable(drop_threshold) {
                        let mut tx = record.to_mempool_tx(tx_hash);
                        let log_entry = record.into_dropped_entry(tx_hash);
                        InflightTable::remove(wx, &tx_hash)?;
                        FinalizedTable::append(wx, log_entry)?;
                        tx.stage = MempoolTxStage::Dropped;
                        debug!(tx.hash = %tx.hash(),"tx dropped");
                        events.push(tx);
                    } else {
                        InflightTable::write(wx, &tx_hash, &record)?;
                    }
                }
            }

            Ok(events)
        })?;

        Ok(())
    }

    fn drop_pending(&self, hashes: &[TxHash]) -> Result<(), MempoolError> {
        let hash_set: HashSet<TxHash> = hashes.iter().copied().collect();

        self.with_write_tx(|wx| {
            let drained = PendingTable::drain_by_hashes(wx, &hash_set)?;
            let mut events = Vec::new();
            for (seq, hash, payload) in drained {
                let record = InflightRecord::new(payload, seq);
                let mut tx = record.to_mempool_tx(hash);
                FinalizedTable::append(wx, record.into_dropped_entry(hash))?;
                tx.stage = MempoolTxStage::Dropped;
                debug!(tx.hash = %tx.hash(),"pending tx dropped");
                events.push(tx);
            }
            Ok(events)
        })?;

        Ok(())
    }

    fn check_status(&self, tx_hash: &TxHash) -> TxStatus {
        let rx = match self.db.begin_read() {
            Ok(rx) => rx,
            Err(_) => {
                return TxStatus {
                    stage: MempoolTxStage::Unknown,
                    confirmations: 0,
                    non_confirmations: 0,
                    confirmed_at: None,
                }
            }
        };

        if let Ok(Some(record)) = InflightTable::get(&rx, tx_hash) {
            return record.to_tx_status();
        }

        if let Ok(true) = PendingTable::contains_hash(&rx, tx_hash) {
            return TxStatus {
                stage: MempoolTxStage::Pending,
                confirmations: 0,
                non_confirmations: 0,
                confirmed_at: None,
            };
        }

        TxStatus {
            stage: MempoolTxStage::Unknown,
            confirmations: 0,
            non_confirmations: 0,
            confirmed_at: None,
        }
    }

    fn dump_finalized(&self, cursor: u64, limit: usize) -> MempoolPage {
        let rx = match self.db.begin_read() {
            Ok(rx) => rx,
            Err(_) => {
                return MempoolPage {
                    items: vec![],
                    next_cursor: None,
                }
            }
        };
        FinalizedTable::paginate(&rx, cursor, limit).unwrap_or(MempoolPage {
            items: vec![],
            next_cursor: None,
        })
    }

    fn read_unconfirmed<R>(&self, f: impl FnOnce(&UnconfirmedIndex) -> R) -> R {
        f(self.unconfirmed.read().unwrap().index())
    }

    fn submit_lock(&self) -> &std::sync::Mutex<()> {
        &self.submit
    }

    fn subscribe(&self) -> Self::Stream {
        RedbMempoolStream {
            inner: BroadcastStream::new(self.updates.subscribe()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dolos_core::PassedBound;

    fn test_store() -> RedbMempool {
        RedbMempool::in_memory().unwrap()
    }

    fn test_hash(n: u8) -> TxHash {
        dolos_testing::tx_sequence_to_hash(n as u64)
    }

    fn test_tx(n: u8) -> MempoolTx {
        dolos_testing::mempool::make_test_mempool_tx(test_hash(n))
    }

    fn test_point() -> ChainPoint {
        ChainPoint::Specific(12345, pallas::crypto::hash::Hash::new([0xAB; 32]))
    }

    fn test_point_2() -> ChainPoint {
        ChainPoint::Specific(12346, pallas::crypto::hash::Hash::new([0xCD; 32]))
    }

    #[test]
    fn test_empty_store() {
        let store = test_store();
        assert!(!store.has_pending());
        assert!(store.peek_pending().is_empty());
    }

    #[test]
    fn test_receive_and_has_pending() {
        let store = test_store();
        let tx = test_tx(1);
        let hash = tx.hash();
        let payload = tx.payload().clone();

        store.receive(tx).unwrap();

        assert!(store.has_pending());

        let peeked = store.peek_pending();
        assert_eq!(peeked.len(), 1);
        assert_eq!(peeked[0].hash(), hash);
        assert_eq!(peeked[0].payload(), &payload);
    }

    #[test]
    fn test_receive_duplicate_hash() {
        let store = test_store();
        let tx = test_tx(1);
        store.receive(tx.clone()).unwrap();

        let err = store.receive(tx).unwrap_err();
        assert!(matches!(err, MempoolError::DuplicateTx));
        assert_eq!(store.peek_pending().len(), 1);
    }

    #[test]
    fn test_mark_inflight() {
        let store = test_store();
        let tx = test_tx(1);
        let hash = tx.hash();

        store.receive(tx).unwrap();
        store.mark_inflight(&[hash]).unwrap();

        assert!(!store.has_pending());
        assert!(matches!(
            store.check_status(&hash).stage,
            MempoolTxStage::Propagated
        ));
        assert!(store.find_inflight(&hash).is_some());
    }

    #[test]
    fn test_mark_acknowledged() {
        let store = test_store();
        let tx = test_tx(1);
        let hash = tx.hash();

        store.receive(tx).unwrap();
        store.mark_inflight(&[hash]).unwrap();
        store.mark_acknowledged(&[hash]).unwrap();

        assert!(matches!(
            store.check_status(&hash).stage,
            MempoolTxStage::Acknowledged
        ));
        // find_inflight now returns any inflight sub-stage (Propagated, Acknowledged,
        // Confirmed)
        assert!(store.find_inflight(&hash).is_some());
    }

    #[test]
    fn test_stage_unknown() {
        let store = test_store();
        let hash = test_hash(99);

        assert!(matches!(
            store.check_status(&hash).stage,
            MempoolTxStage::Unknown
        ));
    }

    #[test]
    fn test_get_tx_status_lifecycle() {
        let store = test_store();
        let tx = test_tx(1);
        let hash = tx.hash();
        let point = test_point();

        // Unknown
        let status = store.check_status(&hash);
        assert!(matches!(status.stage, MempoolTxStage::Unknown));
        assert_eq!(status.confirmations, 0);
        assert!(status.confirmed_at.is_none());

        // Pending
        store.receive(tx).unwrap();
        let status = store.check_status(&hash);
        assert!(matches!(status.stage, MempoolTxStage::Pending));
        assert_eq!(status.confirmations, 0);
        assert!(status.confirmed_at.is_none());

        // Propagated
        store.mark_inflight(&[hash]).unwrap();
        let status = store.check_status(&hash);
        assert!(matches!(status.stage, MempoolTxStage::Propagated));
        assert_eq!(status.confirmations, 0);
        assert!(status.confirmed_at.is_none());

        // Acknowledged
        store.mark_acknowledged(&[hash]).unwrap();
        let status = store.check_status(&hash);
        assert!(matches!(status.stage, MempoolTxStage::Acknowledged));
        assert_eq!(status.confirmations, 0);
        assert!(status.confirmed_at.is_none());

        // Confirmed (1st confirmation)
        store
            .confirm(&point, &[hash], &[], u32::MAX, u32::MAX)
            .unwrap();
        let status = store.check_status(&hash);
        assert!(matches!(status.stage, MempoolTxStage::Confirmed));
        assert_eq!(status.confirmations, 1);
        assert!(status.confirmed_at.is_some());
        assert_eq!(status.confirmed_at.as_ref().unwrap().slot(), point.slot());

        // Confirmed (2nd confirmation — confirmed_at stays the same)
        store
            .confirm(&test_point_2(), &[hash], &[], u32::MAX, u32::MAX)
            .unwrap();
        let status = store.check_status(&hash);
        assert!(matches!(status.stage, MempoolTxStage::Confirmed));
        assert_eq!(status.confirmations, 2);
        assert_eq!(status.confirmed_at.as_ref().unwrap().slot(), point.slot());

        // Finalized — trigger via confirm with finalize_threshold=2
        store
            .confirm(&test_point(), &[hash], &[], 2, u32::MAX)
            .unwrap();
        let status = store.check_status(&hash);
        assert!(matches!(status.stage, MempoolTxStage::Unknown));
        assert_eq!(status.confirmations, 0);
        assert!(status.confirmed_at.is_none());
    }

    #[test]
    fn test_read_finalized_log_empty() {
        let store = test_store();
        let page = store.dump_finalized(0, 50);
        assert!(page.items.is_empty());
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn test_read_finalized_log_pagination() {
        let store = test_store();
        let point = test_point();

        // Finalize 3 transactions
        for n in 0..3u8 {
            let tx = test_tx(n);
            let hash = tx.hash();
            store.receive(tx).unwrap();
            store.mark_inflight(&[hash]).unwrap();
            store.mark_acknowledged(&[hash]).unwrap();
            store
                .confirm(&point, &[hash], &[], u32::MAX, u32::MAX)
                .unwrap();
            // Second confirm with finalize_threshold=2 triggers finalization
            store
                .confirm(&test_point_2(), &[hash], &[], 2, u32::MAX)
                .unwrap();
        }

        // Read all
        let page = store.dump_finalized(0, 50);
        assert_eq!(page.items.len(), 3);
        assert!(page.next_cursor.is_none());
        for entry in &page.items {
            assert_eq!(entry.confirmations, 2);
            assert!(entry.confirmed_at.is_some());
            assert!(
                !entry.payload().1.is_empty(),
                "finalized entry should include payload"
            );
        }

        // Read with limit
        let page = store.dump_finalized(0, 2);
        assert_eq!(page.items.len(), 2);
        assert!(page.next_cursor.is_some());

        // Read remainder
        let page2 = store.dump_finalized(page.next_cursor.unwrap(), 50);
        assert_eq!(page2.items.len(), 1);
        assert!(page2.next_cursor.is_none());
    }

    fn pending(store: &RedbMempool) -> Vec<TxHash> {
        store.peek_pending().iter().map(|tx| tx.hash()).collect()
    }

    #[test]
    fn a_pending_transaction_admitted_later_stays_behind_one_requeued() {
        let store = test_store();
        let (a, t, u) = (test_hash(1), test_hash(2), test_hash(3));
        store.receive(test_tx(1)).unwrap();
        store.mark_inflight(&[a]).unwrap();
        store.receive(test_tx(2)).unwrap();
        store.confirm(&test_point(), &[a], &[], 10, 10).unwrap();
        store.confirm(&test_point_2(), &[], &[a], 10, 10).unwrap();
        assert_eq!(pending(&store), [a, t]);
        store.receive(test_tx(3)).unwrap();
        assert_eq!(pending(&store), [a, t, u]);
    }

    #[test]
    fn a_pending_transaction_seen_in_a_block_is_confirmed() {
        let store = test_store();
        let (a, t) = (test_hash(1), test_hash(2));
        store.receive(test_tx(1)).unwrap();
        store.receive(test_tx(2)).unwrap();
        store.confirm(&test_point_2(), &[a], &[], 10, 10).unwrap();
        assert_eq!(store.check_status(&a).stage, MempoolTxStage::Confirmed);
        assert_eq!(store.check_status(&a).confirmed_at, Some(test_point_2()));
        assert_eq!(pending(&store), [t]);
    }

    fn inflight(store: &RedbMempool) -> Vec<TxHash> {
        store.peek_inflight().iter().map(|tx| tx.hash()).collect()
    }

    #[test]
    fn an_inflight_transaction_requeued_and_sent_again_keeps_its_first_place() {
        let store = test_store();
        let (a, t) = (test_hash(1), test_hash(2));
        store.receive(test_tx(1)).unwrap();
        store.mark_inflight(&[a]).unwrap();
        store.receive(test_tx(2)).unwrap();
        store.mark_inflight(&[t]).unwrap();
        store.confirm(&test_point(), &[a], &[], 10, 10).unwrap();
        store.confirm(&test_point_2(), &[], &[a], 10, 10).unwrap();
        assert_eq!(pending(&store), [a]);
        store.mark_inflight(&[a]).unwrap();
        assert_eq!(inflight(&store), [a, t]);
    }

    #[test]
    fn inflight_transactions_list_in_the_order_they_were_admitted() {
        let store = test_store();
        let admitted: Vec<TxHash> = (1..=8).map(test_hash).collect();
        let mut by_hash = admitted.clone();
        by_hash.sort();
        assert_ne!(by_hash, admitted);
        for n in 1..=8 {
            store.receive(test_tx(n)).unwrap();
        }
        store.mark_inflight(&admitted).unwrap();
        assert_eq!(inflight(&store), admitted);
    }

    #[test]
    fn test_inflight_listing() {
        let store = test_store();
        let h1 = test_hash(1);
        let h2 = test_hash(2);
        let h3 = test_hash(3);

        store.receive(test_tx(1)).unwrap();
        store.receive(test_tx(2)).unwrap();
        store.receive(test_tx(3)).unwrap();

        store.mark_inflight(&[h1, h2, h3]).unwrap();

        // All three start as Propagated
        let listing = store.peek_inflight();
        assert_eq!(listing.len(), 3);
        assert!(listing
            .iter()
            .all(|tx| tx.stage == MempoolTxStage::Propagated));

        // Acknowledge h2
        store.mark_acknowledged(&[h2]).unwrap();
        let listing = store.peek_inflight();
        assert_eq!(listing.len(), 3);
        let h2_stage = listing
            .iter()
            .find(|tx| tx.hash() == h2)
            .unwrap()
            .stage
            .clone();
        assert_eq!(h2_stage, MempoolTxStage::Acknowledged);

        // Confirm h2
        store
            .confirm(&test_point(), &[h2], &[], u32::MAX, u32::MAX)
            .unwrap();
        let listing = store.peek_inflight();
        assert_eq!(listing.len(), 3);
        let h2_stage = listing
            .iter()
            .find(|tx| tx.hash() == h2)
            .unwrap()
            .stage
            .clone();
        assert_eq!(h2_stage, MempoolTxStage::Confirmed);

        // Finalize h2 — second confirm with finalize_threshold=2
        store
            .confirm(&test_point_2(), &[h2], &[], 2, u32::MAX)
            .unwrap();
        let listing = store.peek_inflight();
        assert_eq!(listing.len(), 2);
        assert!(!listing.iter().any(|tx| tx.hash() == h2));
    }

    /// Every transaction of the fixture block that lists a sub transaction.
    fn block_txs() -> Vec<MempoolTx> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../test_data/musashi-w36/ranking-sub-transaction.block");
        let text = std::fs::read_to_string(&path).unwrap();
        let cbor = hex::decode(text.trim()).unwrap();
        let block = pallas::ledger::traverse::MultiEraBlock::decode(&cbor).unwrap();
        block
            .txs()
            .iter()
            .map(|tx| MempoolTx::new(tx.hash(), EraCbor(tx.era().into(), tx.encode()), vec![]))
            .collect()
    }

    /// The index of the transactions the store lists as pending or inflight.
    fn walked(store: &RedbMempool) -> UnconfirmedIndex {
        let inflight = store.peek_inflight();
        UnconfirmedIndex::of(store.peek_pending().iter().chain(&inflight))
    }

    fn index(store: &RedbMempool) -> UnconfirmedIndex {
        store.read_unconfirmed(|index| index.clone())
    }

    #[test]
    fn the_unconfirmed_index_counts_each_live_transaction_through_every_stage() {
        let store = test_store();
        let txs = block_txs();
        let hashes: Vec<TxHash> = txs.iter().map(MempoolTx::hash).collect();
        assert!(hashes.len() >= 4, "fixture transactions: {}", hashes.len());
        for tx in &txs {
            store.receive(tx.clone()).unwrap();
        }
        assert_ne!(index(&store), UnconfirmedIndex::default());
        assert_eq!(index(&store), walked(&store));

        store.mark_inflight(&hashes[..3]).unwrap();
        store.mark_acknowledged(&hashes[..1]).unwrap();
        store.drop_pending(&hashes[3..4]).unwrap();
        assert_eq!(index(&store), walked(&store));

        store
            .confirm(&test_point(), &hashes[..1], &hashes[1..2], 2, 1)
            .unwrap();
        assert_eq!(index(&store), walked(&store));

        store.confirm(&test_point_2(), &[], &[], 2, 1).unwrap();
        assert_eq!(index(&store), walked(&store));
        assert_eq!(
            store.check_status(&hashes[0]).stage,
            MempoolTxStage::Unknown
        );
    }

    #[test]
    fn a_reopened_store_indexes_the_transactions_it_held() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mempool");
        let config = RedbMempoolConfig::default();
        let txs = block_txs();
        let hashes: Vec<TxHash> = txs.iter().map(MempoolTx::hash).collect();
        {
            let store = RedbMempool::open(&path, &config, MempoolBound::default()).unwrap();
            for tx in &txs {
                store.receive(tx.clone()).unwrap();
            }
            store.mark_inflight(&hashes[..2]).unwrap();
            store.drop_pending(&hashes[2..3]).unwrap();
        }

        let store = RedbMempool::open(&path, &config, MempoolBound::default()).unwrap();

        assert_ne!(index(&store), UnconfirmedIndex::default());
        assert_eq!(index(&store), walked(&store));
    }

    fn sized(n: u8, bytes: usize) -> MempoolTx {
        MempoolTx::new(test_hash(n), EraCbor(7, vec![0x80; bytes]), vec![])
    }

    #[test]
    fn a_submit_past_the_bound_is_refused_and_leaves_the_tables_as_they_were() {
        let dir = tempfile::tempdir().unwrap();
        let bound = MempoolBound {
            max_txs: 2,
            max_bytes: 25,
        };
        let store =
            RedbMempool::open(dir.path().join("mempool"), &Default::default(), bound).unwrap();
        store.receive(sized(1, 10)).unwrap();
        store.receive(sized(2, 10)).unwrap();
        let refused = store.receive(sized(3, 1));
        assert!(
            matches!(
                refused,
                Err(MempoolError::Full(PassedBound::Txs {
                    bound: 2,
                    would_hold: 3
                }))
            ),
            "{refused:?}"
        );
        store.drop_pending(&[test_hash(2)]).unwrap();
        let refused = store.receive(sized(4, 16));
        assert!(
            matches!(
                refused,
                Err(MempoolError::Full(PassedBound::Bytes {
                    bound: 25,
                    would_hold: 26
                }))
            ),
            "{refused:?}"
        );
        let pending: Vec<TxHash> = store.peek_pending().iter().map(MempoolTx::hash).collect();
        assert_eq!(pending, [test_hash(1)]);
        store.receive(sized(5, 15)).unwrap();
    }

    #[test]
    fn a_transaction_in_flight_and_one_reloaded_count_against_the_bound() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mempool");
        let bound = MempoolBound {
            max_txs: 10,
            max_bytes: 10,
        };
        {
            let store = RedbMempool::open(&path, &Default::default(), bound).unwrap();
            store.receive(sized(1, 10)).unwrap();
            store.mark_inflight(&[test_hash(1)]).unwrap();
            assert!(store.receive(sized(2, 1)).is_err());
        }

        let store = RedbMempool::open(&path, &Default::default(), bound).unwrap();
        let refused = store.receive(sized(2, 1));
        assert!(
            matches!(
                refused,
                Err(MempoolError::Full(PassedBound::Bytes {
                    bound: 10,
                    would_hold: 11
                }))
            ),
            "{refused:?}"
        );
    }

    #[test]
    fn a_lookup_reads_the_index_the_store_holds() {
        let store = test_store();
        for tx in block_txs() {
            store.receive(tx).unwrap();
        }
        let made =
            |store: &RedbMempool| store.read_unconfirmed(|index| index.scan_produced(|_| true));
        let refs = made(&store);
        assert!(!refs.is_empty());
        let outputs = |store: &RedbMempool| {
            store.read_unconfirmed(|index| index.take_produced(&mut refs.clone()))
        };

        let first = outputs(&store);
        let again = outputs(&store);

        for txoref in &refs {
            assert!(
                Arc::ptr_eq(&first[txoref], &again[txoref]),
                "the lookup built the index again"
            );
        }
    }

    #[test]
    fn a_peek_reads_the_decode_of_the_transaction_the_store_holds() {
        let store = test_store();
        let txs = block_txs();
        let hashes: Vec<TxHash> = txs.iter().map(MempoolTx::hash).collect();
        for tx in txs {
            store.receive(tx).unwrap();
        }
        store.mark_inflight(&hashes[..1]).unwrap();

        let decodes = |txs: &[MempoolTx]| {
            txs.iter()
                .map(|tx| tx.with_tx(|tx| std::ptr::from_ref(tx.unwrap()) as usize))
                .collect::<Vec<_>>()
        };
        let pending = (store.peek_pending(), store.peek_pending());
        let inflight = (store.peek_inflight(), store.peek_inflight());

        assert!(!pending.0.is_empty() && !inflight.0.is_empty());
        assert_eq!(decodes(&pending.0), decodes(&pending.1));
        assert_eq!(decodes(&inflight.0), decodes(&inflight.1));
    }
}
