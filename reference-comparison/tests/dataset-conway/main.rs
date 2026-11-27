//! The Conway values of the cardano-cbor-dataset read by pallas.

#[macro_use]
#[path = "../dataset/mod.rs"]
mod dataset;
mod rules;

use dataset::Era;
use dataset::differences::*;

const CONWAY: Era = Era {
    dir: "conway",
    generator_table: "conwayRules",
    rules: rules::RULES,
    differences: &[
        OTHER_FORM_KEPT,
        REFUSED_CONTENT_KEPT,
        OTHER_ENCODING_KEPT,
        PART_READ_ENDS_INSIDE,
        MISSING_LAST_FIELD_NULL,
        REPEATED_KEY_DROPPED,
        UNKNOWN_KEY_DROPPED,
        UNKNOWN_KEY_UNWRITTEN,
        FRACTION_UNDER_ANY_TAG,
        EMBEDDED_UNDER_ANY_TAG,
        EXTRA_ITEM,
        EMBEDDED_REST,
        FLAT_ENUM_INDEFINITE,
        PAIR_INDEFINITE,
        FRACTION_BEYOND_U64,
    ],
};

#[test]
fn pallas_reads_the_conway_dataset_as_the_ledger_does() {
    dataset::run_on_large_stack(&CONWAY);
}
