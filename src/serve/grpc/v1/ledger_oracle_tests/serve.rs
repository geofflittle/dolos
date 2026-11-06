use std::{collections::BTreeMap, fmt, time::Duration};

use dolos_core::{BlockHash, ChainPoint};
use futures_util::StreamExt as _;
use ledger_oracle_judge::cbor::TxIn;
use tokio_util::sync::CancellationToken;
use tonic::Request;

use super::super::{
    query::QueryServiceImpl,
    spec::{
        cardano,
        query::{self as q, query_service_server::QueryService as _},
        sync::{self as s, sync_service_server::SyncService as _},
        watch::{self as w, watch_service_server::WatchService as _},
    },
    sync::SyncServiceImpl,
    watch::WatchServiceImpl,
};
use crate::{adapters::DomainAdapter, prelude::CancelTokenImpl};

/// How long a stream read waits for its next response.
pub(super) const NEXT: Duration = Duration::from_secs(10);

/// A service that maps a transaction with its resolved inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Service {
    FetchBlock,
    FollowTip,
    WatchTx,
    ReadTx,
}

impl fmt::Display for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Service::FetchBlock => "FetchBlock",
            Service::FollowTip => "FollowTip",
            Service::WatchTx => "WatchTx",
            Service::ReadTx => "ReadTx",
        };
        f.write_str(name)
    }
}

/// The output one service serves for each input a transaction names, by transaction hash.
pub(super) type Served = BTreeMap<[u8; 32], Vec<(TxIn, Option<cardano::TxOutput>)>>;

fn hash32(bytes: &[u8]) -> [u8; 32] {
    bytes.try_into().expect("a served hash is 32 bytes")
}

fn served_input(input: &cardano::TxInput) -> (TxIn, Option<cardano::TxOutput>) {
    let txin = TxIn {
        tx: hash32(&input.tx_hash),
        ix: input.output_index.into(),
    };
    (txin, input.as_output.clone())
}

/// Adds the inputs a served transaction names, with the output served for each.
pub(super) fn add_tx(served: &mut Served, tx: &cardano::Tx) {
    let common = tx
        .common
        .as_ref()
        .expect("a served transaction has common fields");
    let collateral = tx.collateral.iter().flat_map(|c| &c.collateral);
    let subs = tx
        .sub_transactions
        .iter()
        .flat_map(|s| s.inputs.iter().chain(&s.reference_inputs));
    let inputs = common
        .inputs
        .iter()
        .chain(collateral)
        .chain(&common.reference_inputs)
        .chain(subs)
        .map(served_input)
        .collect();
    served.insert(hash32(&common.hash), inputs);
}

/// Adds the inputs each transaction of a served block names, with the output served for each.
pub(super) fn add_block(served: &mut Served, block: &s::AnyChainBlock) {
    let Some(s::any_chain_block::Chain::Cardano(block)) = &block.chain else {
        panic!("a served block is a Cardano block");
    };
    for tx in block.body.iter().flat_map(|b| &b.tx) {
        add_tx(served, tx);
    }
}

fn block_ref(point: &ChainPoint) -> s::BlockRef {
    s::BlockRef {
        slot: point.slot(),
        hash: point.hash().map(|h| h.to_vec()).unwrap_or_default().into(),
        ..Default::default()
    }
}

/// The inputs FetchBlock serves for the transactions of each block.
pub(super) async fn fetch_block(domain: &DomainAdapter, blocks: &[BlockHash]) -> Served {
    let service = SyncServiceImpl::new(domain.clone(), CancelTokenImpl::default());
    let mut served = Served::new();
    for hash in blocks {
        let request = s::FetchBlockRequest {
            r#ref: Some(s::BlockRef {
                hash: hash.to_vec().into(),
                ..Default::default()
            }),
            field_mask: None,
        };
        let response = service.fetch_block(Request::new(request)).await;
        let block = response.unwrap().into_inner().block;
        add_block(
            &mut served,
            &block.expect("FetchBlock serves a block it holds"),
        );
    }
    served
}

/// The inputs ReadTx serves for each transaction hash it finds.
pub(super) async fn read_tx(domain: &DomainAdapter, txs: &[[u8; 32]]) -> Served {
    let service = QueryServiceImpl::new(domain.clone());
    let mut served = Served::new();
    for hash in txs {
        let request = q::ReadTxRequest {
            hash: hash.to_vec().into(),
            ..Default::default()
        };
        let Ok(response) = service.read_tx(Request::new(request)).await else {
            continue;
        };
        let tx = response.into_inner().tx.and_then(|tx| tx.chain);
        if let Some(q::any_chain_tx::Chain::Cardano(tx)) = tx {
            add_tx(&mut served, &tx);
        }
    }
    served
}

/// Which tip event a stream read takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Event {
    Apply,
    Undo,
}

type FollowStream =
    <SyncServiceImpl<DomainAdapter, CancelTokenImpl> as s::sync_service_server::SyncService>::FollowTipStream;
type WatchStream =
    <WatchServiceImpl<DomainAdapter, CancelTokenImpl> as w::watch_service_server::WatchService>::WatchTxStream;

/// A FollowTip stream and a WatchTx stream subscribed at one point.
pub(super) struct Streams {
    follow: FollowStream,
    watch: WatchStream,
    cancel: CancellationToken,
}

impl Streams {
    /// Subscribes both streams at the point.
    pub(super) async fn open(domain: &DomainAdapter, from: &ChainPoint) -> Self {
        let cancel = CancellationToken::new();
        let token = || CancelTokenImpl(cancel.clone());
        let from = block_ref(from);
        let follow = SyncServiceImpl::new(domain.clone(), token())
            .follow_tip(Request::new(s::FollowTipRequest {
                intersect: vec![from.clone()],
                field_mask: None,
            }))
            .await
            .unwrap()
            .into_inner();
        let watch = WatchServiceImpl::new(domain.clone(), token())
            .watch_tx(Request::new(w::WatchTxRequest {
                intersect: vec![w::BlockRef {
                    slot: from.slot,
                    hash: from.hash,
                    height: 0,
                }],
                predicate: None,
                field_mask: None,
            }))
            .await
            .unwrap()
            .into_inner();
        Streams {
            follow,
            watch,
            cancel,
        }
    }

    /// The inputs each stream serves for the next blocks and transactions of the event, reading until the given counts arrive or a read waits too long.
    pub(super) async fn read(
        mut self,
        event: Event,
        blocks: usize,
        txs: usize,
    ) -> [(Service, Served); 2] {
        use s::follow_tip_response::Action as F;
        use w::watch_tx_response::Action as W;
        let mut follow = Served::new();
        let mut taken = 0;
        while taken < blocks {
            let Ok(Some(next)) = tokio::time::timeout(NEXT, self.follow.next()).await else {
                break;
            };
            match (event, next.unwrap().action) {
                (_, Some(F::Reset(_))) => {}
                (Event::Apply, Some(F::Apply(block))) | (Event::Undo, Some(F::Undo(block))) => {
                    add_block(&mut follow, &block);
                    taken += 1;
                }
                (_, other) => panic!("FollowTip serves {other:?} where it reads {event:?}"),
            }
        }
        let mut watch = Served::new();
        let mut taken = 0;
        while taken < txs {
            let Ok(Some(next)) = tokio::time::timeout(NEXT, self.watch.next()).await else {
                break;
            };
            match (event, next.unwrap().action) {
                (Event::Apply, Some(W::Apply(tx))) | (Event::Undo, Some(W::Undo(tx))) => {
                    let Some(w::any_chain_tx::Chain::Cardano(tx)) = &tx.chain else {
                        panic!("a watched transaction is a Cardano transaction");
                    };
                    add_tx(&mut watch, tx);
                    taken += 1;
                }
                (_, other) => panic!("WatchTx serves {other:?} where it reads {event:?}"),
            }
        }
        self.cancel.cancel();
        [(Service::FollowTip, follow), (Service::WatchTx, watch)]
    }
}
