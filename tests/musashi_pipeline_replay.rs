//! Replays every harvested Musashi block through the live roll path and checks
//! the UTxO set and the transaction index it leaves behind.
//!
//! The fixtures are separate points of one chain and not a run of consecutive
//! blocks, so each one is replayed on its own store, seeded with the outputs
//! that block spends.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dolos_core::{
    config::{CardanoConfig, SyncConfig},
    sync::SyncExt,
    ArchiveStore, Domain, EraCbor, Genesis, StateStore, TxoRef, UtxoSetDelta,
};
use dolos_testing::toy_domain::ToyDomain;
use pallas::crypto::hash::Hash;
use pallas::ledger::traverse::{MultiEraBlock, MultiEraTx};
use serde::Deserialize;

const DIR: &str = "test_data/musashi-w36";
const PROVENANCE: &str = "provenance.toml";
const NETWORK_MAGIC: u64 = 164;

/// The protocol version the node's own configuration forces, which is what
/// selects the Dijkstra parameters for every slot of the replay.
const FORCED_PROTOCOL: usize = 11;

/// The number of fixture entries carrying a block. A fixture added to the
/// directory is replayed or this count fails.
const BLOCK_FIXTURES: usize = 9;

#[derive(Deserialize)]
struct Provenance {
    fixture: Vec<Fixture>,
}

#[derive(Deserialize)]
struct Fixture {
    name: String,
    files: Vec<String>,
    network_magic: u64,
    slot: u64,
    block_hash: Option<String>,
    transactions: usize,
}

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(DIR)
}

fn provenance() -> Provenance {
    let path = dir().join(PROVENANCE);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is unreadable: {e}", path.display()));
    toml::from_str(&text).unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()))
}

/// Every entry whose single file is a block, with that file's bytes, in slot
/// order.
fn block_fixtures() -> Vec<(Fixture, Vec<u8>)> {
    let mut out = vec![];

    for entry in provenance().fixture {
        let blocks: Vec<&String> = entry
            .files
            .iter()
            .filter(|f| f.ends_with(".block"))
            .collect();

        if blocks.is_empty() {
            continue;
        }

        assert_eq!(
            blocks.len(),
            1,
            "{} names more than one block file",
            entry.name
        );

        let path = dir().join(blocks[0]);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} is unreadable: {e}", path.display()));
        let bytes = hex::decode(text.trim())
            .unwrap_or_else(|e| panic!("{} is not hex: {e}", path.display()));

        out.push((entry, bytes));
    }

    out.sort_by_key(|(entry, _)| entry.slot);
    out
}

/// Only the Dijkstra parameters and the protocol version are this chain's own.
/// The earlier era files are preview's.
fn genesis() -> Arc<Genesis> {
    let dijkstra = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates")
        .join("core")
        .join("test_data")
        .join("musashi")
        .join("dijkstra-genesis.json");

    let mut genesis = dolos_cardano::include::preview::load();
    genesis.dijkstra = Some(dolos_core::dijkstra::from_file(dijkstra).unwrap());
    genesis.force_protocol = Some(FORCED_PROTOCOL);

    Arc::new(genesis)
}

fn chain_config() -> CardanoConfig {
    CardanoConfig {
        magic: NETWORK_MAGIC,
        is_testnet: true,
        stop_epoch: None,
        custom_utxos: vec![],
    }
}

fn sync_config(lenient: bool) -> SyncConfig {
    let mut config = SyncConfig::default();
    config.leios_lenient_apply = lenient;
    config
}

/// Calls `f` on each transaction of the block, after each sub transaction it
/// carries.
fn each_tx(block: &MultiEraBlock, mut f: impl FnMut(&MultiEraTx<'_>)) {
    for tx in block.txs().iter() {
        for sub in tx.sub_transactions() {
            f(&sub);
        }

        f(tx);
    }
}

/// Every output the block's transactions and sub transactions make, each with
/// the bytes the block declares for it.
fn produced(block: &MultiEraBlock) -> Vec<(TxoRef, Vec<u8>)> {
    let mut out = vec![];

    each_tx(block, |tx| {
        let hash = tx.hash();

        for (idx, output) in tx.produces() {
            out.push((TxoRef(hash, idx as u32), output.encode()));
        }
    });

    out
}

/// Every output the block's transactions and sub transactions spend.
fn consumed(block: &MultiEraBlock) -> Vec<TxoRef> {
    let mut out = vec![];

    each_tx(block, |tx| {
        for input in tx.consumes() {
            out.push(TxoRef(*input.hash(), input.index() as u32));
        }
    });

    out
}

/// The hash of each of the block's transactions and sub transactions.
fn tx_hashes(block: &MultiEraBlock) -> Vec<Hash<32>> {
    let mut out = vec![];

    each_tx(block, |tx| out.push(tx.hash()));

    out
}

/// The outputs the block spends and does not make itself, each carrying a body
/// the store can decode.
///
/// One output of the block stands in for every external body. What is asserted
/// here is which refs exist and what the block's own outputs decode to, so an
/// external body only has to be a readable output of the block's era.
fn external_inputs(block: &MultiEraBlock) -> HashMap<TxoRef, Arc<EraCbor>> {
    let mut seeds = HashMap::new();

    let txs = block.txs();

    let Some(first) = txs.first() else {
        return seeds;
    };

    let outputs = first.produces();

    let Some((_, sample)) = outputs.first() else {
        return seeds;
    };

    let sample = sample.encode();
    let made_here: HashSet<TxoRef> = produced(block).into_iter().map(|(r, _)| r).collect();

    for key in consumed(block) {
        if made_here.contains(&key) {
            continue;
        }

        seeds.insert(key, Arc::new(EraCbor(block.era().into(), sample.clone())));
    }

    seeds
}

/// Refuses a block one of whose transactions spends an output a later
/// transaction of the same block makes.
///
/// Under the node's lenient rule such a spend consumes nothing and the output
/// survives, so the expected set would no longer be outputs created and inputs
/// removed. The harvest found no block of that shape on this chain.
fn assert_no_forward_reference(entry: &Fixture, block: &MultiEraBlock) {
    let made_anywhere: HashSet<TxoRef> = produced(block).into_iter().map(|(r, _)| r).collect();
    let mut made_already: HashSet<TxoRef> = HashSet::new();

    let txs = block.txs();

    for tx in txs.iter() {
        for input in tx.consumes() {
            let key = TxoRef(*input.hash(), input.index() as u32);

            assert!(
                !made_anywhere.contains(&key) || made_already.contains(&key),
                "{} spends {}#{} before the transaction that makes it",
                entry.name,
                key.0,
                key.1
            );
        }

        let hash = tx.hash();
        for (idx, _) in tx.produces() {
            made_already.insert(TxoRef(hash, idx as u32));
        }
    }
}

fn live_refs<D: Domain>(domain: &D) -> HashSet<TxoRef> {
    domain
        .state()
        .iter_utxos()
        .unwrap()
        .map(|entry| entry.unwrap().0)
        .collect()
}

/// What one replay of one block leaves behind.
struct Replayed {
    /// The refs the store held before the block, which are the genesis outputs
    /// and the seeded external inputs.
    before: HashSet<TxoRef>,
    after: HashSet<TxoRef>,
    /// The body the store answers for each output the block makes.
    bodies: HashMap<TxoRef, Vec<u8>>,
    /// The slot the archive answers for each of the block's transaction
    /// hashes, in wire order.
    served: Vec<Option<u64>>,
}

fn replay(entry: &Fixture, cbor: &[u8], lenient: bool) -> Replayed {
    let block = MultiEraBlock::decode(cbor).unwrap();

    let seeds = external_inputs(&block);

    let delta = UtxoSetDelta {
        produced_utxo: seeds.clone(),
        ..Default::default()
    };

    let domain =
        ToyDomain::new_with_genesis_and_config(genesis(), chain_config(), Some(delta), None)
            .with_sync_config(sync_config(lenient));

    let before = live_refs(&domain);

    for key in seeds.keys() {
        assert!(
            before.contains(key),
            "{} was seeded with {}#{} and the store does not hold it",
            entry.name,
            key.0,
            key.1
        );
    }

    domain.roll_forward(Arc::new(cbor.to_vec())).unwrap();

    let after = live_refs(&domain);

    let made: Vec<TxoRef> = produced(&block).into_iter().map(|(r, _)| r).collect();

    let bodies = domain
        .state()
        .get_utxos(made)
        .unwrap()
        .into_iter()
        .map(|(key, body)| (key, body.1.clone()))
        .collect();

    let served = tx_hashes(&block)
        .into_iter()
        .map(|hash| {
            domain
                .archive()
                .slot_by_tx_hash(hash.as_slice())
                .unwrap()
        })
        .collect();

    Replayed {
        before,
        after,
        bodies,
        served,
    }
}

/// The refs the store must hold after the block: what it held before, plus
/// every output the block makes, minus every output the block spends.
fn expected_after(before: &HashSet<TxoRef>, block: &MultiEraBlock) -> HashSet<TxoRef> {
    let mut expected = before.clone();

    for (key, _) in produced(block) {
        expected.insert(key);
    }

    for key in consumed(block) {
        expected.remove(&key);
    }

    expected
}

/// MUST FIRE: after the replay the store holds every output the block makes,
/// byte for byte, and holds no output the block spends.
///
/// MUST NOT FIRE: the store holds nothing else. The set is compared whole
/// against what the store held before the block with the block's own outputs
/// added and its own inputs removed, so an invented output fails here as
/// loudly as a missing one, and a block carrying no transaction must leave the
/// set as it was.
#[test]
fn every_harvested_block_makes_its_outputs_and_spends_its_inputs() {
    let fixtures = block_fixtures();

    assert_eq!(
        fixtures.len(),
        BLOCK_FIXTURES,
        "the directory records another number of blocks"
    );

    for (entry, cbor) in &fixtures {
        assert_eq!(
            entry.network_magic, NETWORK_MAGIC,
            "{} carries another network magic",
            entry.name
        );

        let block = MultiEraBlock::decode(cbor).unwrap();

        assert_eq!(
            block.slot(),
            entry.slot,
            "{} is not the block at the slot it records",
            entry.name
        );

        let recorded = entry
            .block_hash
            .clone()
            .unwrap_or_else(|| panic!("{} records no block hash", entry.name));

        assert_eq!(
            block.hash().to_string(),
            recorded,
            "{} does not hash to the hash it records",
            entry.name
        );

        assert_no_forward_reference(entry, &block);

        let replayed = replay(entry, cbor, true);

        assert_eq!(
            replayed.after,
            expected_after(&replayed.before, &block),
            "{} leaves a UTxO set that is not its inputs removed and its outputs created",
            entry.name
        );

        for (key, bytes) in produced(&block) {
            assert_eq!(
                replayed.bodies.get(&key),
                Some(&bytes),
                "{} makes {}#{} and the store answers another body",
                entry.name,
                key.0,
                key.1
            );
        }
    }
}

/// MUST FIRE: every transaction and sub transaction the block carries is
/// reachable by its own hash and answers the block's slot, and the number of
/// top level transactions is the number the harvest counted.
///
/// MUST NOT FIRE: the blocks the harvest counted no transaction in serve none,
/// so the count is not satisfied by a store that answers for everything.
#[test]
fn every_transaction_of_every_harvested_block_is_served_at_its_own_slot() {
    let fixtures = block_fixtures();

    assert_eq!(
        fixtures.len(),
        BLOCK_FIXTURES,
        "the directory records another number of blocks"
    );

    let mut with_transactions = 0;

    for (entry, cbor) in &fixtures {
        let block = MultiEraBlock::decode(cbor).unwrap();
        let hashes = tx_hashes(&block);

        let distinct: HashSet<&Hash<32>> = hashes.iter().collect();

        assert_eq!(
            distinct.len(),
            hashes.len(),
            "{} carries one transaction hash twice",
            entry.name
        );

        assert_eq!(
            block.txs().len(),
            entry.transactions,
            "{} carries another number of transactions than the harvest counted",
            entry.name
        );

        let replayed = replay(entry, cbor, true);

        for (hash, slot) in hashes.iter().zip(replayed.served.iter()) {
            assert_eq!(
                *slot,
                Some(entry.slot),
                "{} carries {hash} and the archive answers another slot",
                entry.name
            );
        }

        if !hashes.is_empty() {
            with_transactions += 1;
        }
    }

    assert!(
        with_transactions > 0,
        "no fixture carries a transaction, so nothing was served"
    );
}

/// MUST NOT FIRE: the lenient apply rule changes nothing for these blocks.
///
/// The rule only acts on an input that is not there or an output made twice,
/// and the harvest found neither on this chain, so a difference between the
/// two runs means the rule is acting where it has nothing to act on.
#[test]
fn the_lenient_rule_changes_nothing_for_the_harvested_blocks() {
    let fixtures = block_fixtures();

    assert_eq!(
        fixtures.len(),
        BLOCK_FIXTURES,
        "the directory records another number of blocks"
    );

    for (entry, cbor) in &fixtures {
        let lenient = replay(entry, cbor, true);
        let strict = replay(entry, cbor, false);

        assert_eq!(
            lenient.after, strict.after,
            "{} leaves a different UTxO set under the two apply rules",
            entry.name
        );

        assert_eq!(
            lenient.bodies, strict.bodies,
            "{} answers different bodies under the two apply rules",
            entry.name
        );

        assert_eq!(
            lenient.served, strict.served,
            "{} serves different transactions under the two apply rules",
            entry.name
        );
    }
}
