pub mod storage;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use dolos_cardano::CardanoLogic;
use dolos_core::{
    config::{StorageConfig, SyncConfig},
    *,
};
use pallas::ledger::traverse::MultiEraBlock;

pub use storage::{ArchiveStoreBackend, MempoolBackend, StateStoreBackend, WalStoreBackend};

/// Type alias for the WAL store specialized for Cardano.
pub type WalAdapter = WalStoreBackend<dolos_cardano::CardanoDelta>;

pub struct TipSubscription {
    replay: VecDeque<(ChainPoint, RawBlock)>,
    receiver: tokio::sync::broadcast::Receiver<TipEvent>,
}

impl dolos_core::TipSubscription for TipSubscription {
    async fn next_tip(&mut self) -> TipEvent {
        // Oldest first, because a subscriber applies what it is handed and a
        // block cannot be applied before the one it builds on.
        if let Some((point, block)) = self.replay.pop_front() {
            return TipEvent::Apply(point, block);
        }

        self.receiver.recv().await.unwrap()
    }
}

#[derive(Clone)]
pub struct DomainAdapter {
    pub storage_config: Arc<StorageConfig>,
    pub sync_config: Arc<SyncConfig>,
    pub genesis: Arc<Genesis>,
    pub wal: WalAdapter,
    pub chain: Arc<std::sync::RwLock<CardanoLogic>>,
    pub state: StateStoreBackend,
    pub archive: ArchiveStoreBackend,
    pub mempool: MempoolBackend,
    pub tip_broadcast: tokio::sync::broadcast::Sender<TipEvent>,
}

impl DomainAdapter {
    /// Gracefully shutdown all storage backends.
    ///
    /// This method should be called before the DomainAdapter goes out of scope,
    /// especially after heavy write operations like bulk imports. This ensures
    /// that storage backends complete any pending background work before being
    /// dropped.
    pub fn shutdown(&self) -> Result<(), DomainError> {
        tracing::info!("domain adapter: starting graceful shutdown");

        self.wal.shutdown().map_err(DomainError::WalError)?;
        self.state.shutdown().map_err(DomainError::StateError)?;
        self.archive.shutdown().map_err(DomainError::ArchiveError)?;

        tracing::info!("domain adapter: graceful shutdown complete");
        Ok(())
    }
}

impl Domain for DomainAdapter {
    type Entity = dolos_cardano::CardanoEntity;
    type EntityDelta = dolos_cardano::CardanoDelta;
    type Chain = CardanoLogic;
    type WorkUnit = dolos_cardano::CardanoWorkUnit;
    type Wal = WalAdapter;
    type State = StateStoreBackend;
    type Archive = ArchiveStoreBackend;
    type Mempool = MempoolBackend;
    type TipSubscription = TipSubscription;

    fn genesis(&self) -> Arc<Genesis> {
        self.genesis.clone()
    }

    fn read_chain(&self) -> std::sync::RwLockReadGuard<'_, Self::Chain> {
        self.chain.read().expect("chain lock poisoned")
    }

    fn write_chain(&self) -> std::sync::RwLockWriteGuard<'_, Self::Chain> {
        self.chain.write().expect("chain lock poisoned")
    }

    fn wal(&self) -> &Self::Wal {
        &self.wal
    }

    fn state(&self) -> &Self::State {
        &self.state
    }

    fn archive(&self) -> &Self::Archive {
        &self.archive
    }

    fn mempool(&self) -> &Self::Mempool {
        &self.mempool
    }

    fn storage_config(&self) -> &StorageConfig {
        &self.storage_config
    }

    fn sync_config(&self) -> &SyncConfig {
        &self.sync_config
    }

    fn watch_tip(&self, from: Option<ChainPoint>) -> Result<Self::TipSubscription, DomainError> {
        // TODO: do a more thorough analysis to understand if this approach is
        // susceptible to race conditions. Things to explore:
        // - a mutex to block the sending of events while gathering the replay.
        // - storing the previous block hash in the db to use for consistency
        //   checks.

        // We first create the receiver so that the subscriber internal
        // ring-buffer position is defined.
        let receiver = self.tip_broadcast.subscribe();

        // We then collect any gap between the from point and the current tip.
        // This assumes that no event will be sent between the creation
        // of the receiver and the collection of the replay.
        let blocks = self.wal().iter_blocks(from.clone(), None)?;
        let replay = dolos_core::wal::blocks_after(from.as_ref(), blocks).collect::<VecDeque<_>>();

        Ok(TipSubscription { replay, receiver })
    }

    fn notify_tip(&self, tip: TipEvent) {
        if self.tip_broadcast.receiver_count() > 0 {
            self.tip_broadcast.send(tip).unwrap();
        }
    }
}

impl crate::serve::ChainReads for DomainAdapter {
    fn utxos(
        &self,
        refs: &[pallas::interop::utxorpc::TxoRef],
    ) -> Option<pallas::interop::utxorpc::UtxoMap> {
        let dolos_refs: Vec<TxoRef> = refs.iter().map(|x| TxoRef::from(*x)).collect();
        let mut result: pallas::interop::utxorpc::UtxoMap =
            dolos_core::StateStore::get_utxos(self.state(), dolos_refs)
                .ok()?
                .into_iter()
                .map(|(k, v)| {
                    let era = v.0.try_into().expect("era out of range");
                    (k.into(), (era, v.1.clone()))
                })
                .collect();

        let missing: Vec<_> = refs
            .iter()
            .filter(|r| !result.contains_key(r))
            .copied()
            .collect();
        result.extend(resolve_archive_inputs(self.archive(), &missing));

        Some(result)
    }

    fn slot_timestamp(&self, slot: u64) -> Option<u64> {
        let time = dolos_cardano::eras::load_era_summary::<Self>(self.state())
            .ok()?
            .slot_time(slot);

        Some(time)
    }
}

/// Resolve spent inputs in batches: each historical block is loaded and decoded
/// once per call, even when many producing transactions share the same block.
fn resolve_archive_inputs<A: dolos_core::ArchiveStore>(
    archive: &A,
    refs: &[pallas::interop::utxorpc::TxoRef],
) -> pallas::interop::utxorpc::UtxoMap {
    use dolos_core::TxRange;
    use pallas::interop::utxorpc::{TxHash, TxoRef, UtxoMap};
    let start = std::time::Instant::now();
    let mut by_tx: HashMap<TxHash, Vec<TxoRef>> = HashMap::new();
    for r in refs {
        by_tx.entry(r.0).or_default().push(*r);
    }
    let mut by_slot: HashMap<u64, Vec<(TxHash, Vec<TxoRef>, Option<TxRange>)>> = HashMap::new();
    for (hash, refs) in by_tx {
        if let Ok(Some(location)) = archive.tx_location_by_hash(hash.as_ref()) {
            by_slot
                .entry(location.slot)
                .or_default()
                .push((hash, refs, location.range));
        }
    }
    let source_blocks = by_slot.len();
    let mut result = UtxoMap::new();
    for (slot, located) in by_slot {
        let Ok(Some(bytes)) = archive.get_block_by_slot(&slot) else {
            continue;
        };
        let mut wanted: HashMap<TxHash, Vec<TxoRef>> = HashMap::new();
        for (hash, refs, range) in located {
            let found =
                range.and_then(|range| dolos_core::tx_in_range(&bytes, &range, hash.as_ref()));
            let Some((_, tx)) = found else {
                wanted.insert(hash, refs);
                continue;
            };
            let outputs: HashMap<usize, _> = tx.produces().into_iter().collect();
            for r in refs {
                if let Some(output) = outputs.get(&(r.1 as usize)) {
                    result.insert(r, (tx.era(), output.encode()));
                }
            }
        }
        if wanted.is_empty() {
            continue;
        }
        let Ok(block) = MultiEraBlock::decode(&bytes) else {
            continue;
        };
        for (_, tx) in dolos_core::applied_txs(&block) {
            let Some(refs) = wanted.remove(&tx.hash()) else {
                continue;
            };
            let outputs: HashMap<usize, _> = tx.produces().into_iter().collect();
            for r in refs {
                if let Some(output) = outputs.get(&(r.1 as usize)) {
                    result.insert(r, (block.era(), output.encode()));
                }
            }
            if wanted.is_empty() {
                break;
            }
        }
    }
    tracing::trace!(requested = refs.len(), resolved = result.len(), source_blocks,
        elapsed = ?start.elapsed(), "Resolved archived inputs");
    result
}

#[cfg(test)]
mod input_resolution_tests {
    use super::*;
    use dolos_core::{indexes::ArchiveIndexDelta, ArchiveWriter};
    use dolos_testing::{
        measured::MeasuredStores,
        synthetic::{build_synthetic_blocks, SyntheticBlockConfig},
        toy_domain::{MemoryStores, ToyStores},
    };

    fn store_block<A: ArchiveStore>(
        archive: &A,
        bytes: &[u8],
    ) -> pallas::interop::utxorpc::UtxoMap {
        let block = MultiEraBlock::decode(bytes).unwrap();
        let txs = dolos_core::applied_txs(&block);
        let writer = archive.start_writer().unwrap();
        writer
            .apply(
                &ChainPoint::Specific(block.slot(), block.hash()),
                &Arc::new(bytes.to_vec()),
            )
            .unwrap();
        writer
            .apply_index(&[ArchiveIndexDelta {
                slot: block.slot(),
                block_hash: block.hash().to_vec(),
                block_number: Some(block.number()),
                txs: txs
                    .iter()
                    .map(|(_, tx)| dolos_core::IndexedTx::unplaced(tx.hash().to_vec()))
                    .collect(),
                tags: vec![],
            }])
            .unwrap();
        writer.commit().unwrap();
        txs.iter()
            .flat_map(|(_, tx)| {
                tx.produces()
                    .into_iter()
                    .map(|(i, o)| ((tx.hash(), i as u32), (block.era(), o.encode())))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    #[test]
    fn resolves_multiple_transactions_with_one_source_block_read() {
        let stores = MeasuredStores::new(MemoryStores::open());
        let archive = stores.archive();
        let (blocks, _, _) = build_synthetic_blocks(SyntheticBlockConfig {
            block_count: 1,
            txs_per_block: 4,
            ..Default::default()
        });
        let block = MultiEraBlock::decode(&blocks[0]).unwrap();
        let txs = block.txs();
        let expected = store_block(archive, &blocks[0]);
        let mut refs: Vec<_> = expected.keys().copied().collect();
        refs.push(refs[0]);
        refs.push((txs[0].hash(), u32::MAX));
        refs.push(([99u8; 32].into(), 0));
        archive.counters.reset();
        assert_eq!(resolve_archive_inputs(archive, &refs), expected);
        assert_eq!(archive.counters.snapshot().block_reads, 1);
        // Every successful read feeds exactly one MultiEraBlock::decode call.
        assert_eq!(
            archive.counters.snapshot().decoded_bytes,
            blocks[0].len() as u64
        );
        assert_eq!(
            archive.counters.snapshot().exact_lookups,
            txs.len() as u64 + 1
        );
    }

    #[test]
    fn resolves_across_blocks_and_skips_missing_or_malformed_history() {
        let stores = MeasuredStores::new(MemoryStores::open());
        let archive = stores.archive();
        let (blocks, _, _) = build_synthetic_blocks(SyntheticBlockConfig {
            block_count: 2,
            txs_per_block: 4,
            ..Default::default()
        });
        let expected: pallas::interop::utxorpc::UtxoMap = blocks
            .iter()
            .flat_map(|bytes| store_block(archive, bytes))
            .collect();
        let writer = archive.start_writer().unwrap();
        for (slot, hash) in [(100, [97; 32]), (101, [98; 32]), (1, [99; 32])] {
            writer
                .apply_index(&[ArchiveIndexDelta {
                    slot,
                    txs: vec![dolos_core::IndexedTx::unplaced(hash.to_vec())],
                    ..Default::default()
                }])
                .unwrap();
        }
        writer
            .apply(
                &ChainPoint::Specific(100, [97; 32].into()),
                &Arc::new(vec![0xff]),
            )
            .unwrap();
        writer.commit().unwrap();
        let mut refs: Vec<_> = expected.keys().copied().collect();
        refs.extend([
            ([97; 32].into(), 0),
            ([98; 32].into(), 0),
            ([99; 32].into(), 0),
        ]);
        archive.counters.reset();
        assert_eq!(resolve_archive_inputs(archive, &refs), expected);
        assert_eq!(archive.counters.snapshot().block_reads, 3);
        archive.counters.reset();
        assert!(resolve_archive_inputs(archive, &[]).is_empty());
        assert_eq!(archive.counters.snapshot().block_reads, 0);
        assert_eq!(archive.counters.snapshot().exact_lookups, 0);
    }

    #[test]
    fn preserves_archived_dijkstra_subtransaction_outputs() {
        // Reuse an existing unchanged capture; no new fixture is required.
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/test_data/musashi-w36/ranking-sub-transaction.block"
        ))
        .unwrap();
        let bytes = hex::decode(text.trim()).unwrap();
        let block = MultiEraBlock::decode(&bytes).unwrap();
        assert!(dolos_core::applied_txs(&block).iter().any(|(_, tx)| {
            matches!(tx, pallas::ledger::traverse::MultiEraTx::DijkstraSub(..))
                && !tx.outputs().is_empty()
        }));
        let stores = MeasuredStores::new(MemoryStores::open());
        let archive = stores.archive();
        let expected = store_block(archive, &bytes);
        archive.counters.reset();
        let refs: Vec<_> = expected.keys().copied().collect();
        assert_eq!(resolve_archive_inputs(archive, &refs), expected);
        assert_eq!(archive.counters.snapshot().block_reads, 1);
    }

    /// Outputs of a transaction the index locates resolve while every other
    /// transaction of its block is bytes that do not decode.
    #[test]
    fn resolves_a_located_transaction_alone() {
        let fixture = crate::tests::musashi::sub_transaction_block();
        let archive = fixture.domain.archive();
        let stored = archive.get_block_by_slot(&fixture.slot).unwrap().unwrap();
        let block = MultiEraBlock::decode(&stored).unwrap();
        let expected: pallas::interop::utxorpc::UtxoMap = dolos_core::applied_txs(&block)
            .iter()
            .filter(|(_, tx)| [fixture.parent, fixture.sub].contains(&tx.hash()))
            .flat_map(|(_, tx)| {
                tx.produces()
                    .into_iter()
                    .map(|(i, o)| ((tx.hash(), i as u32), (block.era(), o.encode())))
                    .collect::<Vec<_>>()
            })
            .collect();
        let refs: Vec<_> = expected.keys().copied().collect();
        assert!(refs.iter().any(|r| r.0 == fixture.parent));
        assert!(refs.iter().any(|r| r.0 == fixture.sub));

        let damaged = crate::tests::musashi::only_one_tx_decodes(&stored, fixture.parent);
        assert!(MultiEraBlock::decode(&damaged).is_err());
        let writer = archive.start_writer().unwrap();
        writer
            .apply(
                &ChainPoint::Specific(block.slot(), block.hash()),
                &Arc::new(damaged),
            )
            .unwrap();
        writer.commit().unwrap();

        assert_eq!(resolve_archive_inputs(archive, &refs), expected);
    }
}
