use std::collections::VecDeque;

use gasket::framework::*;
use itertools::Itertools as _;
use pallas::crypto::hash::Hash;
use pallas::network::facades::PeerClient;
use pallas::network::miniprotocols::txsubmission::{EraTxBody, EraTxId, Request, TxIdAndSize};
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::adapters::storage::MempoolBackend;
use crate::prelude::*;

// HACK: the tx era number differs from the block era number, we subtract 1 to
// make them match.
fn to_n2n_era(era: u16) -> u16 {
    era - 1
}

fn to_n2n_reply(mempool_tx: &MempoolTx) -> TxIdAndSize<EraTxId> {
    let EraCbor(era, bytes) = mempool_tx.payload();

    let era = to_n2n_era(*era);

    let id = EraTxId(era, mempool_tx.hash().to_vec());

    TxIdAndSize(id, bytes.len() as u32)
}

fn to_n2n_body(mempool_tx: MempoolTx) -> EraTxBody {
    let EraCbor(era, bytes) = mempool_tx.into_payload();

    let era = to_n2n_era(era);

    EraTxBody(era, bytes)
}

pub struct Worker {
    peer_session: PeerClient,
    unfulfilled_request: Option<usize>,
    /// Tracks the hashes of tx IDs we've propagated to the peer, in order.
    /// Used to map the protocol's positional ack count to specific tx hashes.
    propagated_hashes: VecDeque<TxHash>,
    /// Unconfirmed entries inherited from earlier sessions. Reannounce once in
    /// this session, without resetting their chain-driven expiry counters.
    recovery_hashes: VecDeque<TxHash>,
}

impl Worker {
    fn available_txs(&self, mempool: &MempoolBackend) -> Vec<MempoolTx> {
        self.recovery_hashes
            .iter()
            // Consult current state: a queued entry may since have confirmed,
            // expired, or rolled back into pending during a blocking request.
            .filter_map(|hash| mempool.find_inflight(hash))
            .filter(|tx| {
                matches!(
                    tx.stage,
                    MempoolTxStage::Propagated | MempoolTxStage::Acknowledged
                )
            })
            .chain(mempool.peek_pending())
            .unique_by(|tx| tx.hash())
            .collect()
    }

    async fn propagate_txs(
        &mut self,
        mempool: &MempoolBackend,
        txs: Vec<MempoolTx>,
    ) -> Result<(), WorkerError> {
        let hashes: Vec<TxHash> = txs.iter().map(|tx| tx.hash()).collect();
        mempool.mark_inflight(&hashes).or_restart()?;
        debug!(?hashes, "announcing transaction IDs");

        let payload = txs.iter().map(to_n2n_reply).collect_vec();

        self.peer_session
            .txsubmission()
            .reply_tx_ids(payload)
            .await
            .inspect_err(|err| warn!(error=%err, ?hashes, "transaction ID send failed; delivery uncertain, reconnect required"))
            .or_restart()?;

        self.recovery_hashes.retain(|hash| !hashes.contains(hash));
        self.propagated_hashes.extend(hashes);
        Ok(())
    }

    /// Drain the first `count` propagated hashes and mark them as acknowledged.
    fn acknowledge_propagated(
        &mut self,
        mempool: &MempoolBackend,
        count: usize,
    ) -> Result<(), WorkerError> {
        if count > self.propagated_hashes.len() {
            warn!(
                count,
                outstanding = self.propagated_hashes.len(),
                "invalid session acknowledgement count"
            );
            return Err(WorkerError::Restart);
        }
        let drain_count = count;
        if drain_count == 0 {
            return Ok(());
        }

        let acked: Vec<TxHash> = self.propagated_hashes.drain(..drain_count).collect();
        mempool.mark_acknowledged(&acked).or_restart()?;
        debug!(hashes = ?acked, "peer acknowledged transaction IDs");
        Ok(())
    }

    async fn schedule_unfulfilled(
        &mut self,
        stage: &mut Stage,
        request: usize,
    ) -> Result<WorkSchedule<Request<EraTxId>>, WorkerError> {
        if !self.available_txs(&stage.mempool).is_empty() {
            debug!(request, "found txs to fulfill request");

            // we have txs available so we process the work unit as a new one.
            // We don't acknowledge anything because that already
            // happened on the initial attempt to fulfill the
            // request.
            Ok(WorkSchedule::Unit(Request::TxIds(0, request as u16)))
        } else {
            debug!(request, "still not enough txs to fulfill request");

            // we wait a few secs to avoid turning this stage into a hot loop.
            // TODO: we need to watch the mempool and abort the wait if there's
            // a change in the list of available txs.
            tokio::time::sleep(Duration::from_secs(10)).await;

            // we store the request again so that the next schedule knows we're
            // still waiting for new transactions.
            self.unfulfilled_request = Some(request);

            Ok(WorkSchedule::Idle)
        }
    }

    async fn schedule_next(&mut self) -> Result<WorkSchedule<Request<EraTxId>>, WorkerError> {
        debug!("waiting for request from upstream peer");

        let req = self
            .peer_session
            .txsubmission()
            .next_request()
            .await
            .inspect_err(|err| warn!(error=%err, outstanding = ?self.propagated_hashes, "submission session failed; reconnect required"))
            .or_restart()?;

        Ok(WorkSchedule::Unit(req))
    }
}

#[async_trait::async_trait(?Send)]
impl gasket::framework::Worker<Stage> for Worker {
    async fn bootstrap(stage: &Stage) -> Result<Self, WorkerError> {
        debug!("connecting to peer");

        let mut peer_session = PeerClient::connect(&stage.peer_address, stage.network_magic)
            .await
            .or_retry()?;

        info!(
            address = stage.peer_address,
            magic = stage.network_magic,
            "connected to peer"
        );

        debug!("sending txsubmit init message");

        peer_session.txsubmission().send_init().await.or_restart()?;

        let recovery_hashes: VecDeque<_> = stage
            .mempool
            .peek_inflight()
            .into_iter()
            .filter(|tx| {
                matches!(
                    tx.stage,
                    MempoolTxStage::Propagated | MempoolTxStage::Acknowledged
                )
            })
            .map(|tx| tx.hash())
            .collect();
        info!(address = stage.peer_address, hashes = ?recovery_hashes, "submission session initialized; recovering unconfirmed transactions");

        let worker = Self {
            peer_session,
            unfulfilled_request: Default::default(),
            propagated_hashes: VecDeque::new(),
            recovery_hashes,
        };

        Ok(worker)
    }

    async fn schedule(
        &mut self,
        stage: &mut Stage,
    ) -> Result<WorkSchedule<Request<EraTxId>>, WorkerError> {
        if let Some(request) = self.unfulfilled_request.take() {
            self.schedule_unfulfilled(stage, request).await
        } else {
            self.schedule_next().await
        }
    }

    async fn execute(
        &mut self,
        unit: &Request<EraTxId>,
        stage: &mut Stage,
    ) -> Result<(), WorkerError> {
        match unit {
            Request::TxIds(ack, req) => {
                let ack = *ack as usize;
                let req = *req as usize;

                debug!(req, ack, "blocking tx ids request");

                self.acknowledge_propagated(&stage.mempool, ack)?;

                let available = self.available_txs(&stage.mempool);
                if !available.is_empty() {
                    let txs: Vec<_> = available.into_iter().take(req).collect();
                    self.propagate_txs(&stage.mempool, txs).await?;
                } else {
                    debug!(req, "not enough txs to fulfill request");
                    self.unfulfilled_request = Some(req);
                }
            }
            Request::TxIdsNonBlocking(ack, req) => {
                debug!(req, ack, "non-blocking tx ids request");

                self.acknowledge_propagated(&stage.mempool, *ack as usize)?;

                let txs: Vec<_> = self
                    .available_txs(&stage.mempool)
                    .into_iter()
                    .take(*req as usize)
                    .collect();
                self.propagate_txs(&stage.mempool, txs).await?;
            }
            Request::Txs(ids) => {
                debug!("tx batch request");

                let found: Vec<MempoolTx> = ids
                    .iter()
                    .filter_map(|id| {
                        let hash = <[u8; 32]>::try_from(id.1.as_slice()).ok().map(Hash::from)?;
                        if !self.propagated_hashes.contains(&hash) {
                            return None;
                        }
                        stage
                            .mempool
                            .find_inflight(&hash)
                            .filter(|tx| to_n2n_era(tx.payload().0) == id.0)
                    })
                    .collect_vec();

                let hashes: Vec<_> = found.iter().map(|tx| tx.hash()).collect();
                debug!(?hashes, "sending transaction bodies");
                let to_send = found.into_iter().map(to_n2n_body).collect_vec();

                let result = self.peer_session.txsubmission().reply_txs(to_send).await;

                if let Err(err) = &result {
                    warn!(err=%err, ?hashes, "transaction body send failed; delivery uncertain, reconnect required")
                }

                result.or_restart()?;
            }
        };

        Ok(())
    }
}

#[derive(Stage)]
#[stage(name = "submit", unit = "Request<EraTxId>", worker = "Worker")]
pub struct Stage {
    peer_address: String,
    network_magic: u64,
    mempool: MempoolBackend,
}

impl Stage {
    pub fn new(peer_address: String, network_magic: u64, mempool: MempoolBackend) -> Self {
        Self {
            peer_address,
            network_magic,
            mempool,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dolos_core::builtin::EphemeralMempool;
    use dolos_redb3::mempool::RedbMempool;
    use gasket::framework::Worker as _;
    use pallas::network::{facades::PeerServer, miniprotocols::txsubmission::Reply};
    use tokio::net::TcpListener;

    mod capture {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/musashi_submission.rs"
        ));
    }

    // Kill the real muxer after a request has transferred agency. Keep the
    // protocol client and its closed channel, so reply_* fails in channel I/O.
    async fn break_channel(worker: &mut Worker) {
        use pallas::network::multiplexer::{Bearer, Plexer};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (socket, accepted) = tokio::join!(
            tokio::net::TcpStream::connect(listener.local_addr().unwrap()),
            listener.accept()
        );
        let dummy = Plexer::new(Bearer::Tcp(socket.unwrap())).spawn();
        let original = std::mem::replace(&mut worker.peer_session.plexer, dummy);
        original.abort().await;
        // Waiting for another protocol on the same muxer observes cancellation,
        // without a timing-dependent sleep or a TCP send-buffer race.
        assert!(worker
            .peer_session
            .chainsync()
            .request_next()
            .await
            .is_err());
        drop(accepted);
    }

    async fn connect(stage: &Stage, listener: &TcpListener) -> (Worker, PeerServer) {
        let (worker, peer) =
            tokio::join!(Worker::bootstrap(stage), PeerServer::accept(listener, 164));
        let mut peer = peer.unwrap();
        peer.txsubmission().wait_for_init().await.unwrap();
        (worker.unwrap(), peer)
    }

    async fn request_ids(
        worker: &mut Worker,
        peer: &mut PeerServer,
        stage: &mut Stage,
        ack: u16,
        count: u16,
    ) -> Vec<TxIdAndSize<EraTxId>> {
        peer.txsubmission()
            .acknowledge_and_request_tx_ids(false, ack, count)
            .await
            .unwrap();
        let WorkSchedule::Unit(request) = worker.schedule_next().await.unwrap() else {
            panic!("request")
        };
        worker.execute(&request, stage).await.unwrap();
        let Reply::TxIds(ids) = peer.txsubmission().receive_next_reply().await.unwrap() else {
            panic!("IDs")
        };
        ids
    }

    async fn assert_bodies(
        worker: &mut Worker,
        peer: &mut PeerServer,
        stage: &mut Stage,
        ids: &[TxIdAndSize<EraTxId>],
        expected: &[MempoolTx],
    ) {
        peer.txsubmission()
            .request_txs(ids.iter().map(|x| x.0.clone()).collect())
            .await
            .unwrap();
        let WorkSchedule::Unit(request) = worker.schedule_next().await.unwrap() else {
            panic!("request")
        };
        worker.execute(&request, stage).await.unwrap();
        let Reply::Txs(bodies) = peer.txsubmission().receive_next_reply().await.unwrap() else {
            panic!("bodies")
        };
        let originals: Vec<_> = ids
            .iter()
            .map(|id| {
                let tx = expected
                    .iter()
                    .find(|tx| tx.hash().as_ref() == id.0 .1.as_slice())
                    .unwrap();
                assert_eq!(*id, to_n2n_reply(tx));
                to_n2n_body(tx.clone())
            })
            .collect();
        assert_eq!(bodies, originals);
    }

    macro_rules! recovery_test {
        ($name:ident, $persistent:expr, $fault:expr) => {
            #[tokio::test]
            async fn $name() {
                reconnect_recovers_signed_transactions($persistent, $fault).await;
            }
        };
    }
    recovery_test!(recovery_ephemeral_id_send, false, 0);
    recovery_test!(recovery_redb_id_send, true, 0);
    recovery_test!(recovery_ephemeral_body_send, false, 1);
    recovery_test!(recovery_redb_body_send, true, 1);
    recovery_test!(recovery_ephemeral_before_ack, false, 2);
    recovery_test!(recovery_redb_before_ack, true, 2);
    recovery_test!(recovery_ephemeral_after_ack, false, 3);
    recovery_test!(recovery_redb_after_ack, true, 3);

    async fn reconnect_recovers_signed_transactions(persistent: bool, fault: u8) {
        tokio::time::timeout(Duration::from_secs(40), async {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("mempool.redb");
            let store = if persistent {
                MempoolBackend::Redb(
                    RedbMempool::open(&path, &Default::default(), Default::default()).unwrap(),
                )
            } else {
                MempoolBackend::Ephemeral(EphemeralMempool::new())
            };
            let expected: Vec<_> = [false, true]
                .into_iter()
                .map(|settings| capture::assert_submission(settings, false))
                .collect();
            for (tx, name) in expected.iter().zip(["control", "settings-registration"]) {
                let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(format!("test_data/submission-recovery/{name}.mempool.hex"));
                let bytes = hex::decode(std::fs::read_to_string(path).unwrap().trim()).unwrap();
                assert_eq!(
                    tx.payload().1,
                    bytes,
                    "representative signed fixture must be unchanged"
                );
                store.receive(tx.clone()).unwrap();
            }
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut stage = Stage::new(listener.local_addr().unwrap().to_string(), 164, store);
            let (mut worker, mut peer) = connect(&stage, &listener).await;
            if fault == 0 {
                peer.txsubmission()
                    .acknowledge_and_request_tx_ids(false, 0, 2)
                    .await
                    .unwrap();
                let WorkSchedule::Unit(request) = worker.schedule_next().await.unwrap() else {
                    panic!("request")
                };
                break_channel(&mut worker).await;
                let error = worker.execute(&request, &mut stage).await.unwrap_err();
                assert!(matches!(error, WorkerError::Restart));
            } else {
                let ids = request_ids(&mut worker, &mut peer, &mut stage, 0, 2).await;
                assert_eq!(ids.len(), 2);
                if fault == 1 {
                    peer.txsubmission()
                        .request_txs(ids.into_iter().map(|x| x.0).collect())
                        .await
                        .unwrap();
                    let WorkSchedule::Unit(request) = worker.schedule_next().await.unwrap() else {
                        panic!("request")
                    };
                    break_channel(&mut worker).await;
                    let error = worker.execute(&request, &mut stage).await.unwrap_err();
                    assert!(matches!(error, WorkerError::Restart));
                } else if fault == 3 {
                    assert_bodies(&mut worker, &mut peer, &mut stage, &ids, &expected).await;
                    assert!(request_ids(&mut worker, &mut peer, &mut stage, 2, 1)
                        .await
                        .is_empty());
                }
            }
            if fault >= 2 {
                peer.abort().await;
                assert!(matches!(
                    worker.schedule_next().await,
                    Err(WorkerError::Restart)
                ));
            } else {
                peer.abort().await;
            }
            worker.peer_session.abort().await;
            // Age survives recovery; repeated reconnects must not renew TTL.
            stage
                .mempool
                .confirm(&ChainPoint::Origin, &[], &[], 10, 3)
                .unwrap();
            if persistent {
                let Stage {
                    peer_address,
                    network_magic,
                    mempool,
                } = stage;
                drop(mempool);
                stage = Stage::new(
                    peer_address,
                    network_magic,
                    MempoolBackend::Redb(
                        RedbMempool::open(&path, &Default::default(), Default::default()).unwrap(),
                    ),
                );
            }
            let (mut worker, mut peer) = connect(&stage, &listener).await;
            let ids = request_ids(&mut worker, &mut peer, &mut stage, 0, 1).await;
            assert_eq!(
                ids.len(),
                1,
                "replacement must recover inflight IDs: persistent={persistent}, fault={fault}"
            );
            let first = Hash::from(ids[0].0 .1.as_slice());
            assert_bodies(&mut worker, &mut peer, &mut stage, &ids, &expected).await;
            let rest = request_ids(&mut worker, &mut peer, &mut stage, 1, 2).await;
            assert_eq!(rest.len(), 1);
            let second = Hash::from(rest[0].0 .1.as_slice());
            assert_ne!(first, second);
            assert_eq!(
                stage.mempool.check_status(&first).stage,
                MempoolTxStage::Acknowledged
            );
            if fault != 3 {
                assert_eq!(
                    stage.mempool.check_status(&second).stage,
                    MempoolTxStage::Propagated
                );
            }
            assert!(
                request_ids(&mut worker, &mut peer, &mut stage, 0, 2)
                    .await
                    .is_empty(),
                "no repeat announcements within a session"
            );
            assert_bodies(&mut worker, &mut peer, &mut stage, &rest, &expected).await;
            assert!(request_ids(&mut worker, &mut peer, &mut stage, 1, 2)
                .await
                .is_empty());
            for tx in &expected {
                assert_eq!(stage.mempool.check_status(&tx.hash()).non_confirmations, 1);
            }
            assert!(matches!(
                worker.acknowledge_propagated(&stage.mempool, 1),
                Err(WorkerError::Restart)
            ));
            stage
                .mempool
                .confirm(&ChainPoint::Origin, &[first], &[], 10, 3)
                .unwrap();
            worker.peer_session.abort().await;
            peer.abort().await;
            let (mut worker, mut peer) = connect(&stage, &listener).await;
            // The second entry expires after bootstrap queued it for recovery.
            stage
                .mempool
                .confirm(&ChainPoint::Origin, &[], &[], 10, 3)
                .unwrap();
            assert!(
                request_ids(&mut worker, &mut peer, &mut stage, 0, 2)
                    .await
                    .is_empty(),
                "confirmed and dropped transactions must not recover"
            );
            assert_eq!(
                stage.mempool.check_status(&first).stage,
                MempoolTxStage::Confirmed
            );
            assert!(stage
                .mempool
                .dump_finalized(0, 10)
                .items
                .iter()
                .any(|tx| tx.hash() == second && tx.stage == MempoolTxStage::Dropped));
            worker.peer_session.abort().await;
            peer.abort().await;
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn musashi_recording_peer_receives_original_signed_bytes() {
        for persistent in [false, true] {
            for alternate in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let db_path = dir.path().join("mempool.redb");
                let store = if persistent {
                    MempoolBackend::Redb(
                        RedbMempool::open(&db_path, &Default::default(), Default::default())
                            .unwrap(),
                    )
                } else {
                    MempoolBackend::Ephemeral(EphemeralMempool::new())
                };
                let mut expected = vec![];
                for settings in [false, true] {
                    let tx = capture::assert_submission(settings, alternate);
                    expected.push((tx.hash().to_vec(), tx.payload().1.clone()));
                    store.receive(tx).unwrap();
                }
                // Exercise durable encoding and a real reopen, not only Clone.
                let store = if persistent {
                    drop(store);
                    MempoolBackend::Redb(
                        RedbMempool::open(&db_path, &Default::default(), Default::default())
                            .unwrap(),
                    )
                } else {
                    store
                };
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let mut stage = Stage::new(listener.local_addr().unwrap().to_string(), 164, store);
                let server = async {
                    let mut peer = PeerServer::accept(&listener, 164).await.unwrap();
                    let server = peer.txsubmission();
                    server.wait_for_init().await.unwrap();
                    server
                        .acknowledge_and_request_tx_ids(false, 0, 2)
                        .await
                        .unwrap();
                    let Reply::TxIds(ids) = server.receive_next_reply().await.unwrap() else {
                        panic!("expected IDs")
                    };
                    assert_eq!(ids.len(), 2);
                    for (TxIdAndSize(EraTxId(era, hash), size), (expected_hash, bytes)) in
                        ids.iter().zip(&expected)
                    {
                        assert_eq!(*era, 7, "node-to-node Dijkstra tag");
                        assert_eq!(hash, expected_hash);
                        assert_eq!(*size as usize, bytes.len());
                    }
                    server
                        .request_txs(ids.into_iter().map(|x| x.0).collect())
                        .await
                        .unwrap();
                    let Reply::Txs(bodies) = server.receive_next_reply().await.unwrap() else {
                        panic!("expected bodies")
                    };
                    assert_eq!(bodies.len(), 2);
                    for (EraTxBody(era, bytes), (hash, original)) in bodies.iter().zip(&expected) {
                        assert_eq!(*era, 7);
                        assert_eq!(bytes, original);
                        let tx = pallas::ledger::traverse::MultiEraTx::decode_for_era(
                            pallas::ledger::traverse::Era::Dijkstra,
                            bytes,
                        )
                        .unwrap();
                        assert_eq!(tx.hash().to_vec(), *hash);
                    }
                };
                let client = async {
                    let mut worker =
                        <Worker as gasket::framework::Worker<Stage>>::bootstrap(&stage)
                            .await
                            .unwrap();
                    for _ in 0..2 {
                        let WorkSchedule::Unit(request) = worker.schedule_next().await.unwrap()
                        else {
                            panic!("expected request")
                        };
                        worker.execute(&request, &mut stage).await.unwrap();
                    }
                };
                tokio::time::timeout(Duration::from_secs(20), async {
                    tokio::join!(server, client);
                })
                .await
                .unwrap();
            }
        }
    }
}
