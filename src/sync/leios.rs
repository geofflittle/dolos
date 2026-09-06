//! Following a chain whose ranking blocks certify Leios endorser blocks.
//!
//! On such a chain a ranking block header announces an endorser block and a
//! later ranking block certifies that announcement. The certified endorser
//! block carries the transactions, and the certifying ranking block carries
//! none of its own. A client that follows chainsync and blockfetch alone sees
//! the ranking chain and misses most of the ledger, which shows up much later
//! as an input it cannot resolve rather than as an error where the payload was
//! dropped.
//!
//! This module closes that gap for the pull stage. It fetches each certified
//! endorser block over the leios-fetch mini-protocol from a configured Leios
//! peer, and resolves the certifying ranking block against it, so the block the
//! rest of the pipeline receives already carries the endorsed transactions.
//! That is the same shape a node serves its own local clients, so nothing
//! downstream of the pull stage has to learn about Leios.
//!
//! Two refusals here are the point of the module rather than defensive extras.
//! leios-fetch has no not-found reply, so an endorser block the peer does not
//! hold arrives as a well formed empty body, and only the size the announcement
//! committed to separates that from an endorser block that really is empty.
//! And a payload is attached to a block by slot, never by arrival order, so a
//! short or reordered block batch cannot silently move one block's transactions
//! onto another.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::time::Duration;

use pallas::crypto::hash::Hash;

use pallas::ledger::traverse::leios::{
    resolve_certified_block, AnnouncedEndorserBlock, EndorserBlockBody, PendingAnnouncement,
};
use pallas::ledger::traverse::{Era, MultiEraBlock, MultiEraTx};
use pallas::network2::behavior::initiator::{
    Config as HandshakeConfig, HandshakeBehavior, InitiatorBehavior, InitiatorCommand,
    InitiatorEvent,
};
use pallas::network2::behavior::AnyMessage;
use pallas::network2::interface::TcpInterface;
use pallas::network2::protocol::handshake::n2n::{VersionTable, LEIOS_MIN_VERSION};
use pallas::network2::protocol::{leiosfetch, EbId, Point};
use pallas::network2::{Manager, PeerId};
use tokio::time::interval;
use tracing::{debug, info, warn};

use dolos_core::{BlockBody, RawBlock};

/// Transactions asked for in one leios-fetch request.
///
/// The selector addresses transactions in 64 wide windows but carries as many
/// windows as it likes, so this is a choice rather than a protocol limit. The
/// relay caps a response, so a request is asked as wide as the largest endorser
/// block on the chain and the relay's own cap decides the paging. That way the
/// client makes exactly as many round trips as the relay forces and never more,
/// which at one round trip per request is the whole cost of a fetch.
///
/// A short reply is asked for again rather than taken as the end of the
/// endorser block, and the transactions it did deliver are checked against the
/// entries they are filed under before any of them is kept.
const TXS_PER_REQUEST: usize = 8192;

/// How long to wait for one endorser block before giving up and letting the
/// stage retry, which also reconnects.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// How often the real transport asks the network behavior to do housekeeping
/// when nothing else has prompted it.
///
/// This is a keepalive floor and nothing more. A leios-fetch request is put on
/// the wire by housekeeping, so a client that issues a request and then waits
/// for the tick pays this whole period for every round trip. Requests are
/// flushed the moment they are issued, and this tick only covers the case where
/// no request is outstanding at all.
const HOUSEKEEPING_TICK: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("leios peer address {0} is not a valid host and port")]
    BadAddress(String),

    #[error("leios peer negotiated version {version}, below the {minimum} that carries Leios")]
    PreLeiosPeer { version: u64, minimum: u64 },

    #[error("timed out fetching endorser block {hash} announced at slot {slot}")]
    Timeout { slot: u64, hash: String },

    #[error("the leios peer answered a request for endorser block {hash} with nothing")]
    NoReply { hash: String },

    #[error(transparent)]
    Endorser(#[from] pallas::ledger::traverse::leios::Error),

    #[error("block cbor from the peer does not decode: {0}")]
    BadBlock(String),

    #[error(
        "endorser block {hash} certified at slot {slot} was fetched and no block of that slot \
         arrived to carry it"
    )]
    Undelivered { slot: u64, hash: String },

    #[error(
        "the block at slot {slot} certifies an endorser block that was never fetched, so \
         applying it would apply an empty block where a whole endorser block belongs"
    )]
    Unfetched { slot: u64 },

    #[error("a transaction of endorser block {hash} does not decode: {reason}")]
    BadEndorserTx { hash: String, reason: String },

    #[error(
        "an endorser block named {named} transactions, {repeated} of them already applied and \
         {spliced} spliced, and those do not add up"
    )]
    ContributionDoesNotAddUp {
        named: usize,
        repeated: usize,
        spliced: usize,
    },

    #[error(
        "leios-fetch answered a request for {asked} transactions of endorser block {hash} with \
         {delivered}, and the delivery is not the first {delivered} that were asked for: \
         position {index} carries {found} where the endorser block names {named}"
    )]
    Misaligned {
        hash: String,
        asked: usize,
        delivered: usize,
        index: usize,
        named: String,
        found: String,
    },
}

/// What the certification walk knew at a stored point, and the evidence for it.
///
/// The state alone would not say whether it was read off the chain or fallen
/// back to, so the block that settled it is carried beside it and an operator
/// reading the log can see which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumedWalk {
    pub state: PendingAnnouncement,
    /// Blocks read back before the question was settled.
    pub scanned: usize,
    /// Slot of the block that settled it, when one did.
    pub settled_at: Option<u64>,
}

/// Reconstructs the certification walk from stored blocks, read newest first.
///
/// A follower resuming from its own stored point has to know whether an
/// announcement is waiting for a certificate, and nothing on the chain writes
/// that down. It is derived from the blocks the follower already has rather
/// than saved separately, because a separately saved copy can disagree with the
/// cursor after a crash or a rollback and nothing would notice.
///
/// The walk back stops at the first block that settles the question:
///
/// - a header that announces, whatever else it does: that announcement is
///   waiting, because no later block announced or certified;
/// - a header that certifies and announces nothing: nothing is waiting, because
///   it consumed what came before;
/// - a header of an era with no endorsement layer: nothing is waiting, because
///   an announcement cannot cross forward into an era that has none.
///
/// Reading to the end of the stored blocks without meeting any of the three
/// settles nothing, and the answer says so. Answering "nothing is waiting"
/// there would be reading an absence as a fact, and it is the exact absence
/// that loses a whole endorser block at the next certificate.
/// The iterator is consumed lazily and abandoned at the block that settles the
/// question, which on a chain in its endorsement era is almost always the first
/// one read. Draining it first would read every block the retention holds to
/// answer a question one block answers.
pub fn resume_walk(blocks: impl Iterator<Item = RawBlock>) -> Result<ResumedWalk, Error> {
    let mut scanned = 0usize;

    for cbor in blocks {
        scanned += 1;

        let block = MultiEraBlock::decode(&cbor).map_err(|e| Error::BadBlock(e.to_string()))?;
        let header = block.header();
        let slot = header.slot();

        if let Some(announcement) = header.leios_announcement() {
            return Ok(ResumedWalk {
                state: PendingAnnouncement::Waiting(AnnouncedEndorserBlock {
                    slot,
                    hash: announcement.announced_eb,
                    size: announcement.announced_eb_size,
                }),
                scanned,
                settled_at: Some(slot),
            });
        }

        match header.leios_certified() {
            // An era with no endorsement layer at all.
            None => {
                return Ok(ResumedWalk {
                    state: PendingAnnouncement::Nothing,
                    scanned,
                    settled_at: Some(slot),
                })
            }
            Some(true) => {
                return Ok(ResumedWalk {
                    state: PendingAnnouncement::Nothing,
                    scanned,
                    settled_at: Some(slot),
                })
            }
            Some(false) => continue,
        }
    }

    Ok(ResumedWalk {
        state: PendingAnnouncement::Unknown,
        scanned,
        settled_at: None,
    })
}

/// A fetched, verified endorser block waiting for the ranking block that
/// certified it.
#[derive(Debug, Clone)]
pub struct CertifiedPayload {
    pub endorser_block: AnnouncedEndorserBlock,
    /// The transactions in endorser block order, unwrapped from their
    /// leios-fetch byte string envelopes.
    pub txs: Vec<Vec<u8>>,
}

/// How many blocks back the follower remembers which transactions it applied.
///
/// An endorser block is announced by the very ranking block that certifies the
/// one before it, so its producer built it from a mempool that could not yet
/// have dropped the previous endorser block's transactions. Two consecutive
/// certified endorser blocks overlapping is therefore ordinary rather than
/// exceptional, and the follower has to apply each of those transactions once.
///
/// The depth is in blocks because blocks are what the log walks back through,
/// and it is set from a measurement rather than a guess. Over the 37825 ranking
/// blocks of the Musashi chain between slot 1742539 and the tip, 7421 of them
/// certified an endorser block, so a certification lands about every five
/// blocks. Those endorser blocks carried 8213496 transactions of which 2411
/// were repeats, 2410 of them repeated by the very next certified endorser
/// block and the furthest by the seventh. Seven certifications is about thirty
/// six blocks, so this is seven times the furthest repeat measured.
///
/// It is bounded at all because the ids of a whole chain do not fit in memory.
/// Origin to tip is about fifteen million endorser transactions, and this holds
/// on the order of a hundred and fifty thousand ids, about ten megabytes.
///
/// An overlap reaching further back than this is not silently mishandled. The
/// repeated transaction is spliced in, the ledger meets an input that is
/// already spent, and the follower stops on it, which is exactly the behaviour
/// before this window existed.
const APPLIED_WINDOW_BLOCKS: usize = 256;

/// What one certified endorser block contributed to the block that certified it.
///
/// The three counts are one fact in three parts and are held together so a
/// caller cannot read one without the others. Every transaction the endorser
/// block named was either spliced into the certifying block or already applied
/// by an earlier block, so a triple that does not add up is refused rather than
/// constructed. Reporting only the spliced count would say a short block was a
/// small one, and reporting only the named count would hide that anything was
/// dropped at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Contribution {
    named: usize,
    repeated: usize,
    spliced: usize,
}

impl Contribution {
    pub fn new(named: usize, repeated: usize, spliced: usize) -> Result<Self, Error> {
        if repeated + spliced != named {
            return Err(Error::ContributionDoesNotAddUp {
                named,
                repeated,
                spliced,
            });
        }

        Ok(Self {
            named,
            repeated,
            spliced,
        })
    }

    /// Transactions the endorser block named.
    pub fn named(&self) -> usize {
        self.named
    }

    /// Transactions an earlier block had already applied.
    pub fn repeated(&self) -> usize {
        self.repeated
    }

    /// Transactions spliced into the certifying block.
    pub fn spliced(&self) -> usize {
        self.spliced
    }
}

/// The transaction ids the blocks just behind the tip carried.
///
/// This is the follower's answer to "have I applied this transaction already".
/// It holds one set per block rather than one flat set so the oldest block's
/// ids leave together when the window moves on, and so a rollback can be
/// answered by rebuilding from the stored blocks rather than by unpicking a
/// merged set that no longer says where anything came from.
///
/// Both kinds of block are recorded. A ranking block's own transactions and a
/// certified endorser block's transactions are disjoint on the chain measured
/// so far, but nothing in the protocol says they must be, and recording both
/// costs nothing while covering the case where they are not.
#[derive(Debug, Default)]
pub struct AppliedTxWindow {
    /// Oldest first, newest last, at most [`APPLIED_WINDOW_BLOCKS`] entries.
    blocks: VecDeque<(u64, HashSet<Hash<32>>)>,
}

impl AppliedTxWindow {
    /// Whether some block still inside the window carried this transaction.
    pub fn contains(&self, id: &Hash<32>) -> bool {
        self.blocks.iter().any(|(_, ids)| ids.contains(id))
    }

    /// Records the transactions one block carried, dropping the oldest block
    /// once the window is full.
    pub fn record(&mut self, slot: u64, ids: HashSet<Hash<32>>) {
        self.blocks.push_back((slot, ids));

        while self.blocks.len() > APPLIED_WINDOW_BLOCKS {
            self.blocks.pop_front();
        }
    }

    /// Blocks the window is holding.
    pub fn depth(&self) -> usize {
        self.blocks.len()
    }

    /// Transaction ids the window is holding, counted with repeats across
    /// blocks, so a caller can see the memory it is paying for.
    pub fn ids(&self) -> usize {
        self.blocks.iter().map(|(_, ids)| ids.len()).sum()
    }
}

/// Why a window read back from stored blocks stopped where it did.
///
/// A depth on its own cannot say whether the follower remembers as far back as
/// it means to or only as far as its log goes, and those are different
/// positions to resume from. The reason is carried beside the count so the
/// caller never has to read a small number as either one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowReach {
    /// The window filled to its intended depth.
    Full { blocks: usize },
    /// The stored blocks ran out first, and this is how many there were.
    ShortLog { blocks: usize },
}

/// A window rebuilt from stored blocks, and the evidence for how deep it is.
#[derive(Debug)]
pub struct ResumedWindow {
    pub window: AppliedTxWindow,
    pub reach: WindowReach,
}

/// Rebuilds the applied transaction window from stored blocks, read newest
/// first.
///
/// A follower that resumes with an empty memory reapplies the first repeated
/// transaction it meets and stops on an input that is already spent, which is
/// the failure this window exists to prevent, moved from the first sync to the
/// first restart. The memory is derived from the blocks the follower already
/// stored rather than saved beside the cursor, for the same reason the
/// certification walk is: a separately saved copy can disagree with the cursor
/// after a crash or a rollback and nothing would notice.
///
/// The blocks arrive newest first and are recorded oldest first, so the window
/// ends up in the same order a forward sync would have built it in.
pub fn resume_window(blocks: impl Iterator<Item = RawBlock>) -> Result<ResumedWindow, Error> {
    let mut newest_first = Vec::with_capacity(APPLIED_WINDOW_BLOCKS);

    for cbor in blocks.take(APPLIED_WINDOW_BLOCKS) {
        let block = MultiEraBlock::decode(&cbor).map_err(|e| Error::BadBlock(e.to_string()))?;

        newest_first.push((block.slot(), block.txs().iter().map(|tx| tx.hash()).collect()));
    }

    let reach = if newest_first.len() == APPLIED_WINDOW_BLOCKS {
        WindowReach::Full {
            blocks: newest_first.len(),
        }
    } else {
        WindowReach::ShortLog {
            blocks: newest_first.len(),
        }
    };

    let mut window = AppliedTxWindow::default();

    for (slot, ids) in newest_first.into_iter().rev() {
        window.record(slot, ids);
    }

    Ok(ResumedWindow { window, reach })
}

/// The transaction id of every transaction an endorser block delivered, in
/// endorser block order.
///
/// The id is taken by decoding each transaction rather than by hashing the
/// bytes here, so it is the same id the ledger will key the transaction's
/// outputs by. A transaction that does not decode is refused: it would
/// otherwise be a transaction with no id, which no window can answer for and
/// which would be spliced in unchecked.
fn endorser_tx_ids(payload: &CertifiedPayload) -> Result<Vec<Hash<32>>, Error> {
    payload
        .txs
        .iter()
        .map(|tx| {
            MultiEraTx::decode_for_era(Era::Dijkstra, tx)
                .map(|tx| tx.hash())
                .map_err(|e| Error::BadEndorserTx {
                    hash: payload.endorser_block.hash.to_string(),
                    reason: e.to_string(),
                })
        })
        .collect()
}

/// What the walk owes each certifying ranking block, keyed by that block's slot.
///
/// Every certification the walk sees is recorded here before its endorser block
/// is fetched, and the payload is filled in when the fetch returns. Recording
/// the debt first is what makes a fetch that never returned visible: without it
/// a block that certifies and has no payload is indistinguishable from a block
/// that certifies nothing, and both would be passed through carrying no
/// transactions at all.
///
/// Keying by slot rather than by position is what stops a short or reordered
/// block batch from moving one block's transactions onto another.
#[derive(Debug, Default)]
pub struct PendingPayloads(BTreeMap<u64, Option<CertifiedPayload>>);

impl PendingPayloads {
    /// Records that the block at this slot certifies an endorser block, before
    /// anything has been fetched for it.
    pub fn expect(&mut self, certifying_slot: u64) {
        self.0.entry(certifying_slot).or_insert(None);
    }

    /// Fills in the payload for a certification already recorded.
    pub fn deliver(&mut self, certifying_slot: u64, payload: CertifiedPayload) {
        self.0.insert(certifying_slot, Some(payload));
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// The certifications whose endorser block has not been fetched, oldest
    /// first, so a caller can go and get them.
    pub fn outstanding(&self) -> Vec<u64> {
        self.0
            .iter()
            .filter(|(_, payload)| payload.is_none())
            .map(|(slot, _)| *slot)
            .collect()
    }

    /// Resolves each block that certifies an endorser block against the payload
    /// fetched for it, leaving every other block byte identical.
    ///
    /// A block whose certification was recorded and whose payload never arrived
    /// is refused. That block carries no transactions of its own, so passing it
    /// through would apply an empty block where a whole endorser block belonged
    /// and leave the ledger short with nothing said. The cost of that silence is
    /// paid thousands of slots later, by a transaction spending an output the
    /// follower never saw, with nothing at that point to say where it went.
    ///
    /// Every payload this consumes is removed. A payload left over is an
    /// endorser block that was fetched and never applied, which is refused
    /// rather than dropped, for the same reason in the other direction.
    ///
    /// A transaction a block still inside `window` already carried is left out
    /// of the block this builds. Two consecutive certified endorser blocks
    /// overlap as a matter of course, and splicing a repeated transaction in a
    /// second time spends an input that is already spent, which the ledger
    /// meets as an input it cannot find. The window is updated with every
    /// transaction each block carries, including the ones left out, because
    /// after this block they are all applied.
    pub fn apply(
        &mut self,
        window: &mut AppliedTxWindow,
        blocks: Vec<BlockBody>,
    ) -> Result<Vec<BlockBody>, Error> {
        let mut out = Vec::with_capacity(blocks.len());

        for cbor in blocks {
            let (slot, own) = {
                let block =
                    MultiEraBlock::decode(&cbor).map_err(|e| Error::BadBlock(e.to_string()))?;

                let ids: HashSet<Hash<32>> = block.txs().iter().map(|tx| tx.hash()).collect();

                (block.slot(), ids)
            };

            match self.0.remove(&slot) {
                None => {
                    window.record(slot, own);
                    out.push(cbor);
                }
                Some(None) => {
                    return Err(Error::Unfetched { slot });
                }
                Some(Some(payload)) => {
                    let named = endorser_tx_ids(&payload)?;

                    let keep: Vec<&[u8]> = payload
                        .txs
                        .iter()
                        .zip(named.iter())
                        .filter(|(_, id)| !window.contains(id))
                        .map(|(tx, _)| tx.as_slice())
                        .collect();

                    let repeated = payload.txs.len() - keep.len();
                    let contribution =
                        Contribution::new(payload.txs.len(), repeated, keep.len())?;

                    let resolved = resolve_certified_block(&cbor, &keep)?;

                    info!(
                        slot,
                        eb = %payload.endorser_block.hash,
                        named = contribution.named(),
                        repeated = contribution.repeated(),
                        spliced = contribution.spliced(),
                        "applied the transactions of a certified endorser block"
                    );

                    window.record(slot, named.into_iter().collect());
                    out.push(resolved);
                }
            }
        }

        Ok(out)
    }

    /// Refuses anything a completed batch left behind: a payload no block
    /// claimed, and a certification no block came for.
    pub fn refuse_undelivered(&self) -> Result<(), Error> {
        match self.0.iter().next() {
            None => Ok(()),
            Some((slot, None)) => Err(Error::Unfetched { slot: *slot }),
            Some((slot, Some(payload))) => Err(Error::Undelivered {
                slot: *slot,
                hash: payload.endorser_block.hash.to_string(),
            }),
        }
    }
}

/// The wire a leios-fetch client drives.
///
/// The client decides what to ask for next, the transport owns only the
/// connection. The split is here so the paging can be driven against a relay
/// that answers exactly what it was asked, which is the only way to tell how
/// many round trips a fetch really costs and whether a short reply is handled
/// or quietly taken as the end of the block.
#[async_trait::async_trait(?Send)]
pub trait LeiosTransport {
    /// Sends a request to the peer.
    ///
    /// The request must be on the wire when this returns. The behavior serves
    /// one leios-fetch request per peer at a time, so a fetch is a chain of
    /// round trips, and a request left waiting for a timer costs that timer's
    /// period on every link of the chain.
    fn issue(&mut self, command: InitiatorCommand);

    /// Waits for the next event from the peer.
    async fn next_event(&mut self) -> InitiatorEvent;
}

/// The real transport: one TCP connection driven by the network manager.
pub struct ManagerTransport {
    network: Manager<TcpInterface<AnyMessage>, InitiatorBehavior, AnyMessage>,
    ticker: tokio::time::Interval,
}

#[async_trait::async_trait(?Send)]
impl LeiosTransport for ManagerTransport {
    fn issue(&mut self, command: InitiatorCommand) {
        self.network.execute(command);
    }

    async fn next_event(&mut self) -> InitiatorEvent {
        loop {
            tokio::select! {
                _ = self.ticker.tick() => {
                    self.network.execute(InitiatorCommand::Housekeeping);
                }
                event = self.network.poll_next() => {
                    // None means the manager took an internal step, such as
                    // dispatching a queued request to the interface, and has no
                    // event for the client yet.
                    if let Some(event) = event {
                        return event;
                    }
                }
            }
        }
    }
}

/// A leios-fetch client against one peer.
///
/// leios-fetch lives in the newer networking stack, which the chainsync and
/// blockfetch client of this stage does not speak, so this is a second
/// connection to a peer that does.
pub struct LeiosClient<T = ManagerTransport> {
    transport: T,
    peer: Option<PeerId>,
    address: String,
    /// Transactions asked for in one leios-fetch request.
    window: usize,
    /// How long one endorser block may take before the stage retries.
    timeout: Duration,
}

impl LeiosClient<ManagerTransport> {
    pub fn new(address: &str, network_magic: u64) -> Result<Self, Error> {
        let behavior = InitiatorBehavior {
            handshake: HandshakeBehavior::new(HandshakeConfig {
                supported_version: VersionTable::v11_and_above_with_query(network_magic, false),
            }),
            ..Default::default()
        };

        let mut network = Manager::new(TcpInterface::new(), behavior);

        let peer = address
            .parse()
            .map_err(|_| Error::BadAddress(address.to_string()))?;

        network.execute(InitiatorCommand::IncludePeer(peer));

        let transport = ManagerTransport {
            network,
            ticker: interval(HOUSEKEEPING_TICK),
        };

        Ok(Self::with_transport(
            transport,
            address.to_string(),
            TXS_PER_REQUEST,
            FETCH_TIMEOUT,
        ))
    }
}

impl<T: LeiosTransport> LeiosClient<T> {
    pub fn with_transport(
        transport: T,
        address: String,
        window: usize,
        timeout: Duration,
    ) -> Self {
        Self {
            transport,
            peer: None,
            address,
            window,
            timeout,
        }
    }

    /// Fetches one endorser block whole and returns its transactions, in
    /// endorser block order, unwrapped from their byte string envelopes.
    ///
    /// The body is checked against the size its announcement committed to, and
    /// every transaction is checked against the body entry that names it, on
    /// count, length and hash, before any of them is returned. So the answer is
    /// either the whole endorser block or an error saying what was wrong with
    /// it, never a prefix and never an empty list standing in for a failure.
    pub async fn fetch(&mut self, eb: &AnnouncedEndorserBlock) -> Result<Vec<Vec<u8>>, Error> {
        let point: EbId = Point::Specific(eb.slot, eb.hash.to_vec());
        let announcement = pallas::ledger::primitives::dijkstra::LeiosAnnouncement {
            announced_eb: eb.hash,
            announced_eb_size: eb.size,
        };

        let started = tokio::time::Instant::now();

        let mut body: Option<EndorserBlockBody> = None;
        let mut txs: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
        let mut inflight: Vec<usize> = Vec::new();
        let mut asked_for_body = false;
        let mut round_trips = 0usize;

        loop {
            // Issue the one request the state allows. Only one is outstanding
            // at a time, because the behavior serves one leios-fetch request
            // per peer and a second would wait behind the first with nothing
            // saying so.
            if let Some(pid) = self.peer.clone() {
                let issued = match body.as_ref() {
                    None => {
                        if asked_for_body {
                            false
                        } else {
                            asked_for_body = true;
                            self.transport
                                .issue(InitiatorCommand::FetchEb(pid, point.clone()));
                            true
                        }
                    }
                    Some(decoded) if inflight.is_empty() => {
                        let want: Vec<usize> = (0..decoded.len())
                            .filter(|i| !txs.contains_key(i))
                            .take(self.window)
                            .collect();

                        if want.is_empty() {
                            let wire: Vec<Vec<u8>> = txs.into_values().collect();

                            debug!(
                                eb = %eb.hash,
                                round_trips,
                                txs = wire.len(),
                                elapsed_ms = started.elapsed().as_millis() as u64,
                                "endorser block fetched whole"
                            );

                            return finish(decoded, wire);
                        }

                        inflight = want.clone();
                        self.transport.issue(InitiatorCommand::FetchEbTxs(
                            pid,
                            point.clone(),
                            leiosfetch::Bitmaps::from_indices(want),
                        ));
                        true
                    }
                    _ => false,
                };

                if issued {
                    round_trips += 1;
                }
            }

            let remaining = self.timeout.checked_sub(started.elapsed()).ok_or_else(|| {
                Error::Timeout {
                    slot: eb.slot,
                    hash: eb.hash.to_string(),
                }
            })?;

            let event = tokio::time::timeout(remaining, self.transport.next_event())
                .await
                .map_err(|_| Error::Timeout {
                    slot: eb.slot,
                    hash: eb.hash.to_string(),
                })?;

            match event {
                InitiatorEvent::PeerInitialized(pid, (version, _)) => {
                    if version < LEIOS_MIN_VERSION {
                        return Err(Error::PreLeiosPeer {
                            version: version as u64,
                            minimum: LEIOS_MIN_VERSION as u64,
                        });
                    }

                    info!(peer = self.address, version, "leios peer initialized");
                    self.peer = Some(pid);
                }
                InitiatorEvent::EbFetched(_, answered, response) => {
                    if answered != point {
                        debug!("a leios-fetch reply for another endorser block");
                        continue;
                    }

                    match response {
                        leiosfetch::Response::Block(raw) => {
                            // An endorser block the peer does not hold comes
                            // back as a well formed empty body, so the size
                            // the announcement committed to is the only thing
                            // that tells the two apart.
                            let decoded =
                                EndorserBlockBody::decode_announced(raw.raw_bytes(), &announcement)?;

                            debug!(
                                eb = %eb.hash,
                                txs = decoded.len(),
                                "endorser block body fetched"
                            );

                            if decoded.is_empty() {
                                return Ok(Vec::new());
                            }

                            body = Some(decoded);
                        }
                        leiosfetch::Response::BlockTxs { txs: delivered } => {
                            let asked = std::mem::take(&mut inflight);

                            let Some(decoded) = body.as_ref() else {
                                debug!("transactions arrived before the body, ignoring");
                                continue;
                            };

                            if delivered.len() != asked.len() {
                                warn!(
                                    eb = %eb.hash,
                                    asked = asked.len(),
                                    got = delivered.len(),
                                    "short leios-fetch window, asking for the rest"
                                );
                            }

                            // A short reply is filed against the first indices
                            // that were asked for, which assumes the relay
                            // serves a prefix of the request. The assumption is
                            // checked here rather than taken on trust, because
                            // a subset that is not a prefix would otherwise be
                            // filed under the wrong transactions and only
                            // surface as an unexplained hash failure at the end
                            // of every retry, forever.
                            for (index, tx) in asked.iter().zip(delivered.iter()) {
                                let wire = tx.raw_bytes();
                                let named = decoded.entries()[*index].hash;

                                let found = pallas::ledger::traverse::leios::unwrap_tx(wire)
                                    .map(pallas::crypto::hash::Hasher::<256>::hash)
                                    .map_err(|reason| {
                                        Error::Endorser(
                                            pallas::ledger::traverse::leios::Error::Envelope {
                                                index: *index,
                                                reason,
                                            },
                                        )
                                    })?;

                                if found != named {
                                    return Err(Error::Misaligned {
                                        hash: eb.hash.to_string(),
                                        asked: asked.len(),
                                        delivered: delivered.len(),
                                        index: *index,
                                        named: named.to_string(),
                                        found: found.to_string(),
                                    });
                                }

                                txs.insert(*index, wire.to_vec());
                            }
                        }
                    }
                }
                other => debug!(?other, "unhandled leios event"),
            }
        }
    }
}

/// Checks a whole delivery against the body that named it, then strips the
/// envelopes.
///
/// The verification runs before anything is returned, so a caller cannot be
/// handed a transaction the endorser block did not name or one delivered in the
/// wrong place.
fn finish(body: &EndorserBlockBody, wire: Vec<Vec<u8>>) -> Result<Vec<Vec<u8>>, Error> {
    let verified = body.transactions(&wire)?;
    debug!(txs = verified.len(), "endorser block verified whole");

    let mut out = Vec::with_capacity(wire.len());
    for (index, w) in wire.iter().enumerate() {
        let inner = pallas::ledger::traverse::leios::unwrap_tx(w).map_err(|reason| {
            pallas::ledger::traverse::leios::Error::Envelope { index, reason }
        })?;

        out.push(inner.to_vec());
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pallas::crypto::hash::Hash;
    use pallas::network2::protocol::AnyCbor;
    use std::collections::VecDeque;

    /// A relay that answers one request at a time, the way leios-fetch does.
    ///
    /// It records the size of every transaction window the client asks for, so
    /// the round trips a fetch really costs are counted rather than assumed,
    /// and it refuses to have two requests outstanding, because the behavior
    /// under the real transport serves one per peer and a second would wait
    /// behind the first with nothing saying so.
    struct FakeRelay {
        point: EbId,
        body: Vec<u8>,
        wire_txs: Vec<Vec<u8>>,
        queued: VecDeque<InitiatorCommand>,
        answers: VecDeque<InitiatorEvent>,
        handshaken: bool,
        /// Requests on the wire with no answer taken yet.
        outstanding: usize,
        /// Requests issued while another was still unanswered.
        overlaps: usize,
        /// Sizes of the transaction windows the client asked for, in order.
        windows: Vec<usize>,
        /// How many transactions the relay will serve in one reply, whatever
        /// was asked for.
        cap: usize,
        /// Serve a subset that is not a prefix, by dropping the first
        /// transaction asked for. A relay is free to do this and the client
        /// files a short reply as though it never would.
        skip_first: bool,
    }

    impl FakeRelay {
        fn new(point: EbId, body: Vec<u8>, wire_txs: Vec<Vec<u8>>, cap: usize) -> Self {
            Self {
                point,
                body,
                wire_txs,
                queued: VecDeque::new(),
                answers: VecDeque::new(),
                handshaken: false,
                outstanding: 0,
                overlaps: 0,
                windows: Vec::new(),
                cap,
                skip_first: false,
            }
        }

        fn peer(&self) -> PeerId {
            "relay.test:3001".parse().expect("a valid peer id")
        }

        fn indices(bitmaps: &leiosfetch::Bitmaps) -> Vec<usize> {
            let mut out = Vec::new();

            for (window, mask) in bitmaps.0.iter() {
                for bit in 0..64usize {
                    if mask & (1u64 << (63 - bit)) != 0 {
                        out.push(*window as usize * 64 + bit);
                    }
                }
            }

            out.sort_unstable();
            out
        }
    }

    #[async_trait::async_trait(?Send)]
    impl LeiosTransport for FakeRelay {
        fn issue(&mut self, command: InitiatorCommand) {
            if self.outstanding > 0 {
                self.overlaps += 1;
            }

            self.queued.push_back(command);
            self.serve();
        }

        async fn next_event(&mut self) -> InitiatorEvent {
            if !self.handshaken {
                self.handshaken = true;

                return InitiatorEvent::PeerInitialized(
                    self.peer(),
                    (
                        LEIOS_MIN_VERSION,
                        pallas::network2::protocol::handshake::n2n::VersionData::new(
                            164,
                            false,
                            Some(0),
                            Some(false),
                        ),
                    ),
                );
            }

            if let Some(answer) = self.answers.pop_front() {
                self.outstanding -= 1;
                return answer;
            }

            // The client waited with nothing outstanding, which on the real
            // transport is a wait that only a timer can end. The client's own
            // timeout resolves it, and the test that hits this fails on the
            // timeout rather than hanging.
            std::future::pending().await
        }
    }

    impl FakeRelay {
        fn serve(&mut self) {
            while let Some(command) = self.queued.pop_front() {
                let answer = match command {
                    InitiatorCommand::FetchEb(pid, _) => InitiatorEvent::EbFetched(
                        pid,
                        self.point.clone(),
                        leiosfetch::Response::Block(AnyCbor::from_raw_bytes(self.body.clone())),
                    ),
                    InitiatorCommand::FetchEbTxs(pid, _, bitmaps) => {
                        let want = Self::indices(&bitmaps);
                        self.windows.push(want.len());

                        let skip = if self.skip_first { 1 } else { 0 };

                        let served: Vec<AnyCbor> = want
                            .iter()
                            .skip(skip)
                            .take(self.cap)
                            .map(|i| AnyCbor::from_raw_bytes(self.wire_txs[*i].clone()))
                            .collect();

                        InitiatorEvent::EbFetched(
                            pid,
                            self.point.clone(),
                            leiosfetch::Response::BlockTxs { txs: served },
                        )
                    }
                    _ => continue,
                };

                self.outstanding += 1;
                self.answers.push_back(answer);
            }
        }

    }

    /// A real 425 transaction endorser block off Musashi, with its body and
    /// every transaction as the wire carried them.
    ///
    /// Real transactions rather than one repeated, because a repeated
    /// transaction has one hash and a delivery filed under the wrong index
    /// would verify anyway, which is exactly the defect one of these tests
    /// exists to catch.
    fn real_eb() -> (Vec<u8>, Vec<Vec<u8>>, AnnouncedEndorserBlock) {
        let body = hex::decode(include_str!("../../test_data/dijkstra-eb3.ebbody").trim()).unwrap();

        let wire_txs: Vec<Vec<u8>> = include_str!("../../test_data/dijkstra-eb3.ebtxs")
            .split_whitespace()
            .map(|line| hex::decode(line).unwrap())
            .collect();

        assert_eq!(wire_txs.len(), 425, "fixture precondition");
        assert_eq!(body.len(), 15_303, "fixture precondition");

        let announced = AnnouncedEndorserBlock {
            slot: 376_369,
            hash: Hash::new([9; 32]),
            size: body.len() as u32,
        };

        (body, wire_txs, announced)
    }

    fn client(relay: FakeRelay, window: usize) -> LeiosClient<FakeRelay> {
        LeiosClient::with_transport(
            relay,
            "relay.test:3001".to_string(),
            window,
            Duration::from_millis(200),
        )
    }

    /// MUST FIRE: a whole endorser block comes back, and it costs exactly the
    /// round trips the window size implies and no more.
    ///
    /// The window list is the assertion that carries the throughput. Every
    /// request is one round trip, because the behavior serves one leios-fetch
    /// request per peer at a time, so the number of requests is the cost of the
    /// fetch. A client that made twice as many requests would return the same
    /// transactions, so a test that checks only the transactions cannot tell
    /// the cheap fetch from the expensive one.
    ///
    /// MUST NOT FIRE: no request is issued while another is unanswered. A
    /// second would sit in the behavior's queue behind the first with nothing
    /// saying so, which reads as a slow peer.
    #[tokio::test]
    async fn a_whole_block_costs_the_round_trips_the_window_implies() {
        let (body, wire_txs, announced) = real_eb();
        let point: EbId = Point::Specific(announced.slot, announced.hash.to_vec());

        let relay = FakeRelay::new(point, body, wire_txs.clone(), usize::MAX);
        let mut client = client(relay, 64);

        let txs = client.fetch(&announced).await.expect("must fetch whole");

        assert_eq!(txs.len(), 425, "the whole endorser block comes back");
        assert_eq!(
            txs[0],
            pallas::ledger::traverse::leios::unwrap_tx(&wire_txs[0]).unwrap(),
            "the envelopes are stripped"
        );

        assert_eq!(
            client.transport.overlaps, 0,
            "a request was issued while another was still unanswered"
        );
        assert_eq!(
            client.transport.windows,
            vec![64, 64, 64, 64, 64, 64, 41],
            "425 transactions page in ceil(425/64) windows and no more"
        );
    }

    /// MUST FIRE: a relay that serves fewer transactions than were asked for is
    /// asked again for the rest, and the block still comes back whole.
    ///
    /// MUST NOT FIRE: the short reply is not taken as the end of the endorser
    /// block, which would return a prefix and leave the ledger short with no
    /// error anywhere.
    #[tokio::test]
    async fn a_short_reply_is_asked_for_again_rather_than_ending_the_block() {
        let (body, wire_txs, announced) = real_eb();
        let point: EbId = Point::Specific(announced.slot, announced.hash.to_vec());

        // Ask for the whole block at once, and let the relay serve 150 at a
        // time, which is what a relay that caps its reply by size looks like.
        let relay = FakeRelay::new(point, body, wire_txs, 150);
        let mut client = client(relay, 8192);

        let txs = client.fetch(&announced).await.expect("must fetch whole");

        assert_eq!(txs.len(), 425, "the whole endorser block comes back");
        assert_eq!(
            client.transport.windows,
            vec![425, 275, 125],
            "each reply is short and the rest is asked for again"
        );
        assert_eq!(client.transport.overlaps, 0);
    }

    /// MUST FIRE: a request wider than 64 transactions is one request, not one
    /// per 64 wide window, because the selector carries as many windows as it
    /// likes and only the client's own window size decides the round trips.
    ///
    /// This is the whole reason the window is a choice worth making. At one
    /// round trip per request, seven round trips and one are the same work and
    /// seven times the latency.
    #[tokio::test]
    async fn one_request_can_span_many_selector_windows() {
        let (body, wire_txs, announced) = real_eb();
        let point: EbId = Point::Specific(announced.slot, announced.hash.to_vec());

        let relay = FakeRelay::new(point, body, wire_txs, usize::MAX);
        let mut client = client(relay, 8192);

        let txs = client.fetch(&announced).await.expect("must fetch whole");

        assert_eq!(txs.len(), 425);
        assert_eq!(
            client.transport.windows,
            vec![425],
            "one request covers seven selector windows"
        );
    }

    /// MUST FIRE: a reply that is a subset but not a prefix of the request is
    /// refused, naming the position that does not line up.
    ///
    /// Filing a short reply against the first indices asked for assumes the
    /// relay serves a prefix. Nothing on the wire says it must, and a subset
    /// that is not a prefix would otherwise be filed under the wrong
    /// transactions, which the whole block check at the end reports as an
    /// unexplained hash failure on every retry, forever.
    ///
    /// MUST NOT FIRE: the prefix case above must stay accepted, or this refusal
    /// would simply stop the client working at all.
    #[tokio::test]
    async fn a_reply_that_is_not_a_prefix_of_the_request_is_refused() {
        let (body, wire_txs, announced) = real_eb();
        let point: EbId = Point::Specific(announced.slot, announced.hash.to_vec());

        let mut relay = FakeRelay::new(point, body, wire_txs, 150);
        relay.skip_first = true;
        let mut client = client(relay, 8192);

        let err = client
            .fetch(&announced)
            .await
            .expect_err("a delivery that is not a prefix must be refused");

        match err {
            Error::Misaligned { index, asked, delivered, .. } => {
                assert_eq!(index, 0, "the first position is the one that misfiles");
                assert_eq!(asked, 425);
                assert_eq!(delivered, 150);
            }
            other => panic!("wrong refusal: {other}"),
        }
    }

    fn hex_block(text: &str) -> BlockBody {
        hex::decode(text.trim()).unwrap()
    }

    /// Slot 376695 of Musashi: certifies an endorser block and announces
    /// nothing of its own.
    fn certify_only_block() -> BlockBody {
        hex_block(include_str!("../../test_data/dijkstra-certify-only.block"))
    }

    /// Slot 375843 of Musashi: a Dijkstra block that neither certifies nor
    /// announces, so it settles nothing either way.
    fn quiet_block() -> BlockBody {
        hex_block(include_str!("../../test_data/dijkstra-quiet.block"))
    }

    /// A Conway block, from an era with no endorsement layer.
    fn pre_leios_block() -> BlockBody {
        hex_block(include_str!("../../test_data/conway.block"))
    }

    fn walk(blocks: &[BlockBody]) -> ResumedWalk {
        resume_walk(blocks.iter().cloned().map(std::sync::Arc::new)).expect("blocks must decode")
    }

    /// MUST FIRE: reading back to a block that announces gives that
    /// announcement, so a follower resuming inside an endorsement window knows
    /// what the next certificate will name.
    ///
    /// MUST NOT FIRE: the quiet block nearer the resume point settles nothing
    /// and must not stop the walk, because stopping there is how a follower
    /// would decide an announcement was not pending when it was.
    #[test]
    fn reading_back_to_an_announcement_recovers_it() {
        let out = walk(&[quiet_block(), certifying_block()]);

        assert_eq!(out.scanned, 2, "the quiet block does not settle anything");
        assert_eq!(out.settled_at, Some(2_514_319));

        let waiting = out.state.waiting().expect("an announcement is waiting");
        assert_eq!(waiting.slot, 2_514_319);
        assert_eq!(
            waiting.hash.to_string(),
            "2baaaf7169be390e43ec401a12f664cc01c39bfe52c7ce7a13818a2d8922eac6"
        );
        assert_eq!(waiting.size, 28519);
    }

    /// MUST FIRE: a block that certifies and announces nothing settles the walk
    /// as nothing waiting, and the walk stops there.
    ///
    /// MUST NOT FIRE: it must not read past that block to the older
    /// announcement behind it. Doing so would hand the follower an endorser
    /// block that was already certified and applied, and the next certificate
    /// would resolve to the wrong payload.
    #[test]
    fn a_certificate_settles_the_walk_and_hides_the_announcement_behind_it() {
        let out = walk(&[certify_only_block(), certifying_block()]);

        assert_eq!(out.state, PendingAnnouncement::Nothing);
        assert_eq!(out.scanned, 1, "the walk stops at the certificate");
        assert_eq!(out.settled_at, Some(376_695));
    }

    /// MUST FIRE: stored blocks that settle nothing give an answer that says so.
    ///
    /// MUST NOT FIRE: they must not give "nothing is waiting". The two look the
    /// same as an absence and mean opposite things: one lets the follower carry
    /// on and lose a whole endorser block at the next certificate, the other
    /// makes it wait until an announcement tells it where it stands. This is
    /// the case the whole reconstruction exists for.
    #[test]
    fn a_log_that_settles_nothing_says_so_rather_than_saying_nothing_is_waiting() {
        let out = walk(&[quiet_block(), quiet_block(), quiet_block()]);

        assert_eq!(out.state, PendingAnnouncement::Unknown);
        assert_ne!(
            out.state,
            PendingAnnouncement::Nothing,
            "not knowing must not be reported as knowing"
        );
        assert_eq!(out.scanned, 3);
        assert_eq!(out.settled_at, None);

        // An empty log is the same answer, reached with nothing read at all.
        let empty = walk(&[]);
        assert_eq!(empty.state, PendingAnnouncement::Unknown);
        assert_eq!(empty.scanned, 0);
    }

    /// MUST FIRE: an era with no endorsement layer settles the walk as nothing
    /// waiting, because an announcement cannot cross forward into it.
    ///
    /// MUST NOT FIRE: a follower resuming before the hard fork must not be told
    /// the walk cannot tell. That refusal would block every restart on the
    /// whole pre-Leios part of the chain, which is most of it.
    #[test]
    fn an_era_with_no_endorsement_layer_settles_the_walk() {
        let out = walk(&[quiet_block(), pre_leios_block()]);

        assert_eq!(out.state, PendingAnnouncement::Nothing);
        assert_eq!(out.scanned, 2);
        assert!(out.settled_at.is_some());
    }

    fn certifying_block() -> BlockBody {
        hex::decode(include_str!("../../test_data/dijkstra-certifying.block").trim()).unwrap()
    }

    fn plain_block() -> BlockBody {
        hex::decode(include_str!("../../test_data/dijkstra-plain.block").trim()).unwrap()
    }

    fn eb1_txs() -> Vec<Vec<u8>> {
        include_str!("../../test_data/dijkstra-eb1.ebtxs")
            .split_whitespace()
            .map(|l| {
                let wire = hex::decode(l).unwrap();
                pallas::ledger::traverse::leios::unwrap_tx(&wire)
                    .unwrap()
                    .to_vec()
            })
            .collect()
    }

    fn payload() -> CertifiedPayload {
        CertifiedPayload {
            endorser_block: AnnouncedEndorserBlock {
                slot: 2514000,
                hash: Hash::new([7; 32]),
                size: 37,
            },
            txs: eb1_txs(),
        }
    }

    /// MUST FIRE: the block whose slot a payload names comes out carrying the
    /// endorser block's transactions.
    ///
    /// MUST NOT FIRE: every other block in the same batch comes out byte
    /// identical, so the resolution reaches exactly one block.
    #[test]
    fn only_the_certifying_block_is_resolved() {
        let certifying = certifying_block();
        let plain = plain_block();

        let certifying_slot = MultiEraBlock::decode(&certifying).unwrap().slot();
        assert_eq!(certifying_slot, 2514319, "fixture precondition");
        assert_eq!(
            MultiEraBlock::decode(&certifying).unwrap().tx_count(),
            0,
            "fixture precondition"
        );

        let mut pending = PendingPayloads::default();
        pending.expect(certifying_slot);
        pending.deliver(certifying_slot, payload());

        let out = pending
            .apply(&mut AppliedTxWindow::default(), vec![plain.clone(), certifying.clone()])
            .expect("must resolve");

        assert_eq!(out.len(), 2);
        assert_eq!(out[0], plain, "an uncertified block must pass through");
        assert_ne!(out[1], certifying, "the certifying block must be rewritten");

        let resolved = MultiEraBlock::decode(&out[1]).unwrap();
        assert_eq!(resolved.tx_count(), 1);
        assert_eq!(resolved.slot(), certifying_slot);
        assert_eq!(
            resolved.hash(),
            MultiEraBlock::decode(&certifying).unwrap().hash(),
            "the header, and so the hash, is untouched"
        );

        assert!(pending.is_empty());
        assert!(pending.refuse_undelivered().is_ok());
    }

    /// MUST FIRE: a payload keyed to a slot no block in the batch carries is
    /// refused rather than silently dropped, which is what a short or reordered
    /// batch would otherwise cause.
    #[test]
    fn a_payload_no_block_claimed_is_refused() {
        let mut pending = PendingPayloads::default();
        pending.expect(999_999);
        pending.deliver(999_999, payload());

        let out = pending
            .apply(&mut AppliedTxWindow::default(), vec![plain_block()])
            .unwrap();
        assert_eq!(out.len(), 1);

        let err = pending
            .refuse_undelivered()
            .expect_err("an unapplied endorser block must be refused");

        match err {
            Error::Undelivered { slot, .. } => assert_eq!(slot, 999_999),
            other => panic!("wrong refusal: {other}"),
        }
    }

    /// MUST NOT FIRE: with nothing pending, every block passes through byte
    /// identical. This is the case that catches a resolution firing where no
    /// endorser block was certified.
    #[test]
    fn with_nothing_pending_every_block_passes_through() {
        let blocks = vec![plain_block(), certifying_block()];
        let mut pending = PendingPayloads::default();

        let out = pending
            .apply(&mut AppliedTxWindow::default(), blocks.clone())
            .unwrap();

        assert_eq!(out, blocks);
        assert!(pending.refuse_undelivered().is_ok());
    }

    /// MUST FIRE: a block whose certification was recorded and whose endorser
    /// block was never fetched is refused, rather than passed through.
    ///
    /// A certifying block carries no transactions of its own, so passing it
    /// through applies an empty block where a whole endorser block belongs and
    /// says nothing at all. The cost is paid thousands of slots later by a
    /// transaction spending an output the follower never saw, and at that point
    /// nothing connects the failure to the fetch that did not return.
    ///
    /// MUST NOT FIRE: the same block with its payload delivered still resolves,
    /// and a block that certifies nothing still passes through. Otherwise this
    /// refusal would simply stop the follower.
    #[test]
    fn a_certifying_block_whose_endorser_block_never_arrived_is_refused() {
        let certifying = certifying_block();
        let slot = MultiEraBlock::decode(&certifying).unwrap().slot();

        let mut pending = PendingPayloads::default();
        pending.expect(slot);

        assert_eq!(pending.outstanding(), vec![slot]);

        let err = pending
            .apply(
                &mut AppliedTxWindow::default(),
                vec![plain_block(), certifying.clone()],
            )
            .expect_err("a certification with no payload must be refused");

        match err {
            Error::Unfetched { slot: refused } => assert_eq!(refused, slot),
            other => panic!("wrong refusal: {other}"),
        }

        // The same block, with the endorser block delivered, resolves.
        let mut pending = PendingPayloads::default();
        pending.expect(slot);
        pending.deliver(slot, payload());
        assert!(pending.outstanding().is_empty());

        let out = pending
            .apply(&mut AppliedTxWindow::default(), vec![certifying.clone()])
            .expect("must resolve");
        assert_ne!(out[0], certifying, "the certifying block is rewritten");
        assert!(pending.refuse_undelivered().is_ok());
    }

    /// MUST FIRE: a certification recorded and never fetched is refused at the
    /// end of a batch too, and not only when its own block arrives.
    ///
    /// The block that certifies it may be in a later batch, so the end of a
    /// batch is the earliest point at which nothing can still be owed. Both
    /// leftovers are refused, and each says which it was.
    #[test]
    fn both_kinds_of_leftover_are_refused_and_say_which() {
        let mut unfetched = PendingPayloads::default();
        unfetched.expect(4242);

        match unfetched.refuse_undelivered().expect_err("must refuse") {
            Error::Unfetched { slot } => assert_eq!(slot, 4242),
            other => panic!("wrong refusal: {other}"),
        }

        let mut unapplied = PendingPayloads::default();
        unapplied.expect(4242);
        unapplied.deliver(4242, payload());

        match unapplied.refuse_undelivered().expect_err("must refuse") {
            Error::Undelivered { slot, .. } => assert_eq!(slot, 4242),
            other => panic!("wrong refusal: {other}"),
        }
    }

    /// The two ranking blocks of the Musashi chain that first met this, and a
    /// faithful sub-sequence of the two endorser blocks they certify.
    ///
    /// Block 1742305 certifies endorser block
    /// `f8f854f079cab3fe398d9abbb0a9318d5885ad9b5b9e53c796c1ee9593ff957b`,
    /// announced at slot 1742239 with 490 transactions. Block 1742326
    /// certifies endorser block
    /// `e81c96b691602c44e4bd8481c85121dafca0db127ee4e25316abfd542f001f5c`,
    /// announced at slot 1742305 with 613 transactions, of which the first 490
    /// are byte for byte the whole of the endorser block before it.
    ///
    /// The fixtures keep the two blocks whole and four of the repeated
    /// transactions beside three of the new ones, in the order each endorser
    /// block delivered them, so the files stay small. Among the four kept is
    /// `e31a871fbee9e85970bfdeaa9f906220ef3cc3ac0118d4c080865d58a2e92c34`,
    /// which is the transaction the sync from origin stopped on: applied once
    /// from the first endorser block, it spends
    /// `636fe3f97867f643b0f58620620fc0b3b20025c008889e703c6e63d7b010c24e#0`,
    /// and applied a second time from the second there is no such output left.
    fn repeat_first_block() -> BlockBody {
        hex::decode(include_str!("../../test_data/dijkstra-repeat-first.block").trim()).unwrap()
    }

    fn repeat_second_block() -> BlockBody {
        hex::decode(include_str!("../../test_data/dijkstra-repeat-second.block").trim()).unwrap()
    }

    fn wire_txs(listing: &str) -> Vec<Vec<u8>> {
        listing
            .split_whitespace()
            .map(|l| {
                let wire = hex::decode(l).unwrap();
                pallas::ledger::traverse::leios::unwrap_tx(&wire)
                    .unwrap()
                    .to_vec()
            })
            .collect()
    }

    fn repeat_first_payload() -> CertifiedPayload {
        CertifiedPayload {
            endorser_block: AnnouncedEndorserBlock {
                slot: 1742239,
                hash: "f8f854f079cab3fe398d9abbb0a9318d5885ad9b5b9e53c796c1ee9593ff957b"
                    .parse()
                    .unwrap(),
                size: 17643,
            },
            txs: wire_txs(include_str!("../../test_data/dijkstra-repeat-first.ebtxs")),
        }
    }

    fn repeat_second_payload() -> CertifiedPayload {
        CertifiedPayload {
            endorser_block: AnnouncedEndorserBlock {
                slot: 1742305,
                hash: "e81c96b691602c44e4bd8481c85121dafca0db127ee4e25316abfd542f001f5c"
                    .parse()
                    .unwrap(),
                size: 22071,
            },
            txs: wire_txs(include_str!("../../test_data/dijkstra-repeat-second.ebtxs")),
        }
    }

    fn tx_ids(block: &[u8]) -> Vec<Hash<32>> {
        MultiEraBlock::decode(block)
            .unwrap()
            .txs()
            .iter()
            .map(|tx| tx.hash())
            .collect()
    }

    /// MUST FIRE: a transaction the endorser block certified one block earlier
    /// already contributed is spliced once and not twice. This is the whole
    /// defect: applied a second time it spends an input that is already spent,
    /// and the follower stops thousands of slots from anything that explains
    /// it.
    ///
    /// MUST NOT FIRE: the transactions the second endorser block adds are all
    /// spliced. A window that dropped those would leave the ledger short by
    /// exactly the payload this stage exists to deliver, which is the same
    /// silence in the other direction and would not show up until something
    /// spent one of them.
    #[test]
    fn a_transaction_an_earlier_endorser_block_carried_is_spliced_once() {
        let first = repeat_first_block();
        let second = repeat_second_block();

        let first_slot = MultiEraBlock::decode(&first).unwrap().slot();
        let second_slot = MultiEraBlock::decode(&second).unwrap().slot();
        assert_eq!(first_slot, 1742305, "fixture precondition");
        assert_eq!(second_slot, 1742326, "fixture precondition");

        let first_payload = repeat_first_payload();
        let second_payload = repeat_second_payload();
        assert_eq!(first_payload.txs.len(), 4, "fixture precondition");
        assert_eq!(second_payload.txs.len(), 7, "fixture precondition");

        let mut window = AppliedTxWindow::default();

        let mut pending = PendingPayloads::default();
        pending.expect(first_slot);
        pending.deliver(first_slot, first_payload);

        let out = pending
            .apply(&mut window, vec![first.clone()])
            .expect("the first certifying block must resolve");

        let after_first = tx_ids(&out[0]);
        assert_eq!(
            after_first.len(),
            4,
            "the first endorser block's transactions are all new"
        );

        let mut pending = PendingPayloads::default();
        pending.expect(second_slot);
        pending.deliver(second_slot, second_payload);

        let out = pending
            .apply(&mut window, vec![second.clone()])
            .expect("the second certifying block must resolve");

        let after_second = tx_ids(&out[0]);

        assert_eq!(
            after_second.len(),
            3,
            "the four transactions the first endorser block already carried must not be \
             spliced again"
        );

        for id in &after_first {
            assert!(
                !after_second.contains(id),
                "transaction {id} was applied twice"
            );
        }

        let offender: Hash<32> = "e31a871fbee9e85970bfdeaa9f906220ef3cc3ac0118d4c080865d58a2e92c34"
            .parse()
            .unwrap();
        assert!(
            after_first.contains(&offender),
            "the transaction the sync stopped on must be applied once"
        );
        assert!(
            !after_second.contains(&offender),
            "the transaction the sync stopped on must not be applied twice"
        );
    }

    /// MUST NOT FIRE: with an empty window every transaction of an endorser
    /// block is spliced. This is the case that catches a window that answers
    /// yes to everything, which would empty every certifying block on the
    /// chain and say nothing about it.
    #[test]
    fn an_empty_window_suppresses_nothing() {
        let second = repeat_second_block();
        let slot = MultiEraBlock::decode(&second).unwrap().slot();

        let mut pending = PendingPayloads::default();
        pending.expect(slot);
        pending.deliver(slot, repeat_second_payload());

        let out = pending
            .apply(&mut AppliedTxWindow::default(), vec![second])
            .expect("must resolve");

        assert_eq!(tx_ids(&out[0]).len(), 7);
    }

    /// MUST FIRE: the window read back from stored blocks knows what those
    /// blocks carried, so a follower that resumes across the overlap
    /// deduplicates it. A window that started empty on every restart would
    /// move this failure from the first sync to the first restart, and the
    /// restart is where it was actually met.
    ///
    /// MUST NOT FIRE: the read reports a log shorter than the window as short,
    /// with the number of blocks it did read, rather than as a window of the
    /// intended depth.
    #[test]
    fn the_window_is_read_back_out_of_stored_blocks() {
        let stored = vec![plain_block(), certifying_block()];
        let carried = tx_ids(&stored[0]);
        assert!(
            !carried.is_empty(),
            "fixture precondition: the stored block carries transactions"
        );

        // Stored blocks are read newest first, the order the log walks back in.
        let resumed = resume_window(stored.into_iter().rev().map(std::sync::Arc::new))
            .expect("must read back");

        assert_eq!(resumed.reach, WindowReach::ShortLog { blocks: 2 });
        assert_eq!(resumed.window.depth(), 2);

        for id in &carried {
            assert!(
                resumed.window.contains(id),
                "a transaction of a stored block must be remembered"
            );
        }

        assert!(
            !resumed.window.contains(&Hash::new([0; 32])),
            "a transaction no stored block carried must not be remembered"
        );
    }

    /// MUST FIRE: the three counts are refused unless they add up, so a future
    /// edit that filters in one place and counts in another cannot report a
    /// short block as a small one.
    ///
    /// MUST NOT FIRE: counts that do add up are accepted, including the case
    /// where nothing was repeated at all.
    #[test]
    fn a_contribution_that_does_not_add_up_is_refused() {
        match Contribution::new(613, 490, 7).expect_err("must refuse") {
            Error::ContributionDoesNotAddUp {
                named,
                repeated,
                spliced,
            } => {
                assert_eq!((named, repeated, spliced), (613, 490, 7));
            }
            other => panic!("wrong refusal: {other}"),
        }

        let ok = Contribution::new(613, 490, 123).expect("must accept");
        assert_eq!(ok.named(), 613);
        assert_eq!(ok.repeated(), 490);
        assert_eq!(ok.spliced(), 123);

        let none_repeated = Contribution::new(7, 0, 7).expect("must accept");
        assert_eq!(none_repeated.repeated(), 0);
    }
}
