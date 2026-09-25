//! Reads every fixture cut from the Musashi prototype chain and checks each one
//! against what its provenance entry says it is.
//!
//! A fixture whose bytes no longer hash to the block hash the node reported is
//! not the block it claims to be, so every check here is a comparison against a
//! recorded value rather than a parse that only has to succeed. The directory
//! also records the fixture kinds that were looked for and not found, and those
//! entries are checked too, because a kind that silently vanished from the list
//! would otherwise read as a kind that was never wanted.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use pallas::crypto::hash::Hash;
use pallas::ledger::primitives::dijkstra::EbAnnouncement;
use pallas::ledger::traverse::cert::BlsKeySlot;
use pallas::ledger::traverse::leios::EndorserBlockBody;
use pallas::ledger::traverse::MultiEraBlock;
use serde::Deserialize;

const DIR: &str = "test_data/musashi-w36";
const PROVENANCE: &str = "provenance.toml";
const CHAIN_TAG: &str = "prototype-2026w36";
const NETWORK_MAGIC: u64 = 164;

/// Epoch 56 begins here, at 56 times the shelley genesis epochLength of 21600.
const EPOCH_56_FIRST_SLOT: u64 = 1_209_600;

/// Every fixture kind the directory is expected to hold. A kind in this list
/// with no entry fails, and an entry whose kind is not in this list fails, so
/// neither a dropped fixture nor an unannounced one passes.
const KINDS: &[&str] = &[
    "ranking_block_announcing_with_transactions",
    "ranking_block_announcing_without_transactions",
    "pool_registration_with_bls_key",
    "epoch_boundary_last_block_of_epoch_55",
    "epoch_boundary_first_block_of_epoch_56",
    "era_header_variant_6",
    "era_header_variant_7",
    "ranking_block_with_sub_transaction",
    "ranking_block_with_sub_transaction_of_two_outputs",
    "ranking_block_spending_a_sub_transaction_output",
    "endorser_block_large",
    "endorser_block_small",
    "endorser_block_repeat_first",
    "endorser_block_repeat_second",
];

/// Kinds that were searched for and not found on this chain.
const ABSENT_KINDS: &[&str] = &[
    "ranking_block_pair_repeating_a_transaction",
    "block_needing_lenient_apply",
    "protocol_version_12_transition_block",
];

#[derive(Deserialize)]
struct Provenance {
    fixture: Vec<Fixture>,
    absent: Vec<Absent>,
}

#[derive(Deserialize)]
struct Fixture {
    name: String,
    kind: String,
    files: Vec<String>,
    chain_tag: String,
    network_magic: u64,
    slot: u64,
    block_hash: Option<String>,
    endorser_hash: Option<String>,
    transactions: usize,
    bytes: usize,
}

#[derive(Deserialize)]
struct Absent {
    kind: String,
    predicate: String,
    scanned: String,
    blocks_examined: u64,
    found: u64,
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

fn read_bytes(name: &str) -> Vec<u8> {
    let path = dir().join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is unreadable: {e}", path.display()));
    hex::decode(text.trim()).unwrap_or_else(|e| panic!("{} is not hex: {e}", path.display()))
}

fn read_wire_txs(name: &str) -> Vec<Vec<u8>> {
    let path = dir().join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is unreadable: {e}", path.display()));
    text.split_whitespace()
        .map(|line| {
            hex::decode(line).unwrap_or_else(|e| panic!("{} holds a non hex line: {e}", name))
        })
        .collect()
}

fn by_kind(p: &Provenance) -> BTreeMap<&str, &Fixture> {
    let mut out = BTreeMap::new();
    for f in &p.fixture {
        assert!(
            out.insert(f.kind.as_str(), f).is_none(),
            "two entries claim the kind {}",
            f.kind
        );
    }
    out
}

fn hash32(kind: &str, name: &str, value: &Option<String>) -> Hash<32> {
    value
        .as_deref()
        .unwrap_or_else(|| panic!("{name} is a {kind} and records no hash"))
        .parse()
        .unwrap_or_else(|e| panic!("{name} records a hash that does not parse: {e}"))
}

#[test]
fn every_file_has_an_entry_and_every_entry_has_its_files() {
    let p = provenance();
    assert!(!p.fixture.is_empty(), "the provenance names no fixture");

    let mut claimed: BTreeSet<String> = BTreeSet::new();
    for f in &p.fixture {
        assert!(!f.files.is_empty(), "{} names no file", f.name);
        for file in &f.files {
            assert!(
                claimed.insert(file.clone()),
                "two entries claim the file {file}"
            );
            let path = dir().join(file);
            let len = std::fs::metadata(&path)
                .unwrap_or_else(|e| panic!("{} is missing: {e}", path.display()))
                .len();
            assert!(len > 0, "{} is empty", path.display());
        }
    }

    let mut on_disk: BTreeSet<String> = BTreeSet::new();
    for entry in std::fs::read_dir(dir()).unwrap_or_else(|e| panic!("{DIR} is unreadable: {e}")) {
        let name = entry.expect("a directory entry reads").file_name();
        let name = name.to_str().expect("a fixture name is utf8").to_string();
        if name != PROVENANCE {
            on_disk.insert(name);
        }
    }

    assert!(!on_disk.is_empty(), "the fixture directory holds no fixture");
    assert_eq!(
        claimed, on_disk,
        "the files on disk and the files the provenance names differ"
    );
}

#[test]
fn every_entry_carries_the_pinned_chain() {
    let p = provenance();
    assert!(!p.fixture.is_empty(), "the provenance names no fixture");

    for f in &p.fixture {
        assert_eq!(f.chain_tag, CHAIN_TAG, "{} carries another chain tag", f.name);
        assert_eq!(
            f.network_magic, NETWORK_MAGIC,
            "{} carries another network magic",
            f.name
        );
        assert!(f.slot > 0, "{} records slot zero", f.name);
        assert!(f.bytes > 0, "{} records zero bytes", f.name);
    }
}

#[test]
fn the_kinds_present_are_exactly_the_kinds_expected() {
    let p = provenance();
    let present: BTreeSet<&str> = p.fixture.iter().map(|f| f.kind.as_str()).collect();
    let expected: BTreeSet<&str> = KINDS.iter().copied().collect();
    assert_eq!(present, expected, "the kinds present are not the kinds expected");
}

#[test]
fn every_ranking_block_is_the_block_its_entry_names() {
    let p = provenance();
    let mut checked = 0;

    for f in &p.fixture {
        let Some(file) = f.files.iter().find(|n| n.ends_with(".block")) else {
            continue;
        };
        assert_eq!(f.files.len(), 1, "{} names more than one block file", f.name);

        let raw = read_bytes(file);
        assert_eq!(raw.len(), f.bytes, "{} is not the length its entry names", f.name);

        let block = MultiEraBlock::decode(&raw)
            .unwrap_or_else(|e| panic!("{} does not decode as a block: {e}", f.name));
        assert_eq!(
            block.header().hash(),
            hash32("block", &f.name, &f.block_hash),
            "{} does not hash to the hash the node reported",
            f.name
        );
        assert_eq!(block.slot(), f.slot, "{} is not at the slot its entry names", f.name);
        assert_eq!(
            block.tx_count(),
            f.transactions,
            "{} does not carry the transaction count its entry names",
            f.name
        );
        assert!(
            f.endorser_hash.is_none(),
            "{} is a ranking block and records an endorser hash",
            f.name
        );
        checked += 1;
    }

    assert_eq!(checked, 10, "the number of ranking block fixtures changed");
}

#[test]
fn every_endorser_block_verifies_against_its_announcement() {
    let p = provenance();
    let mut checked = 0;

    for f in &p.fixture {
        let Some(body_file) = f.files.iter().find(|n| n.ends_with(".ebbody")) else {
            continue;
        };
        let txs_file = f
            .files
            .iter()
            .find(|n| n.ends_with(".ebtxs"))
            .unwrap_or_else(|| panic!("{} names a body and no transactions", f.name));

        let body_bytes = read_bytes(body_file);
        assert_eq!(
            body_bytes.len(),
            f.bytes,
            "{} is not the length its entry names",
            f.name
        );

        let announcement = EbAnnouncement {
            eb_hash: hash32("endorser block", &f.name, &f.endorser_hash),
            eb_size: f.bytes as u32,
        };
        let body = EndorserBlockBody::decode_announced(&body_bytes, &announcement)
            .unwrap_or_else(|e| panic!("{} does not verify against its announcement: {e}", f.name));
        assert_eq!(
            body.len(),
            f.transactions,
            "{} does not commit to the transaction count its entry names",
            f.name
        );

        let wire = read_wire_txs(txs_file);
        let txs = body
            .transactions(&wire)
            .unwrap_or_else(|e| panic!("{} transactions do not verify: {e}", f.name));
        assert_eq!(
            txs.len(),
            f.transactions,
            "{} serves a different number of transactions than it commits to",
            f.name
        );
        assert!(
            f.block_hash.is_none(),
            "{} is an endorser block and records a block hash",
            f.name
        );
        checked += 1;
    }

    assert_eq!(checked, 4, "the number of endorser block fixtures changed");
}

#[test]
fn each_fixture_shows_the_shape_it_was_cut_for() {
    let p = provenance();
    let k = by_kind(&p);

    let with_txs = k["ranking_block_announcing_with_transactions"];
    let raw = read_bytes(&with_txs.files[0]);
    let block = MultiEraBlock::decode(&raw).expect("the announcing block decodes");
    assert!(
        block.header().eb_announcement().is_some(),
        "the announcing fixture carries no announcement"
    );
    assert!(
        block.tx_count() > 0,
        "the announcing fixture with transactions carries none"
    );

    let quiet = k["ranking_block_announcing_without_transactions"];
    let raw = read_bytes(&quiet.files[0]);
    let block = MultiEraBlock::decode(&raw).expect("the quiet block decodes");
    assert!(
        block.header().eb_announcement().is_some(),
        "the quiet announcing fixture carries no announcement"
    );
    assert_eq!(
        block.tx_count(),
        0,
        "the quiet announcing fixture carries transactions"
    );

    let pool = k["pool_registration_with_bls_key"];
    let raw = read_bytes(&pool.files[0]);
    let block = MultiEraBlock::decode(&raw).expect("the pool registration block decodes");
    let keys = block
        .txs()
        .iter()
        .flat_map(|tx| tx.certs())
        .filter(|cert| matches!(cert.bls_key(), BlsKeySlot::Key(_)))
        .count();
    assert!(
        keys > 0,
        "the pool registration fixture holds no certificate whose key slot holds a key"
    );

    let before = k["epoch_boundary_last_block_of_epoch_55"];
    let after = k["epoch_boundary_first_block_of_epoch_56"];
    assert!(
        before.slot < EPOCH_56_FIRST_SLOT,
        "the last block of epoch 55 is not below the boundary"
    );
    assert!(
        after.slot >= EPOCH_56_FIRST_SLOT,
        "the first block of epoch 56 is not at or above the boundary"
    );

    let old = k["era_header_variant_6"];
    let new = k["era_header_variant_7"];
    let old_era = MultiEraBlock::decode(&read_bytes(&old.files[0]))
        .expect("the earlier era block decodes")
        .era();
    let new_era = MultiEraBlock::decode(&read_bytes(&new.files[0]))
        .expect("the later era block decodes")
        .era();
    assert_ne!(
        old_era, new_era,
        "the era variant pair does not straddle an era change"
    );
    assert!(
        old.slot < new.slot,
        "the era variant pair is recorded out of order"
    );

    let one_output = k["ranking_block_with_sub_transaction"];
    let two_outputs = k["ranking_block_with_sub_transaction_of_two_outputs"];
    for (f, subs, outputs) in [(one_output, 1usize, 1usize), (two_outputs, 1, 2)] {
        let raw = read_bytes(&f.files[0]);
        let block = MultiEraBlock::decode(&raw).expect("the sub transaction block decodes");
        let txs = block.txs();
        let listed = txs
            .iter()
            .filter(|tx| tx.as_dijkstra_sub().is_some())
            .count();
        let carried: Vec<_> = txs.iter().flat_map(|tx| tx.sub_transactions()).collect();

        assert_eq!(
            (
                carried.len(),
                listed,
                carried.iter().map(|tx| tx.produces().len()).sum::<usize>(),
            ),
            (subs, 0, outputs),
            "{} does not carry the sub transaction shape it was cut for",
            f.name
        );
    }

    let made_by_sub: BTreeSet<(Hash<32>, u64)> =
        MultiEraBlock::decode(&read_bytes(&two_outputs.files[0]))
            .expect("the sub transaction block decodes")
            .txs()
            .iter()
            .flat_map(|tx| tx.sub_transactions())
            .flat_map(|sub| {
                let hash = sub.hash();
                sub.produces()
                    .into_iter()
                    .map(|(index, _)| (hash, index as u64))
                    .collect::<Vec<_>>()
            })
            .collect();
    let spender = k["ranking_block_spending_a_sub_transaction_output"];
    let raw = read_bytes(&spender.files[0]);
    let block = MultiEraBlock::decode(&raw).expect("the spending block decodes");
    let txs = block.txs();
    let names_one = |input: &pallas::ledger::traverse::MultiEraInput| {
        made_by_sub.contains(&(*input.hash(), input.index()))
    };
    let spent = txs
        .iter()
        .flat_map(|tx| tx.sub_transactions())
        .map(|sub| {
            sub.consumes()
                .iter()
                .filter(|input| names_one(input))
                .count()
        })
        .sum::<usize>();
    let referenced = txs
        .iter()
        .flat_map(|tx| tx.reference_inputs())
        .filter(|input| names_one(input))
        .count();
    assert_eq!(
        (spent, referenced),
        (1, 1),
        "{} does not spend and reference one sub transaction output",
        spender.name
    );

    let large = k["endorser_block_large"];
    let small = k["endorser_block_small"];
    assert!(
        large.transactions > small.transactions,
        "the large endorser block does not carry more transactions than the small one"
    );

    let first = k["endorser_block_repeat_first"];
    let second = k["endorser_block_repeat_second"];
    let first_txs: BTreeSet<Vec<u8>> = read_wire_txs(
        first
            .files
            .iter()
            .find(|n| n.ends_with(".ebtxs"))
            .expect("the first of the repeat pair names its transactions"),
    )
    .into_iter()
    .collect();
    let second_txs: BTreeSet<Vec<u8>> = read_wire_txs(
        second
            .files
            .iter()
            .find(|n| n.ends_with(".ebtxs"))
            .expect("the second of the repeat pair names its transactions"),
    )
    .into_iter()
    .collect();
    let shared = first_txs.intersection(&second_txs).count();
    assert!(
        shared >= 15,
        "the repeat pair shares {shared} transactions, fewer than the 15 recorded"
    );
}

#[test]
fn every_absent_kind_names_what_was_looked_at() {
    let p = provenance();
    let listed: BTreeSet<&str> = p.absent.iter().map(|a| a.kind.as_str()).collect();
    let expected: BTreeSet<&str> = ABSENT_KINDS.iter().copied().collect();
    assert_eq!(
        listed, expected,
        "the absent kinds recorded are not the absent kinds expected"
    );

    for a in &p.absent {
        assert!(!a.predicate.is_empty(), "{} names no predicate", a.kind);
        assert!(!a.scanned.is_empty(), "{} names no scanned range", a.kind);
        assert!(
            a.blocks_examined > 0,
            "{} reports a search over no blocks, which says nothing about the chain",
            a.kind
        );
        assert_eq!(
            a.found, 0,
            "{} is recorded as absent and reports matches",
            a.kind
        );
    }
}
