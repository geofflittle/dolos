//! The Dijkstra genesis file.
//!
//! A Dijkstra network is governed by parameters no earlier genesis declares:
//! the Leios periods and committee, the endorser block limits, the Peras
//! parameters, the reference script sizing and cost, the reference input costs
//! and the PlutusV4 cost model. Without a path to this file a follower has no
//! value for any of them.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Execution units as this file declares them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExUnits {
    pub memory: u64,
    pub steps: u64,
}

/// Every parameter a Dijkstra genesis declares.
///
/// Unknown fields are refused, because a parameter this file names and Dolos
/// does not model is a rule the node applies and the follower does not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GenesisFile {
    pub leios_announcement_period_length: u64,
    pub leios_committee_size: u16,
    pub leios_diffusion_period_length: u64,
    pub leios_quorum_stake_threshold: f64,
    pub leios_vote_period_length: u64,
    pub max_endorser_block_execution_units: ExUnits,
    pub max_endorser_block_references_size: u64,
    pub max_endorser_block_txs_size: u64,
    /// Null in the node's file, so the type holds the absence.
    pub max_pledge_leverage: Option<f64>,
    pub max_ref_script_size_per_block: u64,
    pub max_ref_script_size_per_endorser_block: u64,
    pub max_ref_script_size_per_tx: u64,
    pub min_pool_margin: f64,
    /// Null in the node's file, so the type holds the absence.
    pub peras_bootstrap_round: Option<u32>,
    pub peras_cert_boost: u16,
    pub peras_healing_factor: f64,
    pub peras_min_candidate_block_age: u64,
    pub peras_quorum_threshold_safety_margin: f64,
    pub peras_target_committee_size: u16,
    /// Entries are signed. The proto-devnet file holds negative ones.
    pub plutus_v4_cost_model: Vec<i64>,
    pub ref_inputs_cost_per_datum_byte: u64,
    pub ref_inputs_cost_per_multi_asset_policy: u64,
    pub ref_script_cost_multiplier: f64,
    pub ref_script_cost_stride: u64,
}

/// Reads a Dijkstra genesis file.
///
/// A file that does not parse is an error and never a default, because every
/// field here is a rule the network is already applying.
pub fn from_file(path: impl AsRef<Path>) -> Result<GenesisFile, std::io::Error> {
    let bytes = std::fs::read(path.as_ref())?;

    serde_json::from_slice(&bytes).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{}: {error}", path.as_ref().display()),
        )
    })
}
