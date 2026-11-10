use std::{
    collections::{BTreeMap, BTreeSet},
    convert::Infallible,
    time::Duration,
};

use dolos_core::{BlockHash, ChainPoint};
use ledger_oracle_judge::{
    cbor::TxIn,
    compare::{OutputRoute, UtxoRoute},
    judgement::Version,
};

use super::output::ServedOutput;

/// How long a stream read waits for its next response.
pub(super) const NEXT: Duration = Duration::from_secs(10);

/// The output one route serves for each input a transaction names, by transaction hash.
pub(super) type Served = BTreeMap<[u8; 32], Vec<(TxIn, Option<ServedOutput>)>>;

/// What one route at one version serves for the inputs of the transactions it serves.
#[derive(Debug)]
pub(super) struct RouteServed {
    pub(super) route: OutputRoute,
    pub(super) version: Version,
    /// Whether the version states the inputs only sub transactions name.
    pub(super) subs: bool,
    pub(super) served: Served,
}

/// What ReadTx at one version serves for a transaction hash, nothing when it answers not found, or the status it answers with otherwise.
pub(super) struct TxFound {
    pub(super) version: Version,
    /// Whether the version states sub transactions, so answers the hash of one with the transaction that lists it.
    pub(super) subs: bool,
    pub(super) answer: Result<Option<super::ServedTx>, tonic::Code>,
}

/// What ReadTx at one version answers for a transaction hash, with `P` holding a panic of the version's mapping.
#[derive(Debug)]
pub(super) enum ReadTxAnswer<T, P> {
    /// The message the version answers with.
    Answered(T),
    /// The status the version answers in place of a message.
    Status(tonic::Status),
    /// A panic of the version's mapping.
    Panicked(P),
}

impl<T> ReadTxAnswer<T, String> {
    /// The answer, or the message of its panic.
    fn unpanicked(self) -> Result<ReadTxAnswer<T, Infallible>, String> {
        match self {
            ReadTxAnswer::Answered(answer) => Ok(ReadTxAnswer::Answered(answer)),
            ReadTxAnswer::Status(status) => Ok(ReadTxAnswer::Status(status)),
            ReadTxAnswer::Panicked(message) => Err(message),
        }
    }
}

/// What ReadTx at one version answers for each transaction hash it was asked.
pub(super) type Answers<T, P> = BTreeMap<[u8; 32], ReadTxAnswer<T, P>>;

/// What ReadTx answers at every version for each transaction hash a run judges, with `P` holding a panic of a version's mapping.
#[derive(Debug)]
pub(super) struct ReadTxs<P> {
    pub(super) v1: Answers<v1::Response, P>,
    pub(super) v1beta: Answers<v1beta::Response, P>,
    pub(super) v1alpha: Answers<v1alpha::Response, P>,
}

/// The answers of one version when its mapping panicked for no hash.
fn unpanicked<T>(answers: Answers<T, String>) -> Option<Answers<T, Infallible>> {
    let unpanicked = answers
        .into_iter()
        .map(|(hash, answer)| Some((hash, answer.unpanicked().ok()?)));
    unpanicked.collect()
}

impl ReadTxs<String> {
    /// What ReadTx answers at every version for each of the hashes.
    pub(super) async fn read(
        domain: &crate::adapters::DomainAdapter,
        hashes: &BTreeSet<[u8; 32]>,
    ) -> Self {
        ReadTxs {
            v1: v1::read_txs(domain, hashes).await,
            v1beta: v1beta::read_txs(domain, hashes).await,
            v1alpha: v1alpha::read_txs(domain, hashes).await,
        }
    }

    /// The answers when no version's mapping panicked for any hash.
    pub(super) fn unpanicked(self) -> Option<ReadTxs<Infallible>> {
        Some(ReadTxs {
            v1: unpanicked(self.v1)?,
            v1beta: unpanicked(self.v1beta)?,
            v1alpha: unpanicked(self.v1alpha)?,
        })
    }
}

impl ReadTxs<Infallible> {
    /// What ReadTx serves at every version for a transaction hash.
    pub(super) fn found(&self, hash: &[u8; 32]) -> [TxFound; 3] {
        [
            v1::read_tx_found(&self.v1, hash),
            v1beta::read_tx_found(&self.v1beta, hash),
            v1alpha::read_tx_found(&self.v1alpha, hash),
        ]
    }
}

/// The bytes of one output a route serves, with the fields it parses from them.
#[derive(Debug)]
pub(super) struct Utxo {
    pub(super) bytes: Vec<u8>,
    pub(super) parsed: Option<ServedOutput>,
}

/// The output a route serves for each input it holds.
pub(super) type Utxos = BTreeMap<TxIn, Utxo>;

/// The parameters message of the latest version.
pub(super) type LatestParams = super::super::spec::cardano::PParams;

/// Which tip event a stream read takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Event {
    Apply,
    Undo,
}

fn hash32(bytes: &[u8]) -> [u8; 32] {
    bytes.try_into().expect("a served hash is 32 bytes")
}

/// Generates the served routes of one version over its own messages.
macro_rules! served_routes {
    (
        $version:ident,
        $name:ident,
        subs: $subs:literal,
        inputs: |$tx:ident| $inputs:expr,
        fetch_ref: |$r:ident| $fetch_ref:expr,
        fetched: |$f:ident| $fetched:expr,
        exact_address: |$a:ident| $exact_address:expr $(,)?
    ) => {
        pub(super) mod $version {
            use std::{collections::BTreeSet, convert::Infallible, panic::AssertUnwindSafe};

            use dolos_core::{BlockHash, ChainPoint};
            use futures_util::{FutureExt as _, StreamExt as _};
            use ledger_oracle_judge::cbor::TxIn;
            use tokio_util::sync::CancellationToken;
            use tonic::Request;

            use super::super::validate::panic_message;
            use super::{hash32, Event, LatestParams, RouteServed, Served, Utxo, Utxos, NEXT};
            use super::{Answers, OutputRoute, ReadTxAnswer, UtxoRoute, Version};
            use crate::serve::grpc::$version::{
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

            /// The version these routes answer at.
            pub(in super::super) const VERSION: Version = Version::$name;

            fn route(route: OutputRoute, served: Served) -> RouteServed {
                RouteServed {
                    route,
                    version: VERSION,
                    subs: $subs,
                    served,
                }
            }

            fn served_input(
                input: &cardano::TxInput,
            ) -> (TxIn, Option<super::super::output::ServedOutput>) {
                let txin = TxIn {
                    tx: hash32(&input.tx_hash),
                    ix: input.output_index.into(),
                };
                let output = input.as_output.as_ref();
                (txin, output.map(super::super::output::$version::output))
            }

            /// Adds the inputs a served transaction names, with the output served for each.
            pub(in super::super) fn add_tx(served: &mut Served, $tx: &cardano::Tx) {
                let (hash, inputs): (&[u8], Vec<&cardano::TxInput>) = $inputs;
                served.insert(hash32(hash), inputs.into_iter().map(served_input).collect());
            }

            /// Adds the inputs each transaction of a served block names, with the output served for each.
            pub(in super::super) fn add_block(served: &mut Served, block: &s::AnyChainBlock) {
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
            pub(in super::super) async fn fetch_block(
                domain: &DomainAdapter,
                blocks: &[BlockHash],
            ) -> RouteServed {
                let service = SyncServiceImpl::new(domain.clone(), CancelTokenImpl::default());
                let mut served = Served::new();
                for hash in blocks {
                    let $r = s::BlockRef {
                        hash: hash.to_vec().into(),
                        ..Default::default()
                    };
                    let request = s::FetchBlockRequest {
                        r#ref: $fetch_ref,
                        field_mask: None,
                    };
                    let response = service.fetch_block(Request::new(request)).await;
                    let $f = response.unwrap().into_inner();
                    let fetched: Vec<s::AnyChainBlock> = $fetched;
                    assert_eq!(fetched.len(), 1, "FetchBlock serves one block a ref names");
                    add_block(&mut served, &fetched[0]);
                }
                route(OutputRoute::FetchBlock, served)
            }

            /// The inputs DumpHistory serves for the transactions of each block, read one block a page from it.
            pub(in super::super) async fn dump_history(
                domain: &DomainAdapter,
                blocks: &[BlockHash],
            ) -> RouteServed {
                let service = SyncServiceImpl::new(domain.clone(), CancelTokenImpl::default());
                let mut served = Served::new();
                for hash in blocks {
                    let request = s::DumpHistoryRequest {
                        start_token: Some(s::BlockRef {
                            hash: hash.to_vec().into(),
                            ..Default::default()
                        }),
                        max_items: 1,
                        field_mask: None,
                    };
                    let response = service.dump_history(Request::new(request)).await;
                    let page = response.unwrap().into_inner().block;
                    assert_eq!(page.len(), 1, "DumpHistory serves the page a token starts");
                    add_block(&mut served, &page[0]);
                }
                route(OutputRoute::DumpHistory, served)
            }

            /// The message ReadTx answers with at this version.
            pub(in super::super) type Response = q::ReadTxResponse;

            /// What ReadTx answers at this version for each of the hashes, with a panic of its mapping caught.
            pub(in super::super) async fn read_txs(
                domain: &DomainAdapter,
                hashes: &BTreeSet<[u8; 32]>,
            ) -> Answers<Response, String> {
                let mut answers = Answers::new();
                for hash in hashes {
                    let request = q::ReadTxRequest {
                        hash: hash.to_vec().into(),
                        ..Default::default()
                    };
                    let service = QueryServiceImpl::new(domain.clone());
                    let answer = AssertUnwindSafe(service.read_tx(Request::new(request)))
                        .catch_unwind()
                        .await;
                    let answer = match answer {
                        Err(payload) => {
                            ReadTxAnswer::Panicked(panic_message(payload).replace('\n', " "))
                        }
                        Ok(Err(status)) => ReadTxAnswer::Status(status),
                        Ok(Ok(response)) => ReadTxAnswer::Answered(response.into_inner()),
                    };
                    answers.insert(*hash, answer);
                }
                answers
            }

            /// The inputs ReadTx serves for each transaction hash it finds.
            pub(in super::super) fn read_tx(
                answers: &Answers<Response, Infallible>,
                txs: &[[u8; 32]],
            ) -> RouteServed {
                let mut served = Served::new();
                for hash in txs {
                    let answer = answers.get(hash).expect("ReadTx was read for every rolled transaction");
                    let response = match answer {
                        ReadTxAnswer::Answered(response) => response,
                        ReadTxAnswer::Status(_) => continue,
                        ReadTxAnswer::Panicked(never) => match *never {},
                    };
                    let tx = response.tx.as_ref().and_then(|tx| tx.chain.as_ref());
                    if let Some(q::any_chain_tx::Chain::Cardano(tx)) = tx {
                        add_tx(&mut served, tx);
                    }
                }
                route(OutputRoute::ReadTx, served)
            }

            /// What ReadTx serves for a transaction hash.
            pub(in super::super) fn read_tx_found(
                answers: &Answers<Response, Infallible>,
                hash: &[u8; 32],
            ) -> super::TxFound {
                let found = |answer| super::TxFound {
                    version: VERSION,
                    subs: $subs,
                    answer,
                };
                let answer = answers.get(hash).expect("ReadTx was read for every judged transaction");
                let tx = match answer {
                    ReadTxAnswer::Answered(response) => {
                        response.tx.as_ref().expect("ReadTx answers with a transaction")
                    }
                    ReadTxAnswer::Status(status) if status.code() == tonic::Code::NotFound => {
                        return found(Ok(None))
                    }
                    ReadTxAnswer::Status(status) => return found(Err(status.code())),
                    ReadTxAnswer::Panicked(never) => match *never {},
                };
                let block = tx.block_ref.as_ref().expect("ReadTx names the block");
                let served = super::super::ServedTx {
                    block: super::super::BlockAt {
                        slot: block.slot,
                        hash: hash32(&block.hash),
                    },
                    bytes: tx.native_bytes.to_vec(),
                };
                found(Ok(Some(served)))
            }

            /// The output ReadUtxos serves for each input it holds.
            pub(in super::super) async fn read_utxos(
                domain: &DomainAdapter,
                inputs: &[&TxIn],
            ) -> ((UtxoRoute, Version), Utxos) {
                let keys = inputs.iter().map(|i| q::TxoRef {
                    hash: i.tx.to_vec().into(),
                    index: super::super::seed::txo_ref(i).1,
                });
                let request = q::ReadUtxosRequest {
                    keys: keys.collect(),
                    field_mask: None,
                };
                let service = QueryServiceImpl::new(domain.clone());
                let response = service.read_utxos(Request::new(request)).await;
                let items = response.unwrap().into_inner().items;
                ((UtxoRoute::ReadUtxos, VERSION), utxos(items))
            }

            /// The output SearchUtxos serves for each input it finds at any of the addresses, one search an address.
            pub(in super::super) async fn search_utxos(
                domain: &DomainAdapter,
                addresses: &[&[u8]],
            ) -> ((UtxoRoute, Version), Utxos) {
                let service = QueryServiceImpl::new(domain.clone());
                let mut served = Utxos::new();
                for $a in addresses {
                    let address = cardano::AddressPattern {
                        exact_address: $exact_address,
                        ..Default::default()
                    };
                    let pattern = cardano::TxOutputPattern {
                        address: Some(address),
                        asset: None,
                    };
                    let pattern = q::any_utxo_pattern::UtxoPattern::Cardano(pattern);
                    let predicate = q::UtxoPredicate {
                        r#match: Some(q::AnyUtxoPattern {
                            utxo_pattern: Some(pattern),
                        }),
                        ..Default::default()
                    };
                    let request = q::SearchUtxosRequest {
                        predicate: Some(predicate),
                        ..Default::default()
                    };
                    let response = service.search_utxos(Request::new(request)).await;
                    served.extend(utxos(response.unwrap().into_inner().items));
                }
                ((UtxoRoute::SearchUtxos, VERSION), served)
            }

            /// The served output of each item, by the input it names.
            fn utxos(items: Vec<q::AnyUtxoData>) -> Utxos {
                let served = items.into_iter().map(|item| {
                    let r = item.txo_ref.expect("a served output names its input");
                    let input = TxIn {
                        tx: hash32(&r.hash),
                        ix: r.index.into(),
                    };
                    let parsed = item.parsed_state.map(|q::any_utxo_data::ParsedState::Cardano(o)| {
                        super::super::output::$version::output(&o)
                    });
                    let utxo = Utxo {
                        bytes: item.native_bytes.to_vec(),
                        parsed,
                    };
                    (input, utxo)
                });
                served.collect()
            }

            /// The parameters ReadParams serves, read as the latest version's message.
            pub(in super::super) async fn read_params(domain: &DomainAdapter) -> (Version, LatestParams) {
                let request = q::ReadParamsRequest { field_mask: None };
                let service = QueryServiceImpl::new(domain.clone());
                let response = service.read_params(Request::new(request)).await;
                let values = response.unwrap().into_inner().values;
                match values.and_then(|v| v.params) {
                    Some(q::any_chain_params::Params::Cardano(p)) => {
                        (VERSION, super::super::params::as_latest(&p))
                    }
                    None => panic!("ReadParams answers with Cardano parameters"),
                }
            }

            type FollowStream = <SyncServiceImpl<DomainAdapter, CancelTokenImpl> as s::sync_service_server::SyncService>::FollowTipStream;
            type WatchStream = <WatchServiceImpl<DomainAdapter, CancelTokenImpl> as w::watch_service_server::WatchService>::WatchTxStream;

            /// A FollowTip stream and a WatchTx stream subscribed at one point.
            pub(in super::super) struct Streams {
                follow: FollowStream,
                watch: WatchStream,
                cancel: CancellationToken,
            }

            impl Streams {
                /// Subscribes both streams at the point.
                pub(in super::super) async fn open(domain: &DomainAdapter, from: &ChainPoint) -> Self {
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
                pub(in super::super) async fn read(
                    mut self,
                    event: Event,
                    blocks: usize,
                    txs: usize,
                ) -> [RouteServed; 2] {
                    use s::follow_tip_response::Action as F;
                    use w::watch_tx_response::Action as W;
                    let mut follow = Served::new();
                    let mut taken = 0;
                    while taken < blocks {
                        let Ok(Some(next)) = tokio::time::timeout(NEXT, self.follow.next()).await
                        else {
                            println!("STREAM short version={VERSION} route=follow_tip event={event:?} wanted={blocks} got={taken}");
                            break;
                        };
                        match (event, next.unwrap().action) {
                            (_, Some(F::Reset(_))) => {}
                            (Event::Apply, Some(F::Apply(block)))
                            | (Event::Undo, Some(F::Undo(block))) => {
                                add_block(&mut follow, &block);
                                taken += 1;
                            }
                            (_, other) => {
                                panic!("FollowTip serves {other:?} where it reads {event:?}")
                            }
                        }
                    }
                    let mut watch = Served::new();
                    let mut taken = 0;
                    while taken < txs {
                        let Ok(Some(next)) = tokio::time::timeout(NEXT, self.watch.next()).await
                        else {
                            println!("STREAM short version={VERSION} route=watch_tx event={event:?} wanted={txs} got={taken}");
                            break;
                        };
                        match (event, next.unwrap().action) {
                            (_, Some(W::Idle(_))) => {}
                            (Event::Apply, Some(W::Apply(tx))) | (Event::Undo, Some(W::Undo(tx))) => {
                                let Some(w::any_chain_tx::Chain::Cardano(tx)) = &tx.chain else {
                                    panic!("a watched transaction is a Cardano transaction");
                                };
                                add_tx(&mut watch, tx);
                                taken += 1;
                            }
                            (_, other) => {
                                panic!("WatchTx serves {other:?} where it reads {event:?}")
                            }
                        }
                    }
                    self.cancel.cancel();
                    [route(OutputRoute::FollowTip, follow), route(OutputRoute::WatchTx, watch)]
                }
            }
        }
    };
}

served_routes!(
    v1,
    V1,
    subs: true,
    inputs: |tx| {
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
            .chain(subs);
        (&common.hash, inputs.collect())
    },
    fetch_ref: |r| Some(r),
    fetched: |f| f.block.into_iter().collect(),
    exact_address: |a| Some(a.to_vec().into()),
);

served_routes!(
    v1beta,
    V1beta,
    subs: false,
    inputs: |tx| {
        let collateral = tx.collateral.iter().flat_map(|c| &c.collateral);
        let inputs = tx.inputs.iter().chain(collateral).chain(&tx.reference_inputs);
        (&tx.hash, inputs.collect())
    },
    fetch_ref: |r| vec![r],
    fetched: |f| f.block,
    exact_address: |a| Some(a.to_vec().into()),
);

served_routes!(
    v1alpha,
    V1alpha,
    subs: false,
    inputs: |tx| {
        let collateral = tx.collateral.iter().flat_map(|c| &c.collateral);
        let inputs = tx.inputs.iter().chain(collateral).chain(&tx.reference_inputs);
        (&tx.hash, inputs.collect())
    },
    fetch_ref: |r| vec![r],
    fetched: |f| f.block,
    exact_address: |a| a.to_vec().into(),
);

/// Streams of FollowTip and WatchTx at every version, subscribed at one point.
pub(super) struct Streams([Box<dyn StreamsAt>; 3]);

/// The streams of one version, read once.
#[async_trait::async_trait]
trait StreamsAt: Send {
    async fn read(self: Box<Self>, event: Event, blocks: usize, txs: usize) -> [RouteServed; 2];
}

macro_rules! streams_at {
    ($($version:ident),*) => {
        $(
            #[async_trait::async_trait]
            impl StreamsAt for $version::Streams {
                async fn read(
                    self: Box<Self>,
                    event: Event,
                    blocks: usize,
                    txs: usize,
                ) -> [RouteServed; 2] {
                    $version::Streams::read(*self, event, blocks, txs).await
                }
            }
        )*
    };
}

streams_at!(v1, v1beta, v1alpha);

impl Streams {
    /// Subscribes the streams of every version at the point.
    pub(super) async fn open(domain: &crate::adapters::DomainAdapter, from: &ChainPoint) -> Self {
        Streams([
            Box::new(v1::Streams::open(domain, from).await),
            Box::new(v1beta::Streams::open(domain, from).await),
            Box::new(v1alpha::Streams::open(domain, from).await),
        ])
    }

    /// The inputs every stream serves for the next blocks and transactions of the event.
    pub(super) async fn read(self, event: Event, blocks: usize, txs: usize) -> Vec<RouteServed> {
        let mut served = Vec::new();
        for streams in self.0 {
            served.extend(streams.read(event, blocks, txs).await);
        }
        served
    }
}

/// The output ReadUtxos serves at every version for each input it holds.
pub(super) async fn read_utxos(
    domain: &crate::adapters::DomainAdapter,
    inputs: &[&TxIn],
) -> [((UtxoRoute, Version), Utxos); 3] {
    [
        v1::read_utxos(domain, inputs).await,
        v1beta::read_utxos(domain, inputs).await,
        v1alpha::read_utxos(domain, inputs).await,
    ]
}

/// The output SearchUtxos serves at every version for each input it finds at any of the addresses.
pub(super) async fn search_utxos(
    domain: &crate::adapters::DomainAdapter,
    addresses: &[&[u8]],
) -> [((UtxoRoute, Version), Utxos); 3] {
    [
        v1::search_utxos(domain, addresses).await,
        v1beta::search_utxos(domain, addresses).await,
        v1alpha::search_utxos(domain, addresses).await,
    ]
}

/// The parameters ReadParams serves at every version.
pub(super) async fn read_params(
    domain: &crate::adapters::DomainAdapter,
) -> [(Version, LatestParams); 3] {
    [
        v1::read_params(domain).await,
        v1beta::read_params(domain).await,
        v1alpha::read_params(domain).await,
    ]
}

/// What FetchBlock, DumpHistory and ReadTx serve at every version for the blocks and transactions.
pub(super) async fn fetched(
    domain: &crate::adapters::DomainAdapter,
    blocks: &[BlockHash],
    read: &ReadTxs<Infallible>,
    txs: &[[u8; 32]],
) -> Vec<RouteServed> {
    vec![
        v1::fetch_block(domain, blocks).await,
        v1beta::fetch_block(domain, blocks).await,
        v1alpha::fetch_block(domain, blocks).await,
        v1::dump_history(domain, blocks).await,
        v1beta::dump_history(domain, blocks).await,
        v1alpha::dump_history(domain, blocks).await,
        v1::read_tx(&read.v1, txs),
        v1beta::read_tx(&read.v1beta, txs),
        v1alpha::read_tx(&read.v1alpha, txs),
    ]
}

#[test]
fn answers_with_no_panic_are_kept_whole() {
    let answers: Answers<u8, String> = BTreeMap::from([
        ([1; 32], ReadTxAnswer::Answered(7)),
        ([2; 32], ReadTxAnswer::Status(tonic::Status::not_found("none"))),
    ]);
    let kept = unpanicked(answers).expect("no answer panicked");
    assert!(matches!(kept[&[1; 32]], ReadTxAnswer::Answered(7)));
    let status = match &kept[&[2; 32]] {
        ReadTxAnswer::Status(status) => status.code(),
        other => panic!("{other:?}"),
    };
    assert_eq!(status, tonic::Code::NotFound);
    assert_eq!(kept.len(), 2);
}

#[test]
fn one_panicked_answer_leaves_no_answers() {
    let answers: Answers<u8, String> = BTreeMap::from([
        ([1; 32], ReadTxAnswer::Answered(7)),
        ([2; 32], ReadTxAnswer::Panicked("mapping".to_string())),
        ([3; 32], ReadTxAnswer::Status(tonic::Status::not_found("none"))),
    ]);
    assert!(unpanicked(answers).is_none());
}
