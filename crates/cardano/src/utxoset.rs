use dolos_core::config::CardanoConfig;
use dolos_core::*;
use pallas::ledger::traverse::{MultiEraBlock, MultiEraOutput, MultiEraTx};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::owned::OwnedMultiEraOutput;

/// The inputs a block spends that a LATER transaction of the same block
/// produces.
///
/// On a Praos chain this is always empty. The ledger applies a block's
/// transactions in sequence, so a transaction spending an output produced after
/// it does not validate and no such block reaches a follower. Treating a
/// block's transactions as a set, which is what resolving every produced output
/// before any consumed one does, is therefore harmless there and is what this
/// module has always done.
///
/// A certified Leios endorser block is not like that. Its transactions arrive
/// in the order the endorser block lists them, nothing requires that order to
/// be topological, and the producing node applies them in that order with
/// validation switched off. A spend of an output that does not exist yet
/// removes nothing there, and the output is still unspent when the later
/// transaction of the same block creates it.
///
/// So resolving such an input against the block's own later output consumes an
/// output the rest of the network still holds. The follower and the chain then
/// disagree about one entry of the UTxO set, and the disagreement surfaces much
/// later as an input not found, when a subsequent block spends it. This names
/// those inputs so they can be left unconsumed, which is what the network does
/// with them.
///
/// Membership is a positive fact about the block in hand and never an absence.
/// An input is here only because a transaction of this same block produces it
/// after the one spending it. An input nothing in the block produces is not
/// here, and stays exactly as missing as it ever was.
pub fn compute_forward_references(block: &MultiEraBlock) -> HashSet<TxoRef> {
    let txs = block.txs();

    let mut produced_at: HashMap<TxoRef, usize> = HashMap::new();

    for (position, tx) in txs.iter().enumerate() {
        let hash = tx.hash();

        for (idx, _) in tx.produces() {
            produced_at.insert(TxoRef(hash, idx as u32), position);
        }
    }

    let mut forward = HashSet::new();

    for (position, tx) in txs.iter().enumerate() {
        for consumed in tx.consumes() {
            let txoref = TxoRef(*consumed.hash(), consumed.index() as u32);

            if let Some(produced) = produced_at.get(&txoref) {
                if *produced > position {
                    forward.insert(txoref);
                }
            }
        }
    }

    forward
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

    // An input a later transaction of this same block produces is spent by a
    // transaction that runs before the output exists, so the network removes
    // nothing for it and neither does this. See `compute_forward_references`.
    let forward = compute_forward_references(block);

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

            if forward.contains(&stxi_ref) {
                continue;
            }

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

    fn load_test_block(name: &str) -> Vec<u8> {
        let path = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("test_data")
            .join(name);

        let content = std::fs::read_to_string(path).unwrap();
        hex::decode(content).unwrap()
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

    /// MUST FIRE: an input a later transaction of the same block produces is
    /// named as a forward reference.
    ///
    /// MUST NOT FIRE: ordinary transaction chaining, where a transaction spends
    /// an output an EARLIER transaction of the same block produced, is not. The
    /// alonzo block is in the fixtures precisely because it chains, and a
    /// predicate that could not tell the two apart would strip the payload out
    /// of every ordinary block on the chain.
    #[test]
    fn a_forward_reference_is_named_and_ordinary_chaining_is_not() {
        let cbor = forward_ref_block();
        let block = MultiEraBlock::decode(&cbor).unwrap();
        assert_eq!(block.slot(), 1861242, "fixture precondition");
        assert_eq!(block.tx_count(), 2, "fixture precondition");

        let forward = super::compute_forward_references(&block);

        assert_eq!(forward.len(), 1);
        assert!(forward.contains(&forward_ref_txoref()));

        let chained = load_test_block("alonzo27.block");
        let chained = MultiEraBlock::decode(&chained).unwrap();

        assert!(
            chained
                .txs()
                .iter()
                .flat_map(MultiEraTx::consumes)
                .any(|i| chained
                    .txs()
                    .iter()
                    .any(|t| t.hash() == *i.hash())),
            "fixture precondition: the alonzo block chains within itself"
        );
        assert!(
            super::compute_forward_references(&chained).is_empty(),
            "backward chaining is not a forward reference"
        );
    }

    /// MUST FIRE: the delta of a block carrying a forward reference does not
    /// consume the forward referenced output, because the network did not
    /// consume it either, and it does produce it, because the later transaction
    /// creates it.
    ///
    /// MUST NOT FIRE: the block's other inputs are still consumed. A delta that
    /// stopped consuming anything would leave every spent output alive and
    /// would pass an assertion about one absent entry while being wrong about
    /// all the rest.
    #[test]
    fn a_forward_referenced_output_is_produced_and_not_consumed() {
        let cbor = forward_ref_block();
        let block = MultiEraBlock::decode(&cbor).unwrap();
        let context = fake_slice_for_block(&block);

        let delta = super::compute_apply_delta(&block, &context).unwrap();

        let target = forward_ref_txoref();

        assert!(
            delta.produced_utxo.contains_key(&target),
            "the later transaction still produces it"
        );
        assert!(
            !delta.consumed_utxo.contains_key(&target),
            "the earlier transaction must not consume an output that does not exist yet"
        );

        let other_inputs: Vec<TxoRef> = block
            .txs()
            .iter()
            .flat_map(MultiEraTx::consumes)
            .map(|i| TxoRef(*i.hash(), i.index() as u32))
            .filter(|r| *r != target)
            .collect();

        assert!(
            !other_inputs.is_empty(),
            "fixture precondition: the pair spends something else too"
        );

        for input in other_inputs {
            assert!(
                delta.consumed_utxo.contains_key(&input),
                "every other input is still consumed"
            );
        }
    }
}
