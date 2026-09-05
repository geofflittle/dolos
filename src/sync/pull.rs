use std::sync::Arc;

use dolos_cardano::consensus::{ChainFragment, RollbackResult};
use dolos_core::config::{PeerConfig, SyncConfig, SyncLimit};
use dolos_core::ChainPoint;
use gasket::framework::*;
use itertools::Itertools;
use pallas::ledger::traverse::leios::CertificationTracker;
use pallas::ledger::traverse::MultiEraHeader;
use pallas::network::facades::PeerClient;
use pallas::network::miniprotocols::chainsync::{HeaderContent, NextResponse, Tip};
use pallas::network::miniprotocols::Point;
use tracing::{debug, error, info, warn};

use crate::adapters::WalAdapter;
use crate::prelude::*;
use crate::sync::leios::{CertifiedPayload, LeiosClient, PendingPayloads};

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
    /// certified them to arrive from blockfetch.
    payloads: PendingPayloads,
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
    /// `EndorserBlockBody::decode_announced`. It surfaces here as a retry, which
    /// reconnects, rather than as a block with no transactions.
    async fn follow_endorsement(
        &mut self,
        header: &MultiEraHeader<'_>,
        stage: &mut Stage,
    ) -> Result<(), WorkerError> {
        let Some(client) = self.leios.as_mut() else {
            return Ok(());
        };

        let outcome = self.certification.observe(header).map_err(|err| {
            // The walk carries the announcement a later block certifies, and
            // nothing persists it, so a sync that resumes between an
            // announcement and its certificate starts cold and meets the
            // certificate with nothing to fetch. Stopping is the only honest
            // answer: continuing would apply a certifying block with no
            // transactions and leave the ledger short with no error anywhere,
            // which is the whole failure this stage exists to prevent.
            error!(
                %err,
                slot = header.slot(),
                "the certification walk cannot resolve this block. If this is a resumed sync, \
                 it began between an announcement and its certificate. Sync from origin, or \
                 from a point that is not inside an endorsement window."
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

        let txs = client.fetch(&eb).await.map_err(|err| {
            warn!(%err, eb = %eb.hash, "endorser block fetch failed, retrying");
            WorkerError::Retry
        })?;

        stage.endorser_block_count.inc(1);
        stage.endorser_tx_count.inc(txs.len() as u64);

        info!(
            certifying_slot = header.slot(),
            eb = %eb.hash,
            txs = txs.len(),
            "endorser block fetched"
        );

        self.payloads.insert(
            header.slot(),
            CertifiedPayload {
                endorser_block: eb,
                txs,
            },
        );

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

        // Each payload is attached to the block of the slot that certified it,
        // never to a position in the batch, so a short or reordered batch cannot
        // move one block's endorsed transactions onto another. A payload no
        // block claimed is refused rather than dropped.
        let blocks = self.payloads.apply(blocks).map_err(|err| {
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

        let leios = match &stage.leios_peer_address {
            None => None,
            Some(address) => {
                info!(address, "connecting to a Leios peer for endorser blocks");

                let client = LeiosClient::new(address, stage.network_magic).or_panic()?;

                Some(client)
            }
        };

        let worker = Self {
            peer_session,
            chain: ChainFragment::start(ChainPoint::from(intersection)),
            leios,
            certification: CertificationTracker::default(),
            payloads: PendingPayloads::default(),
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
                debug!("rollback resets the certification walk");
                self.certification = CertificationTracker::default();
                self.payloads = PendingPayloads::default();

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
