use std::collections::BTreeMap;
use std::sync::Arc;

use dolos_cardano::consensus::{ChainFragment, RollbackResult};
use dolos_core::config::{PeerConfig, SyncConfig, SyncLimit};
use dolos_core::ChainPoint;
use gasket::framework::*;
use itertools::Itertools;
use pallas::ledger::traverse::leios::{AnnouncedEndorserBlock, CertificationTracker};
use pallas::ledger::traverse::MultiEraHeader;
use pallas::network::facades::PeerClient;
use pallas::network::miniprotocols::chainsync::{HeaderContent, NextResponse, Tip};
use pallas::network::miniprotocols::Point;
use tracing::{debug, error, info, warn};

use crate::adapters::WalAdapter;
use crate::prelude::*;
use crate::sync::leios::{
    resume_walk, resume_window, AppliedTxWindow, CertifiedPayload, LeiosClient, PendingPayloads,
};

/// How far back the certification walk reads stored blocks looking for the
/// Leios event that settles what is waiting.
///
/// The write ahead log is already bounded by its own retention, so this only
/// caps a deployment that keeps a very long one. Reaching it is reported as a
/// walk that could not settle, with the number of blocks read, and never as a
/// walk that found nothing waiting.
const RESUME_SCAN_LIMIT: usize = 50_000;

/// Reads the certification walk back out of the blocks already stored at a
/// point.
///
/// Nothing on the chain records which announcement is waiting for a certificate,
/// so the blocks themselves are the only honest source. Deriving it here rather
/// than saving a copy beside the cursor means the two cannot disagree after a
/// crash or a rollback.
fn walk_at(wal: &WalAdapter, point: &ChainPoint) -> Result<CertificationTracker, WorkerError> {
    let blocks = wal
        .iter_blocks(None, Some(point.clone()))
        .or_panic()?
        .rev()
        .take(RESUME_SCAN_LIMIT)
        .map(|(_, raw)| raw);

    let resumed = resume_walk(blocks).or_panic()?;

    info!(
        slot = point.slot(),
        scanned = resumed.scanned,
        settled_at = resumed.settled_at,
        state = ?resumed.state,
        "certification walk read back from stored blocks"
    );

    Ok(CertificationTracker::resume_from(resumed.state))
}

/// Reads the applied transaction window back out of the blocks already stored
/// at a point.
///
/// Two consecutive certified endorser blocks overlap as a matter of course, and
/// the overlap does not care whether the follower restarted in the middle of
/// it. A window that started empty at every resume would leave the follower
/// applying the first repeated transaction a second time, which is the failure
/// this reads back to avoid.
fn window_at(wal: &WalAdapter, point: &ChainPoint) -> Result<AppliedTxWindow, WorkerError> {
    let blocks = wal
        .iter_blocks(None, Some(point.clone()))
        .or_panic()?
        .rev()
        .map(|(_, raw)| raw);

    let resumed = resume_window(blocks).or_panic()?;

    info!(
        slot = point.slot(),
        reach = ?resumed.reach,
        ids = resumed.window.ids(),
        "applied transaction window read back from stored blocks"
    );

    Ok(resumed.window)
}

fn to_traverse(header: &HeaderContent) -> Result<MultiEraHeader<'_>, WorkerError> {
    let out = match header.byron_prefix {
        Some((subtag, _)) => MultiEraHeader::decode(header.variant, Some(subtag), &header.cbor),
        None => MultiEraHeader::decode(header.variant, None, &header.cbor),
    };

    out.or_panic()
}

// ============================================================================
// Pull stage
// ============================================================================

pub type DownstreamPort = gasket::messaging::OutputPort<PullEvent>;

enum PullResult {
    Blocks(Vec<ChainPoint>),
    Rollback(ChainPoint),
    Empty,
}

pub enum WorkUnit {
    Pull,
    Await,
}

pub enum PullQuota {
    WaitingTip,
    Unlimited,
    BlockQuota(u64),
    Reached,
}

impl PullQuota {
    fn should_quit(&self) -> bool {
        matches!(self, Self::Reached)
    }

    fn on_tip(&mut self) {
        if let Self::WaitingTip = self {
            *self = Self::Reached;
        }
    }

    fn consume_blocks(&mut self, count: u64) {
        if let Self::BlockQuota(x) = self {
            let new = x.saturating_sub(count);

            if new == 0 {
                *self = Self::Reached;
            } else {
                *self = Self::BlockQuota(new);
            }
        }
    }
}

impl From<SyncLimit> for PullQuota {
    fn from(limit: SyncLimit) -> Self {
        match limit {
            SyncLimit::UntilTip => Self::WaitingTip,
            SyncLimit::NoLimit => Self::Unlimited,
            SyncLimit::MaxBlocks(blocks) => Self::BlockQuota(blocks),
        }
    }
}

pub struct Worker {
    peer_session: PeerClient,
    chain: ChainFragment,

    /// Present only when a Leios peer is configured. Without it the chain is
    /// followed as an ordinary Praos chain, which on a Leios network builds a
    /// ledger that is short by every endorsed transaction.
    leios: Option<LeiosClient>,

    /// The walk over headers that decides which endorser blocks are certified.
    certification: CertificationTracker,

    /// Endorser blocks already fetched, waiting for the ranking block that
    /// certified them to arrive from blockfetch, and the certifications still
    /// owed one.
    payloads: PendingPayloads,

    /// Which endorser block each recorded certification is owed, so a fetch
    /// that failed can be tried again. The walk consumed the announcement when
    /// it observed the certificate and will not offer it a second time, so this
    /// is the only place it survives.
    outstanding: BTreeMap<u64, AnnouncedEndorserBlock>,

    /// What the blocks just behind the tip carried, so a transaction two
    /// certified endorser blocks both name is applied once.
    applied: AppliedTxWindow,
}

impl Worker {
    /// Receive the next chainsync response, using the appropriate method
    /// depending on whether we have agency (catching up) or not (at the tip).
    async fn recv_next_header(&mut self) -> Result<NextResponse<HeaderContent>, WorkerError> {
        let client = self.peer_session.chainsync();

        if client.has_agency() {
            client.request_next().await.or_restart()
        } else {
            client.recv_while_must_reply().await.or_restart()
        }
    }

    /// Gather up to `max_headers` headers from the upstream peer.
    ///
    /// For each chainsync response:
    /// - RollForward: validate chain continuity, track in fragment
    /// - RollBackward: update fragment; if out of scope, return as rollback
    /// - Await: stop gathering (peer has no more blocks)
    ///
    /// Returns the gathered points to fetch, a rollback to propagate, or empty.
    async fn pull_headers(
        &mut self,
        max_headers: usize,
        stage: &mut Stage,
    ) -> Result<PullResult, WorkerError> {
        let mut gathered = 0;

        while gathered < max_headers {
            let next = self.recv_next_header().await?;

            match next {
                NextResponse::RollForward(header, tip) => {
                    let header = to_traverse(&header).or_panic()?;
                    let point = ChainPoint::Specific(header.slot(), header.hash());
                    let prev_hash = header.previous_hash();

                    self.chain
                        .roll_forward(point.clone(), prev_hash)
                        .map_err(|err| {
                            warn!(%err, "consensus error, reconnecting");
                            WorkerError::Restart
                        })?;

                    debug!(%point, "header received from upstream peer");
                    gathered += 1;

                    self.follow_endorsement(&header, stage).await?;

                    stage.track_tip(&tip);
                }
                NextResponse::RollBackward(point, tip) => {
                    debug!(?point, "rollback sent by upstream peer");

                    let chain_point = ChainPoint::from(point);

                    match self.chain.roll_back(&chain_point) {
                        RollbackResult::OutOfScope(point) => {
                            return Ok(PullResult::Rollback(point))
                        }
                        RollbackResult::Handled => (),
                    }

                    stage.track_tip(&tip);
                }
                NextResponse::Await => break,
            }
        }

        let points = self.chain.take_pending();

        if points.is_empty() {
            Ok(PullResult::Empty)
        } else {
            Ok(PullResult::Blocks(points))
        }
    }

    /// Reads one header's Leios fields and, when it certifies an endorser
    /// block, fetches that endorser block whole so it is ready when the
    /// certifying block's body arrives.
    ///
    /// A peer that does not hold the endorser block answers with a well formed
    /// empty body rather than an error, and that answer is refused by the size
    /// the announcement committed to, several layers down in
    /// `EndorserBlockBody::decode_announced`. It surfaces here as a retry rather
    /// than as a block with no transactions.
    ///
    /// A failed fetch drops the Leios connection and builds a new one. The
    /// networking stack surfaces a peer that has completed its handshake and
    /// never a peer that has gone away, so a client holding a peer identity has
    /// no way to learn that its session ended, and every later request would
    /// wait out its own timeout against a peer that is not there. Rebuilding
    /// costs one handshake and is the only thing here that can tell the two
    /// apart.
    async fn follow_endorsement(
        &mut self,
        header: &MultiEraHeader<'_>,
        stage: &mut Stage,
    ) -> Result<(), WorkerError> {
        if self.leios.is_none() {
            return Ok(());
        }

        // Stopping is the only honest answer to a certificate the walk cannot
        // resolve. Continuing would apply a certifying block with no
        // transactions and leave the ledger short with no error anywhere, which
        // is the whole failure this stage exists to prevent, and skipping the
        // block is the same thing one layer up.
        let outcome = self.certification.observe(header).map_err(|err| {
            error!(
                %err,
                slot = header.slot(),
                "the certification walk cannot resolve this block"
            );
            WorkerError::Panic
        })?;

        let Some(eb) = outcome.certified else {
            return Ok(());
        };

        debug!(
            certifying_slot = header.slot(),
            eb = %eb.hash,
            size = eb.size,
            "a ranking block certifies an endorser block"
        );

        // The debt is recorded before anything is fetched. The walk has already
        // consumed the announcement by this point and will never offer it again,
        // so a fetch that fails and is forgotten here is an endorser block no
        // later pass can know was missing.
        self.payloads.expect(header.slot());
        self.outstanding.insert(header.slot(), eb);

        self.fetch_outstanding(stage).await
    }

    /// Fetches the endorser block of every certification recorded and not yet
    /// delivered, oldest first.
    ///
    /// A fetch that fails leaves its certification recorded and reconnects, so
    /// the next pass tries again and the batch cannot be flushed in the
    /// meantime. That is what turns a peer that stopped answering into a sync
    /// that pauses rather than a ledger that is quietly short an endorser block.
    async fn fetch_outstanding(&mut self, stage: &mut Stage) -> Result<(), WorkerError> {
        let Some(client) = self.leios.as_mut() else {
            return Ok(());
        };

        for slot in self.payloads.outstanding() {
            let Some(eb) = self.outstanding.get(&slot).cloned() else {
                continue;
            };

            let txs = match client.fetch(&eb).await {
                Ok(txs) => txs,
                Err(err) => {
                    warn!(%err, eb = %eb.hash, slot, "endorser block fetch failed, reconnecting");

                    let address = stage
                        .leios_peer_address
                        .as_ref()
                        .expect("a leios client exists only when an address is configured");

                    self.leios = Some(LeiosClient::new(address, stage.network_magic).or_panic()?);

                    return Err(WorkerError::Retry);
                }
            };

            stage.endorser_block_count.inc(1);
            stage.endorser_tx_count.inc(txs.len() as u64);

            info!(
                certifying_slot = slot,
                eb = %eb.hash,
                txs = txs.len(),
                "endorser block fetched"
            );

            self.outstanding.remove(&slot);
            self.payloads.deliver(
                slot,
                CertifiedPayload {
                    endorser_block: eb,
                    txs,
                },
            );
        }

        Ok(())
    }

    /// Fetch block bodies for the given points and flush them downstream.
    async fn fetch_and_flush(
        &mut self,
        points: &[ChainPoint],
        stage: &mut Stage,
    ) -> Result<(), WorkerError> {
        let to_pallas = |cp: &ChainPoint| -> Point {
            Point::try_from(cp.clone()).expect("pending points are always Specific")
        };

        let blocks = match points {
            [single] => {
                let block = self
                    .peer_session
                    .blockfetch()
                    .fetch_single(to_pallas(single))
                    .await
                    .or_restart()?;

                vec![block]
            }
            [first, .., last] => self
                .peer_session
                .blockfetch()
                .fetch_range((to_pallas(first), to_pallas(last)))
                .await
                .or_restart()?,
            [] => return Ok(()),
        };

        debug!(len = blocks.len(), "block batch pulled from peer");

        // Nothing is flushed while a certification is still owed its endorser
        // block. A failed fetch earlier in this batch left its certification
        // recorded, and this is where the sync waits for it rather than
        // applying the block that certifies it with no transactions in it.
        self.fetch_outstanding(stage).await?;

        // Each payload is attached to the block of the slot that certified it,
        // never to a position in the batch, so a short or reordered batch cannot
        // move one block's endorsed transactions onto another. A payload no
        // block claimed is refused rather than dropped.
        let blocks = self.payloads.apply(&mut self.applied, blocks).map_err(|err| {
            warn!(%err, "resolving a certified block failed");
            WorkerError::Panic
        })?;

        self.payloads.refuse_undelivered().map_err(|err| {
            warn!(%err, "an endorser block was fetched and never applied");
            WorkerError::Panic
        })?;

        stage.quota.consume_blocks(blocks.len() as u64);
        stage.flush_blocks(blocks).await?;

        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl gasket::framework::Worker<Stage> for Worker {
    async fn bootstrap(stage: &Stage) -> Result<Self, WorkerError> {
        debug!("finding intersection candidates");

        let mut candidates = stage
            .wal
            .intersect_candidates(5)
            .or_panic()?
            .into_iter()
            .map(TryFrom::try_from)
            .filter_map(|x| x.ok())
            .collect_vec();

        if candidates.is_empty() {
            candidates.push(Point::Origin);
        }

        debug!("connecting to peer");

        let mut peer_session = PeerClient::connect(&stage.peer_address, stage.network_magic)
            .await
            .or_retry()?;

        info!(
            address = stage.peer_address,
            magic = stage.network_magic,
            "connected to peer"
        );

        debug!("finding intersect");

        let (point, _) = peer_session
            .chainsync()
            .find_intersect(candidates)
            .await
            .or_restart()?;

        let intersection = point
            .ok_or(Error::message("couldn't find intersect"))
            .or_panic()?;

        info!(?intersection, "found intersection");

        let intersection = ChainPoint::from(intersection);

        let (leios, certification, applied) = match &stage.leios_peer_address {
            None => (
                None,
                CertificationTracker::default(),
                AppliedTxWindow::default(),
            ),
            Some(address) => {
                info!(address, "connecting to a Leios peer for endorser blocks");

                let client = LeiosClient::new(address, stage.network_magic).or_panic()?;
                let walk = walk_at(&stage.wal, &intersection)?;
                let applied = window_at(&stage.wal, &intersection)?;

                (Some(client), walk, applied)
            }
        };

        let worker = Self {
            peer_session,
            chain: ChainFragment::start(intersection),
            leios,
            certification,
            payloads: PendingPayloads::default(),
            outstanding: BTreeMap::new(),
            applied,
        };

        Ok(worker)
    }

    async fn schedule(&mut self, stage: &mut Stage) -> Result<WorkSchedule<WorkUnit>, WorkerError> {
        if stage.quota.should_quit() {
            warn!("quota reached, stopping sync");
            return Ok(WorkSchedule::Done);
        }

        let client = self.peer_session.chainsync();

        if client.has_agency() {
            debug!("should request next batch of blocks");
            Ok(WorkSchedule::Unit(WorkUnit::Pull))
        } else {
            debug!("should await next block");
            Ok(WorkSchedule::Unit(WorkUnit::Await))
        }
    }

    async fn execute(&mut self, unit: &WorkUnit, stage: &mut Stage) -> Result<(), WorkerError> {
        let max_headers = match unit {
            WorkUnit::Pull => stage.block_fetch_batch_size,
            WorkUnit::Await => 1,
        };

        match self.pull_headers(max_headers, stage).await? {
            PullResult::Blocks(points) => self.fetch_and_flush(&points, stage).await?,
            PullResult::Rollback(point) => {
                // The certification walk is a property of the chain, so a
                // rollback invalidates both what it carries and anything fetched
                // for a block that is no longer on the chain. The endorser
                // transactions of a rolled back block are undone with it,
                // because they are part of the block bytes the log holds.
                //
                // The walk is read back out of the stored blocks at the rollback
                // point rather than emptied. Emptying it would say nothing is
                // waiting, which is a claim about the chain that a rollback
                // gives no grounds for, and the first certificate after the
                // rollback would then be answered with no payload at all.
                self.payloads = PendingPayloads::default();
                self.outstanding.clear();

                if self.leios.is_some() {
                    self.certification = walk_at(&stage.wal, &point)?;

                    // The window says which transactions are already applied,
                    // so a rollback that undid some of them leaves it claiming
                    // more than the ledger holds. It is read back from the
                    // stored blocks at the rollback point for the same reason
                    // the walk is.
                    self.applied = window_at(&stage.wal, &point)?;
                }

                stage.flush_rollback(point).await?
            }
            PullResult::Empty => (),
        }

        if !self.peer_session.chainsync().has_agency() {
            stage.quota.on_tip();
        }

        Ok(())
    }
}

#[derive(Stage)]
#[stage(name = "pull", unit = "WorkUnit", worker = "Worker")]
pub struct Stage {
    peer_address: String,
    leios_peer_address: Option<String>,
    network_magic: u64,
    block_fetch_batch_size: usize,
    wal: WalAdapter,
    quota: PullQuota,

    pub downstream: DownstreamPort,

    #[metric]
    block_count: gasket::metrics::Counter,

    #[metric]
    endorser_block_count: gasket::metrics::Counter,

    #[metric]
    endorser_tx_count: gasket::metrics::Counter,

    #[metric]
    chain_tip: gasket::metrics::Gauge,
}

impl Stage {
    pub fn new(
        config: &SyncConfig,
        upstream: &PeerConfig,
        network_magic: u64,
        wal: WalAdapter,
    ) -> Self {
        Self {
            peer_address: upstream.peer_address.clone(),
            leios_peer_address: upstream.leios_peer_address.clone(),
            network_magic,
            quota: config.sync_limit.clone().into(),
            block_fetch_batch_size: config.pull_batch_size(),
            wal,
            downstream: Default::default(),
            block_count: Default::default(),
            endorser_block_count: Default::default(),
            endorser_tx_count: Default::default(),
            chain_tip: Default::default(),
        }
    }

    async fn flush_blocks(&mut self, blocks: Vec<BlockBody>) -> Result<(), WorkerError> {
        for cbor in blocks {
            self.downstream
                .send(PullEvent::RollForward(Arc::new(cbor)).into())
                .await
                .or_panic()?;
        }

        Ok(())
    }

    async fn flush_rollback(&mut self, point: ChainPoint) -> Result<(), WorkerError> {
        debug!(slot = point.slot(), "rollback");

        self.downstream
            .send(PullEvent::Rollback(point).into())
            .await
            .or_panic()?;

        Ok(())
    }

    fn track_tip(&self, tip: &Tip) {
        self.chain_tip.set(tip.0.slot_or_default() as i64);
    }
}
