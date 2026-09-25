//! Replays every harvested Musashi block through the live roll path and checks
//! the UTxO set and the transaction index it leaves behind.
//!
//! The fixtures are separate points of one chain and not a run of consecutive
//! blocks, so each one is replayed on its own store, seeded with the outputs
//! that block spends.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dolos_core::{
    async_query::{AsyncQueryFacade, BlockMetaResolver},
    config::{CardanoConfig, SyncConfig},
    sync::SyncExt,
    ArchiveStore, Domain, EraCbor, Genesis, StateStore, TxCbor, TxoRef, UtxoSetDelta,
};
use dolos_testing::toy_domain::ToyDomain;
use pallas::codec::minicbor;
use pallas::crypto::hash::{Hash, Hasher};
use pallas::ledger::primitives::dijkstra;
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
const BLOCK_FIXTURES: usize = 10;

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

/// Each sub transaction the block's top level transactions list, with the
/// index of the one that lists it and the verdict on it, read from the block's
/// own fields.
fn sub_transactions<'a>(
    block: &'a MultiEraBlock,
) -> Vec<(usize, bool, &'a dijkstra::SubTransaction<'a>)> {
    let Some(block) = block.as_dijkstra() else {
        return vec![];
    };

    block
        .block_body
        .transactions
        .iter()
        .enumerate()
        .flat_map(|(index, tx)| {
            tx.transaction_body
                .sub_transactions
                .iter()
                .flat_map(|subs| subs.iter())
                .map(move |sub| (index, tx.success, sub))
        })
        .collect()
}

/// The hash of a sub transaction, which is the hash of its body's bytes.
fn sub_hash(sub: &dijkstra::SubTransaction) -> Hash<32> {
    Hasher::<256>::hash(sub.sub_transaction_body.raw_cbor())
}

/// Every output the block's transactions make, each with the bytes the block
/// declares for it, and every output of a sub transaction a valid transaction
/// lists.
fn produced(block: &MultiEraBlock) -> Vec<(TxoRef, Vec<u8>)> {
    let mut out = vec![];

    for tx in block.txs() {
        let hash = tx.hash();

        for (idx, output) in tx.produces() {
            out.push((TxoRef(hash, idx as u32), output.encode()));
        }
    }

    for (_, success, sub) in sub_transactions(block) {
        if !success {
            continue;
        }

        let hash = sub_hash(sub);

        for (idx, output) in sub.sub_transaction_body.outputs.iter().enumerate() {
            out.push((TxoRef(hash, idx as u32), minicbor::to_vec(output).unwrap()));
        }
    }

    out
}

/// Every output the block's transactions spend, and every input of a sub
/// transaction a valid transaction lists.
fn consumed(block: &MultiEraBlock) -> Vec<TxoRef> {
    let mut out = vec![];

    for tx in block.txs() {
        for input in tx.consumes() {
            out.push(TxoRef(*input.hash(), input.index() as u32));
        }
    }

    for (_, success, sub) in sub_transactions(block) {
        if !success {
            continue;
        }

        for input in sub.sub_transaction_body.inputs.iter() {
            out.push(TxoRef(input.transaction_id, input.index as u32));
        }
    }

    out
}

/// The hash of each of the block's transactions and of each sub transaction
/// they list.
fn tx_hashes(block: &MultiEraBlock) -> Vec<Hash<32>> {
    block
        .txs()
        .iter()
        .map(|tx| tx.hash())
        .chain(
            sub_transactions(block)
                .into_iter()
                .map(|(_, _, sub)| sub_hash(sub)),
        )
        .collect()
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
    domain: ToyDomain,
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
        .map(|hash| domain.archive().slot_by_tx_hash(hash.as_slice()).unwrap())
        .collect();

    Replayed {
        before,
        after,
        bodies,
        served,
        domain,
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

/// MUST FIRE: every transaction and sub transaction in the block resolves by
/// its own hash and answers the block's slot, and the number of
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
    let mut sub_hashes = 0;

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

        sub_hashes += sub_transactions(&block).len();
    }

    assert!(
        with_transactions > 0,
        "no fixture carries a transaction, so nothing was served"
    );

    assert!(
        sub_hashes > 0,
        "no fixture lists a sub transaction, so no sub transaction was served"
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

/// A transaction a lookup by hash is asked for: its hash, the index of the top
/// level transaction that holds it, its bytes, the answer `tx_cbor` gives for
/// it and its number of outputs.
struct Probe {
    hash: Hash<32>,
    index: usize,
    bytes: Vec<u8>,
    cbor: TxCbor,
    outputs: usize,
}

/// Every sub transaction the block lists, each top level transaction that
/// lists one, and the block's first and last transaction.
fn probes(block: &MultiEraBlock) -> Vec<Probe> {
    let txs = block.txs();
    let subs = sub_transactions(block);

    let mut indexes: BTreeSet<usize> = subs.iter().map(|(index, _, _)| *index).collect();

    if let Some(last) = txs.len().checked_sub(1) {
        indexes.extend([0, last]);
    }

    let top_level = indexes.into_iter().map(|index| Probe {
        hash: txs[index].hash(),
        index,
        bytes: txs[index].encode(),
        cbor: TxCbor::Tx(EraCbor(block.era().into(), txs[index].encode())),
        outputs: txs[index].outputs().len(),
    });

    let subs = subs.into_iter().map(|(index, success, sub)| Probe {
        hash: sub_hash(sub),
        index,
        bytes: minicbor::to_vec(sub).unwrap(),
        cbor: TxCbor::DijkstraSub(minicbor::to_vec(sub).unwrap(), success),
        outputs: sub.sub_transaction_body.outputs.len(),
    });

    top_level.chain(subs).collect()
}

/// A hash no fixture holds.
const ABSENT: [u8; 32] = [0xff; 32];

/// MUST FIRE: each sub transaction is found by its own hash in every core
/// lookup by hash, which answers its own bytes and the index of the top level
/// transaction that lists it.
///
/// MUST NOT FIRE: a top level transaction is found as itself at its own index,
/// and a hash no block holds is found by no lookup.
#[test]
fn every_lookup_by_hash_finds_each_sub_transaction() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut sub_probes = 0;

    for (entry, cbor) in &block_fixtures() {
        let block = MultiEraBlock::decode(cbor).unwrap();
        let probes = probes(&block);

        if probes.is_empty() {
            continue;
        }

        sub_probes += sub_transactions(&block).len();

        let replayed = replay(entry, cbor, true);
        let query = AsyncQueryFacade::new(replayed.domain.clone());

        runtime.block_on(async {
            for probe in &probes {
                let name = format!("{} {}", entry.name, probe.hash);

                let (raw, index) = query
                    .block_by_tx_hash(probe.hash.to_vec())
                    .await
                    .unwrap()
                    .unwrap_or_else(|| panic!("{name}: block_by_tx_hash finds nothing"));

                assert_eq!(
                    (MultiEraBlock::decode(&raw).unwrap().slot(), index),
                    (entry.slot, probe.index),
                    "{name}: block_by_tx_hash"
                );

                let meta = query
                    .block_meta_by_tx_hash(probe.hash.to_vec())
                    .await
                    .unwrap()
                    .unwrap_or_else(|| panic!("{name}: block_meta_by_tx_hash finds nothing"));

                assert_eq!(
                    (meta.slot, meta.tx_hash, meta.tx_index),
                    (entry.slot, probe.hash, probe.index),
                    "{name}: block_meta_by_tx_hash"
                );

                let batch = BlockMetaResolver::new(query.clone())
                    .resolve_batch([probe.hash])
                    .await
                    .unwrap();

                assert_eq!(
                    batch
                        .get(&probe.hash)
                        .map(|meta| (meta.slot, meta.tx_index)),
                    Some((entry.slot, probe.index)),
                    "{name}: resolve_batch"
                );

                let answer = query
                    .tx_cbor(probe.hash.to_vec())
                    .await
                    .unwrap()
                    .unwrap_or_else(|| panic!("{name}: tx_cbor finds nothing"));

                assert_eq!(answer, probe.cbor, "{name}: tx_cbor");

                assert_eq!(
                    MultiEraTx::try_from(&answer).ok().map(|tx| tx.hash()),
                    Some(probe.hash),
                    "{name}: tx_cbor decodes"
                );
            }

            assert_eq!(
                (
                    query
                        .block_by_tx_hash(ABSENT.to_vec())
                        .await
                        .unwrap()
                        .is_some(),
                    query
                        .block_meta_by_tx_hash(ABSENT.to_vec())
                        .await
                        .unwrap()
                        .is_some(),
                    query.tx_cbor(ABSENT.to_vec()).await.unwrap().is_some(),
                ),
                (false, false, false),
                "{}: a hash no block holds is found",
                entry.name
            );
        });
    }

    assert!(
        sub_probes > 0,
        "no fixture lists a sub transaction, so no sub transaction was looked up"
    );
}

/// MUST FIRE: a sub transaction read back from its `TxCbor` is invalid when
/// the transaction listing it was given as invalid.
///
/// MUST NOT FIRE: it is valid when that transaction was given as valid, and
/// in both cases it keeps its own hash.
#[test]
fn a_sub_transaction_answer_keeps_the_verdict_it_was_given() {
    let mut answers = vec![];

    for (_, cbor) in &block_fixtures() {
        let block = MultiEraBlock::decode(cbor).unwrap();

        for (_, _, sub) in sub_transactions(&block) {
            for success in [false, true] {
                let tx = MultiEraTx::from_dijkstra_sub(sub, success);
                let answer = TxCbor::from(&tx);
                let read = MultiEraTx::try_from(&answer).unwrap();

                answers.push(((read.hash(), read.is_valid()), (sub_hash(sub), success)));
            }
        }
    }

    assert!(!answers.is_empty(), "no fixture lists a sub transaction");

    for (read, given) in answers {
        assert_eq!(read, given);
    }
}

/// The block with the verdict on each transaction that lists a sub transaction
/// set to the one given.
fn with_listing_verdict(cbor: &[u8], success: bool) -> Vec<u8> {
    use pallas::codec::utils::MaybeIndefArray;

    let (era, mut block): (u16, dijkstra::Block) = minicbor::decode(cbor).unwrap();

    let (MaybeIndefArray::Def(txs) | MaybeIndefArray::Indef(txs)) =
        &mut block.block_body.transactions;

    for tx in txs.iter_mut() {
        if tx.transaction_body.sub_transactions.is_some() {
            tx.success = success;
        }
    }

    minicbor::to_vec((era, block)).unwrap()
}

/// MUST FIRE: `tx_cbor` answers a sub transaction as invalid when the stored
/// block gives the transaction that lists it as invalid.
///
/// MUST NOT FIRE: it answers the sub transaction as valid when the stored block
/// gives that transaction as valid.
#[test]
fn a_sub_transaction_is_looked_up_with_the_verdict_on_its_parent() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut answers = vec![];

    for (entry, harvested) in &block_fixtures() {
        if sub_transactions(&MultiEraBlock::decode(harvested).unwrap()).is_empty() {
            continue;
        }

        for success in [true, false] {
            let cbor = with_listing_verdict(harvested, success);
            let block = MultiEraBlock::decode(&cbor).unwrap();
            let query = AsyncQueryFacade::new(replay(entry, &cbor, true).domain);

            for (_, _, sub) in sub_transactions(&block) {
                let hash = sub_hash(sub);
                let answer = runtime.block_on(query.tx_cbor(hash.to_vec())).unwrap();
                let expected = TxCbor::DijkstraSub(minicbor::to_vec(sub).unwrap(), success);

                answers.push((
                    format!("{} {hash} {success}", entry.name),
                    answer,
                    Some(expected),
                ));
            }
        }
    }

    assert!(!answers.is_empty(), "no fixture lists a sub transaction");

    for (name, answer, expected) in answers {
        assert_eq!(answer, expected, "{name}: tx_cbor");
    }
}

#[cfg(any(feature = "minibf", feature = "minikupo"))]
async fn get_json(router: &axum::Router, path: &str) -> (u16, serde_json::Value) {
    use http_body_util::BodyExt as _;
    use tower::util::ServiceExt as _;

    let request = axum::http::Request::builder()
        .uri(path)
        .body(axum::body::Body::empty())
        .unwrap();

    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();

    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// MUST FIRE: minibf answers each sub transaction's own hash, index, bytes and
/// outputs under its own hash.
///
/// MUST NOT FIRE: a top level transaction is answered as itself, and a hash no
/// block holds is answered with not found.
#[cfg(feature = "minibf")]
#[test]
fn minibf_serves_each_sub_transaction_by_its_own_hash() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut sub_probes = 0;

    for (entry, cbor) in &block_fixtures() {
        let block = MultiEraBlock::decode(cbor).unwrap();
        let subs = sub_transactions(&block).len();

        if subs == 0 {
            continue;
        }

        sub_probes += subs;

        let replayed = replay(entry, cbor, true);
        let config = dolos_core::config::MinibfConfig::new("[::]:0".parse().unwrap());
        let router = dolos_minibf::build_router(config, replayed.domain.clone());

        runtime.block_on(async {
            for probe in probes(&block) {
                let hash = hex::encode(probe.hash);
                let name = format!("{} {hash}", entry.name);

                let (status, tx) = get_json(&router, &format!("/txs/{hash}")).await;

                assert_eq!(
                    (
                        status,
                        tx["hash"].as_str(),
                        tx["index"].as_u64(),
                        tx["slot"].as_u64()
                    ),
                    (
                        200,
                        Some(hash.as_str()),
                        Some(probe.index as u64),
                        Some(entry.slot)
                    ),
                    "{name}: /txs"
                );

                let (status, cbor) = get_json(&router, &format!("/txs/{hash}/cbor")).await;

                assert_eq!(
                    (status, cbor["cbor"].as_str()),
                    (200, Some(hex::encode(&probe.bytes).as_str())),
                    "{name}: /txs/cbor"
                );

                let (status, utxos) = get_json(&router, &format!("/txs/{hash}/utxos")).await;

                assert_eq!(
                    (status, utxos["outputs"].as_array().map(Vec::len)),
                    (200, Some(probe.outputs)),
                    "{name}: /txs/utxos"
                );
            }

            let (status, _) = get_json(&router, &format!("/txs/{}", hex::encode(ABSENT))).await;

            assert_eq!(
                status, 404,
                "{}: a hash no block holds is served",
                entry.name
            );
        });
    }

    assert!(
        sub_probes > 0,
        "no fixture lists a sub transaction, so no sub transaction was served"
    );
}

/// The hash, top level index and lovelace of each output a block makes to the
/// given address, sub transactions included, for the transactions the ledger
/// applies.
#[cfg(feature = "minibf")]
fn paid_to(block: &MultiEraBlock, address: &str) -> Vec<(Hash<32>, usize, u64)> {
    let paid = |tx: &MultiEraTx, index: usize| -> Vec<(Hash<32>, usize, u64)> {
        tx.produces()
            .into_iter()
            .filter(|(_, output)| output.address().unwrap().to_string() == address)
            .map(|(_, output)| (tx.hash(), index, output.value().coin()))
            .collect()
    };

    let top_level = block
        .txs()
        .iter()
        .enumerate()
        .flat_map(|(index, tx)| paid(tx, index))
        .collect::<Vec<_>>();

    let subs = sub_transactions(block)
        .into_iter()
        .flat_map(|(index, success, sub)| paid(&MultiEraTx::from_dijkstra_sub(sub, success), index))
        .collect::<Vec<_>>();

    top_level.into_iter().chain(subs).collect()
}

/// MUST FIRE: the address a sub transaction pays is served that payment in its
/// total and that sub transaction in its history, at its parent's index.
///
/// MUST NOT FIRE: the history lists no hash the block does not apply.
#[cfg(feature = "minibf")]
#[test]
fn minibf_address_routes_count_each_sub_transaction() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut addresses = 0;

    for (entry, cbor) in &block_fixtures() {
        let block = MultiEraBlock::decode(cbor).unwrap();
        let subs = sub_transactions(&block);

        let paid_by_sub: BTreeSet<String> = subs
            .iter()
            .filter(|(_, success, _)| *success)
            .flat_map(|(_, _, sub)| {
                MultiEraTx::from_dijkstra_sub(sub, true)
                    .produces()
                    .into_iter()
                    .map(|(_, output)| output.address().unwrap().to_string())
                    .collect::<Vec<_>>()
            })
            .collect();

        if paid_by_sub.is_empty() {
            continue;
        }

        let replayed = replay(entry, cbor, true);
        let config = dolos_core::config::MinibfConfig::new("[::]:0".parse().unwrap());
        let router = dolos_minibf::build_router(config, replayed.domain.clone());

        let applied: HashMap<Hash<32>, usize> = block
            .txs()
            .iter()
            .enumerate()
            .map(|(index, tx)| (tx.hash(), index))
            .chain(subs.iter().map(|(index, _, sub)| (sub_hash(sub), *index)))
            .collect();

        runtime.block_on(async {
            for address in &paid_by_sub {
                addresses += 1;
                let name = format!("{} {address}", entry.name);
                let paid = paid_to(&block, address);

                let (status, total) =
                    get_json(&router, &format!("/addresses/{address}/total")).await;

                let lovelace = total["received_sum"]
                    .as_array()
                    .and_then(|amounts| amounts.iter().find(|x| x["unit"] == "lovelace"))
                    .and_then(|x| x["quantity"].as_str())
                    .map(|x| x.parse::<u64>().unwrap());

                assert_eq!(
                    (status, lovelace),
                    (200, Some(paid.iter().map(|(_, _, coin)| coin).sum())),
                    "{name}: /addresses/total"
                );

                let (status, history) =
                    get_json(&router, &format!("/addresses/{address}/transactions")).await;

                let listed: Vec<(Hash<32>, usize)> = history
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|x| {
                        (
                            x["tx_hash"].as_str().unwrap().parse().unwrap(),
                            x["tx_index"].as_u64().unwrap() as usize,
                        )
                    })
                    .collect();

                let paying: BTreeSet<(Hash<32>, usize)> = paid
                    .iter()
                    .map(|(hash, index, _)| (*hash, *index))
                    .collect();

                assert_eq!(
                    (
                        status,
                        paying.iter().all(|x| listed.contains(x)),
                        listed
                            .iter()
                            .all(|(hash, index)| applied.get(hash) == Some(index)),
                    ),
                    (200, true, true),
                    "{name}: /addresses/transactions lists {listed:?}, the block pays {paying:?}"
                );
            }
        });
    }

    assert!(
        addresses > 0,
        "no fixture lists a sub transaction paying an address, so no address was served"
    );
}

/// MUST FIRE: a block's output total counts what its sub transactions pay, and
/// its address list names each sub transaction under the address it pays.
///
/// MUST NOT FIRE: a block whose transactions list no sub transaction is served
/// the output total of its top level transactions.
#[cfg(feature = "minibf")]
#[test]
fn minibf_block_routes_count_each_sub_transaction() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut blocks = (0, 0);

    for (entry, cbor) in &block_fixtures() {
        let block = MultiEraBlock::decode(cbor).unwrap();
        let subs = sub_transactions(&block);

        let replayed = replay(entry, cbor, true);
        let config = dolos_core::config::MinibfConfig::new("[::]:0".parse().unwrap());
        let router = dolos_minibf::build_router(config, replayed.domain.clone());
        let hash = block.hash();

        let top_level: u64 = block
            .txs()
            .iter()
            .flat_map(|tx| tx.produces())
            .map(|(_, output)| output.value().coin())
            .sum();

        let by_subs: Vec<(Hash<32>, String, u64)> = subs
            .iter()
            .flat_map(|(_, success, sub)| {
                let tx = MultiEraTx::from_dijkstra_sub(sub, *success);
                tx.produces()
                    .into_iter()
                    .map(|(_, output)| {
                        (
                            tx.hash(),
                            output.address().unwrap().to_string(),
                            output.value().coin(),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect();

        if subs.is_empty() {
            blocks.0 += 1;
        } else {
            blocks.1 += 1;
        }

        runtime.block_on(async {
            let (status, content) = get_json(&router, &format!("/blocks/{hash}")).await;

            // the route answers null for a block that pays nothing
            let output = top_level + by_subs.iter().map(|x| x.2).sum::<u64>();
            let output = (output > 0).then(|| output.to_string());

            assert_eq!(
                (status, content["output"].as_str()),
                (200, output.as_deref()),
                "{}: /blocks output",
                entry.name
            );

            let (status, addresses) =
                get_json(&router, &format!("/blocks/{hash}/addresses?count=100")).await;

            let listed: BTreeSet<(String, String)> = addresses
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|x| {
                    let address = x["address"].as_str().unwrap().to_string();
                    x["transactions"].as_array().unwrap().iter().map(move |tx| {
                        (address.clone(), tx["tx_hash"].as_str().unwrap().to_string())
                    })
                })
                .collect();

            let paid: BTreeSet<(String, String)> = by_subs
                .iter()
                .map(|(hash, address, _)| (address.clone(), hash.to_string()))
                .collect();

            assert_eq!(
                (status, paid.difference(&listed).count()),
                (200, 0),
                "{}: /blocks/addresses lists {listed:?}, the sub transactions pay {paid:?}",
                entry.name
            );
        });
    }

    assert!(
        blocks.0 > 0 && blocks.1 > 0,
        "the fixtures hold {} blocks without and {} with a sub transaction",
        blocks.0,
        blocks.1
    );
}

/// MUST FIRE: each input a sub transaction spends is answered with that sub
/// transaction as its spender.
///
/// MUST NOT FIRE: an input the top level transaction that lists it spends is
/// answered with that top level transaction.
#[test]
fn the_spender_of_a_sub_transaction_input_is_that_sub_transaction() {
    use dolos_cardano::indexes::AsyncCardanoQueryExt as _;

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut spent = (0, 0);

    for (entry, cbor) in &block_fixtures() {
        let block = MultiEraBlock::decode(cbor).unwrap();
        let body = match block.as_dijkstra() {
            Some(body) => body,
            None => continue,
        };

        let spenders: Vec<(TxoRef, Hash<32>, bool)> = body
            .block_body
            .transactions
            .iter()
            .filter(|tx| tx.success)
            .flat_map(|tx| {
                let parent = Hasher::<256>::hash(tx.transaction_body.raw_cbor());
                let subs = tx
                    .transaction_body
                    .sub_transactions
                    .iter()
                    .flat_map(|subs| subs.iter())
                    .flat_map(|sub| {
                        let hash = sub_hash(sub);
                        sub.sub_transaction_body
                            .inputs
                            .iter()
                            .map(move |input| (input.clone(), hash, true))
                    });
                let own = tx
                    .transaction_body
                    .inputs
                    .iter()
                    .take(1)
                    .map(move |input| (input.clone(), parent, false));
                subs.chain(own).collect::<Vec<_>>()
            })
            .map(|(input, hash, by_sub)| {
                (
                    TxoRef(input.transaction_id, input.index as u32),
                    hash,
                    by_sub,
                )
            })
            .collect();

        if !spenders.iter().any(|x| x.2) {
            continue;
        }

        let replayed = replay(entry, cbor, true);
        let query = AsyncQueryFacade::new(replayed.domain.clone());

        runtime.block_on(async {
            for (txo, hash, by_sub) in &spenders {
                let bytes: Vec<u8> = txo.clone().into();

                assert_eq!(
                    query.tx_by_spent_txo(&bytes).await.unwrap(),
                    Some(*hash),
                    "{} {txo:?}: the spender",
                    entry.name
                );

                if *by_sub {
                    spent.1 += 1;
                } else {
                    spent.0 += 1;
                }
            }
        });
    }

    assert!(
        spent.0 > 0 && spent.1 > 0,
        "the fixtures spend {} inputs by a top level transaction and {} by a sub transaction",
        spent.0,
        spent.1
    );
}

/// The fixture's block, by its name.
fn block_fixture(name: &str) -> Vec<u8> {
    block_fixtures()
        .into_iter()
        .find(|(entry, _)| entry.name == name)
        .unwrap_or_else(|| panic!("no block fixture is named {name}"))
        .1
}

/// The block whose sub transaction makes the outputs the later block spends.
const SUB_OUTPUTS: &str = "ranking-sub-transaction-two-outputs";

/// The block that spends and references an output of that sub transaction.
const SUB_OUTPUT_SPENDER: &str = "ranking-spends-sub-transaction-output";

/// A store that rolls the first block and then the second, seeded with every
/// output the two spend and neither makes.
fn replay_pair(first: &[u8], second: &[u8]) -> ToyDomain {
    let first_block = MultiEraBlock::decode(first).unwrap();
    let second_block = MultiEraBlock::decode(second).unwrap();

    let made_first: HashSet<TxoRef> = produced(&first_block).into_iter().map(|(r, _)| r).collect();

    let mut seeds = external_inputs(&first_block);
    seeds.extend(
        external_inputs(&second_block)
            .into_iter()
            .filter(|(key, _)| !made_first.contains(key)),
    );

    let delta = UtxoSetDelta {
        produced_utxo: seeds,
        ..Default::default()
    };

    let domain =
        ToyDomain::new_with_genesis_and_config(genesis(), chain_config(), Some(delta), None)
            .with_sync_config(sync_config(true));

    domain.roll_forward(Arc::new(first.to_vec())).unwrap();
    domain.roll_forward(Arc::new(second.to_vec())).unwrap();

    domain
}

/// The address and lovelace of each output the block's valid transactions and
/// their sub transactions make.
#[cfg(feature = "minibf")]
fn outputs_by_ref(block: &MultiEraBlock) -> HashMap<TxoRef, (String, u64)> {
    let entry = |tx: &MultiEraTx| -> Vec<(TxoRef, (String, u64))> {
        tx.produces()
            .into_iter()
            .map(|(index, output)| {
                (
                    TxoRef(tx.hash(), index as u32),
                    (output.address().unwrap().to_string(), output.value().coin()),
                )
            })
            .collect()
    };

    let subs = sub_transactions(block)
        .into_iter()
        .flat_map(|(_, success, sub)| entry(&MultiEraTx::from_dijkstra_sub(sub, success)));

    block
        .txs()
        .iter()
        .flat_map(|tx| entry(tx))
        .chain(subs)
        .collect()
}

/// The input a `/txs/{hash}/utxos` answer lists for the output given, as
/// whether it is a reference input and its lovelace.
#[cfg(feature = "minibf")]
fn listed_input(utxos: &serde_json::Value, txo: &TxoRef) -> Option<(bool, u64)> {
    utxos["inputs"].as_array()?.iter().find_map(|x| {
        let same = x["tx_hash"].as_str()? == txo.0.to_string()
            && x["output_index"].as_u64()? == txo.1 as u64;

        let lovelace = x["amount"]
            .as_array()?
            .iter()
            .find(|a| a["unit"] == "lovelace")?["quantity"]
            .as_str()?
            .parse()
            .ok()?;

        same.then(|| (x["reference"].as_bool().unwrap_or(false), lovelace))
    })
}

/// MUST FIRE: an input made by a sub transaction of an earlier block, spent by
/// a sub transaction and referenced by its parent, is answered with that
/// output's lovelace by `/txs/{hash}/utxos` for both.
///
/// MUST NOT FIRE: the parent's input made by a top level transaction of the
/// earlier block is answered with that output's lovelace too.
#[cfg(feature = "minibf")]
#[test]
fn minibf_resolves_an_input_a_sub_transaction_made() {
    let first = block_fixture(SUB_OUTPUTS);
    let second = block_fixture(SUB_OUTPUT_SPENDER);
    let made = outputs_by_ref(&MultiEraBlock::decode(&first).unwrap());

    let block = MultiEraBlock::decode(&second).unwrap();
    let txs = block.txs();

    let spends_made = |tx: &MultiEraTx| -> Option<TxoRef> {
        tx.consumes()
            .iter()
            .map(TxoRef::from)
            .find(|txo| made.contains_key(txo))
    };

    let (parent, sub, by_sub) = txs
        .iter()
        .find_map(|tx| {
            tx.sub_transactions()
                .into_iter()
                .find_map(|sub| spends_made(&sub).map(|txo| (tx, sub, txo)))
        })
        .expect("no sub transaction spends an output the earlier block makes");

    let by_top_level =
        spends_made(parent).expect("the parent spends no output the earlier block makes");

    let domain = replay_pair(&first, &second);
    let config = dolos_core::config::MinibfConfig::new("[::]:0".parse().unwrap());
    let router = dolos_minibf::build_router(config, domain);
    let runtime = tokio::runtime::Runtime::new().unwrap();

    runtime.block_on(async {
        let (status, utxos) = get_json(&router, &format!("/txs/{}/utxos", parent.hash())).await;

        assert_eq!(
            (
                status,
                listed_input(&utxos, &by_sub),
                listed_input(&utxos, &by_top_level)
            ),
            (
                200,
                Some((true, made[&by_sub].1)),
                Some((false, made[&by_top_level].1))
            ),
            "/txs/{}/utxos",
            parent.hash()
        );

        let (status, utxos) = get_json(&router, &format!("/txs/{}/utxos", sub.hash())).await;

        assert_eq!(
            (status, listed_input(&utxos, &by_sub)),
            (200, Some((false, made[&by_sub].1))),
            "/txs/{}/utxos",
            sub.hash()
        );
    });
}

/// The block with the outputs of the sub transaction given removed.
#[cfg(feature = "minibf")]
fn without_outputs_of(cbor: &[u8], hash: Hash<32>) -> Vec<u8> {
    use pallas::codec::utils::{KeepRaw, MaybeIndefArray};

    let (era, mut block): (u16, dijkstra::Block) = minicbor::decode(cbor).unwrap();

    let (MaybeIndefArray::Def(txs) | MaybeIndefArray::Indef(txs)) =
        &mut block.block_body.transactions;

    let mut emptied = 0;

    for tx in txs.iter_mut() {
        let lists = |subs: &dijkstra::SubTransactions| subs.iter().any(|x| sub_hash(x) == hash);

        if !tx
            .transaction_body
            .sub_transactions
            .as_ref()
            .is_some_and(lists)
        {
            continue;
        }

        let mut body = (*tx.transaction_body).clone();
        let listed = body.sub_transactions.take().unwrap();
        let arm = listed.arm();

        let subs = listed
            .into_vec()
            .into_iter()
            .map(|mut sub| {
                if sub_hash(&sub) == hash {
                    let mut sub_body = (*sub.sub_transaction_body).clone();
                    sub_body.outputs = MaybeIndefArray::Def(vec![]);
                    sub.sub_transaction_body = KeepRaw::from(sub_body);
                    emptied += 1;
                }
                sub
            })
            .collect();

        body.sub_transactions = dijkstra::NonEmptySet::from_vec(subs).map(|x| x.with_arm(arm));
        tx.transaction_body = KeepRaw::from(body);
    }

    assert_eq!(
        emptied, 1,
        "the block lists the sub transaction {hash} once"
    );

    minicbor::to_vec((era, block)).unwrap()
}

/// MUST FIRE: the block route lists a sub transaction under the address of the
/// output it spends that a sub transaction of an earlier block made. The
/// spending sub transaction's outputs are removed, so that address can come
/// only from its resolved input.
///
/// MUST NOT FIRE: it lists that sub transaction under no other address.
#[cfg(feature = "minibf")]
#[test]
fn minibf_block_route_lists_a_sub_transaction_under_the_address_it_spends() {
    let first = block_fixture(SUB_OUTPUTS);
    let made = outputs_by_ref(&MultiEraBlock::decode(&first).unwrap());

    let spent = |sub: &MultiEraTx| -> BTreeSet<String> {
        sub.consumes()
            .iter()
            .filter_map(|input| made.get(&TxoRef::from(input)))
            .map(|(address, _)| address.clone())
            .collect()
    };

    let spender = |block: &MultiEraBlock| -> (Hash<32>, BTreeSet<String>, usize) {
        block
            .txs()
            .iter()
            .flat_map(|tx| tx.sub_transactions())
            .map(|sub| (sub.hash(), spent(&sub), sub.produces().len()))
            .find(|(_, from, _)| !from.is_empty())
            .expect("no sub transaction spends an output the earlier block makes")
    };

    let harvested = block_fixture(SUB_OUTPUT_SPENDER);
    let (hash, _, _) = spender(&MultiEraBlock::decode(&harvested).unwrap());

    let second = without_outputs_of(&harvested, hash);
    let block = MultiEraBlock::decode(&second).unwrap();
    let (hash, spent, outputs) = spender(&block);

    assert_eq!(
        outputs, 0,
        "the rewritten sub transaction {hash} makes outputs"
    );

    let domain = replay_pair(&first, &second);
    let config = dolos_core::config::MinibfConfig::new("[::]:0".parse().unwrap());
    let router = dolos_minibf::build_router(config, domain);
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let (status, listed) = runtime.block_on(async {
        let mut listed = BTreeSet::new();
        let mut page = 1;

        loop {
            let (status, addresses) = get_json(
                &router,
                &format!("/blocks/{}/addresses?count=100&page={page}", block.hash()),
            )
            .await;

            let addresses = match addresses.as_array() {
                Some(x) if status == 200 && !x.is_empty() => x.clone(),
                _ => return (status, listed),
            };

            for x in &addresses {
                for tx in x["transactions"].as_array().unwrap() {
                    if tx["tx_hash"].as_str() == Some(hash.to_string().as_str()) {
                        listed.insert(x["address"].as_str().unwrap().to_string());
                    }
                }
            }

            page += 1;
        }
    });

    assert_eq!(
        (status, listed),
        (200, spent),
        "/blocks/{}/addresses for the sub transaction {hash}",
        block.hash()
    );
}

/// MUST FIRE: a match on every output of a sub transaction answers the outputs
/// of it still unspent after the block that spends one of them.
///
/// MUST NOT FIRE: a match on every output of the top level transaction that
/// lists it answers the outputs of that transaction still unspent.
#[cfg(feature = "minikupo")]
#[test]
fn minikupo_matches_the_outputs_of_a_sub_transaction() {
    let first = block_fixture(SUB_OUTPUTS);
    let second = block_fixture(SUB_OUTPUT_SPENDER);

    let first_block = MultiEraBlock::decode(&first).unwrap();
    let spent: HashSet<TxoRef> = consumed(&MultiEraBlock::decode(&second).unwrap())
        .into_iter()
        .collect();

    let (index, _, sub) = sub_transactions(&first_block)
        .into_iter()
        .next()
        .expect("the earlier block lists no sub transaction");

    let unspent = |hash: Hash<32>| -> Vec<u64> {
        produced(&first_block)
            .into_iter()
            .map(|(txo, _)| txo)
            .filter(|txo| txo.0 == hash && !spent.contains(txo))
            .map(|txo| txo.1 as u64)
            .collect()
    };

    let domain = replay_pair(&first, &second);
    let config = dolos_core::config::MinikupoConfig::new("[::]:0".parse().unwrap());
    let router = dolos_minikupo::build_router(config, domain);
    let runtime = tokio::runtime::Runtime::new().unwrap();

    runtime.block_on(async {
        for hash in [sub_hash(sub), first_block.txs()[index].hash()] {
            let (status, matches) = get_json(&router, &format!("/matches/*@{hash}")).await;

            let mut indexes: Vec<u64> = matches
                .as_array()
                .map(|x| {
                    x.iter()
                        .map(|m| m["output_index"].as_u64().unwrap())
                        .collect()
                })
                .unwrap_or_default();
            indexes.sort();

            assert_eq!((status, indexes), (200, unspent(hash)), "/matches/*@{hash}");
        }
    });
}
