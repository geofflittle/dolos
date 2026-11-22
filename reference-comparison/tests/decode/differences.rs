//! The classes where pallas's reading differs from the ledger's, each with
//! what holds the difference.

use ledger_oracle_judge::comparison::{
    Allowance, Allowed, Answer, Commit, Disagreement, Layer, Owner,
};

use super::{Decoded, Refusal};

type Entry = (&'static str, Allowance<Decoded, Refusal>);

const KEPT_AS_READ: Owner = Owner::Rule {
    clause: "Everything else is accepted and kept as read: duplicates, emptiness, sizes, trailing bytes, indefinite or non canonical encodings.",
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
        accepts("an extra array item is dropped", EXTRA_ITEM_DROPPED, any),
    ),
    (
        "block array-short",
        accepts(
            "a missing optional item reads as absent",
            OPTIONAL_SCRIPTS_DERIVED,
            any,
        ),
    ),
    (
        "block zap",
        accepts("a size is kept as read", KEPT_AS_READ, any),
    ),
    (
        "body array-extra",
        accepts("an extra array item is dropped", EXTRA_ITEM_DROPPED, any),
    ),
    (
        "body array-short",
        accepts(
            "a missing optional item reads as absent",
            OPTIONAL_GOVERNANCE_FIELDS,
            any,
        ),
    ),
    (
        "ppu array-extra",
        accepts("an extra array item is dropped", EXTRA_ITEM_DROPPED, any),
    ),
    (
        "tx array-extra",
        accepts("an extra array item is dropped", EXTRA_ITEM_DROPPED, any),
    ),
    (
        "tx array-short",
        accepts(
            "a missing optional item reads as absent",
            OPTIONAL_SCRIPTS_DERIVED,
            any,
        ),
    ),
    (
        "tx zap",
        accepts("a size is kept as read", KEPT_AS_READ, any),
    ),
    (
        "wits array-extra",
        accepts("an extra array item is dropped", EXTRA_ITEM_DROPPED, any),
    ),
    (
        "wits zap",
        accepts("a size is kept as read", KEPT_AS_READ, any),
    ),
    (
        "eb",
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
