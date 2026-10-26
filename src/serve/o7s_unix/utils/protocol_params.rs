use crate::prelude::*;
use dolos_cardano::load_effective_pparams;
use pallas::codec::utils::{AnyUInt, KeyValuePairs};
use pallas::network::miniprotocols::localstate::queries_v16 as q16;

pub fn build_protocol_params<D: Domain>(domain: &D) -> Result<q16::ProtocolParam, Error> {
    let pparams = load_effective_pparams::<D>(domain.state())
        .map_err(|e| Error::server(format!("failed to load protocol params: {}", e)))?;

    Ok(q16::ProtocolParam {
        minfee_a: pparams.min_fee_a(),
        minfee_b: pparams.min_fee_b(),
        max_block_body_size: pparams.max_block_body_size(),
        max_transaction_size: pparams.max_transaction_size(),
        max_block_header_size: pparams.max_block_header_size(),
        key_deposit: pparams.key_deposit().map(AnyUInt::U64),
        pool_deposit: pparams.pool_deposit().map(AnyUInt::U64),
        maximum_epoch: pparams.maximum_epoch(),
        desired_number_of_stake_pools: pparams.desired_number_of_stake_pools().map(|n| n as u64),
        pool_pledge_influence: pparams.pool_pledge_influence().map(|r| to_q16_rational(&r)),
        expansion_rate: pparams.expansion_rate().map(|r| to_q16_rational(&r)),
        treasury_growth_rate: pparams.treasury_growth_rate().map(|r| to_q16_rational(&r)),
        protocol_version: pparams.protocol_version().map(|v| (v.0, v.1)),
        min_pool_cost: pparams.min_pool_cost().map(AnyUInt::U64),
        ada_per_utxo_byte: pparams.ada_per_utxo_byte().map(AnyUInt::U64),
        cost_models_for_script_languages: Some(to_q16_cost_models(
            &pparams.cost_models_for_script_languages(),
        )),
        execution_costs: pparams.execution_costs().map(|e| to_q16_ex_unit_prices(&e)),
        max_tx_ex_units: pparams.max_tx_ex_units().map(|e| to_q16_ex_units(&e)),
        max_block_ex_units: pparams.max_block_ex_units().map(|e| to_q16_ex_units(&e)),
        max_value_size: pparams.max_value_size().map(|n| n as u64),
        collateral_percentage: pparams.collateral_percentage().map(|n| n as u64),
        max_collateral_inputs: pparams.max_collateral_inputs().map(|n| n as u64),
        pool_voting_thresholds: pparams
            .pool_voting_thresholds()
            .map(|p| to_q16_pool_voting_thresholds(&p)),
        drep_voting_thresholds: pparams
            .drep_voting_thresholds()
            .map(|d| to_q16_drep_voting_thresholds(&d)),
        min_committee_size: pparams.min_committee_size(),
        committee_term_limit: pparams.committee_term_limit(),
        governance_action_validity_period: pparams.governance_action_validity_period(),
        governance_action_deposit: pparams.governance_action_deposit().map(AnyUInt::U64),
        drep_deposit: pparams.drep_deposit().map(AnyUInt::U64),
        drep_inactivity_period: pparams.drep_inactivity_period(),
        minfee_refscript_cost_per_byte: pparams
            .min_fee_ref_script_cost_per_byte()
            .map(|r| to_q16_rational(&r)),
    })
}

fn to_q16_rational(r: &pallas::ledger::primitives::RationalNumber) -> q16::RationalNumber {
    q16::RationalNumber {
        numerator: r.numerator,
        denominator: r.denominator,
    }
}

fn to_q16_ex_units(e: &pallas::ledger::primitives::ExUnits) -> q16::ExUnits {
    q16::ExUnits {
        mem: e.mem,
        steps: e.steps,
    }
}

fn to_q16_ex_unit_prices(e: &pallas::ledger::primitives::ExUnitPrices) -> q16::ExUnitPrices {
    q16::ExUnitPrices {
        mem_price: to_q16_rational(&e.mem_price),
        step_price: to_q16_rational(&e.step_price),
    }
}

fn to_q16_cost_models(c: &pallas::ledger::primitives::conway::CostModels) -> q16::CostModels {
    q16::CostModels {
        plutus_v1: c.plutus_v1.clone(),
        plutus_v2: c.plutus_v2.clone(),
        plutus_v3: c.plutus_v3.clone(),
        unknown: KeyValuePairs::from(c.unknown.clone().into_iter().collect::<Vec<_>>()),
    }
}

fn to_q16_pool_voting_thresholds(
    p: &pallas::ledger::primitives::conway::PoolVotingThresholds,
) -> q16::PoolVotingThresholds {
    q16::PoolVotingThresholds {
        motion_no_confidence: to_q16_rational(&p.motion_no_confidence),
        committee_normal: to_q16_rational(&p.committee_normal),
        committee_no_confidence: to_q16_rational(&p.committee_no_confidence),
        hard_fork_initiation: to_q16_rational(&p.hard_fork_initiation),
        pp_security_group: to_q16_rational(&p.security_voting_threshold),
    }
}

fn to_q16_drep_voting_thresholds(
    d: &pallas::ledger::primitives::conway::DRepVotingThresholds,
) -> q16::DRepVotingThresholds {
    q16::DRepVotingThresholds {
        motion_no_confidence: to_q16_rational(&d.motion_no_confidence),
        committee_normal: to_q16_rational(&d.committee_normal),
        committee_no_confidence: to_q16_rational(&d.committee_no_confidence),
        update_to_constitution: to_q16_rational(&d.update_constitution),
        hard_fork_initiation: to_q16_rational(&d.hard_fork_initiation),
        pp_network_group: to_q16_rational(&d.pp_network_group),
        pp_economic_group: to_q16_rational(&d.pp_economic_group),
        pp_technical_group: to_q16_rational(&d.pp_technical_group),
        pp_gov_group: to_q16_rational(&d.pp_governance_group),
        treasury_withdrawal: to_q16_rational(&d.treasury_withdrawal),
    }
}

#[cfg(test)]
mod cost_model_tests {
    use super::*;
    use dolos_cardano::pallas_extras::PLUTUS_V4_COST_MODEL_KEY;
    use pallas::ledger::primitives::conway::CostModels;

    fn cost_models_with(keys: &[u64]) -> CostModels {
        let mut unknown = std::collections::BTreeMap::new();
        for key in keys {
            unknown.insert(*key, vec![*key as i64]);
        }

        CostModels {
            plutus_v1: Some(vec![1]),
            plutus_v2: Some(vec![2]),
            plutus_v3: Some(vec![3]),
            unknown,
        }
    }

    #[test]
    fn a_plutus_v4_cost_model_under_the_wildcard_key_is_read_as_plutus_v4() {
        let reply = to_q16_cost_models(&cost_models_with(&[PLUTUS_V4_COST_MODEL_KEY]));

        assert_eq!(reply.plutus_v4(), Some(&vec![3]));
        let wildcard_keys: Vec<u64> = reply.unknown.iter().map(|(k, _)| *k).collect();
        assert_eq!(wildcard_keys, vec![PLUTUS_V4_COST_MODEL_KEY]);
    }

    #[test]
    fn a_cost_model_under_any_other_key_stays_in_the_wildcard() {
        let reply = to_q16_cost_models(&cost_models_with(&[4, 5]));

        assert_eq!(reply.plutus_v4(), None);
        assert_eq!(reply.unknown.len(), 2);
    }

    /// A client that reads the three named fields and the wildcard map
    /// together counts every cost model the chain holds, once each.
    #[test]
    fn every_cost_model_is_reported_exactly_once_across_the_named_fields_and_the_wildcard() {
        let models = cost_models_with(&[PLUTUS_V4_COST_MODEL_KEY, 4, 5]);
        let reply = to_q16_cost_models(&models);

        let present = [
            models.plutus_v1.is_some(),
            models.plutus_v2.is_some(),
            models.plutus_v3.is_some(),
        ]
        .iter()
        .filter(|x| **x)
        .count()
            + models.unknown.len();

        let reported = [
            reply.plutus_v1.is_some(),
            reply.plutus_v2.is_some(),
            reply.plutus_v3.is_some(),
        ]
        .iter()
        .filter(|x| **x)
        .count()
            + reply.unknown.len();

        assert_eq!(present, 6);
        assert_eq!(reported, present);

        let wildcard_keys: Vec<u64> = reply.unknown.iter().map(|(k, _)| *k).collect();
        assert_eq!(wildcard_keys, vec![PLUTUS_V4_COST_MODEL_KEY, 4, 5]);
    }

    #[test]
    fn the_three_named_cost_models_are_carried_across_unchanged() {
        let reply = to_q16_cost_models(&cost_models_with(&[]));

        assert_eq!(reply.plutus_v1, Some(vec![1]));
        assert_eq!(reply.plutus_v2, Some(vec![2]));
        assert_eq!(reply.plutus_v3, Some(vec![3]));
        assert_eq!(reply.plutus_v4(), None);
    }
}
