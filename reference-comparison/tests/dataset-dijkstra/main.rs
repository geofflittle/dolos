//! The Dijkstra values of the cardano-cbor-dataset read by pallas.

#[macro_use]
#[path = "../dataset/mod.rs"]
mod dataset;
mod rules;

use dataset::Era;
use dataset::differences::*;

const DIJKSTRA: Era = Era {
    dir: "dijkstra",
    generator_table: "dijkstraRules",
    rules: rules::RULES,
    differences: &[
        OTHER_FORM_KEPT,
        REFUSED_CONTENT_KEPT,
        OTHER_ENCODING_KEPT,
        EXTRA_ITEM,
        INDEFINITE_TEXT,
        INDEFINITE_EMBEDDED_BYTES,
        PAIR_INDEFINITE,
        FRACTION_UNDER_ANY_TAG,
        EMBEDDED_UNDER_ANY_TAG,
        UNKNOWN_KEY_DROPPED,
        REPEATED_KEY_DROPPED,
        MISSING_LAST_FIELD_NULL,
        EMBEDDED_REST,
    ],
};

#[test]
fn pallas_reads_the_dijkstra_dataset_as_the_ledger_does() {
    dataset::run_on_large_stack(&DIJKSTRA);
}
