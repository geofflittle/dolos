//! What SubmitTx, WaitForTx and WatchMempool answer at each version for the
//! transactions one group submits.

use futures_util::{FutureExt as _, Stream, StreamExt as _};
use ledger_oracle_judge::{
    compare::{ByEntry, Tally},
    judgement::{Axis, CellKey, EntryPoint, Finding, Route, Version},
    record::RecordId,
    verdict::DolosVerdict,
};

use crate::adapters::DomainAdapter;

/// The stage the spec names for a transaction the mempool holds.
const MEMPOOL: &str = "STAGE_MEMPOOL";

/// The stage the spec names for a transaction the mempool does not know.
const UNSPECIFIED: &str = "STAGE_UNSPECIFIED";

/// One transaction a WatchMempool stream serves, by its reference, its bytes
/// and the name of its stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Watched {
    pub(super) r#ref: Vec<u8>,
    pub(super) bytes: Vec<u8>,
    pub(super) stage: String,
}

/// Every item the stream holds ready now, in the order it serves them, read
/// outside the task's cooperative budget.
fn ready_now<S: Stream + Unpin>(stream: &mut S) -> Vec<S::Item> {
    let mut ready = Vec::new();
    while let Some(Some(next)) = tokio::task::unconstrained(stream.next()).now_or_never() {
        ready.push(next);
    }
    ready
}

/// Generates the submit routes of one version over its own messages.
macro_rules! submit_routes {
    ($version:ident, $name:ident) => {
        pub(super) mod $version {
            use tonic::Request;

            use super::Watched;
            use crate::adapters::DomainAdapter;
            use crate::serve::grpc::$version::{
                spec::submit::{self as s, submit_service_server::SubmitService},
                submit::SubmitServiceImpl,
            };

            /// The version these routes answer at.
            pub(in super::super) const VERSION: super::Version = super::Version::$name;

            /// The name the spec gives a stage.
            fn stage(stage: i32) -> String {
                let named = s::Stage::try_from(stage).map(|s| s.as_str_name());
                named.map_or_else(|_| format!("STAGE_{stage}"), str::to_string)
            }

            /// The reference SubmitTx answers for the transaction, or the message of
            /// its refusal.
            pub(in super::super) async fn submit_tx(
                domain: &DomainAdapter,
                tx: &[u8],
            ) -> Result<Vec<u8>, String> {
                let request = s::SubmitTxRequest {
                    tx: Some(s::AnyChainTx {
                        r#type: Some(s::any_chain_tx::Type::Raw(tx.to_vec().into())),
                    }),
                };
                let service = SubmitServiceImpl::new(domain.clone());
                match service.submit_tx(Request::new(request)).await {
                    Ok(response) => Ok(response.into_inner().r#ref.to_vec()),
                    Err(status) => Err(status.message().to_string()),
                }
            }

            /// The reference and stage name of the first answer WaitForTx serves for
            /// the hash.
            pub(in super::super) async fn wait_for_tx(
                domain: &DomainAdapter,
                hash: &[u8; 32],
            ) -> Option<(Vec<u8>, String)> {
                let request = s::WaitForTxRequest {
                    r#ref: vec![hash.to_vec().into()],
                };
                let service = SubmitServiceImpl::new(domain.clone());
                let mut stream = service
                    .wait_for_tx(Request::new(request))
                    .await
                    .unwrap()
                    .into_inner();
                let first = super::ready_now(&mut stream).into_iter().next()?;
                let first = first.unwrap();
                Some((first.r#ref.to_vec(), stage(first.stage)))
            }

            type Stream = <SubmitServiceImpl<DomainAdapter> as SubmitService>::WatchMempoolStream;

            /// A WatchMempool stream at this version.
            pub(in super::super) struct Watching(Stream);

            impl Watching {
                /// Subscribes to the mempool of the domain.
                pub(in super::super) async fn open(domain: &DomainAdapter) -> Self {
                    let service = SubmitServiceImpl::new(domain.clone());
                    let request = s::WatchMempoolRequest::default();
                    let stream = service.watch_mempool(Request::new(request)).await;
                    Watching(stream.unwrap().into_inner())
                }

                /// Every transaction the stream holds ready, in the order it serves
                /// them.
                pub(in super::super) fn ready(&mut self) -> Vec<Watched> {
                    let ready = super::ready_now(&mut self.0).into_iter();
                    let watched = ready.map(|next| {
                        let tx = next.unwrap().tx.expect("WatchMempool serves a transaction");
                        Watched {
                            r#ref: tx.r#ref.to_vec(),
                            bytes: tx.native_bytes.to_vec(),
                            stage: stage(tx.stage),
                        }
                    });
                    watched.collect()
                }
            }
        }
    };
}

submit_routes!(v1, V1);
submit_routes!(v1beta, V1beta);
submit_routes!(v1alpha, V1alpha);

/// The WatchMempool streams of every version over one group's mempool.
pub(super) struct Watching {
    v1: v1::Watching,
    v1beta: v1beta::Watching,
    v1alpha: v1alpha::Watching,
}

impl Watching {
    /// Subscribes a stream at every version to the mempool of the domain.
    pub(super) async fn open(domain: &DomainAdapter) -> Self {
        Watching {
            v1: v1::Watching::open(domain).await,
            v1beta: v1beta::Watching::open(domain).await,
            v1alpha: v1alpha::Watching::open(domain).await,
        }
    }

    /// The transactions each stream holds ready, by its entry point.
    fn ready(&mut self) -> [(EntryPoint, Vec<Watched>); 3] {
        let at = |version| EntryPoint::Route(Route::WatchMempool, version);
        [
            (at(v1::VERSION), self.v1.ready()),
            (at(v1beta::VERSION), self.v1beta.ready()),
            (at(v1alpha::VERSION), self.v1alpha.ready()),
        ]
    }
}

/// The version a record's transaction is submitted at, each version in turn by
/// record number.
fn version_of(id: RecordId) -> Version {
    Version::ALL[id.n as usize % Version::ALL.len()]
}

/// Counts what SubmitTx at the record's version, WaitForTx at every version for
/// a transaction with a hash, and WatchMempool at every version answer for one
/// transaction against the answer the instrument gave on the same state.
pub(super) async fn compare(
    domain: &DomainAdapter,
    watching: &mut Watching,
    id: RecordId,
    tx: &[u8],
    hash: Option<[u8; 32]>,
    dolos: &DolosVerdict,
    tallies: &mut ByEntry,
) {
    let accepted = matches!(dolos, DolosVerdict::Accepted);
    let version = version_of(id);
    let submitted = match version {
        Version::V1 => v1::submit_tx(domain, tx).await,
        Version::V1beta => v1beta::submit_tx(domain, tx).await,
        Version::V1alpha => v1alpha::submit_tx(domain, tx).await,
    };
    let submit = EntryPoint::Route(Route::SubmitTx, version);
    let answer = match &submitted {
        Ok(r) => format!("ref={}", hex::encode(r)),
        Err(message) => format!("refused {message}"),
    };
    let agrees = match &submitted {
        Ok(r) => accepted && hash.is_some_and(|h| r.as_slice() == h),
        Err(_) => !accepted,
    };
    add(tallies.at(submit), agrees, id, "submit", dolos, answer);
    if let Some(hash) = hash {
        wait_for_tx(domain, id, hash, dolos, tallies).await;
    }
    let watched = hash.map(|hash| Watched {
        r#ref: hash.to_vec(),
        bytes: tx.to_vec(),
        stage: MEMPOOL.to_string(),
    });
    let expected: Vec<Watched> = watched.filter(|_| accepted).into_iter().collect();
    for (entry, ready) in watching.ready() {
        let answer = ready.iter().map(|w| {
            let held = if w.bytes == tx {
                "bytes=equal"
            } else {
                "bytes=differ"
            };
            format!("ref={} {held} stage={}", hex::encode(&w.r#ref), w.stage)
        });
        let answer = format!("served=[{}]", answer.collect::<Vec<_>>().join(","));
        add(
            tallies.at(entry),
            ready == expected,
            id,
            "watch",
            dolos,
            answer,
        );
    }
}

/// Counts the first answer WaitForTx serves at every version for the hash
/// against the stage the instrument's answer leaves the transaction in.
async fn wait_for_tx(
    domain: &DomainAdapter,
    id: RecordId,
    hash: [u8; 32],
    dolos: &DolosVerdict,
    tallies: &mut ByEntry,
) {
    let accepted = matches!(dolos, DolosVerdict::Accepted);
    let expected = if accepted { MEMPOOL } else { UNSPECIFIED };
    let waits = [
        (v1::VERSION, v1::wait_for_tx(domain, &hash).await),
        (v1beta::VERSION, v1beta::wait_for_tx(domain, &hash).await),
        (v1alpha::VERSION, v1alpha::wait_for_tx(domain, &hash).await),
    ];
    for (version, first) in waits {
        let entry = EntryPoint::Route(Route::WaitForTx, version);
        let agrees = first
            .as_ref()
            .is_some_and(|(r, stage)| r.as_slice() == hash && stage == expected);
        let answer = match &first {
            Some((r, stage)) => format!("ref={} stage={stage}", hex::encode(r)),
            None => "none".to_string(),
        };
        add(tallies.at(entry), agrees, id, "stage", dolos, answer);
    }
}

/// Counts one answer of a route against the instrument's answer.
fn add(
    tally: &mut Tally,
    agrees: bool,
    id: RecordId,
    cell: &str,
    dolos: &DolosVerdict,
    answer: String,
) {
    tally.add(agrees, || {
        let cells = vec![CellKey::new(Axis::Answer, cell)];
        Finding::new(id, cells, format!("instrument={dolos} route={answer}"))
    });
}

#[cfg(test)]
mod tests {
    use tokio::sync::broadcast;
    use tokio_stream::wrappers::BroadcastStream;

    use super::*;

    /// Spends the task's cooperative budget on reads that each find an item
    /// ready.
    fn spend_budget() {
        let (sender, mut receiver) = broadcast::channel(1);
        for n in 0..1024 {
            sender.send(n).unwrap();
            let _ = receiver.recv().now_or_never();
        }
    }

    #[tokio::test]
    async fn a_stream_with_nothing_sent_holds_nothing_ready() {
        let (_sender, receiver) = broadcast::channel::<u8>(1);
        let mut stream = BroadcastStream::new(receiver);
        assert!(ready_now(&mut stream).is_empty());
        spend_budget();
        assert!(ready_now(&mut stream).is_empty());
    }

    #[tokio::test]
    async fn an_item_sent_after_the_budget_is_spent_is_ready() {
        let (sender, receiver) = broadcast::channel(4);
        let mut stream = BroadcastStream::new(receiver);
        sender.send(7u8).unwrap();
        let first: Vec<_> = ready_now(&mut stream)
            .into_iter()
            .map(Result::unwrap)
            .collect();
        spend_budget();
        sender.send(8u8).unwrap();
        let spent: Vec<_> = ready_now(&mut stream)
            .into_iter()
            .map(Result::unwrap)
            .collect();
        assert_eq!((first, spent), (vec![7], vec![8]));
    }
}
