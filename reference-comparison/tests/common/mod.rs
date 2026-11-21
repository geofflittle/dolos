//! Parts shared by the test targets of every layer.

#![allow(dead_code)]

pub mod classes;
pub mod refusal;

use std::path::PathBuf;

use ledger_oracle_judge::comparison::{Layer, Records, cddl_pin};

/// The Dijkstra CDDL of the pallas head under comparison.
pub fn defs() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../pallas-head/pallas-primitives/src/dijkstra/defs.cddl");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The records for `layer` written at the ledger commit `cddl` names.
pub fn records(cddl: &str, layer: Layer) -> Records {
    let pin = cddl_pin(cddl).expect("defs.cddl names no ledger commit");
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("records")
        .join(pin);
    Records::read_dir(&dir, layer).unwrap_or_else(|e| panic!("{e}"))
}
