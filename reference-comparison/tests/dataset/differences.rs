//! The differences between pallas's reading of a sample and the ledger's that
//! the run lets pass, each with what holds it and what in the sample shows it.

// Each era's test target lists the differences it shows from these.
#![allow(dead_code)]

use std::collections::BTreeSet;

use ledger_oracle_judge::comparison::{Allowance, Allowed, Answer, Commit, Disagreement, Owner};

use super::{Axis, Change, Seen};

/// What pallas's reading of one sample shows, for telling which listed
/// difference it is.
pub struct Evidence {
    pub axis: Axis,
    pub refused_by_ledger: bool,
    /// Why pallas refuses the sample, when it does.
    pub refusal: Option<String>,
    /// The first byte of the item pallas refuses, when its refusal names a
    /// position.
    pub refused_byte: Option<u8>,
    /// The bytes after the value pallas read.
    pub trailing: usize,
    /// Whether the bytes read end inside an item of the sample.
    pub stopped_inside: bool,
    /// The changes from the bytes pallas read to its value's encoding, both
    /// in normal form.
    pub changes: Result<BTreeSet<Change>, String>,
    /// The changes from the ledger's encoding to pallas's, in normal form.
    pub ledger_changes: Result<BTreeSet<Change>, String>,
    /// Whether pallas writes the bytes it read, both in normal form.
    pub written_as_read: bool,
    /// Whether the encoding pallas writes reads back as its value, or how
    /// the value read back differs.
    pub round_trip: Result<(), String>,
    /// Whether the normal form of the value's encoding is the normal form of
    /// the ledger's encoding of a sample of the same rule it accepts.
    pub same_as_accepted: bool,
}

/// The changes that keep a value as read: the CDDL makes the set tag
/// optional, the ledger reads a map into a structure without order, and it
/// writes an embedded encoding again in its own form.
const AS_READ: &[Change] = &[
    Change::SetTagAdded,
    Change::SetTagDropped,
    Change::EntriesReordered,
    Change::EmbeddedLedgerForm,
];

/// The changes from the ledger's encoding to one kept as read, where the
/// ledger writes a set or a map in its own order and its own set tag, and an
/// output with no Babbage field as a legacy array.
const LEDGER_FORM: &[Change] = &[
    Change::SetTagAdded,
    Change::SetTagDropped,
    Change::EntriesReordered,
    Change::ItemsReordered,
    Change::OutputAsMap,
];

/// Each change a listed difference owns, in the order a sample showing
/// several is told by.
const OWNED: &[Change] = &[
    Change::DuplicateKeysMerged,
    Change::EntriesDropped,
    Change::FractionTag,
    Change::EmbeddedTag,
    Change::ItemsDropped,
    Change::EmbeddedBytesDropped,
    Change::NullsAdded,
];

impl Evidence {
    /// Whether the value is the one read.
    pub fn kept(&self) -> bool {
        only(&self.changes, AS_READ, &[])
    }

    /// Whether the first owned change the value shows is `change`, every
    /// other change it shows being owned or keeping it as read.
    fn first_owned(&self, change: Change) -> bool {
        let Ok(found) = &self.changes else {
            return false;
        };
        only(&self.changes, AS_READ, OWNED)
            && OWNED.iter().find(|c| found.contains(c)) == Some(&change)
    }

    /// Whether the ledger's encoding becomes pallas's by `change` and by the
    /// ledger's own form.
    fn ledger_writes_other_than(&self, change: Change) -> bool {
        only(
            &self.ledger_changes,
            LEDGER_FORM,
            std::slice::from_ref(&change),
        ) && self
            .ledger_changes
            .as_ref()
            .is_ok_and(|c| c.contains(&change))
    }

    /// Whether the ledger's encoding becomes pallas's by the ledger's own
    /// form alone.
    fn ledger_form_only(&self) -> bool {
        only(&self.ledger_changes, LEDGER_FORM, &[])
    }

    /// Whether pallas refuses with `text`.
    fn refuses_with(&self, text: &str) -> bool {
        self.refusal.as_ref().is_some_and(|r| r.contains(text))
    }

    /// Whether pallas refuses an item whose first byte `holds` accepts.
    fn refuses_at(&self, text: &str, holds: fn(u8) -> bool) -> bool {
        self.refuses_with(text) && self.refused_byte.is_some_and(holds)
    }

    /// Whether the value read back loses the cost models of languages its
    /// type has no field for.
    fn unknown_cost_models_lost(&self) -> bool {
        self.round_trip
            .as_ref()
            .is_err_and(|how| how.contains("unknown: {"))
    }
}

/// Whether every change found is one of `kept` or `owned`.
fn only(found: &Result<BTreeSet<Change>, String>, kept: &[Change], owned: &[Change]) -> bool {
    found
        .as_ref()
        .is_ok_and(|found| found.iter().all(|c| kept.contains(c) || owned.contains(c)))
}

/// A difference the run lets pass and the evidence that shows it.
pub struct Known {
    pub allowance: Allowance<Seen, String>,
    pub shows: fn(&Evidence) -> bool,
}

const KEPT_AS_READ: Owner = Owner::Rule {
    clause: "Everything else is accepted and kept as read: duplicates, emptiness, sizes, trailing bytes, indefinite or non canonical encodings.",
};

const CONTENT_IN_VALIDATE: Owner = Owner::Rule {
    clause: "Ledger rules on content belong in pallas-validate.",
};

const TYPE_CANNOT_HOLD: Owner = Owner::Rule {
    clause: "The output type cannot hold the input: an unknown tag, variant or CBOR type, a wrong length for a fixed size type such as `Hash<N>`, a message the protocol state does not admit.",
};

const EXTRA_ITEM_DROPPED: Owner = Owner::Upstream {
    commit: Commit::new("a8516e16bb38621eeb86b83bef0e146e6123b648"),
};

const FRACTION_ANY_TAG: Owner = Owner::Defect {
    defect: "a rational reads any tag",
};

const EMBEDDED_ANY_TAG: Owner = Owner::Defect {
    defect: "an embedded encoding reads any tag",
};

const STRING_DEFINITE: Owner = Owner::Defect {
    defect: "text and embedded bytes are read in definite length only",
};

const NO_FIELD_FOR_KEY: Owner = Owner::Defect {
    defect: "a map decoder drops an entry whose key its type has no field for",
};

const REPEATED_KEY_MERGED: Owner = Owner::Defect {
    defect: "a map decoder keeps one entry of a repeated key",
};

const EMBEDDED_REST_DROPPED: Owner = Owner::Defect {
    defect: "an embedded encoding keeps only its first item",
};

const COST_MODELS_UNWRITTEN: Owner = Owner::Defect {
    defect: "CostModels writes no cost model past key 2",
};

const FLAT_ENUM_DEFINITE: Owner = Owner::Defect {
    defect: "a flat enum decoder refuses an indefinite array",
};

const PAIR_DEFINITE: Owner = Owner::Defect {
    defect: "a pair decoder refuses an indefinite array",
};

const PART_READ: Owner = Owner::Defect {
    defect: "a decoder returns a value after reading part of an item",
};

const MISSING_AS_NULL: Owner = Owner::Defect {
    defect: "a missing last field of a nullable type is read as null",
};

fn accepts(answer: &Answer<Seen, String>) -> bool {
    matches!(answer, Answer::Accepts(_))
}

fn refuses(answer: &Answer<Seen, String>) -> bool {
    matches!(answer, Answer::Refuses(_))
}

fn differs(answer: &Answer<Seen, String>) -> bool {
    matches!(answer, Answer::Accepts(Seen::Differs(_)))
}

/// Whether a byte starts an indefinite array.
fn indefinite_array(byte: u8) -> bool {
    byte == 0x9f
}

/// Whether a byte starts indefinite text.
fn indefinite_text(byte: u8) -> bool {
    byte == 0x7f
}

/// Whether a byte starts indefinite bytes.
fn indefinite_bytes(byte: u8) -> bool {
    byte == 0x5f
}

/// Whether a byte starts a negative integer or a big number.
fn beyond_u64(byte: u8) -> bool {
    byte >> 5 == 1 || byte == 0xc2 || byte == 0xc3
}

/// Whether the ledger refuses a sample pallas reads without trailing bytes.
fn refused_and_read_whole(e: &Evidence) -> bool {
    e.axis == Axis::Verdict && e.refused_by_ledger && e.refusal.is_none() && e.trailing == 0
}

/// Whether pallas refuses a sample the ledger accepts.
fn refused_by_pallas(e: &Evidence) -> bool {
    !e.refused_by_ledger && e.refusal.is_some()
}

const fn known(
    name: &'static str,
    allows: Disagreement,
    owner: Owner,
    shown: fn(&Answer<Seen, String>) -> bool,
    shows: fn(&Evidence) -> bool,
) -> Known {
    Known {
        allowance: Allowance {
            allowed: Allowed::new(name, allows, owner),
            shown,
        },
        shows,
    }
}

pub const OTHER_FORM_KEPT: Known = known(
    "a value the ledger accepts in another form is kept as read",
    Disagreement::Accepts,
    KEPT_AS_READ,
    accepts,
    |e| refused_and_read_whole(e) && e.same_as_accepted,
);

pub const REFUSED_CONTENT_KEPT: Known = known(
    "a value kept as read whose content the ledger refuses",
    Disagreement::Accepts,
    CONTENT_IN_VALIDATE,
    accepts,
    |e| refused_and_read_whole(e) && !e.same_as_accepted && e.kept(),
);

pub const OTHER_ENCODING_KEPT: Known = known(
    "the ledger writes another encoding of a value kept as read",
    Disagreement::Values,
    KEPT_AS_READ,
    differs,
    |e| {
        e.axis == Axis::Bytes
            && !e.refused_by_ledger
            && (e.written_as_read || (e.kept() && e.ledger_form_only()))
    },
);

pub const EXTRA_ITEM: Known = known(
    "an extra array item is dropped",
    Disagreement::Accepts,
    EXTRA_ITEM_DROPPED,
    accepts,
    |e| refused_and_read_whole(e) && e.first_owned(Change::ItemsDropped),
);

pub const PART_READ_ENDS_INSIDE: Known = known(
    "a value is returned after reading part of an item",
    Disagreement::Accepts,
    PART_READ,
    accepts,
    |e| e.axis == Axis::Verdict && e.refused_by_ledger && e.refusal.is_none() && e.stopped_inside,
);

pub const MISSING_LAST_FIELD_NULL: Known = known(
    "a missing last field is read as null",
    Disagreement::Accepts,
    MISSING_AS_NULL,
    accepts,
    |e| refused_and_read_whole(e) && e.first_owned(Change::NullsAdded),
);

pub const REPEATED_KEY_DROPPED: Known = known(
    "an entry of a repeated key is dropped",
    Disagreement::Accepts,
    REPEATED_KEY_MERGED,
    accepts,
    |e| refused_and_read_whole(e) && e.first_owned(Change::DuplicateKeysMerged),
);

pub const UNKNOWN_KEY_DROPPED: Known = known(
    "an entry whose key the type has no field for is dropped",
    Disagreement::Accepts,
    NO_FIELD_FOR_KEY,
    accepts,
    |e| refused_and_read_whole(e) && e.first_owned(Change::EntriesDropped),
);

pub const UNKNOWN_KEY_UNWRITTEN: Known = known(
    "an entry whose key the type has no field for is dropped",
    Disagreement::Values,
    NO_FIELD_FOR_KEY,
    differs,
    |e| {
        e.axis == Axis::Bytes
            && e.round_trip.is_ok()
            && e.ledger_writes_other_than(Change::EntriesDropped)
    },
);

pub const FRACTION_UNDER_ANY_TAG: Known = known(
    "a fraction is read under any tag",
    Disagreement::Accepts,
    FRACTION_ANY_TAG,
    accepts,
    |e| refused_and_read_whole(e) && e.first_owned(Change::FractionTag),
);

pub const EMBEDDED_UNDER_ANY_TAG: Known = known(
    "an embedded encoding is read under any tag",
    Disagreement::Accepts,
    EMBEDDED_ANY_TAG,
    accepts,
    |e| refused_and_read_whole(e) && e.first_owned(Change::EmbeddedTag),
);

pub const INDEFINITE_TEXT: Known = known(
    "indefinite text is refused",
    Disagreement::Refuses,
    STRING_DEFINITE,
    refuses,
    |e| refused_by_pallas(e) && e.refuses_at("expected text (definite length)", indefinite_text),
);

pub const INDEFINITE_EMBEDDED_BYTES: Known = known(
    "indefinite bytes are refused where an embedded encoding is read",
    Disagreement::Refuses,
    STRING_DEFINITE,
    refuses,
    |e| refused_by_pallas(e) && e.refuses_at("expected bytes (definite length)", indefinite_bytes),
);

pub const EMBEDDED_REST: Known = known(
    "bytes after the first item of an embedded encoding are dropped",
    Disagreement::Accepts,
    EMBEDDED_REST_DROPPED,
    accepts,
    |e| refused_and_read_whole(e) && e.first_owned(Change::EmbeddedBytesDropped),
);

pub const UNKNOWN_COST_MODELS: Known = known(
    "cost models of unknown languages are not written",
    Disagreement::Values,
    COST_MODELS_UNWRITTEN,
    differs,
    |e| {
        !e.refused_by_ledger
            && e.unknown_cost_models_lost()
            && match e.axis {
                Axis::RoundTrip => true,
                Axis::Bytes => e.ledger_writes_other_than(Change::EntriesDropped),
                Axis::Verdict => false,
            }
    },
);

pub const FLAT_ENUM_INDEFINITE: Known = known(
    "an indefinite array is refused where a flat enum is read",
    Disagreement::Refuses,
    FLAT_ENUM_DEFINITE,
    refuses,
    |e| refused_by_pallas(e) && e.refuses_with("flat enum requires definite-length array"),
);

pub const PAIR_INDEFINITE: Known = known(
    "an indefinite array is refused where a pair is read",
    Disagreement::Refuses,
    PAIR_DEFINITE,
    refuses,
    |e| refused_by_pallas(e) && e.refuses_at("invalid 2-tuple length", indefinite_array),
);

pub const FRACTION_BEYOND_U64: Known = known(
    "a fraction number beyond u64 is refused",
    Disagreement::Refuses,
    TYPE_CANNOT_HOLD,
    refuses,
    |e| refused_by_pallas(e) && e.refuses_at("expected u64", beyond_u64),
);

pub const EMPTY_NONEMPTY_SET: Known = known(
    "an empty set is refused where the rule is a nonempty set",
    Disagreement::Refuses,
    TYPE_CANNOT_HOLD,
    refuses,
    |e| refused_by_pallas(e) && e.refuses_with("too few elements for a nonempty set"),
);
