//! Judges the transactions Dolos serves for a ranking block against the node's endorser block.

use std::{io::Read as _, path::PathBuf, sync::Arc};

use dolos_core::SyncExt as _;
use futures_util::StreamExt as _;
use ledger_oracle_judge::compare::Tally;
use pallas::{
    codec::{
        minicbor::{self, data::Type, Decoder},
        utils::Nullable,
    },
    crypto::hash::{Hash, Hasher},
    ledger::traverse::MultiEraBlock,
    network2::protocol::Point,
};
use serde::Deserialize;
use tonic::Request;

use super::super::{
    spec::sync::{self as s, sync_service_server::SyncService as _},
    sync::SyncServiceImpl,
};
use super::serve::{Service, NEXT};
use crate::{
    prelude::CancelTokenImpl,
    sync::{
        leios::{
            tests::FakeRelay, CertifiedPayload, LeiosClient, PendingPayloads, FETCH_TIMEOUT,
            TXS_PER_REQUEST,
        },
        pull::announced_by,
    },
};

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

/// How a served list compares with the node's, index by index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Listed<T> {
    /// Both lists hold this many items, equal at every index.
    Same(usize),
    /// The first index where the lists differ, with what each holds there.
    Differs {
        index: usize,
        served: Option<T>,
        node: Option<T>,
    },
}

/// Compares a served list with the node's at every index either holds.
pub(super) fn listed<T: Clone + PartialEq>(served: &[T], node: &[T]) -> Listed<T> {
    let len = served.len().max(node.len());
    match (0..len).find(|i| served.get(*i) != node.get(*i)) {
        None => Listed::Same(len),
        Some(index) => Listed::Differs {
            index,
            served: served.get(index).cloned(),
            node: node.get(index).cloned(),
        },
    }
}

/// How the transactions one service serves for a block compare with the node's, by the ids of the mapped block and by the bytes of the native block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Judged {
    pub ids: Listed<Hash<32>>,
    pub bytes: Listed<Vec<u8>>,
}

impl Judged {
    /// The transactions both lists agree on, none when either list differs.
    pub(super) fn agreed(&self) -> Option<usize> {
        match (&self.ids, &self.bytes) {
            (Listed::Same(ids), Listed::Same(bytes)) if ids == bytes => Some(*ids),
            _ => None,
        }
    }

    /// The disagreement lines of the service at the slot, one per list that differs.
    pub(super) fn lines(&self, service: Service, slot: u64) -> Vec<String> {
        let at = format!("ENDORSER-DIFF service={service} slot={slot}");
        let mut lines = Vec::new();
        if let Listed::Differs {
            index,
            served,
            node,
        } = &self.ids
        {
            let show = |id: &Option<Hash<32>>| id.map_or("none".to_string(), |h| h.to_string());
            lines.push(format!(
                "{at} list=ids index={index} served={} node={}",
                show(served),
                show(node)
            ));
        }
        if let Listed::Differs {
            index,
            served,
            node,
        } = &self.bytes
        {
            let show = |bytes: &Option<Vec<u8>>| {
                bytes.as_ref().map_or("none".to_string(), |b| {
                    format!("{}:{}", b.len(), Hasher::<256>::hash(b))
                })
            };
            lines.push(format!(
                "{at} list=bytes index={index} served={} node={}",
                show(served),
                show(node)
            ));
        }
        if let (Listed::Same(ids), Listed::Same(bytes)) = (&self.ids, &self.bytes) {
            if ids != bytes {
                lines.push(format!("{at} ids={ids} bytes={bytes}"));
            }
        }
        lines
    }
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

/// The id of a `block_transaction`, the hash of its body bytes.
pub(super) fn tx_id(tx: &[u8]) -> Result<Hash<32>, String> {
    let mut d = Decoder::new(tx);
    let items = array_items(&mut d, tx).map_err(|e| format!("transaction: {e}"))?;
    let body = items.first().ok_or("a transaction with no body")?;
    Ok(Hasher::<256>::hash(body))
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

/// The transactions one served block holds, judged against the node's.
pub(super) fn judge_served(served: &s::AnyChainBlock, node: &[Vec<u8>]) -> Result<Judged, String> {
    let Some(s::any_chain_block::Chain::Cardano(block)) = &served.chain else {
        return Err("serves no Cardano block".into());
    };
    let ids: Vec<Hash<32>> = block
        .body
        .iter()
        .flat_map(|b| &b.tx)
        .map(|tx| {
            let common = tx
                .common
                .as_ref()
                .ok_or("a transaction with no common fields")?;
            let hash: [u8; 32] = common
                .hash
                .as_ref()
                .try_into()
                .map_err(|_| "a short hash")?;
            Ok(Hash::from(hash))
        })
        .collect::<Result<_, &str>>()?;
    let bytes = block_txs(&served.native_bytes)?;
    let node_ids = node
        .iter()
        .map(|t| tx_id(t))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Judged {
        ids: listed(&ids, &node_ids),
        bytes: listed(&bytes, node),
    })
}

/// The block Dolos's pull stage passes on for the case, its endorser block fetched through the leios-fetch client from a relay serving the node's endorser block.
async fn resolved(case: &Case) -> Result<Vec<u8>, String> {
    let parent = MultiEraBlock::decode(&case.parent).map_err(|e| e.to_string())?;
    let block = MultiEraBlock::decode(&case.block).map_err(|e| e.to_string())?;
    let announced =
        announced_by(Some(&parent.header()), &block.header()).map_err(|e| e.to_string())?;
    let mut payloads = PendingPayloads::default();
    match (announced, &case.endorser) {
        (None, None) => {}
        (Some(eb), Some(node)) => {
            payloads.expect(block.slot());
            let point = Point::Specific(node.slot, Hasher::<256>::hash(&node.body).to_vec());
            let relay = FakeRelay::new(point, node.body.clone(), node.txs.clone(), usize::MAX);
            let mut client =
                LeiosClient::with_transport(relay, "node".into(), TXS_PER_REQUEST, FETCH_TIMEOUT);
            let txs = client.fetch(&eb).await.map_err(|e| e.to_string())?;
            let payload = CertifiedPayload {
                endorser_block: eb,
                txs,
            };
            payloads.deliver(block.slot(), payload);
        }
        (announced, node) => {
            return Err(format!(
                "dolos certifies={} node stores an endorser block={}",
                announced.is_some(),
                node.is_some()
            ))
        }
    }
    let mut out = payloads
        .apply(vec![case.block.clone()])
        .map_err(|e| e.to_string())?;
    payloads.refuse_undelivered().map_err(|e| e.to_string())?;
    out.pop()
        .ok_or("the pending payloads passed on no block".into())
}

/// The block FetchBlock and FollowTip serve after Dolos applies the parent and the resolved block to an empty Musashi store.
async fn served(
    case: &Case,
    resolved: Vec<u8>,
) -> Result<[(Service, s::AnyChainBlock); 2], String> {
    let toy = crate::tests::musashi::musashi_domain();
    let parent = MultiEraBlock::decode(&case.parent).map_err(|e| e.to_string())?;
    let block = MultiEraBlock::decode(&case.block).map_err(|e| e.to_string())?;
    for raw in [case.parent.clone(), resolved] {
        toy.roll_forward(Arc::new(raw))
            .map_err(|e| format!("roll forward: {e}"))?;
    }
    let domain = crate::tests::toy_adapter(&toy);
    let service = SyncServiceImpl::new(domain.clone(), CancelTokenImpl::default());
    let fetched = service
        .fetch_block(Request::new(s::FetchBlockRequest {
            r#ref: Some(s::BlockRef {
                hash: block.hash().to_vec().into(),
                ..Default::default()
            }),
            field_mask: None,
        }))
        .await
        .map_err(|e| format!("FetchBlock: {e}"))?
        .into_inner()
        .block
        .ok_or("FetchBlock serves no block")?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut follow = SyncServiceImpl::new(domain.clone(), CancelTokenImpl(cancel.clone()))
        .follow_tip(Request::new(s::FollowTipRequest {
            intersect: vec![s::BlockRef {
                slot: parent.slot(),
                hash: parent.hash().to_vec().into(),
                ..Default::default()
            }],
            field_mask: None,
        }))
        .await
        .map_err(|e| format!("FollowTip: {e}"))?
        .into_inner();
    let followed = loop {
        let Ok(Some(next)) = tokio::time::timeout(NEXT, follow.next()).await else {
            break Err("FollowTip serves no block".to_string());
        };
        match next.map_err(|e| e.to_string())?.action {
            Some(s::follow_tip_response::Action::Reset(_)) => {}
            Some(s::follow_tip_response::Action::Apply(block)) => break Ok(block),
            other => break Err(format!("FollowTip serves {other:?}")),
        }
    };
    cancel.cancel();
    Ok([
        (Service::FetchBlock, fetched),
        (Service::FollowTip, followed?),
    ])
}

/// The tally of the endorser transaction judge over cases, its lines, and whether every served block agreed.
#[derive(Debug, Default)]
pub(super) struct EndorserTally {
    tally: Tally,
    txs: usize,
    certifying: usize,
}

impl EndorserTally {
    /// Counts one served block, keeping its lines when it disagrees.
    pub(super) fn add(&mut self, judged: Result<Judged, String>, service: Service, slot: u64) {
        match judged {
            Ok(judged) => {
                let lines = judged.lines(service, slot);
                self.txs += judged.agreed().unwrap_or(0);
                self.tally
                    .add(judged.agreed().is_some(), || lines.join("\n"));
            }
            Err(e) => self.tally.add(false, || {
                format!("ENDORSER-DIFF service={service} slot={slot} unjudged={e}")
            }),
        }
    }

    /// The tally line and the disagreement lines.
    pub(super) fn lines(&self) -> Vec<String> {
        let Tally { compared, disagree } = &self.tally;
        let mut lines = vec![format!(
            "ENDORSER compared={compared} agree={} disagree={} certifying={} txs={}",
            compared - disagree.len(),
            disagree.len(),
            self.certifying,
            self.txs
        )];
        lines.extend(disagree.iter().cloned());
        lines
    }

    /// Whether every served block agreed and the cases judged served blocks of each kind.
    pub(super) fn pass(&self, cases: usize) -> bool {
        let served = self.tally.compared == 2 * cases;
        let kinds = self.certifying > 0 && self.certifying < cases;
        served && kinds && self.tally.disagree.is_empty()
    }
}

/// Judges what FetchBlock and FollowTip serve for each case against the node's transactions.
pub(super) async fn judge(cases: &[Case]) -> EndorserTally {
    let mut tally = EndorserTally::default();
    for case in cases {
        let slot = MultiEraBlock::decode(&case.block).map_or(0, |b| b.slot());
        tally.certifying += usize::from(case.endorser.is_some());
        let node = node_txs(&case.parent, &case.block, case.endorser.as_ref());
        let served = match resolved(case).await {
            Ok(block) => served(case, block).await,
            Err(e) => Err(format!("resolve: {e}")),
        };
        for service in [Service::FetchBlock, Service::FollowTip] {
            let judged = match (&node, &served) {
                (Err(e), _) => Err(format!("node: {e}")),
                (_, Err(e)) => Err(e.clone()),
                (Ok(node), Ok(served)) => {
                    let block = &served.iter().find(|(s, _)| *s == service).unwrap().1;
                    judge_served(block, node)
                }
            };
            tally.add(judged, service, slot);
        }
    }
    tally
}

#[tokio::test(flavor = "multi_thread")]
async fn served_endorser_transactions_are_the_nodes() {
    let cases = load(&fixture_path());
    let tally = judge(&cases).await;
    for line in tally.lines() {
        println!("{line}");
    }
    assert!(tally.pass(cases.len()));
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

#[test]
#[ignore = "judges the FetchBlock answers FIXTURE_DIR/block-<slot>.json holds against the cases of FIXTURE_DIR/cases.jsonl"]
fn endpoint_endorser() {
    let dir = PathBuf::from(std::env::var("FIXTURE_DIR").expect("FIXTURE_DIR"));
    let text = std::fs::read_to_string(dir.join("cases.jsonl")).unwrap();
    let mut tally = EndorserTally::default();
    let mut cases = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let case: Case = serde_json::from_str(line).unwrap();
        let slot = MultiEraBlock::decode(&case.block).unwrap().slot();
        cases += 1;
        tally.certifying += usize::from(case.endorser.is_some());
        let path = dir.join(format!("block-{slot}.json"));
        let read = std::fs::read_to_string(&path).map_err(|e| format!("{} {e}", path.display()));
        let judged = read.and_then(|text| {
            let response: s::FetchBlockResponse =
                serde_json::from_str(&text).map_err(|e| e.to_string())?;
            let served = response.block.ok_or("FetchBlock serves no block")?;
            let node = node_txs(&case.parent, &case.block, case.endorser.as_ref())?;
            judge_served(&served, &node)
        });
        tally.add(judged, Service::FetchBlock, slot);
    }
    for line in tally.lines() {
        println!("{line}");
    }
    assert_eq!(tally.tally.compared, cases);
    assert!(tally.tally.disagree.is_empty());
}

#[test]
fn equal_lists_are_the_same() {
    let list = [1, 2, 3];
    assert_eq!(listed(&list, &list), Listed::Same(3));
    assert_eq!(listed::<u8>(&[], &[]), Listed::Same(0));
}

#[test]
fn a_dropped_item_differs_at_its_index() {
    assert_eq!(
        listed(&[1, 3], &[1, 2, 3]),
        Listed::Differs {
            index: 1,
            served: Some(3),
            node: Some(2)
        }
    );
    assert_eq!(
        listed(&[1, 2], &[1, 2, 3]),
        Listed::Differs {
            index: 2,
            served: None,
            node: Some(3)
        }
    );
}

#[test]
fn a_swap_and_a_duplicate_differ() {
    assert_eq!(
        listed(&[2, 1], &[1, 2]),
        Listed::Differs {
            index: 0,
            served: Some(2),
            node: Some(1)
        }
    );
    assert_eq!(
        listed(&[1, 2, 2], &[1, 2]),
        Listed::Differs {
            index: 2,
            served: Some(2),
            node: None
        }
    );
}

#[test]
fn a_block_agrees_only_when_both_lists_are_the_same() {
    let same = Judged {
        ids: Listed::Same(2),
        bytes: Listed::Same(2),
    };
    assert_eq!(same.agreed(), Some(2));
    assert!(same.lines(Service::FetchBlock, 7).is_empty());
    let bytes_differ = Judged {
        ids: Listed::Same(2),
        bytes: Listed::Differs {
            index: 1,
            served: Some(vec![1]),
            node: Some(vec![2]),
        },
    };
    assert_eq!(bytes_differ.agreed(), None);
    let lines = bytes_differ.lines(Service::FollowTip, 7);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].starts_with("ENDORSER-DIFF service=FollowTip slot=7 list=bytes index=1 "));
}
