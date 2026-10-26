use std::collections::HashMap;

use crate::{
    mapping::{rational_to_f64, IntoModel, Rounded},
    routes::epochs::cost_models::get_named_cost_model,
};
use blockfrost_openapi::models::{
    epoch_content::EpochContent, epoch_param_content::EpochParamContent,
};
use dolos_cardano::{model::EpochState, pallas_extras::PLUTUS_V4_COST_MODEL_KEY, PParamsSet};
use dolos_core::{cbor, Genesis};
use pallas::ledger::primitives::{conway::CostModels, Epoch};

/// Every cost model the parameters carry, each with the Plutus version whose
/// operation names it is read with.
///
/// The Conway cost model type names three languages and reads every further key
/// into a wildcard map, so the PlutusV4 model is taken from that map by key.
fn cost_models_to_key_value(cost_models: &CostModels) -> Vec<(&'static str, u64, &[i64])> {
    let maybe = vec![
        ("PlutusV1", 1, cost_models.plutus_v1.as_ref()),
        ("PlutusV2", 2, cost_models.plutus_v2.as_ref()),
        ("PlutusV3", 3, cost_models.plutus_v3.as_ref()),
        (
            "PlutusV4",
            4,
            cost_models.unknown.get(&PLUTUS_V4_COST_MODEL_KEY),
        ),
    ];

    maybe
        .into_iter()
        .filter_map(|(k, version, v)| v.map(|v| (k, version, v.as_slice())))
        .collect()
}

/// Cost models as the raw operation-cost vectors the chain carries.
///
/// `/governance/proposals/…/parameters` returns exactly this, while the epoch
/// endpoints run the vectors through [`map_cost_models_named`] first.
pub(crate) fn map_cost_models_raw(
    cost_models: &CostModels,
) -> Option<Option<HashMap<String, serde_json::Value>>> {
    let as_vec = cost_models_to_key_value(cost_models);
    if as_vec.is_empty() {
        None
    } else {
        Some(Some(
            as_vec
                .into_iter()
                .map(|(k, _, v)| (k.to_string(), serde_json::to_value(v).unwrap()))
                .collect(),
        ))
    }
}

fn map_cost_models_named(cost_models: &CostModels) -> Option<HashMap<String, serde_json::Value>> {
    let as_vec = cost_models_to_key_value(cost_models);
    if as_vec.is_empty() {
        None
    } else {
        Some(
            as_vec
                .into_iter()
                .map(|(k, version, v)| (k.to_string(), get_named_cost_model(version, v)))
                .collect(),
        )
    }
}

/// A protocol parameter model built off a [`PParamsSet`], with the fields
/// every Blockfrost parameter model renders the same way filled in.
///
/// `/epochs/{n}/parameters` and `/governance/proposals/…/parameters` share
/// thirty-odd nullable fields that read straight off the set and differ in
/// the rest: the epoch model reports the values in force, so it types most
/// of its fields as plain values and falls back to genesis, while the
/// proposal model reports a change, so every field of it is nullable. The
/// caller passes the set, the [`RatioFormat`](crate::mapping::RatioFormat)
/// to write ratios with and the model literal holding the fields the model
/// owns, and the shared ones are added to it. The literal stays exhaustive,
/// so a field the model grows, or one named on both sides, is a compile
/// error rather than a silent default.
///
/// The expansion uses `?`, so it belongs in a function returning
/// `Result<_, StatusCode>`: the models type counts and sizes as `i32` while a
/// proposal can set them anywhere in the chain's range, and a value past
/// `i32::MAX` is a 500 rather than a parameter wrapped negative.
macro_rules! protocol_params_model {
    ($params:ident, $ratio:ident, $model:ident { $($field:ident: $value:expr),* $(,)? }) => {{
        use $crate::mapping::RatioFormat as _;

        $model {
            $($field: $value,)*
            max_val_size: $params.max_value_size().map(|x| x.to_string()),
            collateral_percent: $params
                .collateral_percentage()
                .map($crate::mapping::i32_or_500)
                .transpose()?,
            max_collateral_inputs: $params
                .max_collateral_inputs()
                .map($crate::mapping::i32_or_500)
                .transpose()?,
            // One db-sync column, under both of the names Blockfrost gives it.
            coins_per_utxo_size: $params.ada_per_utxo_byte().map(|x| x.to_string()),
            coins_per_utxo_word: $params.ada_per_utxo_byte().map(|x| x.to_string()),
            price_mem: $params
                .execution_costs()
                .map(|x| $ratio::to_f64::<4>(&x.mem_price)),
            price_step: $params
                .execution_costs()
                .map(|x| $ratio::to_f64::<9>(&x.step_price)),
            max_tx_ex_mem: $params.max_tx_ex_units().map(|x| x.mem.to_string()),
            max_tx_ex_steps: $params.max_tx_ex_units().map(|x| x.steps.to_string()),
            max_block_ex_mem: $params.max_block_ex_units().map(|x| x.mem.to_string()),
            max_block_ex_steps: $params.max_block_ex_units().map(|x| x.steps.to_string()),
            min_fee_ref_script_cost_per_byte: $params
                .min_fee_ref_script_cost_per_byte()
                .map(|x| $ratio::to_f64::<3>(&x)),
            drep_deposit: $params.drep_deposit().map(|x| x.to_string()),
            drep_activity: $params.drep_inactivity_period().map(|x| x.to_string()),
            pvt_motion_no_confidence: $params
                .pool_voting_thresholds()
                .map(|x| $ratio::to_f64::<3>(&x.motion_no_confidence)),
            pvt_committee_normal: $params
                .pool_voting_thresholds()
                .map(|x| $ratio::to_f64::<3>(&x.committee_normal)),
            pvt_committee_no_confidence: $params
                .pool_voting_thresholds()
                .map(|x| $ratio::to_f64::<3>(&x.committee_no_confidence)),
            pvt_hard_fork_initiation: $params
                .pool_voting_thresholds()
                .map(|x| $ratio::to_f64::<3>(&x.hard_fork_initiation)),
            // The other column Blockfrost renders under two names.
            pvtpp_security_group: $params
                .pool_voting_thresholds()
                .map(|x| $ratio::to_f64::<3>(&x.security_voting_threshold)),
            pvt_p_p_security_group: $params
                .pool_voting_thresholds()
                .map(|x| $ratio::to_f64::<3>(&x.security_voting_threshold)),
            dvt_motion_no_confidence: $params
                .drep_voting_thresholds()
                .map(|x| $ratio::to_f64::<3>(&x.motion_no_confidence)),
            dvt_committee_normal: $params
                .drep_voting_thresholds()
                .map(|x| $ratio::to_f64::<3>(&x.committee_normal)),
            dvt_committee_no_confidence: $params
                .drep_voting_thresholds()
                .map(|x| $ratio::to_f64::<3>(&x.committee_no_confidence)),
            dvt_update_to_constitution: $params
                .drep_voting_thresholds()
                .map(|x| $ratio::to_f64::<3>(&x.update_constitution)),
            dvt_hard_fork_initiation: $params
                .drep_voting_thresholds()
                .map(|x| $ratio::to_f64::<3>(&x.hard_fork_initiation)),
            dvt_p_p_network_group: $params
                .drep_voting_thresholds()
                .map(|x| $ratio::to_f64::<3>(&x.pp_network_group)),
            dvt_p_p_economic_group: $params
                .drep_voting_thresholds()
                .map(|x| $ratio::to_f64::<3>(&x.pp_economic_group)),
            dvt_p_p_technical_group: $params
                .drep_voting_thresholds()
                .map(|x| $ratio::to_f64::<3>(&x.pp_technical_group)),
            dvt_p_p_gov_group: $params
                .drep_voting_thresholds()
                .map(|x| $ratio::to_f64::<3>(&x.pp_governance_group)),
            dvt_treasury_withdrawal: $params
                .drep_voting_thresholds()
                .map(|x| $ratio::to_f64::<3>(&x.treasury_withdrawal)),
            committee_min_size: $params.min_committee_size().map(|x| x.to_string()),
            committee_max_term_length: $params.committee_term_limit().map(|x| x.to_string()),
            gov_action_lifetime: $params
                .governance_action_validity_period()
                .map(|x| x.to_string()),
            gov_action_deposit: $params.governance_action_deposit().map(|x| x.to_string()),
            // Babbage retired it, so neither model has a value to show.
            extra_entropy: None,
        }
    }};
}

pub(crate) use protocol_params_model;

pub struct ParametersModelBuilder<'a> {
    pub epoch: Epoch,
    pub params: PParamsSet,
    pub genesis: &'a Genesis,
    pub nonce: Option<String>,
}

impl<'a> IntoModel<EpochParamContent> for ParametersModelBuilder<'a> {
    type SortKey = ();

    fn into_model(self) -> Result<EpochParamContent, axum::http::StatusCode> {
        let Self {
            genesis,
            epoch,
            params,
            nonce,
        } = self;

        let out = protocol_params_model!(
            params,
            Rounded,
            EpochParamContent {
                epoch: epoch as i32,
                a0: rational_to_f64::<3>(&genesis.shelley.protocol_params.a0),
                e_max: genesis.shelley.protocol_params.e_max as i32,
                max_tx_size: params.max_transaction_size_or_default() as i32,
                max_block_size: params.max_block_body_size_or_default() as i32,
                max_block_header_size: params.max_block_header_size_or_default() as i32,
                min_fee_a: params.min_fee_a_or_default() as i32,
                min_fee_b: params.min_fee_b_or_default() as i32,
                min_utxo: params
                    .ada_per_utxo_byte()
                    .unwrap_or(genesis.shelley.protocol_params.min_utxo_value)
                    .to_string(),
                key_deposit: params.key_deposit_or_default().to_string(),
                pool_deposit: params.pool_deposit_or_default().to_string(),
                n_opt: params.desired_number_of_stake_pools_or_default() as i32,
                rho: params
                    .rho()
                    .map(|x| rational_to_f64::<3>(&x))
                    .unwrap_or_default(),
                tau: params
                    .tau()
                    .map(|x| rational_to_f64::<3>(&x))
                    .unwrap_or_default(),
                min_pool_cost: params.min_pool_cost_or_default().to_string(),
                protocol_major_ver: params.protocol_major().unwrap_or_default() as i32,
                protocol_minor_ver: params.protocol_version_or_default().1 as i32,
                cost_models_raw: map_cost_models_raw(&params.cost_models_for_script_languages()),
                cost_models: map_cost_models_named(&params.cost_models_for_script_languages()),
                nonce: nonce.unwrap_or_default(),
                decentralisation_param: rational_to_f64::<3>(
                    &params.decentralization_constant_or_default(),
                ),
            }
        );

        Ok(out)
    }
}

pub struct EpochContentModelBuilder {
    pub state: EpochState,
    pub start_time: u64,
    pub end_time: u64,
    pub first_block_time: u64,
    pub last_block_time: u64,
    pub tx_count: u64,
    pub output: cbor::U128,
    pub active_stake: Option<u64>,
}

impl IntoModel<EpochContent> for EpochContentModelBuilder {
    type SortKey = Epoch;

    fn sort_key(&self) -> Option<Self::SortKey> {
        Some(self.state.number)
    }

    fn into_model(self) -> Result<EpochContent, axum::http::StatusCode> {
        let Self {
            state,
            start_time,
            end_time,
            first_block_time,
            last_block_time,
            tx_count,
            output,
            active_stake,
        } = self;

        let rolling = state.rolling.live().cloned().unwrap_or_default();

        let out = EpochContent {
            epoch: state.number as i32,
            start_time: start_time as i32,
            end_time: end_time as i32,
            first_block_time: first_block_time as i32,
            last_block_time: last_block_time as i32,
            block_count: rolling.blocks_minted as i32,
            tx_count: tx_count as i32,
            output: output.to_string(),
            fees: rolling.gathered_fees.to_string(),
            active_stake: active_stake.map(|x| x.to_string()),
        };

        Ok(out)
    }
}

#[cfg(test)]
mod cost_model_tests {
    use super::*;

    fn cost_models_with(wildcard: &[u64]) -> CostModels {
        let mut unknown = std::collections::BTreeMap::new();
        for key in wildcard {
            unknown.insert(*key, vec![*key as i64; 251]);
        }

        CostModels {
            plutus_v1: Some(vec![1; 332]),
            plutus_v2: Some(vec![2; 332]),
            plutus_v3: Some(vec![3; 350]),
            unknown,
        }
    }

    /// A cost model under a wildcard key that names no language is reported
    /// under no language name.
    #[test]
    fn a_cost_model_under_another_wildcard_key_is_reported_under_no_language() {
        let raw = map_cost_models_raw(&cost_models_with(&[4, 5]))
            .flatten()
            .expect("no cost models at all");

        assert_eq!(raw.len(), 3);
        assert!(!raw.contains_key("PlutusV4"));

        let named = map_cost_models_named(&cost_models_with(&[4, 5])).expect("no cost models");

        assert_eq!(named.len(), 3);
        assert!(!named.contains_key("PlutusV4"));
    }

    /// The model under the PlutusV4 key is reported as PlutusV4, on the raw
    /// path and on the named one, with every cost it holds.
    #[test]
    fn the_model_under_the_plutus_v4_key_is_reported_as_plutus_v4() {
        let models = cost_models_with(&[PLUTUS_V4_COST_MODEL_KEY]);

        let raw = map_cost_models_raw(&models)
            .flatten()
            .expect("no cost models at all");

        assert_eq!(raw.len(), 4);
        assert_eq!(
            raw.get("PlutusV4").and_then(|x| x.as_array()).map(Vec::len),
            Some(251),
        );

        let named = map_cost_models_named(&models).expect("no cost models");

        assert_eq!(named.len(), 4);
        assert_eq!(
            named
                .get("PlutusV4")
                .and_then(|x| x.as_array())
                .map(Vec::len),
            Some(251),
        );
    }

    /// Reporting PlutusV4 leaves the three named models as they were, with
    /// PlutusV3 still named operation by operation.
    #[test]
    fn the_three_named_models_keep_their_operation_names() {
        let models = cost_models_with(&[PLUTUS_V4_COST_MODEL_KEY]);
        let named = map_cost_models_named(&models).expect("no cost models");

        assert_eq!(
            named
                .get("PlutusV3")
                .and_then(|x| x.as_object())
                .map(serde_json::Map::len),
            Some(350),
        );
        assert_eq!(
            named
                .get("PlutusV1")
                .and_then(|x| x.as_object())
                .map(serde_json::Map::len),
            Some(332),
        );
    }

    /// Parameters that hold no cost model at all report none, rather than an
    /// empty PlutusV4.
    #[test]
    fn parameters_with_no_cost_model_report_none() {
        let models = CostModels {
            plutus_v1: None,
            plutus_v2: None,
            plutus_v3: None,
            unknown: Default::default(),
        };

        assert!(map_cost_models_raw(&models).is_none());
        assert!(map_cost_models_named(&models).is_none());
    }
}
