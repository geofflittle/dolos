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

use std::collections::BTreeMap;
use std::time::Duration;

use pallas::ledger::traverse::leios::{
    resolve_certified_block, AnnouncedEndorserBlock, EndorserBlockBody,
};
use pallas::ledger::traverse::MultiEraBlock;
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

use dolos_core::BlockBody;

/// Transactions asked for in one leios-fetch request. The selector addresses
/// transactions in 64 wide windows, and the relay caps a response, so a request
/// that spans more than one window can come back short. A short reply is asked
/// for again rather than taken as the end of the endorser block.
const TXS_PER_REQUEST: usize = 64;

/// How long to wait for one endorser block before giving up and letting the
/// stage retry, which also reconnects.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

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

/// Payloads fetched for blocks not yet pulled, keyed by the slot of the ranking
/// block that certifies them.
///
/// Keying by slot rather than by position is what stops a short or reordered
/// block batch from moving one block's transactions onto another.
#[derive(Debug, Default)]
pub struct PendingPayloads(BTreeMap<u64, CertifiedPayload>);

impl PendingPayloads {
    pub fn insert(&mut self, certifying_slot: u64, payload: CertifiedPayload) {
        self.0.insert(certifying_slot, payload);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Resolves each block that certifies an endorser block against the payload
    /// fetched for it, leaving every other block byte identical.
    ///
    /// Every payload this consumes is removed. A payload left over is an
    /// endorser block that was fetched and never applied, which is refused
    /// rather than dropped, because dropping it is exactly the silent loss this
    /// module exists to prevent.
    pub fn apply(&mut self, blocks: Vec<BlockBody>) -> Result<Vec<BlockBody>, Error> {
        let mut out = Vec::with_capacity(blocks.len());

        for cbor in blocks {
            let slot = MultiEraBlock::decode(&cbor)
                .map_err(|e| Error::BadBlock(e.to_string()))?
                .slot();

            match self.0.remove(&slot) {
                None => out.push(cbor),
                Some(payload) => {
                    let borrowed: Vec<&[u8]> = payload.txs.iter().map(|t| t.as_slice()).collect();
                    let resolved = resolve_certified_block(&cbor, &borrowed)?;

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

    /// Refuses any payload that no block claimed, once a batch is known to be
    /// complete.
    pub fn refuse_undelivered(&self) -> Result<(), Error> {
        if let Some((slot, payload)) = self.0.iter().next() {
            return Err(Error::Undelivered {
                slot: *slot,
                hash: payload.endorser_block.hash.to_string(),
            });
        }

        Ok(())
    }
}

/// A leios-fetch client against one peer.
///
/// leios-fetch lives in the newer networking stack, which the chainsync and
/// blockfetch client of this stage does not speak, so this is a second
/// connection to a peer that does.
pub struct LeiosClient {
    network: Manager<TcpInterface<AnyMessage>, InitiatorBehavior, AnyMessage>,
    peer: Option<PeerId>,
    address: String,
}

impl LeiosClient {
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

        Ok(Self {
            network,
            peer: None,
            address: address.to_string(),
        })
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

        let mut ticker = interval(Duration::from_secs(1));
        let started = tokio::time::Instant::now();

        let mut body: Option<EndorserBlockBody> = None;
        let mut txs: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
        let mut inflight: Vec<usize> = Vec::new();
        let mut asked_for_body = false;

        loop {
            if started.elapsed() > FETCH_TIMEOUT {
                return Err(Error::Timeout {
                    slot: eb.slot,
                    hash: eb.hash.to_string(),
                });
            }

            // issue the next request the state allows
            if let Some(pid) = self.peer.clone() {
                match body.as_ref() {
                    None => {
                        if !asked_for_body {
                            asked_for_body = true;
                            self.network
                                .execute(InitiatorCommand::FetchEb(pid, point.clone()));
                        }
                    }
                    Some(decoded) if inflight.is_empty() => {
                        let want: Vec<usize> = (0..decoded.len())
                            .filter(|i| !txs.contains_key(i))
                            .take(TXS_PER_REQUEST)
                            .collect();

                        if want.is_empty() {
                            let wire: Vec<Vec<u8>> = txs.into_values().collect();
                            return finish(decoded, wire);
                        }

                        inflight = want.clone();
                        self.network.execute(InitiatorCommand::FetchEbTxs(
                            pid,
                            point.clone(),
                            leiosfetch::Bitmaps::from_indices(want),
                        ));
                    }
                    _ => (),
                }
            }

            tokio::select! {
                _ = ticker.tick() => {
                    self.network.execute(InitiatorCommand::Housekeeping);
                }
                event = self.network.poll_next() => {
                    let Some(event) = event else { continue };

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

                                    if delivered.len() != asked.len() {
                                        warn!(
                                            eb = %eb.hash,
                                            asked = asked.len(),
                                            got = delivered.len(),
                                            "short leios-fetch window, asking for the rest"
                                        );
                                    }

                                    for (index, tx) in asked.iter().zip(delivered.iter()) {
                                        txs.insert(*index, tx.raw_bytes().to_vec());
                                    }
                                }
                            }
                        }
                        other => debug!(?other, "unhandled leios event"),
                    }
                }
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
        pending.insert(certifying_slot, payload());

        let out = pending
            .apply(vec![plain.clone(), certifying.clone()])
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
        pending.insert(999_999, payload());

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
}
