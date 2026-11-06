//! Judges the transactions Dolos serves for a ranking block against the node's endorser block.

use std::{io::Read as _, path::PathBuf};

use pallas::{
    codec::{
        minicbor::{self, data::Type, Decoder},
        utils::Nullable,
    },
    crypto::hash::{Hash, Hasher},
    ledger::traverse::MultiEraBlock,
};
use serde::Deserialize;

/// The endorser block the node stores, as the slot of the block that announced it, its body and its transactions in body order.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct NodeEndorser {
    pub slot: u64,
    #[serde(deserialize_with = "hex_bytes")]
    pub body: Vec<u8>,
    #[serde(deserialize_with = "hex_list")]
    pub txs: Vec<Vec<u8>>,
}

/// One fixture case, a ranking block and its parent as the node serves them with the endorser block the node stores for it.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct Case {
    #[serde(deserialize_with = "hex_bytes")]
    pub parent: Vec<u8>,
    #[serde(deserialize_with = "hex_bytes")]
    pub block: Vec<u8>,
    pub endorser: Option<NodeEndorser>,
}

fn hex_bytes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    let text = String::deserialize(d)?;
    hex::decode(text).map_err(serde::de::Error::custom)
}

fn hex_list<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Vec<u8>>, D::Error> {
    let texts = Vec::<String>::deserialize(d)?;
    texts
        .iter()
        .map(|t| hex::decode(t).map_err(serde::de::Error::custom))
        .collect()
}

/// The cases of a gzipped file of one JSON case per line.
pub(super) fn load(path: &std::path::Path) -> Vec<Case> {
    let gz = std::fs::read(path).unwrap_or_else(|e| panic!("{} {e}", path.display()));
    let mut text = String::new();
    flate2::read::GzDecoder::new(gz.as_slice())
        .read_to_string(&mut text)
        .unwrap_or_else(|e| panic!("{} {e}", path.display()));
    let lines = text.lines().filter(|l| !l.trim().is_empty());
    lines
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{} {e}", path.display())))
        .collect()
}

/// The committed fixture of the endorser transaction judge.
pub(super) fn fixture_path() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "test_data", "ledger-oracle"]
        .iter()
        .collect::<PathBuf>()
        .join("endorser.jsonl.gz")
}

/// The byte spans of the items of the CBOR array that starts at the decoder's position.
fn array_items<'b>(
    d: &mut Decoder<'b>,
    cbor: &'b [u8],
) -> Result<Vec<&'b [u8]>, minicbor::decode::Error> {
    let len = d.array()?;
    let mut items = Vec::new();
    loop {
        match len {
            Some(len) if items.len() as u64 == len => break,
            None if d.datatype()? == Type::Break => break,
            _ => {}
        }
        let start = d.position();
        d.skip()?;
        items.push(&cbor[start..d.position()]);
    }
    Ok(items)
}

/// The bytes of each transaction of a Dijkstra block, `[era, [header, [transactions, leios_certificate, peras_certificate]]]`.
pub(super) fn block_txs(block: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    fn walk(block: &[u8]) -> Result<Vec<Vec<u8>>, minicbor::decode::Error> {
        let mut d = Decoder::new(block);
        d.array()?;
        d.u16()?;
        d.array()?;
        d.skip()?;
        d.array()?;
        let txs = array_items(&mut d, block)?;
        Ok(txs.into_iter().map(<[u8]>::to_vec).collect())
    }
    walk(block).map_err(|e| format!("block transactions: {e}"))
}

/// The hashes an endorser block body names, in body order.
fn body_hashes(body: &[u8]) -> Result<Vec<Hash<32>>, String> {
    let mut d = Decoder::new(body);
    let walk = |d: &mut Decoder| -> Result<Vec<Hash<32>>, minicbor::decode::Error> {
        let len = d.map()?;
        let mut hashes = Vec::new();
        loop {
            match len {
                Some(len) if hashes.len() as u64 == len => break,
                None if d.datatype()? == Type::Break => break,
                _ => {}
            }
            let hash: [u8; 32] = d
                .bytes()?
                .try_into()
                .map_err(|_| minicbor::decode::Error::message("a hash of other than 32 bytes"))?;
            d.skip()?;
            hashes.push(Hash::from(hash));
        }
        Ok(hashes)
    };
    walk(&mut d).map_err(|e| format!("endorser block body: {e}"))
}

/// The node's transactions for a ranking block, its endorser block's when its body holds a certificate and its own otherwise, each checked against the node's commitments.
pub(super) fn node_txs(
    parent: &[u8],
    block: &[u8],
    endorser: Option<&NodeEndorser>,
) -> Result<Vec<Vec<u8>>, String> {
    let decode = |cbor| MultiEraBlock::decode(cbor).map_err(|e| format!("node block: {e}"));
    let (parent, decoded) = (decode(parent)?, decode(block)?);
    if decoded.header().previous_hash() != Some(parent.hash()) {
        return Err(format!(
            "node block names parent {:?}",
            decoded.header().previous_hash()
        ));
    }
    let body = &decoded
        .as_dijkstra()
        .ok_or("not a Dijkstra block")?
        .block_body;
    let certifies = matches!(body.leios_certificate, Nullable::Some(_));
    match (certifies, endorser) {
        (false, None) => block_txs(block),
        (true, Some(endorser)) => {
            let header = parent.header();
            let announced = header
                .eb_announcement()
                .ok_or("the parent announces nothing")?;
            if Hasher::<256>::hash(&endorser.body) != announced.eb_hash {
                return Err(format!("the stored body is not {}", announced.eb_hash));
            }
            let named = body_hashes(&endorser.body)?;
            let stored: Vec<_> = endorser
                .txs
                .iter()
                .map(|t| Hasher::<256>::hash(t))
                .collect();
            if named != stored {
                return Err("the stored transactions are not the ones the body names".into());
            }
            Ok(endorser.txs.clone())
        }
        (certifies, endorser) => Err(format!(
            "certifies={certifies} stored endorser block={}",
            endorser.is_some()
        )),
    }
}

#[test]
fn fixture_cases_hold_the_nodes_commitments() {
    let cases = load(&fixture_path());
    for case in &cases {
        let slot = case.endorser.as_ref().map(|e| e.slot);
        node_txs(&case.parent, &case.block, case.endorser.as_ref())
            .unwrap_or_else(|e| panic!("announced at {slot:?} {e}"));
    }
    let certifying = cases.iter().filter(|c| c.endorser.is_some()).count();
    assert!(certifying > 0 && certifying < cases.len());
}

#[tokio::test]
#[ignore = "fetches the blocks FIXTURE_DIR/points names from the peer FIXTURE_DIR/peer names"]
async fn fetch_node_blocks() {
    let dir = PathBuf::from(std::env::var("FIXTURE_DIR").expect("FIXTURE_DIR"));
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap();
    let peer = read("peer");
    let (address, magic) = peer.trim().split_once(' ').expect("an address and a magic");
    let mut client = pallas::network::facades::PeerClient::connect(address, magic.parse().unwrap())
        .await
        .unwrap();
    for line in read("points").lines().filter(|l| !l.trim().is_empty()) {
        let (slot, hash) = line.trim().split_once(' ').expect("a slot and a hash");
        let point = pallas::network::miniprotocols::Point::Specific(
            slot.parse().unwrap(),
            hex::decode(hash).unwrap(),
        );
        let block = client.blockfetch().fetch_single(point).await.unwrap();
        let decoded = MultiEraBlock::decode(&block).unwrap();
        assert_eq!(
            decoded.hash().to_string(),
            hash,
            "the peer serves another block"
        );
        std::fs::write(dir.join(format!("node-{slot}.cbor")), &block).unwrap();
        println!("FETCHED slot={slot} bytes={}", block.len());
    }
}
