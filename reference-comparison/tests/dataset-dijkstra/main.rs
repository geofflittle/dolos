//! The Dijkstra values of the cardano-cbor-dataset read by pallas.

#[macro_use]
#[path = "../dataset/mod.rs"]
mod dataset;
mod rules;
mod unraw;

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
    ],
};

#[test]
fn pallas_reads_the_dijkstra_dataset_as_the_ledger_does() {
    dataset::run_on_large_stack(&DIJKSTRA);
}
