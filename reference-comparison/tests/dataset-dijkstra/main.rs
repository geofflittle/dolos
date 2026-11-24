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
        OTHER_ENCODING_KEPT,
        SOURCE_FORM_KEPT,
        INDEFINITE_TEXT,
        INDEFINITE_EMBEDDED_BYTES,
        PAIR_INDEFINITE,
        EMBEDDED_UNDER_ANY_TAG,
        REPEATED_KEY_DROPPED,
        EMBEDDED_REST,
        EMPTY_NONEMPTY_SET,
    ],
};

#[test]
fn pallas_reads_the_dijkstra_dataset_as_the_ledger_does() {
    dataset::run_on_large_stack(&DIJKSTRA);
}
