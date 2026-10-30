//! Built-in in-memory mempool implementation.
//!
//! A basic FIFO mempool backed by in-memory data structures and a
//! broadcast channel for event notifications. Suitable for single-node
//! deployments and development/testing.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, RwLock},
};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tracing::debug;

use crate::{
    ChainPoint, MempoolError, MempoolEvent, MempoolPage, MempoolStore, MempoolTx, MempoolTxStage,
    TxHash, TxStatus,
};

/// The position a transaction took in the pending queue when first admitted.
type Seq = u64;

#[derive(Default)]
struct MempoolState {
    /// Each transaction not yet finalized or dropped, with its admission position, at the stage the transaction holds.
    live: HashMap<TxHash, (Seq, MempoolTx)>,
    finalized_log: VecDeque<MempoolTx>,
}

impl MempoolState {
    fn next_seq(&self) -> Seq {
        self.live
            .values()
            .map(|(seq, _)| *seq)
            .max()
            .map_or(0, |seq| seq + 1)
    }

    /// The hashes of the live transactions the filter admits, in admission order.
    fn in_order(&self, admit: impl Fn(&MempoolTx) -> bool) -> Vec<TxHash> {
        let mut admitted: Vec<&(Seq, MempoolTx)> =
            self.live.values().filter(|(_, tx)| admit(tx)).collect();
        admitted.sort_by_key(|(seq, _)| *seq);
        admitted.into_iter().map(|(_, tx)| tx.hash).collect()
    }

    fn count(&self, stage: MempoolTxStage) -> usize {
        self.live
            .values()
            .filter(|(_, tx)| tx.stage == stage)
            .count()
    }
}

fn is_pending(tx: &MempoolTx) -> bool {
    tx.stage == MempoolTxStage::Pending
}

const MAX_FINALIZED_LOG: usize = 1000;

/// A basic, FIFO, in-memory mempool.
#[derive(Clone)]
pub struct EphemeralMempool {
    state: Arc<RwLock<MempoolState>>,
    updates: broadcast::Sender<MempoolEvent>,
}

impl Default for EphemeralMempool {
    fn default() -> Self {
        Self::new()
    }
}

impl EphemeralMempool {
    pub fn new() -> Self {
        let state = Arc::new(RwLock::new(MempoolState::default()));
        let (updates, _) = broadcast::channel(16);

        Self { state, updates }
    }

    fn notify(&self, tx: MempoolTx) {
        if self.updates.send(MempoolEvent { tx }).is_err() {
            debug!("no mempool update receivers");
        }
    }

    fn log_state(&self, state: &MempoolState) {
        debug!(
            pending = state.count(MempoolTxStage::Pending),
            inflight = state.count(MempoolTxStage::Propagated),
            acknowledged = state.live.len()
                - state.count(MempoolTxStage::Pending)
                - state.count(MempoolTxStage::Propagated),
            "mempool state changed"
        );
    }
}

pub struct EphemeralMempoolStream {
    inner: BroadcastStream<MempoolEvent>,
}

impl futures_core::Stream for EphemeralMempoolStream {
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

impl MempoolStore for EphemeralMempool {
    type Stream = EphemeralMempoolStream;

    fn receive(&self, tx: MempoolTx) -> Result<(), MempoolError> {
        let mut state = self.state.write().unwrap();

        if state.live.contains_key(&tx.hash) {
            return Err(MempoolError::DuplicateTx);
        }

        debug!(tx.hash = %tx.hash, "tx received");
        let seq = state.next_seq();
        state.live.insert(tx.hash, (seq, tx.clone()));
        self.notify(tx);
        self.log_state(&state);

        Ok(())
    }

    fn has_pending(&self) -> bool {
        let state = self.state.read().unwrap();
        state.live.values().any(|(_, tx)| is_pending(tx))
    }

    fn peek_pending(&self) -> Vec<MempoolTx> {
        let state = self.state.read().unwrap();
        state
            .in_order(is_pending)
            .iter()
            .map(|hash| state.live[hash].1.clone())
            .collect()
    }

    fn mark_inflight(&self, hashes: &[TxHash]) -> Result<(), MempoolError> {
        let hash_set: HashSet<_> = hashes.iter().collect();
        let mut state = self.state.write().unwrap();

        for hash in state.in_order(|tx| is_pending(tx) && hash_set.contains(&tx.hash)) {
            let (_, tx) = state.live.get_mut(&hash).unwrap();
            debug!(tx.hash = %tx.hash, "tx inflight");
            tx.stage = MempoolTxStage::Propagated;
            self.notify(tx.clone());
        }

        self.log_state(&state);
        Ok(())
    }

    fn mark_acknowledged(&self, hashes: &[TxHash]) -> Result<(), MempoolError> {
        let hash_set: HashSet<_> = hashes.iter().collect();
        let mut state = self.state.write().unwrap();

        // A confirmed transaction keeps its stage and expiry counters.
        let propagated =
            |tx: &MempoolTx| tx.stage == MempoolTxStage::Propagated && hash_set.contains(&tx.hash);
        for hash in state.in_order(propagated) {
            let (_, tx) = state.live.get_mut(&hash).unwrap();
            debug!(tx.hash = %tx.hash, "tx acknowledged");
            tx.stage = MempoolTxStage::Acknowledged;
            self.notify(tx.clone());
        }

        self.log_state(&state);
        Ok(())
    }

    fn find_inflight(&self, tx_hash: &TxHash) -> Option<MempoolTx> {
        let state = self.state.read().unwrap();
        state
            .live
            .get(tx_hash)
            .map(|(_, tx)| tx)
            .filter(|tx| !is_pending(tx))
            .cloned()
    }

    fn peek_inflight(&self) -> Vec<MempoolTx> {
        let state = self.state.read().unwrap();
        state
            .in_order(|tx| !is_pending(tx))
            .iter()
            .map(|hash| state.live[hash].1.clone())
            .collect()
    }

    fn confirm(
        &self,
        point: &ChainPoint,
        seen_txs: &[TxHash],
        unseen_txs: &[TxHash],
        finalize_threshold: u32,
        drop_threshold: u32,
    ) -> Result<(), MempoolError> {
        let mut state = self.state.write().unwrap();
        let seen_set: HashSet<&TxHash> = seen_txs.iter().collect();
        let unseen_set: HashSet<&TxHash> = unseen_txs.iter().collect();

        // A pending transaction the block holds is confirmed.
        let hashes = state.in_order(|tx| !is_pending(tx) || seen_set.contains(&tx.hash));

        if hashes.is_empty() {
            return Ok(());
        }

        for tx_hash in hashes {
            if seen_set.contains(&tx_hash) {
                let (_, tx) = state.live.get_mut(&tx_hash).unwrap();
                tx.confirm(point);
                // Check if finalizable
                if tx.confirmations >= finalize_threshold {
                    let mut finalized = tx.clone();
                    finalized.stage = MempoolTxStage::Finalized;
                    state.finalized_log.push_back(finalized.clone());
                    state.live.remove(&tx_hash);
                    debug!(tx.hash = %tx_hash, "tx finalized");
                    self.notify(finalized);
                } else {
                    self.notify(tx.clone());
                    debug!(tx.hash = %tx_hash, "tx confirmed");
                }
            } else if unseen_set.contains(&tx_hash) {
                let (_, tx) = state.live.get_mut(&tx_hash).unwrap();

                let mut event_tx = tx.clone();
                event_tx.stage = MempoolTxStage::RolledBack;
                self.notify(event_tx);

                tx.retry();
                debug!(tx.hash = %tx_hash, "retry tx");
            } else {
                let (_, tx) = state.live.get_mut(&tx_hash).unwrap();
                if tx.stage == MempoolTxStage::Confirmed {
                    // Already confirmed on-chain; treat subsequent blocks as
                    // additional confirmations (increasing depth).
                    tx.confirm(point);
                    if tx.confirmations >= finalize_threshold {
                        let mut finalized = tx.clone();
                        finalized.stage = MempoolTxStage::Finalized;
                        state.finalized_log.push_back(finalized.clone());
                        state.live.remove(&tx_hash);
                        debug!(tx.hash = %tx_hash, "tx finalized");
                        self.notify(finalized);
                    } else {
                        self.notify(tx.clone());
                    }
                } else {
                    tx.mark_stale();
                    // Check if droppable
                    if tx.non_confirmations >= drop_threshold {
                        let mut dropped = tx.clone();
                        dropped.stage = MempoolTxStage::Dropped;
                        state.finalized_log.push_back(dropped.clone());
                        state.live.remove(&tx_hash);
                        debug!(tx.hash = %tx_hash, "tx dropped");
                        self.notify(dropped);
                    }
                }
            }
        }

        if state.finalized_log.len() > MAX_FINALIZED_LOG {
            let excess = state.finalized_log.len() - MAX_FINALIZED_LOG;
            state.finalized_log.drain(..excess);
        }

        Ok(())
    }

    fn drop_pending(&self, hashes: &[TxHash]) -> Result<(), MempoolError> {
        let hash_set: HashSet<_> = hashes.iter().collect();
        let mut state = self.state.write().unwrap();

        for hash in state.in_order(|tx| is_pending(tx) && hash_set.contains(&tx.hash)) {
            let (_, mut tx) = state.live.remove(&hash).unwrap();
            tx.stage = MempoolTxStage::Dropped;
            state.finalized_log.push_back(tx.clone());
            debug!(tx.hash = %tx.hash, "pending tx dropped");
            self.notify(tx);
        }

        if state.finalized_log.len() > MAX_FINALIZED_LOG {
            let excess = state.finalized_log.len() - MAX_FINALIZED_LOG;
            state.finalized_log.drain(..excess);
        }

        self.log_state(&state);

        Ok(())
    }

    fn check_status(&self, tx_hash: &TxHash) -> TxStatus {
        let state = self.state.read().unwrap();

        if let Some((_, tx)) = state.live.get(tx_hash) {
            TxStatus {
                stage: tx.stage.clone(),
                confirmations: tx.confirmations,
                non_confirmations: tx.non_confirmations,
                confirmed_at: tx.confirmed_at.clone(),
            }
        } else {
            TxStatus {
                stage: MempoolTxStage::Unknown,
                confirmations: 0,
                non_confirmations: 0,
                confirmed_at: None,
            }
        }
    }

    fn dump_finalized(&self, cursor: u64, limit: usize) -> MempoolPage {
        let state = self.state.read().unwrap();
        let start = cursor as usize;

        let items: Vec<MempoolTx> = state
            .finalized_log
            .iter()
            .skip(start)
            .take(limit)
            .cloned()
            .collect();

        let end = start + items.len();
        let next_cursor = if end < state.finalized_log.len() {
            Some(end as u64)
        } else {
            None
        };

        MempoolPage { items, next_cursor }
    }

    fn subscribe(&self) -> Self::Stream {
        EphemeralMempoolStream {
            inner: BroadcastStream::new(self.updates.subscribe()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EraCbor;

    fn tx(n: u8) -> MempoolTx {
        MempoolTx::new(TxHash::new([n; 32]), EraCbor(7, vec![0x80]), vec![])
    }

    fn pending(store: &EphemeralMempool) -> Vec<TxHash> {
        store.peek_pending().iter().map(|tx| tx.hash).collect()
    }

    #[test]
    fn a_pending_transaction_admitted_later_stays_behind_one_requeued() {
        let store = EphemeralMempool::new();
        let (a, t, u) = (tx(1).hash, tx(2).hash, tx(3).hash);
        store.receive(tx(1)).unwrap();
        store.mark_inflight(&[a]).unwrap();
        store.receive(tx(2)).unwrap();
        store
            .confirm(&ChainPoint::Slot(11), &[a], &[], 10, 10)
            .unwrap();
        store
            .confirm(&ChainPoint::Slot(7), &[], &[a], 10, 10)
            .unwrap();
        assert_eq!(pending(&store), [a, t]);
        store.receive(tx(3)).unwrap();
        assert_eq!(pending(&store), [a, t, u]);
    }

    #[test]
    fn a_transaction_in_flight_is_not_admitted_again() {
        let store = EphemeralMempool::new();
        let a = tx(1).hash;
        store.receive(tx(1)).unwrap();
        store.mark_inflight(&[a]).unwrap();
        assert!(matches!(
            store.receive(tx(1)),
            Err(MempoolError::DuplicateTx)
        ));
        assert!(pending(&store).is_empty());
        assert_eq!(store.check_status(&a).stage, MempoolTxStage::Propagated);
    }

    #[test]
    fn a_new_transaction_is_admitted_beside_one_in_flight() {
        let store = EphemeralMempool::new();
        let (a, t) = (tx(1).hash, tx(2).hash);
        store.receive(tx(1)).unwrap();
        store.mark_inflight(&[a]).unwrap();
        store.receive(tx(2)).unwrap();
        assert_eq!(pending(&store), [t]);
        assert_eq!(store.check_status(&a).stage, MempoolTxStage::Propagated);
    }

    #[test]
    fn a_pending_transaction_seen_in_a_block_is_confirmed() {
        let store = EphemeralMempool::new();
        let (a, t) = (tx(1).hash, tx(2).hash);
        store.receive(tx(1)).unwrap();
        store.receive(tx(2)).unwrap();
        store
            .confirm(&ChainPoint::Slot(12), &[a], &[], 10, 10)
            .unwrap();
        assert_eq!(store.check_status(&a).stage, MempoolTxStage::Confirmed);
        assert_eq!(
            store.check_status(&a).confirmed_at,
            Some(ChainPoint::Slot(12))
        );
        assert_eq!(pending(&store), [t]);
    }
}
