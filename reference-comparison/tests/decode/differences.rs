//! The classes where pallas's reading differs from the ledger's, each with
//! what holds the difference.

use ledger_oracle_judge::comparison::{
    Allowance, Allowed, Answer, Commit, Disagreement, Layer, Owner, Row,
};

use super::{Decoded, Refusal};

/// The rules whose changed rows an entry lets pass.
pub enum Rules {
    /// Every rule a row of the class changes.
    Every,
    /// Only the named rules.
    Only(&'static [&'static str]),
}

impl Rules {
    /// Whether the entry lets pass a difference of `row`.
    pub fn cover(&self, row: &Row) -> bool {
        match self {
            Rules::Every => true,
            Rules::Only(names) => row.mutated.as_deref().is_some_and(|m| names.contains(&m)),
        }
    }
}

type Entry = (&'static str, Rules, Allowance<Decoded, Refusal>);

const KEPT_AS_READ: Owner = Owner::Rule {
    clause: "Everything else is accepted and kept as read: duplicates, emptiness, sizes, trailing bytes, indefinite or non canonical encodings.",
};
const NO_INVARIANT_ON_DECODE: Owner = Owner::Rule {
    clause: "The `Decode` impl for the same type does not (maintainer commit 7aa5103, #506).",
};
const CONTENT_IN_VALIDATE: Owner = Owner::Rule {
    clause: "Ledger rules on content belong in pallas-validate.",
};
const EXTRA_ITEM_DROPPED: Owner = Owner::Upstream {
    commit: Commit::new("a8516e16bb38621eeb86b83bef0e146e6123b648"),
};
const OPTIONAL_SCRIPTS_DERIVED: Owner = Owner::Upstream {
    commit: Commit::new("f67d36e7fa32b1a690f8f74dd50bd7f62d801006"),
};
const OPTIONAL_GOVERNANCE_FIELDS: Owner = Owner::Upstream {
    commit: Commit::new("91f979f5c6e926a785049ceec80f62a1e9346be4"),
};

fn any(answer: &Answer<Decoded, Refusal>) -> bool {
    matches!(answer, Answer::Accepts(_))
}

fn keeps_duplicate(answer: &Answer<Decoded, Refusal>) -> bool {
    matches!(answer, Answer::Accepts(got) if !got.duplicates.is_empty())
}

fn expected_map(answer: &Answer<Decoded, Refusal>) -> bool {
    matches!(answer, Answer::Refuses(r) if r.error.contains("expected map"))
}

const fn accepts(
    name: &'static str,
    owner: Owner,
    shown: fn(&Answer<Decoded, Refusal>) -> bool,
) -> Allowance<Decoded, Refusal> {
    Allowance {
        allowed: Allowed::new(name, Disagreement::Accepts, owner),
        shown,
    }
}

pub const LIST: &[Entry] = &[
    (
        "block array-extra",
        Rules::Every,
        accepts("an extra array item is dropped", EXTRA_ITEM_DROPPED, any),
    ),
    (
        "block array-short",
        Rules::Every,
        accepts(
            "a missing optional item reads as absent",
            OPTIONAL_SCRIPTS_DERIVED,
            any,
        ),
    ),
    (
        "block zap",
        Rules::Every,
        accepts("a size is kept as read", KEPT_AS_READ, any),
    ),
    (
        "body array-extra",
        Rules::Every,
        accepts("an extra array item is dropped", EXTRA_ITEM_DROPPED, any),
    ),
    (
        "body array-short",
        Rules::Every,
        accepts(
            "a missing optional item reads as absent",
            OPTIONAL_GOVERNANCE_FIELDS,
            any,
        ),
    ),
    (
        "body range-above",
        Rules::Only(&["major_protocol_version"]),
        accepts(
            "a protocol version above the ledger's range is kept as read",
            CONTENT_IN_VALIDATE,
            any,
        ),
    ),
    (
        "body range-below",
        Rules::Only(&["positive_coin"]),
        accepts(
            "a zero positive coin is kept as read",
            NO_INVARIANT_ON_DECODE,
            any,
        ),
    ),
    (
        "body set-duplicate",
        Rules::Every,
        accepts("a duplicate is kept as read", KEPT_AS_READ, keeps_duplicate),
    ),
    (
        "body set-two-ways",
        Rules::Every,
        accepts("a duplicate is kept as read", KEPT_AS_READ, keeps_duplicate),
    ),
    (
        "body zap",
        Rules::Every,
        accepts("a size is kept as read", NO_INVARIANT_ON_DECODE, any),
    ),
    (
        "ppu array-extra",
        Rules::Every,
        accepts("an extra array item is dropped", EXTRA_ITEM_DROPPED, any),
    ),
    (
        "ppu range-above",
        Rules::Only(&["positive_word32"]),
        accepts(
            "a parameter outside its 32 bit range is kept as read",
            CONTENT_IN_VALIDATE,
            any,
        ),
    ),
    (
        "ppu range-below",
        Rules::Only(&["positive_word32"]),
        accepts(
            "a parameter outside its 32 bit range is kept as read",
            CONTENT_IN_VALIDATE,
            any,
        ),
    ),
    (
        "tx array-extra",
        Rules::Every,
        accepts("an extra array item is dropped", EXTRA_ITEM_DROPPED, any),
    ),
    (
        "tx array-short",
        Rules::Every,
        accepts(
            "a missing optional item reads as absent",
            OPTIONAL_SCRIPTS_DERIVED,
            any,
        ),
    ),
    (
        "tx set-duplicate",
        Rules::Every,
        accepts("a duplicate is kept as read", KEPT_AS_READ, keeps_duplicate),
    ),
    (
        "tx set-two-ways",
        Rules::Every,
        accepts("a duplicate is kept as read", KEPT_AS_READ, keeps_duplicate),
    ),
    (
        "tx zap",
        Rules::Every,
        accepts("a size is kept as read", KEPT_AS_READ, any),
    ),
    (
        "wits array-extra",
        Rules::Every,
        accepts("an extra array item is dropped", EXTRA_ITEM_DROPPED, any),
    ),
    (
        "wits set-duplicate",
        Rules::Every,
        accepts("a duplicate is kept as read", KEPT_AS_READ, keeps_duplicate),
    ),
    (
        "wits set-two-ways",
        Rules::Every,
        accepts("a duplicate is kept as read", KEPT_AS_READ, keeps_duplicate),
    ),
    (
        "wits zap",
        Rules::Every,
        accepts("a size is kept as read", KEPT_AS_READ, any),
    ),
    (
        "eb",
        Rules::Every,
        Allowance {
            allowed: Allowed::new(
                "endorser block references as the node sends them",
                Disagreement::Refuses,
                Owner::Check {
                    layer: Layer::Networking,
                    test: "pallas-primitives dijkstra::tests::endorser_block_isomorphic_decoding_encoding",
                },
            ),
            shown: expected_map,
        },
    ),
];
