use std::{path::PathBuf, sync::Arc};

use dolos_core::{
    config::{CardanoConfig, SyncConfig},
    sync::SyncExt as _,
};
use dolos_testing::toy_domain::ToyDomain;
use pallas::crypto::hash::{Hash, Hasher};
use pallas::ledger::traverse::MultiEraBlock;

/// A store holding a harvested Musashi block one of whose transactions lists a
/// sub transaction, with the hashes, index and bytes the lookups answer.
pub(crate) struct SubTransactionBlock {
    pub domain: ToyDomain,
    pub slot: u64,
    pub parent: Hash<32>,
    pub parent_index: usize,
    pub sub: Hash<32>,
    pub sub_bytes: Vec<u8>,
}

fn manifest_path(parts: &[&str]) -> PathBuf {
    parts
        .iter()
        .fold(PathBuf::from(env!("CARGO_MANIFEST_DIR")), |path, part| {
            path.join(part)
        })
}

pub(crate) fn sub_transaction_block() -> SubTransactionBlock {
    let text = std::fs::read_to_string(manifest_path(&[
        "test_data",
        "musashi-w36",
        "ranking-sub-transaction.block",
    ]))
    .unwrap();
    let cbor = hex::decode(text.trim()).unwrap();

    let mut genesis = dolos_cardano::include::preview::load();
    genesis.dijkstra = Some(
        dolos_core::dijkstra::from_file(manifest_path(&[
            "crates",
            "core",
            "test_data",
            "musashi",
            "dijkstra-genesis.json",
        ]))
        .unwrap(),
    );
    genesis.force_protocol = Some(11);

    let config = CardanoConfig {
        magic: 164,
        is_testnet: true,
        stop_epoch: None,
        custom_utxos: vec![],
    };

    let mut sync = SyncConfig::default();
    sync.leios_lenient_apply = true;

    let domain = ToyDomain::new_with_genesis_and_config(Arc::new(genesis), config, None, None)
        .with_sync_config(sync);

    domain.roll_forward(Arc::new(cbor.clone())).unwrap();

    let block = MultiEraBlock::decode(&cbor).unwrap();
    let body = block.as_dijkstra().unwrap();

    let (parent_index, parent, sub) = body
        .block_body
        .transactions
        .iter()
        .enumerate()
        .find_map(|(index, tx)| {
            let sub = tx.transaction_body.sub_transactions.as_ref()?.first()?;
            Some((index, tx, sub))
        })
        .unwrap();

    SubTransactionBlock {
        domain,
        slot: block.slot(),
        parent: Hasher::<256>::hash(parent.transaction_body.raw_cbor()),
        parent_index,
        sub: Hasher::<256>::hash(sub.sub_transaction_body.raw_cbor()),
        sub_bytes: pallas::codec::minicbor::to_vec(sub).unwrap(),
    }
}
