//! Checks that a fetched endorser block body hashes to its announcement and
//! that a certifying header names the parent its announcement is read from.

use std::ops::Deref;

use pallas::codec::minicbor;
use pallas::codec::utils::KeepRaw;
use pallas::crypto::hash::Hash;
use pallas::ledger::primitives::dijkstra;
use pallas::ledger::traverse::{MultiEraHeader, OriginalHash};

/// Why an endorser block body, one of its transactions, or a certificate was refused.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid endorser block body: {0}")]
    InvalidBody(String),

    #[error("endorser block hash {found}, announced {announced}")]
    BodyHash {
        announced: Hash<32>,
        found: Hash<32>,
    },

    #[error("transaction {index} is not a Dijkstra transaction: {reason}")]
    TxDecode { index: usize, reason: String },

    #[error("header at slot {slot} has parent {previous:?}, given {parent:?}")]
    NotParent {
        slot: u64,
        previous: Option<Hash<32>>,
        parent: Option<Hash<32>>,
    },

    #[error("header at slot {slot} certifies, its parent announced none")]
    CertifiesNothing { slot: u64 },
}

/// An endorser block body whose bytes hash to the hash its announcement names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndorserBlockBody(dijkstra::EndorserBlock);

impl Deref for EndorserBlockBody {
    type Target = dijkstra::EndorserBlock;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl EndorserBlockBody {
    /// Decodes the body map at the start of `cbor`, refusing a map whose bytes
    /// do not hash to the announced hash.
    pub fn decode_announced(
        cbor: &[u8],
        announcement: &dijkstra::EbAnnouncement,
    ) -> Result<Self, Error> {
        let block: KeepRaw<dijkstra::EndorserBlock> =
            minicbor::decode(cbor).map_err(|e| Error::InvalidBody(e.to_string()))?;

        let found = block.original_hash();
        if found != announcement.eb_hash {
            return Err(Error::BodyHash {
                announced: announcement.eb_hash,
                found,
            });
        }

        Ok(Self(block.unwrap()))
    }
}

/// The announcement of `parent` that `header` certifies, where a `None` parent
/// is genesis.
pub fn certification<'a>(
    parent: Option<&'a MultiEraHeader<'_>>,
    header: &MultiEraHeader<'_>,
) -> Result<Option<&'a dijkstra::EbAnnouncement>, Error> {
    if header.block_body_contains_leios_cert() != Some(true) {
        return Ok(None);
    }

    let parent_hash = parent.map(|p| p.hash());

    if header.previous_hash() != parent_hash {
        return Err(Error::NotParent {
            slot: header.slot(),
            previous: header.previous_hash(),
            parent: parent_hash,
        });
    }

    parent
        .and_then(|p| p.eb_announcement())
        .map(Some)
        .ok_or(Error::CertifiesNothing {
            slot: header.slot(),
        })
}

#[cfg(test)]
mod tests {
    use pallas::crypto::hash::Hasher;
    use pallas::ledger::traverse::MultiEraBlock;

    use super::*;

    fn hex_bytes(text: &str) -> Vec<u8> {
        hex::decode(text.trim()).unwrap()
    }

    fn body() -> Vec<u8> {
        hex_bytes(include_str!("../../test_data/dijkstra-eb3.ebbody"))
    }

    fn announcing() -> Vec<u8> {
        hex_bytes(include_str!(
            "../../test_data/musashi-w36/ranking-announce-with-txs.block"
        ))
    }

    fn certifying() -> Vec<u8> {
        hex_bytes(include_str!(
            "../../test_data/musashi-w36/ranking-announce-quiet.block"
        ))
    }

    fn unrelated() -> Vec<u8> {
        hex_bytes(include_str!(
            "../../test_data/musashi-w36/epoch-boundary-before.block"
        ))
    }

    fn certifies_nothing() -> Vec<u8> {
        hex_bytes(include_str!(
            "../../test_data/musashi-w36/epoch-boundary-after.block"
        ))
    }

    fn decoded(cbor: &[u8]) -> MultiEraBlock<'_> {
        MultiEraBlock::decode(cbor).unwrap()
    }

    fn announced_as(hash: Hash<32>, cbor: &[u8]) -> dijkstra::EbAnnouncement {
        dijkstra::EbAnnouncement {
            eb_hash: hash,
            eb_size: cbor.len() as u32,
        }
    }

    fn announced_as_itself(cbor: &[u8]) -> Result<EndorserBlockBody, Error> {
        EndorserBlockBody::decode_announced(cbor, &announced_as(Hasher::<256>::hash(cbor), cbor))
    }

    fn own_announcement() -> dijkstra::EbAnnouncement {
        let cbor = body();
        announced_as(Hasher::<256>::hash(&cbor), &cbor)
    }

    fn entries() -> Vec<(Hash<32>, u32)> {
        announced_as_itself(&body()).unwrap()[..].to_vec()
    }

    /// The entries as a definite length map with each size in its shortest form.
    fn encode_entries(entries: &[(Hash<32>, u32)]) -> Vec<u8> {
        let mut e = minicbor::Encoder::new(Vec::new());
        e.map(entries.len() as u64).unwrap();
        for (hash, size) in entries {
            e.bytes(hash.as_ref()).unwrap().u32(*size).unwrap();
        }
        e.into_writer()
    }

    /// The fixture's entries in two encodings other than the announced one: each
    /// size written in four bytes, and a map of indefinite length.
    fn other_forms_of_the_body() -> [(&'static str, Vec<u8>); 2] {
        let entries = entries();
        assert_eq!(encode_entries(&entries), body(), "fixture precondition");

        let mut wide = minicbor::Encoder::new(Vec::new());
        wide.map(entries.len() as u64).unwrap();
        for (hash, size) in &entries {
            wide.bytes(hash.as_ref()).unwrap();
            wide.writer_mut().push(0x1a);
            wide.writer_mut().extend_from_slice(&size.to_be_bytes());
        }

        let mut indefinite = minicbor::Encoder::new(Vec::new());
        indefinite.begin_map().unwrap();
        for (hash, size) in &entries {
            indefinite.bytes(hash.as_ref()).unwrap().u32(*size).unwrap();
        }
        indefinite.end().unwrap();

        [
            ("wide", wide.into_writer()),
            ("indefinite", indefinite.into_writer()),
        ]
    }

    fn header_cbor(block: &[u8]) -> Vec<u8> {
        decoded(block).header().cbor().to_vec()
    }

    fn dijkstra_header(cbor: &[u8]) -> MultiEraHeader<'_> {
        MultiEraHeader::decode(7, None, cbor).unwrap()
    }

    /// A body decodes under the announcement of its own hash.
    #[test]
    fn a_body_decodes_under_its_own_hash() {
        let cbor = body();
        let own = Hasher::<256>::hash(&cbor);

        let decoded = EndorserBlockBody::decode_announced(&cbor, &announced_as(own, &cbor))
            .expect("the body is announced under its own hash");

        assert_eq!(decoded.len(), 425);
    }

    /// A body under an announcement of another hash is refused, and the
    /// refusal names both hashes.
    #[test]
    fn a_body_under_another_hash_is_refused() {
        let cbor = body();
        let own = Hasher::<256>::hash(&cbor);
        let other = Hash::new([9; 32]);

        let refused = EndorserBlockBody::decode_announced(&cbor, &announced_as(other, &cbor));

        match refused {
            Err(Error::BodyHash { announced, found }) => {
                assert_eq!(announced, other);
                assert_eq!(found, own);
            }
            other => panic!("wrong answer: {other:?}"),
        }
    }

    /// A certificate given the parent it names reads that parent's
    /// announcement.
    #[test]
    fn a_certificate_given_its_parent_reads_the_announcement() {
        let (parent, child) = (announcing(), certifying());
        let (parent, child) = (decoded(&parent), decoded(&child));
        let (parent, child) = (parent.header(), child.header());

        let read = certification(Some(&parent), &child).unwrap();

        assert_eq!(read, parent.eb_announcement());
        assert!(read.is_some());
    }

    /// A header that certifies nothing reads nothing, whatever it is given as
    /// its parent.
    #[test]
    fn a_header_that_certifies_nothing_is_not_checked_against_its_parent() {
        let (given, child) = (announcing(), certifies_nothing());
        let (given, child) = (decoded(&given), decoded(&child));
        let (given, child) = (given.header(), child.header());

        let read = certification(Some(&given), &child).unwrap();

        assert_eq!(read, None);
    }

    /// A certificate given a header other than the parent it names is refused,
    /// and the refusal names both hashes.
    #[test]
    fn a_certificate_given_another_parent_is_refused() {
        let (named, given, child) = (announcing(), unrelated(), certifying());
        let named = decoded(&named).hash();
        let (given, child) = (decoded(&given), decoded(&child));
        let (given, child) = (given.header(), child.header());

        let refused = certification(Some(&given), &child);

        match refused {
            Err(Error::NotParent {
                slot,
                previous,
                parent,
            }) => {
                assert_eq!(slot, child.slot());
                assert_eq!(previous, Some(named));
                assert_eq!(parent, Some(given.hash()));
            }
            other => panic!("wrong answer: {other:?}"),
        }
    }

    /// A certificate with no parent given is refused as not naming genesis.
    #[test]
    fn a_certificate_with_no_parent_given_is_refused() {
        let (named, child) = (announcing(), certifying());
        let named = decoded(&named).hash();
        let child = decoded(&child);
        let child = child.header();

        let refused = certification(None, &child);

        match refused {
            Err(Error::NotParent {
                previous, parent, ..
            }) => {
                assert_eq!(previous, Some(named));
                assert_eq!(parent, None);
            }
            other => panic!("wrong answer: {other:?}"),
        }
    }

    /// Bytes after the body map are not part of the body, and the map is
    /// accepted against the hash of its own bytes.
    #[test]
    fn a_body_with_trailing_bytes_is_accepted_against_the_hash_of_its_map() {
        let mut cbor = body();
        cbor.push(0x00);

        let decoded = EndorserBlockBody::decode_announced(&cbor, &own_announcement())
            .unwrap_or_else(|e| panic!("trailing bytes: {e}"));

        assert_eq!(decoded.len(), 425);
    }

    /// An empty body, which is how leios-fetch answers for a block the peer
    /// lacks, is refused under the announcement of a body that names
    /// transactions. An empty body under the announcement of its own hash is an
    /// endorser block that names nothing, and is accepted.
    #[test]
    fn an_empty_reply_is_refused_against_the_announcement_that_named_it() {
        let announcement = own_announcement();

        let refused = EndorserBlockBody::decode_announced(&[0xa0], &announcement);

        match refused {
            Err(Error::BodyHash { announced, found }) => {
                assert_eq!(announced, announcement.eb_hash);
                assert_eq!(found, Hasher::<256>::hash(&[0xa0]));
            }
            other => panic!("wrong answer: {other:?}"),
        }

        let empty = announced_as_itself(&[0xa0]).expect("an empty endorser block");
        assert!(empty.is_empty());
    }

    /// A body is accepted whatever size its announcement names.
    #[test]
    fn a_body_is_accepted_whatever_size_the_announcement_names() {
        let cbor = body();
        let own = own_announcement();
        assert_eq!(own.eb_size, 15_303, "fixture precondition");

        for eb_size in [0, 15_302, 15_304, u32::MAX] {
            let announcement = dijkstra::EbAnnouncement {
                eb_size,
                ..own.clone()
            };

            let decoded = EndorserBlockBody::decode_announced(&cbor, &announcement)
                .unwrap_or_else(|e| panic!("announced size {eb_size}: {e}"));
            assert_eq!(decoded.len(), 425);
        }
    }

    /// A body that is not a map is refused as an invalid body.
    #[test]
    fn a_body_that_is_not_a_map_is_refused() {
        let refused = announced_as_itself(&[0x83, 0x01, 0x02]);

        assert!(
            matches!(refused, Err(Error::InvalidBody(_))),
            "wrong answer: {refused:?}"
        );
    }

    /// A body of the announced length that is another endorser block is
    /// refused, and the message names both hashes.
    #[test]
    fn a_body_that_is_a_different_endorser_block_is_refused() {
        let cbor = body();
        let announcement = own_announcement();

        assert_eq!(&cbor[3..5], &[0x58, 0x20], "fixture precondition");
        let mut other = cbor.clone();
        other[5] ^= 0xff;

        let refused = EndorserBlockBody::decode_announced(&other, &announcement)
            .expect_err("another endorser block must be refused");

        let said = refused.to_string();
        assert!(
            said.contains(&announcement.eb_hash.to_string()),
            "the message does not name the announced hash: {said}"
        );
        assert!(
            said.contains(&Hasher::<256>::hash(&other).to_string()),
            "the message does not name the hash of the body that arrived: {said}"
        );
    }

    /// The same entries in another encoding are accepted under the hash of
    /// their own bytes.
    #[test]
    fn a_body_is_accepted_when_the_announcement_is_the_hash_of_its_bytes() {
        let entries = entries();

        for (form, cbor) in other_forms_of_the_body() {
            assert_ne!(cbor, body(), "{form} precondition");

            let decoded = announced_as_itself(&cbor).unwrap_or_else(|e| panic!("{form}: {e}"));

            assert_eq!(decoded[..], entries[..], "{form}");
        }
    }

    /// The same entries in another encoding are refused under the hash of the
    /// announced bytes.
    #[test]
    fn a_body_whose_bytes_differ_from_the_announced_ones_is_refused_on_hash() {
        let announcement = own_announcement();

        for (form, cbor) in other_forms_of_the_body() {
            match EndorserBlockBody::decode_announced(&cbor, &announcement) {
                Err(Error::BodyHash { announced, found }) => {
                    assert_eq!(announced, announcement.eb_hash, "{form}");
                    assert_eq!(found, Hasher::<256>::hash(&cbor), "{form}");
                }
                other => panic!("{form}: wrong answer: {other:?}"),
            }
        }
    }

    /// A body that names one transaction twice is accepted with both entries.
    #[test]
    fn a_body_naming_one_transaction_twice_is_accepted() {
        let entries = entries();
        let (first, second) = (entries[0], entries[1]);
        assert_ne!(first.0, second.0, "fixture precondition");

        let decoded = announced_as_itself(&encode_entries(&[first, second, first]))
            .unwrap_or_else(|e| panic!("a repeated entry: {e}"));

        assert_eq!(decoded[..], [first, second, first]);
    }

    /// A certifying header whose parent announced nothing is refused. No
    /// fixture certifies after such a parent, so the first header of epoch 56
    /// is rewritten to certify after the last block of epoch 55.
    #[test]
    fn a_certificate_after_a_parent_that_announced_nothing_is_refused() {
        let parent = header_cbor(&unrelated());
        let parent = dijkstra_header(&parent);
        assert!(parent.eb_announcement().is_none(), "fixture precondition");

        let child = header_cbor(&certifies_nothing());
        let mut child: dijkstra::Header = minicbor::decode(&child).unwrap();
        child.header_body.block_body_contains_leios_cert = true;
        let child = minicbor::to_vec(&child).unwrap();
        let child = dijkstra_header(&child);
        assert_eq!(
            child.previous_hash(),
            Some(parent.hash()),
            "fixture precondition"
        );

        match certification(Some(&parent), &child) {
            Err(Error::CertifiesNothing { slot }) => assert_eq!(slot, 1_209_609),
            other => panic!("wrong answer: {other:?}"),
        }
    }

    /// A header of an era with no certificates certifies nothing, whatever it
    /// is given as its parent.
    #[test]
    fn a_header_of_an_era_before_leios_certifies_nothing() {
        let given = announcing();
        let conway = hex_bytes(include_str!("../../test_data/conway.block"));
        let (given, conway) = (decoded(&given), decoded(&conway));
        let (given, conway) = (given.header(), conway.header());
        assert_eq!(
            conway.block_body_contains_leios_cert(),
            None,
            "fixture precondition"
        );

        assert_eq!(certification(Some(&given), &conway).unwrap(), None);
        assert_eq!(certification(None, &conway).unwrap(), None);
    }
}
