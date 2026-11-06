use std::{path::PathBuf, sync::Arc};

use dolos_core::{
    config::{CardanoConfig, SyncConfig},
    sync::SyncExt as _,
};
use dolos_testing::toy_domain::ToyDomain;
use pallas::crypto::hash::{Hash, Hasher};
use pallas::ledger::traverse::MultiEraBlock;

/// A store holding a harvested Musashi block one of whose transactions lists a
/// sub transaction, with the hashes and bytes the lookups answer.
pub(crate) struct SubTransactionBlock {
    pub domain: ToyDomain,
    pub slot: u64,
    pub parent: Hash<32>,
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

    let domain = musashi_domain();

    domain.roll_forward(Arc::new(cbor.clone())).unwrap();

    let block = MultiEraBlock::decode(&cbor).unwrap();
    let body = block.as_dijkstra().unwrap();

    let (parent, sub) = body
        .block_body
        .transactions
        .iter()
        .find_map(|tx| {
            let sub = tx.transaction_body.sub_transactions.as_ref()?.first()?;
            Some((tx, sub))
        })
        .unwrap();

    SubTransactionBlock {
        domain,
        slot: block.slot(),
        parent: Hasher::<256>::hash(parent.transaction_body.raw_cbor()),
        sub: Hasher::<256>::hash(sub.sub_transaction_body.raw_cbor()),
        sub_bytes: pallas::codec::minicbor::to_vec(sub).unwrap(),
    }
}

/// An empty store with the Musashi network magic and the Dijkstra genesis,
/// applying endorsed transactions leniently.
pub(crate) fn musashi_domain() -> ToyDomain {
    let mut genesis = dolos_cardano::include::preview::load();
    genesis.dijkstra = Some(
        dolos_core::dijkstra::from_file(manifest_path(&[
            "crates",
            "core",
            "test_data",
            "proto-devnet",
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

    ToyDomain::new_with_genesis_and_config(Arc::new(genesis), config, None, None)
        .with_sync_config(sync)
}

/// Synthetic serving fixture derived from the captured ranking block. Only its
/// batch is retained, parent reference inputs are removed, and the producer
/// verdict is selectable. The original capture is never rewritten. Signatures
/// and header commitments are not valid for this wrapper; this tests mapping,
/// not ledger acceptance. The resolved outputs below are synthetic too.
pub(crate) fn child_only_inputs(
    success: bool,
) -> (dolos_core::RawBlock, pallas::interop::utxorpc::UtxoMap) {
    use pallas::{
        codec::minicbor,
        ledger::primitives::{KeepRaw, MaybeIndefArray},
    };

    let raw = hex::decode(
        include_str!("../../test_data/musashi-w36/ranking-sub-transaction.block").trim(),
    )
    .unwrap();
    let block = MultiEraBlock::decode(&raw).unwrap();
    let mut native = block.as_dijkstra().unwrap().clone();
    let mut parent = native.block_body.transactions.last().unwrap().clone();
    let mut body = parent.transaction_body.unwrap();
    assert!(body.sub_transactions.is_some());
    body.reference_inputs = None;
    parent.transaction_body = KeepRaw::from(body);
    parent.success = success;
    native.block_body.transactions = MaybeIndefArray::Def(vec![parent]);
    let raw = minicbor::to_vec((8u16, native)).unwrap();
    let block = MultiEraBlock::decode(&raw).unwrap();
    let mut outputs = pallas::interop::utxorpc::UtxoMap::new();
    for (_, tx) in dolos_core::applied_txs(&block) {
        let marker = if tx.as_dijkstra_sub().is_some() {
            42
        } else {
            41
        };
        let mut address = vec![0x60];
        address.extend([marker; 28]);
        // A synthetic output with an address and asset unique to child inputs.
        let mut e = minicbor::Encoder::new(Vec::new());
        e.map(2)
            .unwrap()
            .u8(0)
            .unwrap()
            .bytes(&address)
            .unwrap()
            .u8(1)
            .unwrap()
            .array(2)
            .unwrap()
            .u64(9_000_000)
            .unwrap()
            .map(1)
            .unwrap()
            .bytes(&[marker; 28])
            .unwrap()
            .map(1)
            .unwrap()
            .bytes(b"child-input")
            .unwrap()
            .u64(7)
            .unwrap();
        let output = e.into_writer();
        for input in tx
            .inputs()
            .into_iter()
            .chain(tx.collateral())
            .chain(tx.reference_inputs())
        {
            outputs.insert(
                (*input.hash(), input.index() as u32),
                (block.era(), output.clone()),
            );
        }
    }
    (Arc::new(raw), outputs)
}
