//! Resolved inputs for the transaction mappings of one block response.
//! The map is discarded after the response; no state survives a rollback.
use pallas::{
    interop::utxorpc::{LedgerContext, TxoRef, UtxoMap},
    ledger::traverse::{Era, MultiEraBlock, MultiEraTx},
};
use std::{borrow::Cow, collections::HashSet, sync::Arc};

use crate::serve::ChainReads;

/// A ledger context that holds no outputs, for mapping values that name no inputs.
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

/// The transactions of one block, with the era and slot of that block.
pub(super) struct BlockTxs<'b> {
    era: Era,
    slot: u64,
    txs: Vec<MultiEraTx<'b>>,
}

/// A transaction of a block, held with the transactions of that block.
pub(super) struct TxOfBlock<'t, 'b> {
    block: &'t BlockTxs<'b>,
    tx: Cow<'t, MultiEraTx<'b>>,
}

impl<'b> BlockTxs<'b> {
    /// The transactions of `block`.
    pub(super) fn of(block: &'b MultiEraBlock<'_>) -> Self {
        Self {
            era: block.era(),
            slot: block.slot(),
            txs: block.txs(),
        }
    }

    /// The top level transactions of the block, in the order it lists them.
    pub(super) fn top_level(&self) -> &[MultiEraTx<'b>] {
        &self.txs
    }

    /// The top level transaction at `index` of the block.
    pub(super) fn at(&self, index: usize) -> Option<TxOfBlock<'_, 'b>> {
        self.txs.get(index).map(|tx| TxOfBlock {
            block: self,
            tx: Cow::Borrowed(tx),
        })
    }

    /// The transaction of the block whose hash is `hash`, a sub transaction
    /// included.
    pub(super) fn applied(&self, hash: &[u8]) -> Option<TxOfBlock<'_, 'b>> {
        dolos_core::tx_in(&self.txs, hash).map(|(_, tx)| TxOfBlock {
            block: self,
            tx: Cow::Owned(tx),
        })
    }
}

impl<'b> TxOfBlock<'_, 'b> {
    /// The transaction.
    pub(super) fn tx(&self) -> &MultiEraTx<'b> {
        &self.tx
    }
}

/// The outputs among `named` that the transactions of the block produce, sub
/// transactions included.
fn made_among(block: &BlockTxs<'_>, named: &HashSet<TxoRef>) -> UtxoMap {
    let makers: HashSet<_> = named.iter().map(|(hash, _)| *hash).collect();
    let mut made = UtxoMap::new();
    for tx in &block.txs {
        let Ok(()) = dolos_core::for_each_applied_tx(tx, |tx| {
            let hash = tx.hash();
            if makers.contains(&hash) {
                for (i, output) in tx.produces() {
                    let made_ref = (hash, i as u32);
                    if named.contains(&made_ref) {
                        made.insert(made_ref, (block.era, output.encode()));
                    }
                }
            }
            Ok::<_, std::convert::Infallible>(())
        });
    }
    made
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
        let block = BlockTxs::of(block);
        Self::resolving(inner, &block, &block.txs)
    }

    /// A context for mapping one transaction of a block, resolving only the inputs that transaction names.
    pub(super) fn for_tx(inner: C, tx: &TxOfBlock<'_, '_>) -> Self {
        Self::resolving(inner, tx.block, std::slice::from_ref(tx.tx()))
    }

    /// A context for mapping `mapped`, transactions of `block`.
    fn resolving(inner: C, block: &BlockTxs<'_>, mapped: &[MultiEraTx<'_>]) -> Self {
        let start = std::time::Instant::now();
        let named: HashSet<TxoRef> = mapped
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
        let made = made_among(block, &named);
        let refs: Vec<_> = named
            .into_iter()
            .filter(|r| !made.contains_key(r))
            .collect();
        let resolved = if refs.is_empty() {
            Some(UtxoMap::new())
        } else {
            inner.utxos(&refs)
        };
        tracing::trace!(slot = block.slot, input_count = refs.len(),
            resolved_count = resolved.as_ref().map(|x| x.len()),
            elapsed = ?start.elapsed(), "Prepared block input context");
        Self {
            inner,
            resolved: resolved.map(Arc::new),
            made: Arc::new(made),
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

    /// A block whose sub transaction carrier also spends the first output of its first sub transaction.
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

    /// The output the v1 mapper serves for each input of the block's top level transactions.
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

    /// A ledger context that holds no outputs and records each reference asked
    /// of it.
    #[derive(Clone, Default)]
    struct Recording(Arc<std::sync::Mutex<Vec<TxoRef>>>);

    impl LedgerContext for Recording {
        fn get_utxos(&self, refs: &[TxoRef]) -> Option<UtxoMap> {
            self.0.lock().unwrap().extend_from_slice(refs);
            Some(UtxoMap::new())
        }
        fn get_slot_timestamp(&self, _: u64) -> Option<u64> {
            Some(0)
        }
    }

    /// The references a transaction and the sub transactions it lists name.
    fn named_with_subs(tx: &MultiEraTx<'_>) -> HashSet<TxoRef> {
        let mut named: HashSet<_> = named_inputs(tx).into_iter().collect();
        for sub in tx.sub_transactions() {
            named.extend(named_inputs(&sub));
        }
        named
    }

    /// The references of the outputs the transactions of a block produce.
    fn produced_by(block: &MultiEraBlock<'_>) -> HashSet<TxoRef> {
        dolos_core::applied_txs(block)
            .iter()
            .flat_map(|(_, tx)| {
                let hash = tx.hash();
                tx.produces()
                    .into_iter()
                    .map(|(i, _)| (hash, i as u32))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// The references asked of the context and the outputs served from the
    /// block, for one transaction.
    fn resolve_one(tx: &TxOfBlock<'_, '_>) -> (HashSet<TxoRef>, HashSet<TxoRef>) {
        let context = Recording::default();
        let prepared = BlockContext::for_tx(context.clone(), tx);
        let asked = context.0.lock().unwrap().iter().copied().collect();
        (asked, prepared.made.keys().copied().collect())
    }

    /// The first top level transaction of `txs` that `pick` accepts.
    fn top_level_where<'t, 'b>(
        txs: &'t BlockTxs<'b>,
        pick: impl Fn(&MultiEraTx<'b>) -> bool,
    ) -> TxOfBlock<'t, 'b> {
        let index = txs.top_level().iter().position(pick).unwrap();
        txs.at(index).unwrap()
    }

    #[test]
    fn a_transaction_naming_no_output_of_its_block_asks_the_context_for_every_input() {
        let (raw, _) = spends_its_own_sub_output();
        let block = MultiEraBlock::decode(&raw).unwrap();
        let txs = BlockTxs::of(&block);
        let produced = produced_by(&block);
        let tx = top_level_where(&txs, |tx| named_with_subs(tx).is_disjoint(&produced));

        let (asked, served) = resolve_one(&tx);

        assert_eq!(asked, named_with_subs(tx.tx()));
        assert_eq!(served, HashSet::new());
    }

    #[test]
    fn a_transaction_spending_an_output_of_its_block_serves_it_from_the_block() {
        let (raw, made) = spends_its_own_sub_output();
        let block = MultiEraBlock::decode(&raw).unwrap();
        let txs = BlockTxs::of(&block);
        let carrier = top_level_where(&txs, |tx| named_with_subs(tx).contains(&made));
        let produced = produced_by(&block);
        let (inside, outside): (HashSet<_>, HashSet<_>) = named_with_subs(carrier.tx())
            .into_iter()
            .partition(|r| produced.contains(r));
        assert!(inside.contains(&made));

        let (asked, served) = resolve_one(&carrier);

        assert_eq!(asked, outside);
        assert_eq!(served, inside);
    }

    #[test]
    fn a_hash_no_transaction_of_the_block_has_names_no_transaction() {
        let (raw, _) = spends_its_own_sub_output();
        let block = MultiEraBlock::decode(&raw).unwrap();
        let txs = BlockTxs::of(&block);
        let absent = [0u8; 32];
        assert!(dolos_core::applied_txs(&block)
            .iter()
            .all(|(_, tx)| tx.hash().as_slice() != absent));

        assert!(txs.applied(&absent).is_none());
    }

    #[test]
    fn a_sub_transaction_found_by_its_hash_is_resolved_against_its_block() {
        let (raw, made) = spends_its_own_sub_output();
        let block = MultiEraBlock::decode(&raw).unwrap();
        let txs = BlockTxs::of(&block);

        let sub = txs.applied(made.0.as_slice()).unwrap();

        assert_eq!(sub.tx().hash(), made.0);
        let produced = produced_by(&block);
        let (inside, outside): (HashSet<_>, HashSet<_>) = named_with_subs(sub.tx())
            .into_iter()
            .partition(|r| produced.contains(r));
        let (asked, served) = resolve_one(&sub);
        assert_eq!(asked, outside);
        assert_eq!(served, inside);
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
