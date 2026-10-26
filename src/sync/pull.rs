use std::collections::BTreeMap;
use std::sync::Arc;

use dolos_cardano::consensus::{ChainFragment, RollbackResult};
use dolos_core::config::{PeerConfig, SyncConfig, SyncLimit};
use dolos_core::ChainPoint;
use gasket::framework::*;
use itertools::Itertools;
use pallas::ledger::traverse::{MultiEraBlock, MultiEraHeader};
use pallas::network::facades::PeerClient;
use pallas::network::miniprotocols::chainsync::{HeaderContent, NextResponse, Tip};
use pallas::network::miniprotocols::Point;
use tracing::{debug, error, info, warn};

use crate::adapters::WalAdapter;
use crate::prelude::*;
use crate::sync::endorser;
use crate::sync::leios::{AnnouncedEndorserBlock, CertifiedPayload, LeiosClient, PendingPayloads};

/// A header kept in the bytes it arrived in, for a later header to name as its
/// parent.
enum HeldHeader {
    Header(HeaderContent),
    Block(RawBlock),
}

/// The block at `point` with its hash, read from the write ahead log or
/// fetched with `fetch`, and none at the origin or a point without a hash.
async fn held_at(
    wal: &WalAdapter,
    point: &ChainPoint,
    fetch: impl AsyncFnOnce(Point) -> Result<BlockBody, WorkerError>,
) -> Result<Vec<(BlockHash, HeldHeader)>, WorkerError> {
    let logged = wal
        .iter_blocks(None, Some(point.clone()))
        .or_panic()?
        .next_back()
        .filter(|(at, raw)| at == point && !raw.is_empty());

    let (raw, source) = match (logged, point) {
        (Some((_, raw)), _) => (raw, "log"),
        (None, ChainPoint::Specific(slot, hash)) => {
            let fetched = fetch(Point::Specific(*slot, hash.to_vec())).await?;
            (Arc::new(fetched), "peer")
        }
        (None, ChainPoint::Origin | ChainPoint::Slot(_)) => return Ok(Vec::new()),
    };

    let (slot, hash) = MultiEraBlock::decode(&raw)
        .map(|block| (block.slot(), block.hash()))
        .or_panic()?;

    info!(point = point.slot(), slot, %hash, source, "parent header read back");

    Ok(vec![(hash, HeldHeader::Block(raw))])
}

/// The block at `point` as the peer serves it.
async fn peer_block(peer: &mut PeerClient, point: Point) -> Result<BlockBody, WorkerError> {
    peer.blockfetch().fetch_single(point).await.or_restart()
}

/// The announcement of `parent` that `header` certifies, with the parent's slot.
fn announced_by(
    parent: Option<&MultiEraHeader<'_>>,
    header: &MultiEraHeader<'_>,
) -> Result<Option<AnnouncedEndorserBlock>, endorser::Error> {
    let certified = endorser::certification(parent, header)?;

    Ok(certified
        .zip(parent)
        .map(|(eb, parent)| AnnouncedEndorserBlock {
            slot: parent.slot(),
            hash: eb.eb_hash,
            size: eb.eb_size,
        }))
}

/// The endorser block `header` certifies, read from the held header whose hash
/// it names as its parent.
fn certified_by(
    held: &[(BlockHash, HeldHeader)],
    header: &MultiEraHeader<'_>,
) -> Result<Option<AnnouncedEndorserBlock>, endorser::Error> {
    let parent = held
        .iter()
        .rev()
        .find(|(hash, _)| Some(*hash) == header.previous_hash())
        .map(|(_, held)| held);

    match parent {
        None => announced_by(None, header),
        Some(HeldHeader::Header(content)) => {
            let parent = to_traverse(content).expect("decoded when it was held");
            announced_by(Some(&parent), header)
        }
        Some(HeldHeader::Block(raw)) => {
            let block = MultiEraBlock::decode(raw).expect("decoded when it was held");
            announced_by(Some(&block.header()), header)
        }
    }
}

fn to_traverse(header: &HeaderContent) -> Result<MultiEraHeader<'_>, WorkerError> {
    let out = match header.byron_prefix {
        Some((subtag, _)) => MultiEraHeader::decode(header.variant, Some(subtag), &header.cbor),
        None => MultiEraHeader::decode(header.variant, None, &header.cbor),
    };

    out.or_panic()
}

/// Records the fetch a certified endorser block makes the follower owe, and
/// answers whether one is now owed.
///
/// One fetch is owed for each endorser block a header certifies and none for
/// one a header only announces, so an announcement that a later header
/// supersedes before any certificate names it costs nothing at all.
///
/// The debt is recorded before anything is fetched, so a fetch that fails
/// leaves a debt a later pass retries.
fn record_certification(
    payloads: &mut PendingPayloads,
    outstanding: &mut BTreeMap<u64, AnnouncedEndorserBlock>,
    certifying_slot: u64,
    certified: Option<AnnouncedEndorserBlock>,
) -> bool {
    let Some(eb) = certified else {
        return false;
    };

    debug!(
        certifying_slot,
        eb = %eb.hash,
        size = eb.size,
        "a ranking block certifies an endorser block"
    );

    payloads.expect(certifying_slot);
    outstanding.insert(certifying_slot, eb);

    true
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

    /// Headers a certificate may name as its parent: the block stored at the
    /// point the pull resumed from, then the headers pulled since.
    held: Vec<(BlockHash, HeldHeader)>,

    /// Endorser blocks already fetched, waiting for the ranking block that
    /// certified them to arrive from blockfetch, and the certifications still
    /// owed one.
    payloads: PendingPayloads,

    /// Which endorser block each recorded certification is owed, so a fetch
    /// that failed can be tried again.
    outstanding: BTreeMap<u64, AnnouncedEndorserBlock>,
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
                NextResponse::RollForward(content, tip) => {
                    let header = to_traverse(&content).or_panic()?;
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

                    self.held
                        .push((header.hash(), HeldHeader::Header(content.clone())));

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

        // A fetch that failed in an earlier pass leaves its endorser block
        // owed. It is fetched here, before the drain, so a second failure
        // retries with the points still pending.
        self.fetch_outstanding(stage).await?;

        let points = self.chain.take_pending();

        // A rollback past the headers just taken is answered from the store, so
        // only the last is kept, as the parent of the next.
        self.held.drain(..self.held.len().saturating_sub(1));

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
    /// empty body rather than an error, and `EndorserBlockBody::decode_announced`
    /// refuses that answer by the hash the announcement committed to. It
    /// surfaces here as a retry rather than as a block with no transactions.
    ///
    /// A failed fetch drops the Leios connection and builds a new one, because
    /// a session that has ended cannot serve the next request either. The
    /// ending itself is reported, so the fetch that was waiting on it returns
    /// at once rather than spending its remaining budget on a connection that
    /// is gone.
    async fn follow_endorsement(
        &mut self,
        header: &MultiEraHeader<'_>,
        stage: &mut Stage,
    ) -> Result<(), WorkerError> {
        if self.leios.is_none() {
            return Ok(());
        }

        // Stopping is the only honest answer to a certificate that names no
        // announcement. Continuing would apply a certifying block with no
        // transactions and leave the ledger short with no error anywhere, which
        // is the whole failure this stage exists to prevent, and skipping the
        // block is the same thing one layer up.
        let certified = certified_by(&self.held, header).map_err(|err| {
            error!(
                %err,
                slot = header.slot(),
                "the certificate on this block names no announcement"
            );
            WorkerError::Panic
        })?;

        let owed = record_certification(
            &mut self.payloads,
            &mut self.outstanding,
            header.slot(),
            certified,
        );

        if !owed {
            return Ok(());
        }

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

        let intersection = ChainPoint::from(intersection);

        let (leios, held) = match &stage.leios_peer_address {
            None => (None, Vec::new()),
            Some(address) => {
                info!(address, "connecting to a Leios peer for endorser blocks");

                let client = LeiosClient::new(address, stage.network_magic).or_panic()?;
                let held = held_at(&stage.wal, &intersection, async |at| {
                    peer_block(&mut peer_session, at).await
                })
                .await?;

                (Some(client), held)
            }
        };

        let worker = Self {
            peer_session,
            chain: ChainFragment::start(intersection),
            leios,
            held,
            payloads: PendingPayloads::default(),
            outstanding: BTreeMap::new(),
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
                // A rollback invalidates anything fetched for a block that is no
                // longer on the chain, and the next header names the block
                // stored at the rollback point as its parent. The endorser
                // transactions of a rolled back block are undone with it,
                // because they are part of the block bytes the log holds.
                self.payloads = PendingPayloads::default();
                self.outstanding.clear();

                if self.leios.is_some() {
                    let peer = &mut self.peer_session;
                    self.held =
                        held_at(&stage.wal, &point, async |at| peer_block(peer, at).await).await?;
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

#[cfg(test)]
mod endorser_retry;

#[cfg(test)]
mod tests {
    use dolos_core::import::{seed_wal_from_state, WalSeed};
    use dolos_core::{StateStore as _, StateWriter as _};
    use pallas::ledger::traverse::MultiEraBlock;
    use pallas::network::facades::PeerServer;

    use super::*;
    use crate::adapters::StateStoreBackend;

    fn block(hex_text: &str) -> Vec<u8> {
        hex::decode(hex_text.trim()).unwrap()
    }

    fn announce_with_txs() -> Vec<u8> {
        block(include_str!(
            "../../test_data/musashi-w36/ranking-announce-with-txs.block"
        ))
    }

    fn announce_quiet() -> Vec<u8> {
        block(include_str!(
            "../../test_data/musashi-w36/ranking-announce-quiet.block"
        ))
    }

    fn silent() -> Vec<u8> {
        block(include_str!("../../test_data/dijkstra-quiet.block"))
    }

    fn certify_only() -> Vec<u8> {
        block(include_str!("../../test_data/dijkstra-certify-only.block"))
    }

    fn certifying() -> Vec<u8> {
        block(include_str!("../../test_data/dijkstra-certifying.block"))
    }

    fn pre_leios() -> Vec<u8> {
        block(include_str!("../../test_data/conway.block"))
    }

    /// The two Leios fields a header carries, read straight off the bytes so a
    /// sequence built from these fixtures rests on what they say rather than on
    /// what their names suggest.
    fn leios_fields(cbor: &[u8]) -> (Option<bool>, bool) {
        let decoded = MultiEraBlock::decode(cbor).unwrap();
        let header = decoded.header();

        (
            header.block_body_contains_leios_cert(),
            header.eb_announcement().is_some(),
        )
    }

    /// One fixture announces without certifying, two do both, one certifies
    /// without announcing, one does neither, and one is of an era that has no
    /// such fields at all. The sequences below are built from these roles.
    #[test]
    fn each_fixture_carries_the_leios_fields_the_sequences_rest_on() {
        assert_eq!(leios_fields(&announce_with_txs()), (Some(false), true));
        assert_eq!(leios_fields(&announce_quiet()), (Some(true), true));
        assert_eq!(leios_fields(&certify_only()), (Some(true), false));
        assert_eq!(leios_fields(&certifying()), (Some(true), true));
        assert_eq!(leios_fields(&silent()), (Some(false), false));
        assert_eq!(leios_fields(&pre_leios()), (None, false));
    }

    /// Slot 1209593, the parent of the next fixture, neither announcing nor
    /// certifying.
    fn epoch_before() -> Vec<u8> {
        block(include_str!(
            "../../test_data/musashi-w36/epoch-boundary-before.block"
        ))
    }

    /// Slot 1209609, neither announcing nor certifying.
    fn epoch_after() -> Vec<u8> {
        block(include_str!(
            "../../test_data/musashi-w36/epoch-boundary-after.block"
        ))
    }

    /// A block held as the write ahead log holds it.
    fn stored(cbor: &[u8]) -> (BlockHash, HeldHeader) {
        let hash = MultiEraBlock::decode(cbor).unwrap().hash();
        (hash, HeldHeader::Block(Arc::new(cbor.to_vec())))
    }

    /// A block's header held as chainsync delivers it.
    fn delivered(cbor: &[u8]) -> (BlockHash, HeldHeader) {
        let decoded = MultiEraBlock::decode(cbor).unwrap();
        let header = decoded.header();

        let content = HeaderContent {
            variant: 7,
            byron_prefix: None,
            cbor: header.cbor().to_vec(),
        };

        (header.hash(), HeldHeader::Header(content))
    }

    fn certified_after(
        held: &[(BlockHash, HeldHeader)],
        child: &[u8],
    ) -> Result<Option<AnnouncedEndorserBlock>, endorser::Error> {
        let decoded = MultiEraBlock::decode(child).unwrap();
        certified_by(held, &decoded.header())
    }

    /// A header whose parent is held and which certifies nothing owes no fetch,
    /// although an announcing header is held beside it.
    #[test]
    fn a_header_that_certifies_nothing_owes_no_fetch() {
        let held = [stored(&announce_with_txs()), stored(&epoch_before())];

        let certified = certified_after(&held, &epoch_after()).unwrap();
        assert_eq!(certified, None);

        let mut payloads = PendingPayloads::default();
        let mut outstanding = BTreeMap::new();
        assert!(!record_certification(
            &mut payloads,
            &mut outstanding,
            1_209_609,
            certified
        ));
        assert!(payloads.is_empty());
    }

    /// A certificate owes the fetch of its parent's announcement, at the
    /// parent's slot, whether the parent was delivered by chainsync or read back
    /// from the store, and whatever else is held after it.
    #[test]
    fn a_certificate_owes_the_fetch_of_its_parents_announcement() {
        let parent = announce_with_txs();
        let (hash, size) = MultiEraBlock::decode(&parent)
            .unwrap()
            .header()
            .eb_announcement()
            .map(|eb| (eb.eb_hash, eb.eb_size))
            .expect("fixture precondition");

        for held in [
            [delivered(&parent), stored(&epoch_before())],
            [stored(&parent), stored(&epoch_before())],
        ] {
            let certified = certified_after(&held, &announce_quiet()).unwrap();

            assert_eq!(
                certified,
                Some(AnnouncedEndorserBlock {
                    slot: 1_202_730,
                    hash,
                    size,
                })
            );

            let mut payloads = PendingPayloads::default();
            let mut outstanding = BTreeMap::new();
            assert!(record_certification(
                &mut payloads,
                &mut outstanding,
                1_202_752,
                certified
            ));
            assert_eq!(payloads.outstanding(), vec![1_202_752]);
        }
    }

    /// A certificate whose parent is not held is refused, since the
    /// announcement it names cannot be read.
    #[test]
    fn a_certificate_whose_parent_is_not_held_is_refused() {
        let held = [stored(&announce_quiet()), stored(&epoch_before())];

        let refused = certified_after(&held, &announce_quiet());

        assert!(
            matches!(
                refused,
                Err(endorser::Error::NotParent {
                    slot: 1_202_752,
                    ..
                })
            ),
            "{refused:?}"
        );
    }

    fn point_of(cbor: &[u8]) -> ChainPoint {
        let decoded = MultiEraBlock::decode(cbor).unwrap();
        ChainPoint::Specific(decoded.slot(), decoded.hash())
    }

    fn hashes(held: &[(BlockHash, HeldHeader)]) -> Vec<BlockHash> {
        held.iter().map(|(hash, _)| *hash).collect()
    }

    /// What `held_at` holds at `point` against a peer that serves `served`,
    /// with each point it asked that peer for.
    async fn held_from(
        wal: &WalAdapter,
        point: &ChainPoint,
        served: &[u8],
    ) -> (Vec<(BlockHash, HeldHeader)>, Vec<Point>) {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let mut asked = Vec::new();

        let held = held_at(wal, point, async |at| {
            asked.push(at);
            Ok(served.to_vec())
        })
        .await
        .unwrap();

        (held, asked)
    }

    /// A log seeded from the state cursor holds no block at the cursor, so the
    /// parent is the peer's block there, and the certificate in the next header
    /// names its announcement.
    #[tokio::test]
    async fn a_log_seeded_from_the_state_holds_the_peers_block_as_the_parent() {
        let parent = announce_with_txs();
        let point = point_of(&parent);
        let wal = WalAdapter::in_memory().unwrap();
        let state = StateStoreBackend::in_memory().unwrap();

        let writer = state.start_writer().unwrap();
        writer.set_cursor(point.clone()).unwrap();
        writer.commit().unwrap();

        assert_eq!(
            seed_wal_from_state(&state, &wal).unwrap(),
            WalSeed::Seeded(point.clone())
        );

        let (held, asked) = held_from(&wal, &point, &parent).await;

        assert_eq!(
            certified_after(&held, &announce_quiet())
                .unwrap()
                .map(|eb| eb.slot),
            Some(1_202_730)
        );
        assert_eq!(asked, vec![Point::try_from(point).unwrap()]);
    }

    /// A block logged at the point is held from the log, and the peer is asked
    /// for nothing.
    #[tokio::test]
    async fn a_block_logged_at_the_point_is_held_without_asking_the_peer() {
        let wal = WalAdapter::in_memory().unwrap();
        let reset = point_of(&epoch_before());
        let cbor = epoch_after();
        let point = point_of(&cbor);

        wal.reset_to(&reset).unwrap();
        wal.append_entries(vec![(
            point.clone(),
            LogValue {
                block: cbor,
                delta: Vec::new(),
                inputs: Default::default(),
            },
        )])
        .unwrap();

        let (held, asked) = held_from(&wal, &point, &announce_quiet()).await;

        assert_eq!(hashes(&held), vec![stored(&epoch_after()).0]);
        assert_eq!(asked, Vec::<Point>::new());
    }

    /// A log reset to the origin holds no parent, since no block is there, and
    /// the peer is asked for nothing.
    #[tokio::test]
    async fn a_log_reset_to_the_origin_holds_no_parent() {
        let wal = WalAdapter::in_memory().unwrap();

        wal.reset_to(&ChainPoint::Origin).unwrap();

        let (held, asked) = held_from(&wal, &ChainPoint::Origin, &announce_quiet()).await;

        assert_eq!(hashes(&held), Vec::<BlockHash>::new());
        assert_eq!(asked, Vec::<Point>::new());
    }

    /// The parent at a reset point is the block a blockfetch server answers for
    /// that point.
    #[tokio::test]
    async fn the_parent_at_a_reset_point_is_fetched_over_blockfetch() {
        let parent = announce_with_txs();
        let point = point_of(&parent);
        let wal = WalAdapter::in_memory().unwrap();

        wal.reset_to(&point).unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let served = parent.clone();

        let server = tokio::spawn(async move {
            let mut server = PeerServer::accept(&listener, 164).await.unwrap();
            let request = server.blockfetch().recv_while_idle().await.unwrap();
            server
                .blockfetch()
                .send_block_range(vec![served])
                .await
                .unwrap();
            request.map(|request| request.0)
        });

        let mut peer = PeerClient::connect(&address, 164).await.unwrap();
        let held = held_at(&wal, &point, async |at| peer_block(&mut peer, at).await)
            .await
            .unwrap();
        peer.abort().await;

        assert_eq!(
            certified_after(&held, &announce_quiet())
                .unwrap()
                .map(|eb| eb.slot),
            Some(1_202_730)
        );

        let at = Point::try_from(point).unwrap();
        assert_eq!(server.await.unwrap(), Some((at.clone(), at)));
    }
}
