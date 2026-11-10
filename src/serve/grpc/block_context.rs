//! Resolved inputs for the transaction mappings of one block response.
//! The map is discarded after the response; no state survives a rollback.
use pallas::{
    interop::utxorpc::{LedgerContext, TxoRef, UtxoMap},
    ledger::traverse::{MultiEraBlock, MultiEraTx},
};
use std::{collections::HashSet, sync::Arc};

use crate::serve::ChainReads;

/// A ledger context that holds no outputs, for mapping values that name no
/// inputs.
#[derive(Clone, Default)]
pub(super) struct NoInputs;

impl LedgerContext for NoInputs {
    fn get_utxos(&self, _: &[TxoRef]) -> Option<UtxoMap> {
        None
    }
    fn get_slot_timestamp(&self, _: u64) -> Option<u64> {
        None
    }
}

#[derive(Clone)]
pub(super) struct BlockContext<C: ChainReads> {
    inner: C,
    resolved: Option<Arc<UtxoMap>>,
    made: Arc<UtxoMap>,
}

/// The outputs each transaction of the block produces, sub transactions
/// included.
fn made_outputs(block: &MultiEraBlock<'_>) -> UtxoMap {
    dolos_core::applied_txs(block)
        .into_iter()
        .flat_map(|(_, tx)| {
            let hash = tx.hash();
            tx.produces()
                .into_iter()
                .map(|(i, o)| ((hash, i as u32), (block.era(), o.encode())))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The inputs, collateral inputs and reference inputs a transaction names.
fn named_inputs(tx: &MultiEraTx<'_>) -> Vec<TxoRef> {
    tx.inputs()
        .into_iter()
        .chain(tx.collateral())
        .chain(tx.reference_inputs())
        .map(|i| (*i.hash(), i.index() as u32))
        .collect()
}

impl<C: ChainReads> BlockContext<C> {
    pub(super) fn new(inner: C, block: &MultiEraBlock<'_>) -> Self {
        Self::resolving(inner, block, &block.txs())
    }

    /// A context for mapping one transaction of a block, resolving only the
    /// inputs that transaction names.
    pub(super) fn for_tx(inner: C, block: &MultiEraBlock<'_>, tx: &MultiEraTx<'_>) -> Self {
        Self::resolving(inner, block, std::slice::from_ref(tx))
    }

    fn resolving(inner: C, block: &MultiEraBlock<'_>, txs: &[MultiEraTx<'_>]) -> Self {
        let start = std::time::Instant::now();
        let refs: HashSet<TxoRef> = txs
            .iter()
            .flat_map(|tx| {
                // The v1 mapper resolves every child separately. Its inputs
                // need not also occur in the parent's reference inputs.
                let mut refs = named_inputs(tx);
                for sub in tx.sub_transactions() {
                    refs.extend(named_inputs(&sub));
                }
                refs
            })
            .collect();
        let refs: Vec<_> = refs.into_iter().collect();
        let resolved = if refs.is_empty() {
            Some(UtxoMap::new())
        } else {
            inner.utxos(&refs)
        };
        tracing::trace!(slot = block.slot(), input_count = refs.len(),
            resolved_count = resolved.as_ref().map(|x| x.len()),
            elapsed = ?start.elapsed(), "Prepared block input context");
        Self {
            inner,
            resolved: resolved.map(Arc::new),
            made: Arc::new(made_outputs(block)),
        }
    }
}

impl<C: ChainReads> LedgerContext for BlockContext<C> {
    fn get_utxos(&self, refs: &[TxoRef]) -> Option<UtxoMap> {
        // An output the block makes is served from the block, so applying and
        // undoing the block serve the same output.
        let (made, outside): (Vec<TxoRef>, Vec<TxoRef>) = refs
            .iter()
            .copied()
            .partition(|r| self.made.contains_key(r));
        let mut found = match &self.resolved {
            Some(resolved) => outside
                .iter()
                .filter_map(|r| resolved.get(r).map(|v| (*r, v.clone())))
                .collect(),
            // Preserve the old per-transaction behavior if the bulk read failed.
            None => self.inner.utxos(&outside)?,
        };
        found.extend(made.iter().map(|r| (*r, self.made[r].clone())));
        Some(found)
    }
    fn get_slot_timestamp(&self, slot: u64) -> Option<u64> {
        self.inner.slot_timestamp(slot)
    }
}

#[cfg(test)]
mod made_in_block_tests {
    use super::*;
    use pallas::codec::minicbor;
    use pallas::codec::utils::MaybeIndefArray;
    use pallas::interop::utxorpc::v1;
    use pallas::ledger::primitives::dijkstra::{self, TransactionInput};

    /// A ledger context that resolves only the outputs it holds.
    #[derive(Clone)]
    struct Known(Arc<UtxoMap>);

    impl LedgerContext for Known {
        fn get_utxos(&self, refs: &[TxoRef]) -> Option<UtxoMap> {
            Some(
                refs.iter()
                    .filter_map(|r| self.0.get(r).map(|v| (*r, v.clone())))
                    .collect(),
            )
        }
        fn get_slot_timestamp(&self, _: u64) -> Option<u64> {
            Some(0)
        }
    }

    /// A block whose sub transaction carrier also spends the first output of
    /// its first sub transaction.
    fn spends_its_own_sub_output() -> (Vec<u8>, TxoRef) {
        let text = include_str!("../../../test_data/musashi-w36/ranking-sub-transaction.block");
        let cbor = hex::decode(text.trim()).unwrap();
        let made = MultiEraBlock::decode(&cbor)
            .unwrap()
            .txs()
            .iter()
            .find_map(|tx| tx.sub_transactions().first().map(|sub| sub.hash()))
            .expect("a transaction lists a sub transaction");
        let (era, mut block): (u16, dijkstra::Block) = minicbor::decode(&cbor).unwrap();
        let mut txs: Vec<_> = block.block_body.transactions.to_vec();
        let carrier = txs
            .iter_mut()
            .find(|tx| tx.transaction_body.sub_transactions.is_some())
            .unwrap();
        let mut body = (*carrier.transaction_body).clone();
        let mut inputs = body.inputs.to_vec();
        inputs.push(TransactionInput {
            transaction_id: made,
            index: 0,
        });
        body.inputs = inputs.into();
        carrier.transaction_body = body.into();
        block.block_body.transactions = MaybeIndefArray::Def(txs);
        (minicbor::to_vec((era, block)).unwrap(), (made, 0))
    }

    /// The output the v1 mapper serves for each input of the block's top level
    /// transactions.
    fn served(
        block: &MultiEraBlock<'_>,
        context: Known,
    ) -> Vec<(TxoRef, Option<v1::spec::cardano::TxOutput>)> {
        let mapped = v1::Mapper::new(BlockContext::new(context, block)).map_block(block);
        mapped
            .body
            .unwrap()
            .tx
            .into_iter()
            .flat_map(|tx| tx.common.unwrap().inputs)
            .map(|i| {
                let hash: [u8; 32] = i.tx_hash.as_ref().try_into().unwrap();
                ((hash.into(), i.output_index), i.as_output)
            })
            .collect()
    }

    #[test]
    fn an_input_the_block_does_not_make_is_served_only_from_the_context() {
        let (raw, made) = spends_its_own_sub_output();
        let block = MultiEraBlock::decode(&raw).unwrap();
        let outside: Vec<_> = served(&block, Known(Arc::default()))
            .into_iter()
            .filter(|(r, _)| *r != made)
            .collect();
        assert!(!outside.is_empty());
        for (r, output) in outside {
            assert!(output.is_none(), "{r:?} {output:?}");
        }
    }

    #[test]
    fn an_input_the_block_makes_is_served_the_same_with_or_without_the_context() {
        let (raw, made) = spends_its_own_sub_output();
        let block = MultiEraBlock::decode(&raw).unwrap();
        let output = dolos_core::applied_txs(&block)
            .into_iter()
            .find(|(_, tx)| tx.hash() == made.0)
            .map(|(_, tx)| (block.era(), tx.outputs()[0].encode()))
            .unwrap();
        let applied = Known(Arc::new(UtxoMap::from([(made, output)])));
        let pick = |served: Vec<(TxoRef, Option<_>)>| {
            served.into_iter().find(|(r, _)| *r == made).unwrap().1
        };
        let apply = pick(served(&block, applied));
        let undo = pick(served(&block, Known(Arc::default())));
        assert!(apply.is_some());
        assert_eq!(undo, apply);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dolos_testing::synthetic::{build_synthetic_blocks, SyntheticBlockConfig};
    use pallas::interop::utxorpc::{v1, v1alpha, v1beta};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    };
    #[derive(Clone)]
    struct CountingContext {
        calls: Arc<AtomicUsize>,
        outputs: Arc<Mutex<UtxoMap>>,
        fail_bulk: bool,
    }
    impl LedgerContext for CountingContext {
        fn get_utxos(&self, refs: &[TxoRef]) -> Option<UtxoMap> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.fail_bulk && refs.len() > 2 {
                return None;
            }
            let outputs = self.outputs.lock().unwrap();
            Some(
                refs.iter()
                    .filter_map(|r| outputs.get(r).map(|v| (*r, v.clone())))
                    .collect(),
            )
        }
        fn get_slot_timestamp(&self, _: u64) -> Option<u64> {
            Some(1234)
        }
    }
    fn fixture() -> (Vec<dolos_core::RawBlock>, CountingContext) {
        let (blocks, _, _) = build_synthetic_blocks(SyntheticBlockConfig {
            block_count: 2,
            txs_per_block: 4,
            spend_previous_outputs: true,
            ..Default::default()
        });
        let parent = MultiEraBlock::decode(&blocks[0]).unwrap();
        let outputs = parent
            .txs()
            .iter()
            .flat_map(|tx| {
                tx.outputs()
                    .iter()
                    .enumerate()
                    .map(|(i, o)| ((tx.hash(), i as u32), (parent.era(), o.encode())))
                    .collect::<Vec<_>>()
            })
            .collect();
        (
            blocks,
            CountingContext {
                calls: Arc::default(),
                outputs: Arc::new(Mutex::new(outputs)),
                fail_bulk: false,
            },
        )
    }
    #[test]
    fn resolves_once_and_preserves_both_wire_versions() {
        let (blocks, context) = fixture();
        let block = MultiEraBlock::decode(&blocks[1]).unwrap();
        let alpha = v1alpha::Mapper::new(context.clone()).map_block(&block);
        let beta = v1beta::Mapper::new(context.clone()).map_block(&block);
        assert!(alpha
            .body
            .as_ref()
            .unwrap()
            .tx
            .iter()
            .any(|t| t.inputs.iter().any(|i| i.as_output.is_some())));
        context.calls.store(0, Ordering::Relaxed);
        let prepared = BlockContext::new(context.clone(), &block);
        assert_eq!(
            v1alpha::Mapper::new(prepared.clone()).map_block(&block),
            alpha
        );
        assert_eq!(v1beta::Mapper::new(prepared).map_block(&block), beta);
        assert_eq!(context.calls.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn new_response_does_not_reuse_previous_resolution() {
        let (blocks, context) = fixture();
        let block = MultiEraBlock::decode(&blocks[1]).unwrap();
        let before =
            v1alpha::Mapper::new(BlockContext::new(context.clone(), &block)).map_block(&block);
        context.outputs.lock().unwrap().clear();
        let after =
            v1alpha::Mapper::new(BlockContext::new(context.clone(), &block)).map_block(&block);
        assert_ne!(before, after);
        assert_eq!(after, v1alpha::Mapper::new(context).map_block(&block));
    }
    #[test]
    fn bulk_failure_falls_back_to_per_transaction_resolution() {
        let (blocks, mut context) = fixture();
        context.fail_bulk = true;
        let block = MultiEraBlock::decode(&blocks[1]).unwrap();
        let expected = v1alpha::Mapper::new(context.clone()).map_block(&block);
        let prepared = BlockContext::new(context.clone(), &block);
        assert!(prepared.resolved.is_none());
        assert_eq!(v1alpha::Mapper::new(prepared).map_block(&block), expected);
    }

    #[test]
    fn child_only_spending_and_reference_inputs_are_resolved_once() {
        // Both successful and failed parents are mapped for inspection. Their
        // producer verdict governs whether a consumer applies child effects.
        for success in [true, false] {
            let (raw, outputs) = crate::tests::musashi::child_only_inputs(success);
            let context = CountingContext {
                calls: Arc::default(),
                outputs: Arc::new(Mutex::new(outputs)),
                fail_bulk: false,
            };
            let block = MultiEraBlock::decode(&raw).unwrap();
            let expected = v1::Mapper::new(context.clone()).map_block(&block);
            context.calls.store(0, Ordering::Relaxed);
            let mapped =
                v1::Mapper::new(BlockContext::new(context.clone(), &block)).map_block(&block);
            let parent = &mapped.body.as_ref().unwrap().tx[0];
            let child = &parent.sub_transactions[0];
            assert_eq!(parent.successful, success);
            assert_eq!((child.inputs.len(), child.reference_inputs.len()), (1, 1));
            for input in child.inputs.iter().chain(&child.reference_inputs) {
                assert!(
                    input.as_output.is_some(),
                    "child-only input must resolve: {input:?}"
                );
            }
            assert_eq!(mapped, expected);
            assert_eq!(context.calls.load(Ordering::Relaxed), 1);

            // A partial successful lookup is legitimate. A later response must
            // resolve afresh, including after an undo or archive/state change.
            context.outputs.lock().unwrap().clear();
            let after =
                v1::Mapper::new(BlockContext::new(context.clone(), &block)).map_block(&block);
            assert_ne!(mapped, after);
            assert_eq!(after, v1::Mapper::new(context.clone()).map_block(&block));
        }
    }

    #[test]
    fn captured_child_spend_resolves_the_original_captured_output() {
        let producer = hex::decode(
            include_str!(
                "../../../test_data/musashi-w36/ranking-sub-transaction-two-outputs.block"
            )
            .trim(),
        )
        .unwrap();
        let consumer = hex::decode(
            include_str!(
                "../../../test_data/musashi-w36/ranking-spends-sub-transaction-output.block"
            )
            .trim(),
        )
        .unwrap();
        let source = MultiEraBlock::decode(&producer).unwrap();
        let outputs = dolos_core::applied_txs(&source)
            .into_iter()
            .flat_map(|(_, tx)| {
                tx.outputs()
                    .iter()
                    .enumerate()
                    .map(|(i, o)| ((tx.hash(), i as u32), (source.era(), o.encode())))
                    .collect::<Vec<_>>()
            })
            .collect();
        let context = CountingContext {
            calls: Arc::default(),
            outputs: Arc::new(Mutex::new(outputs)),
            fail_bulk: false,
        };
        let block = MultiEraBlock::decode(&consumer).unwrap();
        let expected = v1::Mapper::new(context.clone()).map_block(&block);
        context.calls.store(0, Ordering::Relaxed);
        let mapped = v1::Mapper::new(BlockContext::new(context.clone(), &block)).map_block(&block);
        let child = &mapped.body.as_ref().unwrap().tx[205].sub_transactions[0];
        assert_eq!(child.inputs.len(), 1);
        assert!(child.inputs[0].as_output.is_some());
        assert_eq!(mapped, expected);
        assert_eq!(context.calls.load(Ordering::Relaxed), 1);
    }
}
