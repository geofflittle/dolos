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
//! hold arrives as a well formed empty body, and only the hash the announcement
//! committed to tells that body apart from an endorser block that really is empty.
//! And a payload is attached to a block by slot, never by arrival order, so a
//! short or reordered block batch cannot silently move one block's transactions
//! onto another.

use std::collections::BTreeMap;
use std::time::Duration;

use dolos_cardano::pallas_extras::inline_endorser_transactions;
use pallas::crypto::hash::Hash;
use pallas::ledger::traverse::{Era, MultiEraBlock, MultiEraTx};
use pallas::network2::behavior::initiator::{
    Config as HandshakeConfig, DisconnectReason, HandshakeBehavior, InitiatorBehavior,
    InitiatorCommand, InitiatorEvent,
};
use pallas::network2::behavior::AnyMessage;
use pallas::network2::interface::TcpInterface;
use pallas::network2::protocol::handshake::n2n::{VersionTable, LEIOS_MIN_VERSION};
use pallas::network2::protocol::{leiosfetch, EbId, Point};
use pallas::network2::{Manager, PeerId};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, info, warn};

use dolos_core::BlockBody;

use crate::sync::endorser::{self, EndorserBlockBody};

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

/// How often the real transport runs housekeeping, which sends the peer a
/// keepalive message.
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

    /// The peer session ended while a fetch was waiting on it.
    ///
    /// The reason and the elapsed time are carried rather than formatted away,
    /// because the two reasons are reported by different code paths and a
    /// caller counting sessions needs to tell them apart.
    #[error(
        "the leios peer session ended {} {waited_ms} ms into the fetch of endorser block {hash} \
         announced at slot {slot}, and the rest of the fetch budget would have been spent \
         waiting on a connection that is gone",
        match reason {
            DisconnectReason::Closed => "with no error reported",
            DisconnectReason::Errored => "on a connection error",
            _ => "on a connection error",
        }
    )]
    Disconnected {
        slot: u64,
        hash: String,
        reason: DisconnectReason,
        waited_ms: u64,
    },

    #[error(transparent)]
    Endorser(#[from] endorser::Error),

    #[error("block cbor from the peer does not decode: {0}")]
    BadBlock(String),

    #[error(
        "the block at slot {slot} certifies an endorser block and carries {count} transactions \
         of its own"
    )]
    CertifiesAndCarries { slot: u64, count: usize },

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

/// A certified endorser block's announcement, with the slot of the header that
/// made it, which is the slot a leios-fetch point carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnouncedEndorserBlock {
    pub slot: u64,
    pub hash: Hash<32>,
    pub size: u32,
}

/// A fetched, verified endorser block waiting for the ranking block that
/// certified it.
#[derive(Debug, Clone)]
pub struct CertifiedPayload {
    pub endorser_block: AnnouncedEndorserBlock,
    /// The `block_transaction` bytes of the transactions, in endorser block
    /// order.
    pub txs: Vec<Vec<u8>>,
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
    /// Nothing is left out here. A transaction two blocks both carry is applied
    /// twice, which is what the network does with it, and the ledger's lenient
    /// rule is what makes the second application harmless. Filtering repeats
    /// here was tried and is wrong: a second application re-creates an output
    /// that was spent in between, and the chain relies on that.
    pub fn apply(&mut self, blocks: Vec<BlockBody>) -> Result<Vec<BlockBody>, Error> {
        let mut out = Vec::with_capacity(blocks.len());

        for cbor in blocks {
            let (slot, own) = MultiEraBlock::decode(&cbor)
                .map(|block| (block.slot(), block.tx_count()))
                .map_err(|e| Error::BadBlock(e.to_string()))?;

            match self.0.remove(&slot) {
                None => out.push(cbor),
                Some(None) => {
                    return Err(Error::Unfetched { slot });
                }
                Some(Some(payload)) => {
                    if own > 0 {
                        return Err(Error::CertifiesAndCarries { slot, count: own });
                    }

                    let resolved = inline_endorser_transactions(&cbor, &payload.txs)
                        .map_err(|e| Error::BadBlock(e.to_string()))?;

                    info!(
                        slot,
                        eb = %payload.endorser_block.hash,
                        txs = payload.txs.len(),
                        "applied the transactions of a certified endorser block"
                    );

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

type LeiosNetwork = Manager<TcpInterface<AnyMessage>, InitiatorBehavior, AnyMessage>;

/// The real transport: one TCP connection driven by the network manager.
pub struct ManagerTransport {
    commands: mpsc::UnboundedSender<InitiatorCommand>,
    events: mpsc::UnboundedReceiver<InitiatorEvent>,
    driver: JoinHandle<()>,
}

impl ManagerTransport {
    fn spawn(network: LeiosNetwork) -> Self {
        let (commands, issued) = mpsc::unbounded_channel();
        let (emitted, events) = mpsc::unbounded_channel();

        Self {
            commands,
            events,
            driver: tokio::spawn(drive(network, issued, emitted)),
        }
    }
}

/// Runs `network` until its transport is dropped, with housekeeping on every
/// tick.
async fn drive(
    mut network: LeiosNetwork,
    mut issued: mpsc::UnboundedReceiver<InitiatorCommand>,
    emitted: mpsc::UnboundedSender<InitiatorEvent>,
) {
    let mut ticker = interval(HOUSEKEEPING_TICK);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                network.execute(InitiatorCommand::Housekeeping);
            }
            command = issued.recv() => match command {
                Some(command) => network.execute(command),
                None => return,
            },
            event = network.poll_next() => {
                // None means the manager took an internal step, such as
                // dispatching a queued request to the interface, and has no
                // event for the client yet.
                match event {
                    Some(event) if read_by_fetch(&event) => {
                        if emitted.send(event).is_err() {
                            return;
                        }
                    }
                    Some(other) => debug!(?other, "leios event the client does not read"),
                    None => (),
                }
            }
        }
    }
}

/// Whether [`LeiosClient::fetch`] reads `event`.
fn read_by_fetch(event: &InitiatorEvent) -> bool {
    matches!(
        event,
        InitiatorEvent::PeerInitialized(..)
            | InitiatorEvent::EbFetched(..)
            | InitiatorEvent::PeerDisconnected(..)
    )
}

#[async_trait::async_trait(?Send)]
impl LeiosTransport for ManagerTransport {
    fn issue(&mut self, command: InitiatorCommand) {
        // A send fails only once the driver has panicked, which `next_event`
        // reports.
        let _ = self.commands.send(command);
    }

    async fn next_event(&mut self) -> InitiatorEvent {
        if let Some(event) = self.events.recv().await {
            return event;
        }

        match (&mut self.driver).await {
            Err(ended) if ended.is_panic() => std::panic::resume_unwind(ended.into_panic()),
            ended => unreachable!("the leios driver ended with the transport alive: {ended:?}"),
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

        Ok(Self::with_transport(
            ManagerTransport::spawn(network),
            address.to_string(),
            TXS_PER_REQUEST,
            FETCH_TIMEOUT,
        ))
    }
}

impl<T: LeiosTransport> LeiosClient<T> {
    pub fn with_transport(transport: T, address: String, window: usize, timeout: Duration) -> Self {
        Self {
            transport,
            peer: None,
            address,
            window,
            timeout,
        }
    }

    /// Fetches one endorser block whole and returns the `block_transaction`
    /// bytes of its transactions, in endorser block order.
    ///
    /// The body is checked against the hash its announcement committed to, and
    /// every transaction against the hash its body entry names, before any of
    /// them is returned. So the answer is
    /// either the whole endorser block or an error saying what was wrong with
    /// it, never a prefix and never an empty list standing in for a failure.
    pub async fn fetch(&mut self, eb: &AnnouncedEndorserBlock) -> Result<Vec<Vec<u8>>, Error> {
        let point: EbId = Point::Specific(eb.slot, eb.hash.to_vec());
        let announcement = pallas::ledger::primitives::dijkstra::EbAnnouncement {
            eb_hash: eb.hash,
            eb_size: eb.size,
        };

        let started = tokio::time::Instant::now();

        let mut body: Option<EndorserBlockBody> = None;
        let mut txs: BTreeMap<usize, leiosfetch::TxCbor> = BTreeMap::new();
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
                            let wire: Vec<leiosfetch::TxCbor> = txs.into_values().collect();

                            debug!(
                                eb = %eb.hash,
                                round_trips,
                                txs = wire.len(),
                                elapsed_ms = started.elapsed().as_millis() as u64,
                                "endorser block fetched whole"
                            );

                            return finish(wire);
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

            let remaining =
                self.timeout
                    .checked_sub(started.elapsed())
                    .ok_or_else(|| Error::Timeout {
                        slot: eb.slot,
                        hash: eb.hash.to_string(),
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
                            version,
                            minimum: LEIOS_MIN_VERSION,
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
                            // back as a well formed empty body, so the hash
                            // the announcement committed to is the only thing
                            // that tells the two apart.
                            let decoded = EndorserBlockBody::decode_announced(
                                raw.raw_bytes(),
                                &announcement,
                            )?;

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
                                let named = decoded[*index].0;

                                let found = pallas::crypto::hash::Hasher::<256>::hash(tx);

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

                                txs.insert(*index, tx.clone());
                            }
                        }
                    }
                }
                InitiatorEvent::PeerDisconnected(pid, reason) => {
                    if self.peer.as_ref().is_some_and(|held| *held != pid) {
                        debug!("a leios session ending for another peer");
                        continue;
                    }

                    return Err(Error::Disconnected {
                        slot: eb.slot,
                        hash: eb.hash.to_string(),
                        reason,
                        waited_ms: started.elapsed().as_millis() as u64,
                    });
                }
                other => debug!(?other, "unhandled leios event"),
            }
        }
    }
}

/// The transactions of a whole delivery, each decoded as a Dijkstra
/// transaction before any is returned.
fn finish(wire: Vec<leiosfetch::TxCbor>) -> Result<Vec<Vec<u8>>, Error> {
    let mut out = Vec::with_capacity(wire.len());
    for (index, w) in wire.into_iter().enumerate() {
        MultiEraTx::decode_for_era(Era::Dijkstra, &w).map_err(|e| endorser::Error::TxDecode {
            index,
            reason: e.to_string(),
        })?;

        out.push(w.unwrap());
    }

    debug!(txs = out.len(), "endorser block verified whole");

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dolos_cardano::owned::OwnedMultiEraOutput;
    use dolos_cardano::utxoset::compute_apply_delta;
    use dolos_core::{EraCbor, TxoRef};
    use pallas::codec::minicbor::{self, bytes::ByteSlice};
    use pallas::crypto::hash::Hash;
    use pallas::ledger::traverse::MultiEraInput;
    use pallas::network2::protocol::AnyCbor;
    use std::collections::HashMap;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

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
        eb_txs: Vec<Vec<u8>>,
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
        /// Report one session ending, for this peer, before anything is
        /// served.
        dies: Option<DisconnectReason>,
        /// Nothing more is served once the session has ended, which is what an
        /// ended session does.
        dead: bool,
        /// Report one session ending, for a peer this client never held,
        /// before anything is served.
        stray_death: bool,
    }

    impl FakeRelay {
        fn new(point: EbId, body: Vec<u8>, eb_txs: Vec<Vec<u8>>, cap: usize) -> Self {
            Self {
                point,
                body,
                eb_txs,
                queued: VecDeque::new(),
                answers: VecDeque::new(),
                handshaken: false,
                outstanding: 0,
                overlaps: 0,
                windows: Vec::new(),
                cap,
                skip_first: false,
                dies: None,
                dead: false,
                stray_death: false,
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

            if self.stray_death {
                self.stray_death = false;

                return InitiatorEvent::PeerDisconnected(
                    "other.test:3001".parse().expect("a valid peer id"),
                    DisconnectReason::Closed,
                );
            }

            if let Some(reason) = self.dies.take() {
                self.dead = true;
                self.answers.clear();

                return InitiatorEvent::PeerDisconnected(self.peer(), reason);
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
            if self.dead {
                self.queued.clear();
                return;
            }

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

                        let served: Vec<leiosfetch::TxCbor> = want
                            .iter()
                            .skip(skip)
                            .take(self.cap)
                            .map(|i| leiosfetch::TxCbor::from_raw_bytes(self.eb_txs[*i].clone()))
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

    /// Blake2b-256 of the endorser block body fixture, the hash the ranking
    /// block header announced it under.
    const REAL_EB_BODY_HASH: &str =
        "a8f2a746b33a74bf39f9f8a56e108746d1512a5af91f48ba4336a61b81cf5fcd";

    /// A real 425 transaction endorser block, with its body and the bytes of
    /// every transaction it names.
    ///
    /// Real transactions rather than one repeated, because a repeated
    /// transaction has one hash and a delivery filed under the wrong index
    /// would verify anyway, which is exactly the defect one of these tests
    /// exists to catch.
    ///
    /// The announcement carries the body's own hash, because a fetch is only
    /// answered when the body it gets back is the body that was committed to.
    fn real_eb() -> (Vec<u8>, Vec<Vec<u8>>, AnnouncedEndorserBlock) {
        let body = hex::decode(include_str!("../../test_data/dijkstra-eb3.ebbody").trim()).unwrap();

        let eb_txs: Vec<Vec<u8>> = include_str!("../../test_data/dijkstra-eb3.ebtxs")
            .split_whitespace()
            .map(|line| {
                let wire = hex::decode(line).unwrap();
                let inner: &ByteSlice = minicbor::decode(&wire).unwrap();
                inner.to_vec()
            })
            .collect();

        assert_eq!(eb_txs.len(), 425, "fixture precondition");
        assert_eq!(body.len(), 15_303, "fixture precondition");

        let announced = AnnouncedEndorserBlock {
            slot: 376_369,
            hash: REAL_EB_BODY_HASH.parse().expect("fixture precondition"),
            size: body.len() as u32,
        };

        (body, eb_txs, announced)
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
        let (body, eb_txs, announced) = real_eb();
        let point: EbId = Point::Specific(announced.slot, announced.hash.to_vec());

        let relay = FakeRelay::new(point, body, eb_txs.clone(), usize::MAX);
        let mut client = client(relay, 64);

        let txs = client.fetch(&announced).await.expect("must fetch whole");

        assert_eq!(txs.len(), 425, "the whole endorser block comes back");
        assert_eq!(txs, eb_txs, "the transactions come back in body order");

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

    /// MUST FIRE: a session that ends while a fetch is waiting on it is
    /// reported as a session that ended.
    ///
    /// The fetch budget is a minute and a session can end in its first second,
    /// so a fetch that never hears the ending spends the rest of the budget
    /// waiting on a connection that is gone, and reports a timeout, which names
    /// the peer as slow.
    #[tokio::test]
    async fn a_session_that_ends_mid_fetch_is_reported_not_waited_out() {
        for reason in [DisconnectReason::Closed, DisconnectReason::Errored] {
            let (body, eb_txs, announced) = real_eb();
            let point: EbId = Point::Specific(announced.slot, announced.hash.to_vec());

            let mut relay = FakeRelay::new(point, body, eb_txs, usize::MAX);
            relay.dies = Some(reason);

            let mut client = client(relay, 64);

            let error = client
                .fetch(&announced)
                .await
                .expect_err("a fetch against an ended session returned a block");

            assert!(
                matches!(&error, Error::Disconnected { reason: got, .. } if *got == reason),
                "{error}"
            );
        }
    }

    /// MUST NOT FIRE: a session ending for a peer this client never held is not
    /// this fetch's failure, and the endorser block still comes back whole.
    #[tokio::test]
    async fn a_session_ending_for_another_peer_does_not_end_the_fetch() {
        let (body, eb_txs, announced) = real_eb();
        let point: EbId = Point::Specific(announced.slot, announced.hash.to_vec());

        let mut relay = FakeRelay::new(point, body, eb_txs, usize::MAX);
        relay.stray_death = true;

        let mut client = client(relay, 64);

        let txs = client.fetch(&announced).await.expect("must fetch whole");

        assert_eq!(txs.len(), 425, "the whole endorser block comes back");
    }

    /// MUST FIRE: a relay that serves fewer transactions than were asked for is
    /// asked again for the rest, and the block still comes back whole.
    ///
    /// MUST NOT FIRE: the short reply is not taken as the end of the endorser
    /// block, which would return a prefix and leave the ledger short with no
    /// error anywhere.
    #[tokio::test]
    async fn a_short_reply_is_asked_for_again_rather_than_ending_the_block() {
        let (body, eb_txs, announced) = real_eb();
        let point: EbId = Point::Specific(announced.slot, announced.hash.to_vec());

        // Ask for the whole block at once, and let the relay serve 150 at a
        // time, which is what a relay that caps its reply by size looks like.
        let relay = FakeRelay::new(point, body, eb_txs, 150);
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
        let (body, eb_txs, announced) = real_eb();
        let point: EbId = Point::Specific(announced.slot, announced.hash.to_vec());

        let relay = FakeRelay::new(point, body, eb_txs, usize::MAX);
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
        let (body, eb_txs, announced) = real_eb();
        let point: EbId = Point::Specific(announced.slot, announced.hash.to_vec());

        let mut relay = FakeRelay::new(point, body, eb_txs, 150);
        relay.skip_first = true;
        let mut client = client(relay, 8192);

        let err = client
            .fetch(&announced)
            .await
            .expect_err("a delivery that is not a prefix must be refused");

        match err {
            Error::Misaligned {
                index,
                asked,
                delivered,
                ..
            } => {
                assert_eq!(index, 0, "the first position is the one that misfiles");
                assert_eq!(asked, 425);
                assert_eq!(delivered, 150);
            }
            other => panic!("wrong refusal: {other}"),
        }
    }

    /// MUST FIRE: a body served under an announcement that named a different
    /// hash is refused, and the refusal names both hashes.
    ///
    /// The announced size alone does not identify a body, because any other
    /// body of the same length passes it. Only the hash says the peer served
    /// the endorser block the chain certified, and applying a different one
    /// under a certified block's name puts transactions nobody voted on into
    /// the ledger.
    ///
    /// MUST NOT FIRE: every other test in this module fetches the same body
    /// under its own hash and is answered, so the check refuses the wrong body
    /// rather than refusing every body.
    #[tokio::test]
    async fn a_body_served_under_a_hash_it_does_not_have_is_refused() {
        let (body, eb_txs, mut announced) = real_eb();
        let real = announced.hash;
        announced.hash = Hash::new([9; 32]);
        let point: EbId = Point::Specific(announced.slot, announced.hash.to_vec());

        let mut client = client(FakeRelay::new(point, body, eb_txs, usize::MAX), 64);

        let err = client
            .fetch(&announced)
            .await
            .expect_err("a body that is not the announced one must be refused");

        match err {
            Error::Endorser(endorser::Error::BodyHash {
                announced: named,
                found,
            }) => {
                assert_eq!(
                    named, announced.hash,
                    "the refusal names what was asked for"
                );
                assert_eq!(found, real, "the refusal names what the peer served");
            }
            other => panic!("wrong refusal: {other}"),
        }
    }

    /// MUST NOT FIRE: a transaction is accepted whatever size its body entry
    /// names.
    #[tokio::test]
    async fn a_transaction_is_accepted_whatever_size_its_entry_names() {
        let (body, eb_txs, announced) = real_eb();
        let own = pallas::ledger::primitives::dijkstra::EbAnnouncement {
            eb_hash: announced.hash,
            eb_size: announced.size,
        };
        let entries = EndorserBlockBody::decode_announced(&body, &own).unwrap()[..].to_vec();

        type Resize = fn(u32) -> u32;

        let resizes: [(&str, Resize); 3] = [
            ("zero", |_| 0),
            ("one more", |named| named + 1),
            ("largest", |_| u32::MAX),
        ];

        for (label, resize) in resizes {
            let mut e = minicbor::Encoder::new(Vec::new());
            e.map(entries.len() as u64).unwrap();
            for (hash, size) in &entries {
                e.bytes(hash.as_ref()).unwrap().u32(resize(*size)).unwrap();
            }
            let resized = e.into_writer();

            let announced = AnnouncedEndorserBlock {
                slot: announced.slot,
                hash: pallas::crypto::hash::Hasher::<256>::hash(&resized),
                size: resized.len() as u32,
            };
            let point: EbId = Point::Specific(announced.slot, announced.hash.to_vec());

            let relay = FakeRelay::new(point, resized, eb_txs.clone(), usize::MAX);
            let mut client = client(relay, 64);

            let txs = client
                .fetch(&announced)
                .await
                .unwrap_or_else(|e| panic!("{label}: {e}"));
            assert_eq!(txs.len(), 425, "{label}");
        }
    }

    /// MUST NOT FIRE: every transaction of a real endorser block decodes.
    #[test]
    fn a_delivery_of_dijkstra_transactions_is_finished() {
        let (_, eb_txs, _) = real_eb();
        let wire = eb_txs
            .into_iter()
            .map(leiosfetch::TxCbor::from_raw_bytes)
            .collect();

        let finished = finish(wire).expect("every transaction decodes");

        assert_eq!(finished.len(), 425);
    }

    /// MUST FIRE: delivered bytes that hold no Dijkstra transaction are refused
    /// at their position.
    #[test]
    fn a_delivery_holding_something_other_than_a_transaction_is_refused() {
        let (_, mut eb_txs, _) = real_eb();
        eb_txs[3] = vec![0x01];
        let wire = eb_txs
            .into_iter()
            .map(leiosfetch::TxCbor::from_raw_bytes)
            .collect();

        match finish(wire) {
            Err(Error::Endorser(endorser::Error::TxDecode { index, .. })) => {
                assert_eq!(index, 3);
            }
            other => panic!("wrong answer: {other:?}"),
        }
    }

    /// The body and the transactions of a captured rc-2026w42 endorser block,
    /// taken from the Block and BlockTxs messages a relay sent for it.
    fn captured_eb() -> (Vec<u8>, Vec<leiosfetch::TxCbor>, AnnouncedEndorserBlock) {
        let block: leiosfetch::Message = minicbor::decode(include_bytes!(
            "../../test_data/leios-rc-2026w42/fetch-block.cbor"
        ))
        .unwrap();
        let leiosfetch::Message::Block(body) = block else {
            panic!("fixture precondition");
        };
        let body = body.raw_bytes().to_vec();

        let block_txs: leiosfetch::Message = minicbor::decode(include_bytes!(
            "../../test_data/leios-rc-2026w42/fetch-block-txs.cbor"
        ))
        .unwrap();
        let leiosfetch::Message::BlockTxs { txs } = block_txs else {
            panic!("fixture precondition");
        };

        let announced = AnnouncedEndorserBlock {
            slot: 529,
            hash: "17c497727d273e8dbd0bc188752ef8a7334f5fac6dc077324424a5410cc77fc0"
                .parse()
                .unwrap(),
            size: 2481,
        };

        (body, txs, announced)
    }

    /// MUST FIRE: a captured endorser block comes back whole, each transaction
    /// the `block_transaction` bytes its BlockTxs item held.
    ///
    /// MUST NOT FIRE: none of its 67 transactions is refused as undecodable or
    /// as filed under the wrong reference.
    #[tokio::test]
    async fn a_captured_endorser_block_is_fetched_whole() {
        let (body, txs, announced) = captured_eb();
        assert_eq!(txs.len(), 67, "fixture precondition");
        assert_eq!(body.len(), announced.size as usize, "fixture precondition");

        let point: EbId = Point::Specific(announced.slot, announced.hash.to_vec());
        let eb_txs: Vec<Vec<u8>> = txs.into_iter().map(|tx| tx.unwrap()).collect();
        let relay = FakeRelay::new(point, body, eb_txs.clone(), usize::MAX);
        let mut client = client(relay, 64);

        let fetched = client.fetch(&announced).await.expect("must fetch whole");

        assert_eq!(fetched, eb_txs, "the transactions come back in body order");
        assert_eq!(client.transport.windows, vec![64, 3]);
    }

    fn certifying_block() -> BlockBody {
        include_bytes!("../../test_data/leios-rc-2026w42/certifying-block.cbor").to_vec()
    }

    fn plain_block() -> BlockBody {
        hex::decode(include_str!("../../test_data/dijkstra-plain.block").trim()).unwrap()
    }

    fn payload() -> CertifiedPayload {
        let (_, txs, endorser_block) = captured_eb();

        CertifiedPayload {
            endorser_block,
            txs: txs.into_iter().map(|tx| tx.unwrap()).collect(),
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
        assert_eq!(certifying_slot, 544, "fixture precondition");
        assert_eq!(
            MultiEraBlock::decode(&certifying).unwrap().tx_count(),
            0,
            "fixture precondition"
        );

        let mut pending = PendingPayloads::default();
        pending.expect(certifying_slot);
        pending.deliver(certifying_slot, payload());

        let out = pending
            .apply(vec![plain.clone(), certifying.clone()])
            .expect("must resolve");

        assert_eq!(out.len(), 2);
        assert_eq!(out[0], plain, "an uncertified block must pass through");
        assert_ne!(out[1], certifying, "the certifying block must be rewritten");

        let resolved = MultiEraBlock::decode(&out[1]).unwrap();
        assert_eq!(resolved.tx_count(), 67);
        assert!(resolved.txs().iter().all(|tx| tx.is_valid()));
        assert_eq!(resolved.slot(), certifying_slot);
        assert_eq!(
            resolved.hash(),
            MultiEraBlock::decode(&certifying).unwrap().hash(),
            "the header, and so the hash, is untouched"
        );

        assert!(pending.is_empty());
        assert!(pending.refuse_undelivered().is_ok());
    }

    /// MUST FIRE: a transaction its endorser block commits to with a false flag
    /// is fetched, checked against its reference and applied as invalid, so it
    /// spends only its collateral, and neither it nor its sub transaction
    /// spends any other input or produces an output.
    ///
    /// MUST NOT FIRE: every other transaction of the block, and each of their
    /// sub transactions, spends its inputs and produces its outputs.
    #[tokio::test]
    async fn a_transaction_committed_with_a_false_flag_is_applied_invalid() {
        // Index 5 spends an input besides its collateral, index 8 has a sub transaction.
        const FLAGGED: [usize; 2] = [5, 8];

        let (body, txs, announced) = captured_eb();
        let mut eb_txs: Vec<Vec<u8>> = txs.into_iter().map(|tx| tx.unwrap()).collect();

        for index in FLAGGED {
            let flag = eb_txs[index].last_mut().unwrap();
            assert_eq!(*flag, 0xf5, "fixture precondition");
            *flag = 0xf4;
        }

        let own = pallas::ledger::primitives::dijkstra::EbAnnouncement {
            eb_hash: announced.hash,
            eb_size: announced.size,
        };
        let entries = EndorserBlockBody::decode_announced(&body, &own).unwrap()[..].to_vec();
        let mut e = minicbor::Encoder::new(Vec::new());
        e.map(entries.len() as u64).unwrap();
        for (index, (hash, size)) in entries.iter().enumerate() {
            let hash = match FLAGGED.contains(&index) {
                true => pallas::crypto::hash::Hasher::<256>::hash(&eb_txs[index]),
                false => *hash,
            };
            e.bytes(hash.as_ref()).unwrap().u32(*size).unwrap();
        }
        let body = e.into_writer();

        let announced = AnnouncedEndorserBlock {
            slot: announced.slot,
            hash: pallas::crypto::hash::Hasher::<256>::hash(&body),
            size: body.len() as u32,
        };
        let point: EbId = Point::Specific(announced.slot, announced.hash.to_vec());
        let mut client = client(FakeRelay::new(point, body, eb_txs, usize::MAX), 64);
        let fetched = client.fetch(&announced).await.expect("must fetch whole");

        let certifying = certifying_block();
        let slot = MultiEraBlock::decode(&certifying).unwrap().slot();
        let mut pending = PendingPayloads::default();
        pending.expect(slot);
        pending.deliver(
            slot,
            CertifiedPayload {
                endorser_block: announced,
                txs: fetched,
            },
        );
        let out = pending.apply(vec![certifying]).expect("must resolve");

        let resolved = MultiEraBlock::decode(&out[0]).unwrap();
        let invalid: Vec<usize> = resolved
            .txs()
            .iter()
            .enumerate()
            .filter(|(_, tx)| !tx.is_valid())
            .map(|(index, _)| index)
            .collect();
        assert_eq!(invalid, FLAGGED.to_vec());

        fn refs(inputs: Vec<MultiEraInput<'_>>) -> Vec<TxoRef> {
            inputs
                .iter()
                .map(|input| TxoRef(*input.hash(), input.index() as u32))
                .collect()
        }
        let txs = resolved.txs();
        let beyond = |tx: &MultiEraTx, inputs: Vec<TxoRef>| -> Vec<TxoRef> {
            let collateral = refs(tx.collateral());
            inputs
                .into_iter()
                .filter(|r| !collateral.contains(r))
                .collect()
        };
        let [five, eight] = FLAGGED.map(|index| &txs[index]);
        assert!(
            !beyond(five, refs(five.inputs())).is_empty(),
            "fixture precondition"
        );
        assert!(!five.outputs().is_empty(), "fixture precondition");
        assert_eq!(eight.sub_transactions().len(), 1, "fixture precondition");
        let sub = &eight.sub_transactions()[0];
        assert!(
            !beyond(eight, refs(sub.inputs())).is_empty(),
            "fixture precondition"
        );
        assert!(!sub.outputs().is_empty(), "fixture precondition");

        let sample = txs[0].outputs()[0].encode();
        let loaded: HashMap<TxoRef, OwnedMultiEraOutput> = txs
            .iter()
            .flat_map(|tx| {
                let mut keys = [refs(tx.inputs()), refs(tx.collateral())].concat();
                for sub in tx.sub_transactions() {
                    keys.extend(refs(sub.inputs()));
                }
                keys
            })
            .map(|key| {
                let body = EraCbor(resolved.era().into(), sample.clone());
                (key, OwnedMultiEraOutput::decode(Arc::new(body)).unwrap())
            })
            .collect();

        let delta = compute_apply_delta(&resolved, &loaded).expect("must apply");

        let produced = |hash: Hash<32>| {
            delta
                .produced_utxo
                .keys()
                .filter(|TxoRef(produced, _)| *produced == hash)
                .count()
        };
        let spends = |label: String, spent: Vec<TxoRef>, kept: Vec<TxoRef>| {
            assert!(
                spent.iter().all(|r| delta.consumed_utxo.contains_key(r)),
                "{label} spends {spent:?}"
            );
            assert!(
                kept.iter().all(|r| !delta.consumed_utxo.contains_key(r)),
                "{label} keeps {kept:?}"
            );
        };

        for (index, tx) in txs.iter().enumerate() {
            let flagged = FLAGGED.contains(&index);
            let (spent, kept) = match flagged {
                true => (refs(tx.collateral()), beyond(tx, refs(tx.inputs()))),
                false => (refs(tx.inputs()), vec![]),
            };
            assert!(!spent.is_empty(), "fixture precondition");
            spends(format!("transaction {index}"), spent, kept);

            let outputs = match flagged {
                true => tx.collateral_return().into_iter().count(),
                false => tx.outputs().len(),
            };
            assert_eq!(produced(tx.hash()), outputs, "transaction {index} produces");

            for sub in tx.sub_transactions() {
                let label = format!("sub transaction of {index}");
                let (spent, kept, outputs) = match flagged {
                    true => (vec![], beyond(tx, refs(sub.inputs())), 0),
                    false => (refs(sub.inputs()), vec![], sub.outputs().len()),
                };
                spends(label.clone(), spent, kept);
                assert_eq!(produced(sub.hash()), outputs, "{label} produces");
            }
        }
    }

    /// An rc-2026w42 endorser block whose last transaction the node stored with
    /// a false flag, and the block that certifies it.
    fn invalid_tx_eb() -> (Vec<u8>, Vec<Vec<u8>>, AnnouncedEndorserBlock, BlockBody) {
        let body = include_bytes!("../../test_data/leios-rc-2026w42-invalid-tx/eb-body.cbor");
        let array = include_bytes!("../../test_data/leios-rc-2026w42-invalid-tx/eb-txs.cbor");

        let mut d = minicbor::Decoder::new(array);
        let count = d.array().unwrap().unwrap();
        let txs = (0..count)
            .map(|_| {
                let start = d.position();
                d.skip().unwrap();
                array[start..d.position()].to_vec()
            })
            .collect();

        let announced = AnnouncedEndorserBlock {
            slot: 427,
            hash: "60dd69251684ae70378f3f2c2b1619f71f6dd49a36799b523f052582fb944d18"
                .parse()
                .unwrap(),
            size: 2888,
        };
        let certifying =
            include_bytes!("../../test_data/leios-rc-2026w42-invalid-tx/certifying-block.cbor");

        (body.to_vec(), txs, announced, certifying.to_vec())
    }

    /// MUST FIRE: a transaction the node applied as invalid in a certified
    /// endorser block is fetched, checked against its reference and applied as
    /// invalid, so it spends only its collateral and produces only its
    /// collateral return, at the index after its outputs.
    ///
    /// MUST NOT FIRE: each of the other 77 transactions of the block spends its
    /// inputs and produces its outputs.
    #[tokio::test]
    async fn a_transaction_the_node_applied_as_invalid_is_applied_invalid() {
        const INVALID: usize = 77;

        let (body, eb_txs, announced, certifying) = invalid_tx_eb();
        assert_eq!(eb_txs.len(), 78, "fixture precondition");
        assert_eq!(eb_txs[INVALID].last(), Some(&0xf4), "fixture precondition");
        assert_eq!(body.len(), announced.size as usize, "fixture precondition");

        let point: EbId = Point::Specific(announced.slot, announced.hash.to_vec());
        let relay = FakeRelay::new(point, body, eb_txs.clone(), usize::MAX);
        let mut client = client(relay, 64);
        let fetched = client.fetch(&announced).await.expect("must fetch whole");
        assert_eq!(fetched, eb_txs, "the transactions come back in body order");

        let slot = MultiEraBlock::decode(&certifying).unwrap().slot();
        assert_eq!(slot, 491, "fixture precondition");
        let mut pending = PendingPayloads::default();
        pending.expect(slot);
        pending.deliver(
            slot,
            CertifiedPayload {
                endorser_block: announced,
                txs: fetched,
            },
        );
        let out = pending.apply(vec![certifying]).expect("must resolve");

        let resolved = MultiEraBlock::decode(&out[0]).unwrap();
        let txs = resolved.txs();
        let invalid: Vec<usize> = txs
            .iter()
            .enumerate()
            .filter(|(_, tx)| !tx.is_valid())
            .map(|(index, _)| index)
            .collect();
        assert_eq!(invalid, vec![INVALID]);

        fn refs(inputs: Vec<MultiEraInput<'_>>) -> Vec<TxoRef> {
            inputs
                .iter()
                .map(|input| TxoRef(*input.hash(), input.index() as u32))
                .collect()
        }
        let failed = &txs[INVALID];
        assert_eq!(
            failed.hash().to_string(),
            "9035d7a679633106742b7c4b857883e9e37943a8862f9a629407a5f2e991046b",
            "fixture precondition"
        );
        let collateral = refs(failed.collateral());
        let script_inputs = refs(failed.inputs());
        assert_eq!(collateral.len(), 1, "fixture precondition");
        assert!(
            script_inputs.iter().all(|r| !collateral.contains(r)),
            "fixture precondition"
        );
        let collateral_return = failed.collateral_return().expect("fixture precondition");
        assert_eq!(
            collateral_return.value().coin(),
            18_000_000,
            "fixture precondition"
        );
        assert!(
            txs.iter().all(|tx| tx.sub_transactions().is_empty()),
            "fixture precondition"
        );

        let sample = txs[0].outputs()[0].encode();
        let loaded: HashMap<TxoRef, OwnedMultiEraOutput> = txs
            .iter()
            .flat_map(|tx| [refs(tx.inputs()), refs(tx.collateral())].concat())
            .map(|key| {
                let body = EraCbor(resolved.era().into(), sample.clone());
                (key, OwnedMultiEraOutput::decode(Arc::new(body)).unwrap())
            })
            .collect();

        let delta = compute_apply_delta(&resolved, &loaded).expect("must apply");

        for (index, tx) in txs.iter().enumerate() {
            let (spent, kept) = match index == INVALID {
                true => (collateral.clone(), script_inputs.clone()),
                false => (refs(tx.inputs()), vec![]),
            };
            assert!(
                spent.iter().all(|r| delta.consumed_utxo.contains_key(r)),
                "transaction {index} spends {spent:?}"
            );
            assert!(
                kept.iter().all(|r| !delta.consumed_utxo.contains_key(r)),
                "transaction {index} keeps {kept:?}"
            );

            let mut produced: Vec<u32> = delta
                .produced_utxo
                .keys()
                .filter(|TxoRef(hash, _)| *hash == tx.hash())
                .map(|TxoRef(_, at)| *at)
                .collect();
            produced.sort();
            let expected: Vec<u32> = match index == INVALID {
                true => vec![tx.outputs().len() as u32],
                false => (0..tx.outputs().len() as u32).collect(),
            };
            assert_eq!(produced, expected, "transaction {index} produces");
        }

        let returned = &delta.produced_utxo[&TxoRef(failed.hash(), failed.outputs().len() as u32)];
        assert_eq!(
            returned.1,
            collateral_return.encode(),
            "the output produced is the collateral return"
        );
    }

    /// MUST FIRE: a block that carries transactions of its own is refused when
    /// a payload is delivered at its slot, since inlining would replace them.
    #[test]
    fn a_block_with_transactions_of_its_own_is_not_inlined() {
        let plain = plain_block();
        let decoded = MultiEraBlock::decode(&plain).unwrap();
        let (slot, own) = (decoded.slot(), decoded.tx_count());
        assert!(own > 0, "fixture precondition");

        let mut pending = PendingPayloads::default();
        pending.expect(slot);
        pending.deliver(slot, payload());

        match pending.apply(vec![plain.clone()]) {
            Err(Error::CertifiesAndCarries {
                slot: refused,
                count,
            }) => {
                assert_eq!((refused, count), (slot, own));
            }
            other => panic!("wrong answer: {other:?}"),
        }
    }

    /// MUST FIRE: a payload keyed to a slot no block in the batch carries is
    /// refused rather than silently dropped, which is what a short or reordered
    /// batch would otherwise cause.
    #[test]
    fn a_payload_no_block_claimed_is_refused() {
        let mut pending = PendingPayloads::default();
        pending.expect(999_999);
        pending.deliver(999_999, payload());

        let out = pending.apply(vec![plain_block()]).unwrap();
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

        let out = pending.apply(blocks.clone()).unwrap();

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
            .apply(vec![plain_block(), certifying.clone()])
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
            .apply(vec![certifying.clone()])
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

    /// How a connection through [`idle_cutting_proxy`] ended.
    #[derive(Debug, PartialEq)]
    enum Ended {
        ClientClosed,
        ClientIdle,
    }

    /// A TCP proxy to `upstream` that closes a connection once its client has
    /// sent nothing for `cutoff`, reporting how each connection ended.
    async fn idle_cutting_proxy(
        upstream: std::net::SocketAddr,
        cutoff: Duration,
    ) -> (
        std::net::SocketAddr,
        tokio::sync::mpsc::UnboundedReceiver<Ended>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (report, ended) = tokio::sync::mpsc::unbounded_channel();

        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let report = report.clone();

                tokio::spawn(async move {
                    let server = tokio::net::TcpStream::connect(upstream).await.unwrap();
                    let (mut client_read, mut client_write) = client.into_split();
                    let (mut server_read, mut server_write) = server.into_split();

                    let downstream = tokio::spawn(async move {
                        let _ = tokio::io::copy(&mut server_read, &mut client_write).await;
                    });

                    let mut buffer = vec![0u8; 1 << 16];

                    let how = loop {
                        match tokio::time::timeout(cutoff, client_read.read(&mut buffer)).await {
                            Err(_) => break Ended::ClientIdle,
                            Ok(Ok(0)) | Ok(Err(_)) => break Ended::ClientClosed,
                            Ok(Ok(read)) => {
                                if server_write.write_all(&buffer[..read]).await.is_err() {
                                    break Ended::ClientClosed;
                                }
                            }
                        }
                    };

                    downstream.abort();
                    let _ = report.send(how);
                });
            }
        });

        (address, ended)
    }

    /// A leios-fetch responder on a local port that serves one endorser block
    /// and answers each leios-notify request with an offer, counting the
    /// offers.
    async fn endorser_block_responder(
        body: Vec<u8>,
        eb_txs: Vec<Vec<u8>>,
    ) -> (std::net::SocketAddr, Arc<AtomicUsize>) {
        use pallas::network2::behavior::responder::handshake::{
            HandshakeResponder, HandshakeResponderConfig,
        };
        use pallas::network2::behavior::responder::{
            ResponderBehavior, ResponderCommand, ResponderEvent,
        };
        use pallas::network2::interface::TcpListenerInterface;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let behavior = ResponderBehavior {
            handshake: HandshakeResponder::new(HandshakeResponderConfig {
                supported_version: VersionTable::v11_and_above_with_query(164, false),
            }),
            ..Default::default()
        };

        let mut network: Manager<TcpListenerInterface<AnyMessage>, _, AnyMessage> =
            Manager::new(TcpListenerInterface::new(listener), behavior);

        let offered = Arc::new(AtomicUsize::new(0));
        let counter = offered.clone();

        tokio::spawn(async move {
            loop {
                let Some(event) = network.poll_next().await else {
                    continue;
                };

                match event {
                    ResponderEvent::PeerInitialized(..) => {
                        network.execute(ResponderCommand::Housekeeping)
                    }
                    ResponderEvent::EbRequested(pid, _) => network.execute(
                        ResponderCommand::ProvideEb(pid, AnyCbor::from_raw_bytes(body.clone())),
                    ),
                    ResponderEvent::EbTxsRequested(pid, _, bitmaps) => {
                        let txs = FakeRelay::indices(&bitmaps)
                            .into_iter()
                            .map(|index| leiosfetch::TxCbor::from_raw_bytes(eb_txs[index].clone()))
                            .collect();

                        network.execute(ResponderCommand::ProvideEbTxs(pid, txs))
                    }
                    ResponderEvent::EbNotificationRequested(pid) => {
                        counter.fetch_add(1, Ordering::SeqCst);
                        network.execute(ResponderCommand::ProvideEbOffer(
                            pid,
                            Point::Specific(1, vec![0; 32]),
                            1,
                        ))
                    }
                    _ => (),
                }
            }
        });

        (address, offered)
    }

    /// The gap the proxy in front of the responder lets a client stay silent.
    const PROXY_CUTOFF: Duration = Duration::from_secs(3);

    /// MUST FIRE: a fetch after the client was idle for twice the gap the peer
    /// allows a silent client still gets the whole endorser block, because the
    /// connection is kept alive between fetches.
    #[tokio::test]
    async fn a_fetch_after_an_idle_gap_longer_than_the_peer_allows_still_answers() {
        let (body, eb_txs, announced) = real_eb();
        let (responder, _) = endorser_block_responder(body, eb_txs).await;
        let (proxy, mut ended) = idle_cutting_proxy(responder, PROXY_CUTOFF).await;

        let mut client = LeiosClient::new(&proxy.to_string(), 164).unwrap();

        let first = client.fetch(&announced).await.map(|txs| txs.len());
        tokio::time::sleep(PROXY_CUTOFF * 2).await;
        let second = client.fetch(&announced).await.map(|txs| txs.len());

        assert!(
            matches!((&first, &second), (Ok(425), Ok(425))),
            "{first:?} then {second:?}"
        );
        assert!(ended.try_recv().is_err(), "the proxy cut the connection");
    }

    /// MUST FIRE: a fetch after the client's thread was blocked for several
    /// housekeeping ticks still gets the whole endorser block.
    #[tokio::test]
    async fn a_fetch_after_a_blocked_thread_still_answers() {
        let (body, eb_txs, announced) = real_eb();
        let (responder, _) = endorser_block_responder(body, eb_txs).await;

        let mut client = LeiosClient::new(&responder.to_string(), 164).unwrap();

        let first = client.fetch(&announced).await.map(|txs| txs.len());
        std::thread::sleep(Duration::from_secs(5));
        tokio::time::sleep(Duration::from_secs(3)).await;
        let second = client.fetch(&announced).await.map(|txs| txs.len());

        assert!(
            matches!((&first, &second), (Ok(425), Ok(425))),
            "{first:?} then {second:?}"
        );
    }

    /// MUST FIRE: the proxy closes a connection whose client sends nothing for
    /// longer than the cutoff.
    #[tokio::test]
    async fn the_proxy_closes_a_connection_its_client_leaves_silent() {
        use tokio::io::AsyncReadExt;

        let (body, eb_txs, _) = real_eb();
        let (responder, _) = endorser_block_responder(body, eb_txs).await;
        let (proxy, mut ended) = idle_cutting_proxy(responder, PROXY_CUTOFF).await;

        let mut silent = tokio::net::TcpStream::connect(proxy).await.unwrap();
        let mut buffer = [0u8; 1];

        let read = tokio::time::timeout(PROXY_CUTOFF * 2, silent.read(&mut buffer)).await;

        assert!(matches!(read, Ok(Ok(0))), "{read:?}");
        assert_eq!(ended.recv().await, Some(Ended::ClientIdle));
    }

    /// MUST NOT FIRE: dropping a client closes its connection.
    #[tokio::test]
    async fn a_dropped_client_closes_its_connection() {
        let (body, eb_txs, announced) = real_eb();
        let (responder, _) = endorser_block_responder(body, eb_txs).await;
        let (proxy, mut ended) = idle_cutting_proxy(responder, PROXY_CUTOFF).await;

        let mut client = LeiosClient::new(&proxy.to_string(), 164).unwrap();
        let fetched = client.fetch(&announced).await.map(|txs| txs.len());
        drop(client);

        let how = tokio::time::timeout(PROXY_CUTOFF / 2, ended.recv()).await;

        assert!(matches!(fetched, Ok(425)), "{fetched:?}");
        assert_eq!(how, Ok(Some(Ended::ClientClosed)));
    }

    /// MUST NOT FIRE: the client holds none of the offers a peer makes between
    /// fetches.
    #[tokio::test]
    async fn offers_made_between_fetches_are_not_held() {
        let (body, eb_txs, announced) = real_eb();
        let (responder, offered) = endorser_block_responder(body, eb_txs).await;

        let mut client = LeiosClient::new(&responder.to_string(), 164).unwrap();
        let fetched = client.fetch(&announced).await.map(|txs| txs.len());
        let before = offered.load(Ordering::SeqCst);

        tokio::time::sleep(HOUSEKEEPING_TICK * 4).await;

        let between = offered.load(Ordering::SeqCst) - before;

        assert!(matches!(fetched, Ok(425)), "{fetched:?}");
        assert!(
            between >= 2,
            "the peer made {between} offers between fetches"
        );
        assert_eq!(client.transport.events.len(), 0);
    }

    /// MUST FIRE: a fetch from a peer that closes every connection it accepts
    /// ends with the disconnection.
    #[tokio::test]
    async fn a_peer_that_closes_the_connection_ends_the_fetch() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                drop(socket);
            }
        });

        let (_, _, announced) = real_eb();
        let mut client = LeiosClient::new(&address.to_string(), 164).unwrap();
        let fetched = client.fetch(&announced).await.map(|txs| txs.len());

        assert!(
            matches!(fetched, Err(Error::Disconnected { .. })),
            "{fetched:?}"
        );
    }
}
