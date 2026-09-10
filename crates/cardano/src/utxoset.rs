use dolos_core::config::CardanoConfig;
use dolos_core::*;
use itertools::Itertools as _;
use pallas::ledger::traverse::{MultiEraBlock, MultiEraOutput, MultiEraTx};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::owned::OwnedMultiEraOutput;

/// What applying a block leniently did that applying it strictly would not.
///
/// Both counts are facts carried out of the walk rather than inferred from the
/// delta afterwards, because neither can be recovered from it: an input that was
/// left unconsumed leaves nothing behind, and an output written over an existing
/// one is indistinguishable from a fresh one once written.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LenientApply {
    /// Inputs that were not in the ledger and not produced earlier in this
    /// block, so nothing was consumed for them.
    pub skipped_inputs: usize,
    /// Outputs created over an entry that was already there.
    pub recreated_outputs: usize,
}

/// The refs a block spends, all of them, for the lenient path.
///
/// The strict path asks the store only for what the block does not produce
/// itself, because it resolves a block's transactions as a set. The lenient path
/// applies them in order, so it has to know what the store really holds and
/// cannot let a block's own later output stand in for it.
pub fn compute_block_dependencies_lenient(block: &MultiEraBlock) -> Vec<TxoRef> {
    block
        .txs()
        .iter()
        .flat_map(MultiEraTx::consumes)
        .map(|utxo| TxoRef(*utxo.hash(), utxo.index() as u32))
        .unique()
        .collect()
}

/// Applies a block the way the Leios prototype node applies one, in wire order,
/// consuming what is there and leaving what is not.
///
/// This mirrors a deployment's observed behaviour and is not a reading of any
/// specification. The node folds a certified endorser block's transactions with
/// validation switched off, so removing an input that is absent removes nothing
/// and the transaction still creates its outputs. Three cases on the Musashi
/// chain each depend on some part of that, and no rule narrower than this one
/// covers all three:
///
/// - a transaction spending an output a LATER transaction of the same block
///   produces, which consumes nothing there and leaves the output for whoever
///   spends it next;
/// - a transaction carried twice, whose second application finds its input
///   already spent and consumes nothing;
/// - a transaction carried twice whose output was spent in between, whose
///   second application RE-CREATES that output, which a later block then spends.
///
/// The third is why suppressing repeats is not equivalent and why this is done
/// here rather than by filtering blocks upstream.
///
/// An input is available if the store holds it and this block has not already
/// consumed it, or if an earlier transaction of this block produced it and this
/// block has not already consumed it.
///
/// `store_has` is which refs the store itself answered for, which is not the
/// same question as which refs `loaded` has a body for. `loaded` also carries
/// the outputs the block produces, so that the visitors can resolve an
/// intra-block spend, and letting a block's own later output stand in for one
/// the store holds is the exact mistake this rule exists to stop.
pub fn compute_apply_delta_lenient(
    block: &MultiEraBlock,
    loaded: &HashMap<TxoRef, OwnedMultiEraOutput>,
    store_has: &HashSet<TxoRef>,
) -> Result<(UtxoSetDelta, LenientApply), BrokenInvariant> {
    let mut delta = UtxoSetDelta::default();
    let mut stats = LenientApply::default();

    // What this block has created so far, and what it has spent so far, so
    // availability is answered at each transaction's own position rather than
    // for the block as a whole.
    let mut produced_here: HashMap<TxoRef, Arc<EraCbor>> = HashMap::new();
    let mut spent_here: HashSet<TxoRef> = HashSet::new();

    for tx in block.txs().iter() {
        let tx_hash = tx.hash();

        for consumed in tx.consumes() {
            let stxi_ref = TxoRef(*consumed.hash(), consumed.index() as u32);

            if spent_here.contains(&stxi_ref) {
                stats.skipped_inputs += 1;
                continue;
            }

            let body = match produced_here.get(&stxi_ref) {
                Some(body) => Some(body.clone()),
                None if store_has.contains(&stxi_ref) => {
                    loaded.get(&stxi_ref).map(|x| x.borrow_owner().clone())
                }
                None => None,
            };

            match body {
                Some(body) => {
                    spent_here.insert(stxi_ref.clone());
                    delta.consumed_utxo.insert(stxi_ref, body);
                }
                None => {
                    stats.skipped_inputs += 1;
                }
            }
        }

        for (idx, produced) in tx.produces() {
            let utxo_ref = TxoRef(tx_hash, idx as u32);
            let body: Arc<EraCbor> = Arc::new(produced.into());

            let existed = store_has.contains(&utxo_ref) || produced_here.contains_key(&utxo_ref);

            if existed && !spent_here.contains(&utxo_ref) {
                stats.recreated_outputs += 1;
            }

            // Creating it again un-spends it, which is exactly what the node
            // does and the reason a later block can spend it a second time.
            spent_here.remove(&utxo_ref);
            delta.consumed_utxo.remove(&utxo_ref);

            produced_here.insert(utxo_ref.clone(), body.clone());
            delta.produced_utxo.insert(utxo_ref, body);
        }
    }

    Ok((delta, stats))
}

pub fn compute_block_dependencies(block: &MultiEraBlock, loaded: &mut RawUtxoMap) -> Vec<TxoRef> {
    let txs: HashMap<_, _> = block.txs().into_iter().map(|tx| (tx.hash(), tx)).collect();

    // TODO: turn this into "referenced utxos" instead of just consumed.

    // add all produced utxos to the loaded map
    for (tx_hash, tx) in txs.iter() {
        for (idx, utxo) in tx.produces() {
            let utxo_ref = TxoRef(*tx_hash, idx as u32);
            loaded.insert(utxo_ref, Arc::new(utxo.into()));
        }
    }

    // find all consumed utxos in the block
    let consumed: HashSet<_> = txs
        .values()
        .flat_map(MultiEraTx::consumes)
        .map(|utxo| TxoRef(*utxo.hash(), utxo.index() as u32))
        .collect();

    // find all missing utxos that are not already in the loaded map

    consumed
        .into_iter()
        .filter(|x| !loaded.contains_key(x))
        .collect::<Vec<_>>()
}

/// Computes the ledger delta of applying a particular block.
///
/// The output represent a self-contained description of the changes that need
/// to occur at the data layer to advance the ledger to the new position (new
/// slot).
///
/// The function is pure (stateless and without side-effects) with the goal of
/// allowing the logic to execute as an idem-potent, atomic operation, allowing
/// higher-layers to retry the logic if required.
///
/// This method assumes that the block has already been validated, it will
/// return an error if any of the assumed invariants have been broken in the
/// process of computing the delta, but it doesn't provide a comprehensive
/// validation of the ledger rules.
pub fn compute_apply_delta(
    block: &MultiEraBlock,
    loaded: &HashMap<TxoRef, OwnedMultiEraOutput>,
) -> Result<UtxoSetDelta, BrokenInvariant> {
    let mut delta = UtxoSetDelta::default();

    let txs: HashMap<_, _> = block.txs().into_iter().map(|tx| (tx.hash(), tx)).collect();

    for (tx_hash, tx) in txs.iter() {
        for (idx, produced) in tx.produces() {
            let uxto_ref = TxoRef(*tx_hash, idx as u32);
            delta
                .produced_utxo
                .insert(uxto_ref, Arc::new(produced.into()));
        }

        for consumed in tx.consumes() {
            let stxi_ref = TxoRef(*consumed.hash(), consumed.index() as u32);

            let stxi_body = loaded
                .get(&stxi_ref)
                .ok_or_else(|| BrokenInvariant::MissingUtxo(stxi_ref.clone()))?;

            let stxi_body_arc = stxi_body.borrow_owner().clone();

            delta.consumed_utxo.insert(stxi_ref, stxi_body_arc);
        }
    }

    Ok(delta)
}

pub fn compute_undo_delta(
    block: &MultiEraBlock,
    context: &HashMap<TxoRef, OwnedMultiEraOutput>,
) -> Result<UtxoSetDelta, BrokenInvariant> {
    let mut delta = UtxoSetDelta::default();

    let txs: HashMap<_, _> = block.txs().into_iter().map(|tx| (tx.hash(), tx)).collect();

    for (tx_hash, tx) in txs.iter() {
        for (idx, body) in tx.produces() {
            let utxo_ref = TxoRef(*tx_hash, idx as u32);
            delta.undone_utxo.insert(utxo_ref, Arc::new(body.into()));
        }
    }

    for (_, tx) in txs.iter() {
        for consumed in tx.consumes() {
            let stxi_ref = TxoRef(*consumed.hash(), consumed.index() as u32);

            let stxi_body = context
                .get(&stxi_ref)
                .ok_or_else(|| BrokenInvariant::MissingUtxo(stxi_ref.clone()))?;

            let stxi_body_arc = stxi_body.borrow_owner().clone();

            delta.recovered_stxi.insert(stxi_ref, stxi_body_arc);
        }
    }

    Ok(delta)
}

pub fn compute_origin_delta(genesis: &Genesis) -> UtxoSetDelta {
    let mut delta = UtxoSetDelta::default();

    // byron
    {
        let utxos = pallas::interop::hardano::configs::byron::genesis_utxos(&genesis.byron);

        for (tx, addr, amount) in utxos {
            let utxo_ref = TxoRef(tx, 0);
            let utxo_body = pallas::ledger::primitives::byron::TxOut {
                address: pallas::ledger::primitives::byron::Address {
                    payload: addr.payload,
                    crc: addr.crc,
                },
                amount,
            };

            let utxo_body = MultiEraOutput::from_byron(&utxo_body).to_owned();
            delta
                .produced_utxo
                .insert(utxo_ref, Arc::new(utxo_body.into()));
        }
    }
    // shelley
    {
        let utxos = pallas::interop::hardano::configs::shelley::shelley_utxos(&genesis.shelley);

        for (tx, addr, amount) in utxos {
            let utxo_ref = TxoRef(tx, 0);
            let utxo_body = pallas::ledger::primitives::alonzo::TransactionOutput {
                address: addr.to_vec().into(),
                amount: pallas::ledger::primitives::alonzo::Value::Coin(amount),
                datum_hash: None,
            };
            let utxo_body =
                pallas::ledger::primitives::conway::TransactionOutput::Legacy(utxo_body.into());

            let utxo_body = MultiEraOutput::from_conway(&utxo_body).to_owned();

            delta
                .produced_utxo
                .insert(utxo_ref, Arc::new(utxo_body.into()));
        }
    }

    delta
}

pub fn build_custom_utxos_delta(config: &CardanoConfig) -> Result<UtxoSetDelta, ChainError> {
    let mut delta = UtxoSetDelta::default();

    for utxo in config.custom_utxos.iter() {
        let era = utxo
            .era
            .unwrap_or(pallas::ledger::traverse::Era::Conway.into());

        let eracbor = EraCbor(era, utxo.cbor.clone());

        delta
            .produced_utxo
            .insert(utxo.ref_.clone(), Arc::new(eracbor));
    }

    Ok(delta)
}

#[cfg(test)]
mod tests {
    use pallas::{
        crypto::hash::Hash,
        ledger::{addresses::Address, traverse::MultiEraTx},
    };
    use std::str::FromStr;

    use super::*;

    fn fake_slice_for_block(block: &MultiEraBlock) -> HashMap<TxoRef, OwnedMultiEraOutput> {
        let valid_utxo = block
            .txs()
            .first()
            .unwrap()
            .produces()
            .first()
            .unwrap()
            .1
            .encode();
        let consumed: HashMap<_, _> = block
            .txs()
            .iter()
            .flat_map(MultiEraTx::consumes)
            .map(|utxo| TxoRef(*utxo.hash(), utxo.index() as u32))
            .map(|key| {
                (
                    key,
                    OwnedMultiEraOutput::decode(Arc::new(EraCbor(
                        block.era().into(),
                        valid_utxo.clone(),
                    )))
                    .unwrap(),
                )
            })
            .collect();

        consumed
    }

    fn assert_genesis_utxo_exists(db: &UtxoSetDelta, tx_hex: &str, addr_base58: &str, amount: u64) {
        let tx = Hash::<32>::from_str(tx_hex).unwrap();

        let utxo_body = db.produced_utxo.get(&TxoRef(tx, 0));

        assert!(utxo_body.is_some(), "utxo not found");
        let utxo_body = utxo_body.unwrap();
        let utxo_body = MultiEraOutput::try_from(utxo_body.as_ref()).unwrap();

        assert_eq!(utxo_body.era(), pallas::ledger::traverse::Era::Byron);

        assert_eq!(
            utxo_body.value().coin(),
            amount,
            "utxo amount doesn't match"
        );

        let addr = match utxo_body.address() {
            Ok(Address::Byron(x)) => x.to_base58(),
            _ => panic!(),
        };

        assert_eq!(addr, addr_base58);
    }

    #[test]
    fn test_mainnet_genesis_utxos() {
        let path = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("test_data")
            .join("mainnet")
            .join("genesis");

        let genesis = crate::utils::load_genesis(&path);

        let delta = compute_origin_delta(&genesis);

        assert_genesis_utxo_exists(
            &delta,
            "0ae3da29711600e94a33fb7441d2e76876a9a1e98b5ebdefbf2e3bc535617616",
            "Ae2tdPwUPEZKQuZh2UndEoTKEakMYHGNjJVYmNZgJk2qqgHouxDsA5oT83n",
            2_463_071_701_000_000,
        )
    }

    #[test]
    fn test_preview_genesis_utxos() {
        let path = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("test_data")
            .join("preview")
            .join("genesis");

        let genesis = crate::utils::load_genesis(&path);

        let delta = compute_origin_delta(&genesis);

        assert_genesis_utxo_exists(
            &delta,
            "4843cf2e582b2f9ce37600e5ab4cc678991f988f8780fed05407f9537f7712bd",
            "FHnt4NL7yPXvDWHa8bVs73UEUdJd64VxWXSFNqetECtYfTd9TtJguJ14Lu3feth",
            30_000_000_000_000_000,
        );
    }

    /// Trimmed, because whether a fixture file ends in a newline is not
    /// something a test result should turn on. Half these files carried a
    /// trailing newline and half did not, and the ones that did only worked
    /// because they went through a different helper that trimmed.
    fn load_test_block(name: &str) -> Vec<u8> {
        let path = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("test_data")
            .join(name);

        let content = std::fs::read_to_string(path).unwrap();
        hex::decode(content.trim()).unwrap()
    }

    #[test]
    fn test_apply_delta() {
        // nice block with several txs, it includes chaining edge case
        let cbor = load_test_block("alonzo27.block");
        let block = MultiEraBlock::decode(&cbor).unwrap();
        let context = fake_slice_for_block(&block);

        let delta = super::compute_apply_delta(&block, &context).unwrap();

        for tx in block.txs() {
            for input in tx.consumes() {
                let consumed = delta
                    .consumed_utxo
                    .contains_key(&TxoRef(*input.hash(), input.index() as u32));

                assert!(consumed);
            }

            for (idx, expected) in tx.produces() {
                let utxo = delta.produced_utxo.get(&TxoRef(tx.hash(), idx as u32));
                let utxo = utxo.unwrap();
                let utxo = MultiEraOutput::try_from(utxo.as_ref()).unwrap();
                assert_eq!(utxo, expected);
            }
        }
    }

    #[test]
    fn test_undo_block() {
        // nice block with several txs, it includes chaining edge case
        let cbor = load_test_block("alonzo27.block");
        let block = MultiEraBlock::decode(&cbor).unwrap();
        let context = fake_slice_for_block(&block);

        let apply = super::compute_apply_delta(&block, &context).unwrap();
        let undo = super::compute_undo_delta(&block, &context).unwrap();

        for (produced, _) in apply.produced_utxo.iter() {
            assert!(undo.undone_utxo.contains_key(produced));
        }

        for (consumed, _) in apply.consumed_utxo.iter() {
            assert!(undo.recovered_stxi.contains_key(consumed));
        }
    }

    /// The block and the transaction pair that first met this on the Musashi
    /// chain.
    ///
    /// The ranking block at slot 1861242 certifies endorser block
    /// `b1d005f34c47d00b03e11028a70260517814e0efc9e294cc58e325ffb530acb9`,
    /// which carries 1803 transactions and lists eighteen of them before the
    /// transaction of the same endorser block that produces what they spend.
    /// The fixture keeps the block whole and the first such pair in the order
    /// the endorser block delivered them, index 946 and index 1125, so the
    /// file stays small.
    ///
    /// Transaction `837edbc2…` at index 946 spends
    /// `ec42621b8c6705b724291d2511039ac7760b1d235cfd21c507f891f14231e410#0`,
    /// which transaction `ec42621b…` at index 1125 produces. The chain settles
    /// what the network did with that: the ordinary ranking block at slot
    /// 1861279, applied with full validation like every ranking block, spends
    /// that same output, so it was still unspent on every node after 1861242.
    fn forward_ref_block() -> Vec<u8> {
        let block = load_test_block("dijkstra-forward-ref.block");

        let txs: Vec<Vec<u8>> = include_str!("../test_data/dijkstra-forward-ref.ebtxs")
            .split_whitespace()
            .map(|line| {
                let wire = hex::decode(line).unwrap();
                pallas::ledger::traverse::leios::unwrap_tx(&wire)
                    .unwrap()
                    .to_vec()
            })
            .collect();

        let borrowed: Vec<&[u8]> = txs.iter().map(|t| t.as_slice()).collect();

        pallas::ledger::traverse::leios::resolve_certified_block(&block, &borrowed).unwrap()
    }

    fn forward_ref_txoref() -> TxoRef {
        TxoRef(
            Hash::from_str("ec42621b8c6705b724291d2511039ac7760b1d235cfd21c507f891f14231e410")
                .unwrap(),
            0,
        )
    }

    fn trimmed_block(name: &str) -> Vec<u8> {
        let path = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("test_data")
            .join(name);

        hex::decode(std::fs::read_to_string(path).unwrap().trim()).unwrap()
    }

    /// A ledger the tests can walk a sequence of blocks over, which is what the
    /// two block cases need: the second block's behaviour depends on what the
    /// first one did to the store, and a fabricated slice cannot express that.
    #[derive(Default)]
    struct FakeLedger {
        bodies: HashMap<TxoRef, OwnedMultiEraOutput>,
        present: HashSet<TxoRef>,
    }

    impl FakeLedger {
        /// Seeds the inputs the block spends that the block does not make
        /// itself, so its spends of the outside world resolve.
        ///
        /// A ref the block produces is deliberately left out: the outside world
        /// does not have it yet, and seeding it would make the block's own
        /// chaining look like re-creation and hide what the counters mean.
        fn seed_externals(&mut self, block: &MultiEraBlock) {
            let sample = block
                .txs()
                .first()
                .unwrap()
                .produces()
                .first()
                .unwrap()
                .1
                .encode();

            let made_here: HashSet<TxoRef> = block
                .txs()
                .iter()
                .flat_map(|tx| {
                    let hash = tx.hash();
                    tx.produces()
                        .into_iter()
                        .map(move |(idx, _)| TxoRef(hash, idx as u32))
                })
                .collect();

            for input in block.txs().iter().flat_map(MultiEraTx::consumes) {
                let key = TxoRef(*input.hash(), input.index() as u32);

                if self.present.contains(&key) || made_here.contains(&key) {
                    continue;
                }

                let body = OwnedMultiEraOutput::decode(Arc::new(EraCbor(
                    block.era().into(),
                    sample.clone(),
                )))
                .unwrap();

                self.bodies.insert(key.clone(), body);
                self.present.insert(key);
            }
        }

        fn apply(&mut self, block: &MultiEraBlock) -> LenientApply {
            let (delta, stats) =
                super::compute_apply_delta_lenient(block, &self.bodies, &self.present).unwrap();

            // Produced first, then consumed. A ref a block both makes and
            // spends has to end up spent, which is what ordinary transaction
            // chaining inside a block means, and the walk has already removed
            // from `consumed_utxo` anything a later transaction made again.
            for (produced, body) in delta.produced_utxo.iter() {
                self.bodies.insert(
                    produced.clone(),
                    OwnedMultiEraOutput::decode(body.clone()).unwrap(),
                );
                self.present.insert(produced.clone());
            }

            for consumed in delta.consumed_utxo.keys() {
                self.present.remove(consumed);
            }

            stats
        }

        fn holds(&self, key: &TxoRef) -> bool {
            self.present.contains(key)
        }
    }

    /// MUST FIRE: a transaction spending an output a LATER transaction of the
    /// same block produces consumes nothing, and the output survives for
    /// whoever spends it next. This is the case at slot 1861242, and the chain
    /// settles it: the ordinary ranking block at 1861279 spends that same
    /// output, so it was still there.
    ///
    /// MUST NOT FIRE: the transactions are still applied. Their outputs are
    /// created and the inputs that WERE there are still consumed, so this is
    /// not a rule that quietly drops a transaction.
    #[test]
    fn a_forward_referenced_input_is_left_and_the_transaction_still_applies() {
        let cbor = forward_ref_block();
        let block = MultiEraBlock::decode(&cbor).unwrap();
        assert_eq!(block.slot(), 1861242, "fixture precondition");
        assert_eq!(block.tx_count(), 2, "fixture precondition");

        let target = forward_ref_txoref();

        let mut ledger = FakeLedger::default();
        ledger.seed_externals(&block);

        // The forward referenced output is the one thing the outside world does
        // not have: this block's own later transaction makes it.
        ledger.present.remove(&target);

        let stats = ledger.apply(&block);

        assert_eq!(
            stats.skipped_inputs, 1,
            "exactly the forward reference was left unconsumed"
        );
        assert!(
            ledger.holds(&target),
            "the output the later transaction makes must survive, the chain spends it at 1861279"
        );

        for tx in block.txs().iter() {
            for (idx, _) in tx.produces() {
                assert!(
                    ledger.holds(&TxoRef(tx.hash(), idx as u32)),
                    "every transaction still creates its outputs"
                );
            }
        }
    }

    /// MUST FIRE: a transaction carried a second time re-creates an output that
    /// was spent in between, and a later block can spend it again.
    ///
    /// The two real ordinary ranking blocks of the Musashi chain that showed
    /// this. In block 1861279, index 50 creates
    /// `3fe3ab01a255960d22d63e7ad8fabdee8cd7d9d884790105c35fa3bea1b53e01#0` and
    /// index 234 spends it. Block 1861288 carries index 50's transaction again,
    /// which on the node re-creates that output, and a later block spends it a
    /// second time. Suppressing the repeat instead was tried and stopped the
    /// sync here, because the resurrection never happened.
    ///
    /// MUST NOT FIRE: the first block's own chain still resolves, so this is
    /// not a rule that makes everything succeed by consuming nothing. Index 17
    /// spends what index 2 made and index 50 spends what index 17 made, and all
    /// three are consumed.
    #[test]
    fn a_repeated_transaction_recreates_an_output_spent_in_between() {
        let first = trimmed_block("dijkstra-repeat-ranking-first.block");
        let second = trimmed_block("dijkstra-repeat-ranking-second.block");

        let first = MultiEraBlock::decode(&first).unwrap();
        let second = MultiEraBlock::decode(&second).unwrap();

        assert_eq!(first.slot(), 1861279, "fixture precondition");
        assert_eq!(second.slot(), 1861288, "fixture precondition");

        let resurrected = TxoRef(
            Hash::from_str("3fe3ab01a255960d22d63e7ad8fabdee8cd7d9d884790105c35fa3bea1b53e01")
                .unwrap(),
            0,
        );

        let mut ledger = FakeLedger::default();
        ledger.seed_externals(&first);

        let first_stats = ledger.apply(&first);

        assert_eq!(
            first_stats.skipped_inputs, 0,
            "the first block's own chain resolves, nothing is skipped"
        );
        assert!(
            !ledger.holds(&resurrected),
            "fixture precondition: index 234 of the first block spends what index 50 made"
        );

        let second_stats = ledger.apply(&second);

        assert!(
            ledger.holds(&resurrected),
            "the repeat must re-create the output the first block spent"
        );
        assert!(
            second_stats.skipped_inputs > 0,
            "the repeat's own input was already spent, so it consumed nothing"
        );
    }

    /// MUST FIRE: applying the same block twice reports every one of its
    /// outputs as re-created the second time, and consumes nothing the second
    /// time because the first application already spent it all.
    ///
    /// The counter is what an operator reads to see how much of a chain is
    /// re-application, so it has to count the case it is named for. Asserting
    /// it is merely non-zero on a real block would pass on any transaction that
    /// happened to repeat, which is a different fact.
    #[test]
    fn re_applying_a_block_counts_every_output_as_re_created() {
        let cbor = trimmed_block("dijkstra-repeat-ranking-first.block");
        let block = MultiEraBlock::decode(&cbor).unwrap();

        let outputs: usize = block.txs().iter().map(|tx| tx.produces().len()).sum();
        let inputs: usize = block.txs().iter().map(|tx| tx.consumes().len()).sum();
        assert!(outputs > 0 && inputs > 0, "fixture precondition");

        // The block's own outputs that the block itself spends. Those are gone
        // by the end of the first application, so the second application finds
        // them absent and creates them afresh rather than over anything.
        let made_here: HashSet<TxoRef> = block
            .txs()
            .iter()
            .flat_map(|tx| {
                let hash = tx.hash();
                tx.produces()
                    .into_iter()
                    .map(move |(idx, _)| TxoRef(hash, idx as u32))
            })
            .collect();

        let chained = block
            .txs()
            .iter()
            .flat_map(MultiEraTx::consumes)
            .filter(|i| made_here.contains(&TxoRef(*i.hash(), i.index() as u32)))
            .count();

        assert!(chained > 0, "fixture precondition: the block chains in itself");

        let mut ledger = FakeLedger::default();
        ledger.seed_externals(&block);

        let first = ledger.apply(&block);
        assert_eq!(
            first.recreated_outputs, 0,
            "nothing is re-created the first time"
        );
        assert_eq!(
            first.skipped_inputs, 0,
            "and every input resolves the first time"
        );

        let second = ledger.apply(&block);
        assert_eq!(
            second.recreated_outputs,
            outputs - chained,
            "every output still standing is re-created the second time"
        );
        assert_eq!(
            second.skipped_inputs,
            inputs - chained,
            "every input from outside the block was already spent, so none is consumed again"
        );
    }

    /// MUST NOT FIRE: the strict rule is untouched and still refuses an input
    /// it cannot resolve. Every network other than this one runs it, and a
    /// leniency that leaked into it would hide a real defect rather than mirror
    /// a prototype.
    #[test]
    fn the_strict_rule_still_refuses_a_missing_input() {
        let cbor = forward_ref_block();
        let block = MultiEraBlock::decode(&cbor).unwrap();

        let mut context = fake_slice_for_block(&block);
        context.remove(&forward_ref_txoref());

        let err = super::compute_apply_delta(&block, &context)
            .expect_err("the strict rule must refuse an input it cannot resolve");

        match err {
            BrokenInvariant::MissingUtxo(r) => assert_eq!(r, forward_ref_txoref()),
            other => panic!("wrong refusal: {other:?}"),
        }
    }
}

/// What the w35 to w36 fixture rewrite decided, pinned.
///
/// The nine Dijkstra block fixtures in this repository are generated from the
/// `.w35hex` bytes beside them by `test_data/regen-dijkstra-w36.py`, because
/// the chain they were cut from no longer exists and the chain that does
/// carries none of the shapes they hold. That rewrite makes exactly two claims
/// about every fixture and neither of them is checked by any other test here:
///
/// - the header is carried over byte for byte, so the block is still the one
///   the chain produced. The block hash is the hash of the header, so a header
///   that changed by one byte changes the value pinned below.
/// - every transaction's verdict is `true`, which follows from the deleted
///   element having been nil in every source and does not follow from anything
///   else.
///
/// The second claim needs pinning here because nothing else notices it. Flipping
/// all 1185 verdicts across the three fixtures that have any made two tests in
/// this file panic on an `unwrap` inside their own setup, which is a crash and
/// not a verdict, and left every other test passing.
#[cfg(test)]
mod dijkstra_fixture_shape {
    use pallas::ledger::traverse::MultiEraBlock;

    /// Directory relative to this crate, file name, the hash of its header, and
    /// how many transactions it holds. The last two were read from the
    /// `.w35hex` source, so they are the chain's numbers and not the rewrite's.
    const FIXTURES: &[(&str, &str, &str, usize)] = &[
        (
            "../../test_data",
            "dijkstra-quiet.block",
            "4ddd143b2d4b65e57057ba62d9640639bfd8808764d67701bbbf6c17eaa6649e",
            0,
        ),
        (
            "../../test_data",
            "dijkstra-plain.block",
            "56fa1ce910f496749e69bb72f1b85020cc9cda876d3118269970decbc2e4c87a",
            426,
        ),
        (
            "../../test_data",
            "dijkstra-certifying.block",
            "02a42d1c692166e03eecf385b394664c86fc31dfa82d3cd7b91a1b114549a44e",
            0,
        ),
        (
            "../../test_data",
            "dijkstra-certify-only.block",
            "3df49aa4c2ced2fa9e9e8f3a9f26baf21267f26a8b56f30b6f149a527d34f21e",
            0,
        ),
        (
            "test_data",
            "dijkstra-forward-ref.block",
            "7fec72c2101360ecfa39a82d0e5a2594b1a57053728db6dfd23aad44ba8739dc",
            0,
        ),
        (
            "test_data",
            "dijkstra-repeat-first.block",
            "92cff4b6bd454762aed3d8f99cc1998a2c9bf78570bd545b8d5d9507122a21b3",
            0,
        ),
        (
            "test_data",
            "dijkstra-repeat-second.block",
            "23459432ba22c4cc12e7b3fbff2e5171bf31268755b165e5e44a2fead8df044c",
            0,
        ),
        (
            "test_data",
            "dijkstra-repeat-ranking-first.block",
            "f839003db51a41866d107aa2447d5aa9a07a77f7be04bd394a3b3368902dd38e",
            375,
        ),
        (
            "test_data",
            "dijkstra-repeat-ranking-second.block",
            "6a6028d03ce37419c82492588f6ddc16719286a9ac47b66a2c745d3934da3c78",
            384,
        ),
    ];

    fn read(dir: &str, name: &str) -> Vec<u8> {
        let path = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join(dir)
            .join(name);
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        hex::decode(text.trim()).unwrap_or_else(|e| panic!("{path:?}: {e}"))
    }

    /// The must-fire case for the header: a rewrite that disturbed one byte of
    /// a header changes that block's hash and this stops.
    #[test]
    fn every_rewritten_fixture_kept_its_header_and_its_transaction_count() {
        let mut checked = 0;

        for (dir, name, hash, count) in FIXTURES {
            let raw = read(dir, name);
            let block = MultiEraBlock::decode(&raw).unwrap_or_else(|e| panic!("{name}: {e}"));

            assert_eq!(
                block.hash().to_string(),
                *hash,
                "{name}: the header is not the one the chain produced"
            );
            assert_eq!(
                block.txs().len(),
                *count,
                "{name}: the rewrite changed how many transactions the block holds"
            );
            checked += 1;
        }

        assert_eq!(
            checked,
            FIXTURES.len(),
            "every fixture named has to have been read"
        );
    }

    /// The must-fire case for the verdict, and the reason this module exists.
    /// The rewrite wrote `true` on every transaction because the source said no
    /// transaction in the block was invalid. Nothing else here would notice a
    /// `false`.
    #[test]
    fn every_rewritten_transaction_carries_the_verdict_the_source_implied() {
        let mut verdicts = 0;

        for (dir, name, _, count) in FIXTURES {
            let raw = read(dir, name);
            let block = MultiEraBlock::decode(&raw).unwrap_or_else(|e| panic!("{name}: {e}"));

            for (index, tx) in block.txs().iter().enumerate() {
                assert!(
                    tx.is_valid(),
                    "{name}: transaction {index} carries the verdict false, and the w35 source \
                     named no transaction as rejected, so nothing justifies it"
                );
                verdicts += 1;
            }

            assert_eq!(block.txs().len(), *count, "{name}: transaction count moved");
        }

        // A count, so that a run in which every block came back empty says so
        // rather than passing on nothing.
        assert_eq!(
            verdicts, 1185,
            "the fixtures hold 1185 transactions between them and every one of \
             them has to have been looked at"
        );
    }
}
