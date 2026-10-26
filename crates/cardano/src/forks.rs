use crate::{utils::float_to_rational, PParamValue, PParamsSet};
use dolos_core::{BrokenInvariant, Genesis};
use pallas::{
    crypto::hash::Hash,
    interop::hardano::configs::{alonzo, byron, conway, shelley},
    ledger::primitives::{
        conway::{DRepVotingThresholds, PoolVotingThresholds},
        CostModel, ExUnits, Nonce, NonceVariant,
    },
};
use tracing::debug;

pub type Val = PParamValue;

fn from_config_nonce(config: &shelley::ExtraEntropy) -> Nonce {
    Nonce {
        variant: match config.tag {
            shelley::NonceVariant::NeutralNonce => NonceVariant::NeutralNonce,
            shelley::NonceVariant::Nonce => NonceVariant::Nonce,
        },
        hash: config.hash.as_ref().map(|x| {
            let bytes = hex::decode(x).unwrap();
            Hash::from(bytes.as_slice())
        }),
    }
}

fn from_config_exunits(config: &alonzo::ExUnits) -> ExUnits {
    ExUnits {
        mem: config.ex_units_mem,
        steps: config.ex_units_steps,
    }
}

fn from_alonzo_cost_models_map(
    config: &alonzo::CostModelPerLanguage,
    language: &alonzo::Language,
) -> Option<CostModel> {
    config
        .iter()
        .filter(|(k, _)| *k == language)
        .map(|(_, v)| CostModel::from(v.clone()))
        .next()
}

fn from_conway_pool_voting_thresholds(
    config: &conway::PoolVotingThresholds,
) -> PoolVotingThresholds {
    PoolVotingThresholds {
        motion_no_confidence: float_to_rational(config.motion_no_confidence),
        committee_normal: float_to_rational(config.committee_normal),
        committee_no_confidence: float_to_rational(config.committee_no_confidence),
        hard_fork_initiation: float_to_rational(config.hard_fork_initiation),
        security_voting_threshold: float_to_rational(config.pp_security_group),
    }
}

fn from_conway_drep_voting_thresholds(
    config: &conway::DRepVotingThresholds,
) -> DRepVotingThresholds {
    pallas::ledger::primitives::conway::DRepVotingThresholds {
        motion_no_confidence: float_to_rational(config.motion_no_confidence),
        committee_normal: float_to_rational(config.committee_normal),
        committee_no_confidence: float_to_rational(config.committee_no_confidence),
        update_constitution: float_to_rational(config.update_to_constitution),
        hard_fork_initiation: float_to_rational(config.hard_fork_initiation),
        pp_network_group: float_to_rational(config.pp_network_group),
        pp_economic_group: float_to_rational(config.pp_economic_group),
        pp_technical_group: float_to_rational(config.pp_technical_group),
        pp_governance_group: float_to_rational(config.pp_gov_group),
        treasury_withdrawal: float_to_rational(config.treasury_withdrawal),
    }
}

/// The Byron epoch and slot lengths, a Byron epoch being ten times k slots.
fn byron_constants(byron: &byron::GenesisFile) -> ProtocolConstants {
    ProtocolConstants {
        epoch_length: 10 * byron.protocol_consts.k as u64,
        slot_length: byron.block_version_data.slot_duration / 1000,
    }
}

pub fn from_byron_genesis(byron: &byron::GenesisFile) -> PParamsSet {
    let version = &byron.block_version_data;
    let constants = byron_constants(byron);

    PParamsSet::default()
        .with(Val::ProtocolVersion((0, 0)))
        .with(Val::SystemStart(byron.start_time))
        .with(Val::SlotLength(constants.slot_length))
        .with(Val::EpochLength(constants.epoch_length))
        .with(Val::MinFeeA(version.tx_fee_policy.multiplier))
        .with(Val::MinFeeB(version.tx_fee_policy.summand))
        .with(Val::MaxBlockBodySize(version.max_block_size))
        .with(Val::MaxTransactionSize(version.max_tx_size))
        .with(Val::MaxBlockHeaderSize(version.max_header_size))
}

pub fn from_shelley_genesis(shelley: &shelley::GenesisFile) -> PParamsSet {
    let system_start = chrono::DateTime::parse_from_rfc3339(shelley.system_start.as_ref().unwrap())
        .expect("invalid system start value");

    let epoch_length = shelley.epoch_length.unwrap_or_default();
    let slot_length = shelley.slot_length.unwrap_or_default();
    let shelley = &shelley.protocol_params;
    let version = &shelley.protocol_version;

    PParamsSet::default()
        .with(Val::SystemStart(system_start.timestamp() as u64))
        .with(Val::ProtocolVersion(version.clone().into()))
        .with(Val::EpochLength(epoch_length as u64))
        .with(Val::SlotLength(slot_length as u64))
        .with(Val::MaxBlockBodySize(shelley.max_block_body_size as u64))
        .with(Val::MaxTransactionSize(shelley.max_tx_size as u64))
        .with(Val::MaxBlockHeaderSize(
            shelley.max_block_header_size as u64,
        ))
        .with(Val::KeyDeposit(shelley.key_deposit))
        .with(Val::MinUtxoValue(shelley.min_utxo_value))
        .with(Val::MinFeeA(shelley.min_fee_a as u64))
        .with(Val::MinFeeB(shelley.min_fee_b as u64))
        .with(Val::PoolDeposit(shelley.pool_deposit))
        .with(Val::DesiredNumberOfStakePools(shelley.n_opt))
        .with(Val::MinPoolCost(shelley.min_pool_cost))
        .with(Val::ExpansionRate(shelley.rho.clone()))
        .with(Val::TreasuryGrowthRate(shelley.tau.clone()))
        .with(Val::MaximumEpoch(shelley.e_max))
        .with(Val::PoolPledgeInfluence(shelley.a0.clone()))
        .with(Val::DecentralizationConstant(
            shelley.decentralisation_param.clone(),
        ))
        .with(Val::ExtraEntropy(from_config_nonce(&shelley.extra_entropy)))
}

pub fn into_alonzo(previous: &PParamsSet, genesis: &alonzo::GenesisFile) -> PParamsSet {
    let set = previous
        .clone()
        .with(Val::ProtocolVersion((5, 0)))
        .with(Val::AdaPerUtxoByte(genesis.lovelace_per_utxo_word))
        .with(Val::ExecutionCosts(genesis.execution_prices.clone().into()))
        .with(Val::MaxTxExUnits(from_config_exunits(
            &genesis.max_tx_ex_units,
        )))
        .with(Val::MaxBlockExUnits(from_config_exunits(
            &genesis.max_block_ex_units,
        )))
        .with(Val::MaxValueSize(genesis.max_value_size))
        .with(Val::CollateralPercentage(genesis.collateral_percentage))
        .with(Val::MaxCollateralInputs(genesis.max_collateral_inputs));

    if let Some(v1) = from_alonzo_cost_models_map(&genesis.cost_models, &alonzo::Language::PlutusV1)
    {
        set.with(Val::CostModelsPlutusV1(v1))
    } else {
        set
    }
}

pub fn into_babbage(previous: &PParamsSet, genesis: &alonzo::GenesisFile) -> PParamsSet {
    // In the hardfork, the value got translated from words to bytes
    // Since the transformation from words to bytes is hardcoded, the transformation
    // here is also hardcoded
    let ada_per_utxo_byte = previous.ada_per_utxo_byte().unwrap_or_default() / 8;

    let set = previous
        .clone()
        .with(Val::ProtocolVersion((7, 0)))
        .with(Val::AdaPerUtxoByte(ada_per_utxo_byte));

    if let Some(v2) = from_alonzo_cost_models_map(&genesis.cost_models, &alonzo::Language::PlutusV2)
    {
        set.with(Val::CostModelsPlutusV2(v2))
    } else {
        set
    }
}

/// The cost models of the alonzo genesis file, its extra configuration included.
fn alonzo_cost_models(
    genesis: &alonzo::GenesisFile,
) -> pallas::ledger::primitives::conway::CostModels {
    genesis.cost_models.clone().into()
}

pub fn into_conway(
    previous: &PParamsSet,
    alonzo: &alonzo::GenesisFile,
    genesis: &conway::GenesisFile,
) -> PParamsSet {
    let plutus_v3 = alonzo_cost_models(alonzo)
        .plutus_v3
        .unwrap_or_else(|| genesis.plutus_v3_cost_model.clone());

    previous
        .clone()
        .with(Val::ProtocolVersion((9, 0)))
        .with(Val::CostModelsPlutusV3(plutus_v3))
        .with(Val::PoolVotingThresholds(
            from_conway_pool_voting_thresholds(&genesis.pool_voting_thresholds),
        ))
        .with(Val::DrepVotingThresholds(
            from_conway_drep_voting_thresholds(&genesis.d_rep_voting_thresholds),
        ))
        .with(Val::MinCommitteeSize(genesis.committee_min_size))
        .with(Val::CommitteeTermLimit(
            genesis.committee_max_term_length.into(),
        ))
        .with(Val::GovernanceActionValidityPeriod(
            genesis.gov_action_lifetime.into(),
        ))
        .with(Val::GovernanceActionDeposit(genesis.gov_action_deposit))
        .with(Val::DrepDeposit(genesis.d_rep_deposit))
        .with(Val::DrepInactivityPeriod(genesis.d_rep_activity.into()))
        .with(Val::MinFeeRefScriptCostPerByte(
            pallas::ledger::primitives::conway::RationalNumber {
                numerator: genesis.min_fee_ref_script_cost_per_byte,
                denominator: 1,
            },
        ))
}

/// Moves a Conway parameter set to Dijkstra, adding the PlutusV4 cost model and
/// the parameters the Dijkstra genesis file declares.
pub fn into_dijkstra(previous: &PParamsSet, genesis: &Genesis) -> PParamsSet {
    let Some(dijkstra) = genesis.dijkstra.as_ref() else {
        panic!("reaching protocol version 12 needs a dijkstra genesis file and this configuration has no path to one")
    };

    let plutus_v4 = alonzo_cost_models(&genesis.alonzo)
        .unknown
        .remove(&crate::pallas_extras::PLUTUS_V4_COST_MODEL_KEY)
        .unwrap_or_else(|| dijkstra.plutus_v4_cost_model.clone());

    let mut unknown = previous.cost_models_unknown_or_default();
    unknown.insert(crate::pallas_extras::PLUTUS_V4_COST_MODEL_KEY, plutus_v4);

    previous
        .clone()
        .with(Val::ProtocolVersion((12, 0)))
        .with(Val::CostModelsUnknown(unknown))
        .with(Val::MaxRefScriptSizePerBlock(
            dijkstra.max_ref_script_size_per_block,
        ))
        .with(Val::MaxRefScriptSizePerTx(
            dijkstra.max_ref_script_size_per_tx,
        ))
        .with(Val::RefScriptCostStride(dijkstra.ref_script_cost_stride))
        .with(Val::RefScriptCostMultiplier(f64_to_rational(
            dijkstra.ref_script_cost_multiplier,
        )))
        .with(Val::MaxPledgeLeverage(
            dijkstra.max_pledge_leverage.map(f64_to_rational),
        ))
        .with(Val::MinPoolMargin(f64_to_rational(
            dijkstra.min_pool_margin,
        )))
        .with(Val::LeiosAnnouncementPeriodLength(
            dijkstra.leios_announcement_period_length,
        ))
        .with(Val::LeiosVotePeriodLength(
            dijkstra.leios_vote_period_length,
        ))
        .with(Val::LeiosDiffusionPeriodLength(
            dijkstra.leios_diffusion_period_length,
        ))
        .with(Val::LeiosCommitteeSize(
            dijkstra.leios_committee_size.into(),
        ))
        .with(Val::LeiosQuorumStakeThreshold(f64_to_rational(
            dijkstra.leios_quorum_stake_threshold,
        )))
        .with(Val::MaxEndorserBlockReferencesSize(
            dijkstra.max_endorser_block_references_size,
        ))
        .with(Val::MaxEndorserBlockTxsSize(
            dijkstra.max_endorser_block_txs_size,
        ))
        .with(Val::MaxEndorserBlockExUnits(ExUnits {
            mem: dijkstra.max_endorser_block_execution_units.memory,
            steps: dijkstra.max_endorser_block_execution_units.steps,
        }))
        .with(Val::MaxRefScriptSizePerEndorserBlock(
            dijkstra.max_ref_script_size_per_endorser_block,
        ))
        .with(Val::PerasMinCandidateBlockAge(
            dijkstra.peras_min_candidate_block_age,
        ))
        .with(Val::PerasHealingFactor(f64_to_rational(
            dijkstra.peras_healing_factor,
        )))
        .with(Val::PerasCertBoost(dijkstra.peras_cert_boost.into()))
        .with(Val::PerasTargetCommitteeSize(
            dijkstra.peras_target_committee_size.into(),
        ))
        .with(Val::PerasBootstrapRound(
            dijkstra.peras_bootstrap_round.map(u64::from),
        ))
        .with(Val::PerasQuorumThresholdSafetyMargin(f64_to_rational(
            dijkstra.peras_quorum_threshold_safety_margin,
        )))
        .with(Val::RefInputsCostPerMultiAssetPolicy(
            dijkstra.ref_inputs_cost_per_multi_asset_policy,
        ))
        .with(Val::RefInputsCostPerDatumByte(
            dijkstra.ref_inputs_cost_per_datum_byte,
        ))
}

/// The rational nearest a decimal the Dijkstra genesis file writes.
fn f64_to_rational(x: f64) -> pallas::ledger::primitives::RationalNumber {
    let ratio: num_rational::Ratio<i64> = num_rational::Ratio::approximate_float(x).unwrap();

    pallas::ledger::primitives::RationalNumber {
        numerator: *ratio.numer() as u64,
        denominator: *ratio.denom() as u64,
    }
}

/// Increments the protocol version by 1 without changing any other fields
pub fn intra_era_hardfork(current: &PParamsSet, target: u16) -> PParamsSet {
    current
        .clone()
        .with(PParamValue::ProtocolVersion((target as u64, 0)))
}

// Source: https://github.com/cardano-foundation/CIPs/blob/master/CIP-0059/feature-table.md
pub fn migrate_pparams_version(
    from: u16,
    to: u16,
    current: &PParamsSet,
    genesis: &Genesis,
) -> PParamsSet {
    debug!(from, to, "migrating pparams version");

    match (from, to) {
        // Protocol starts at version 0;
        // There was one intra-era "hard fork" in byron (even though they weren't called that yet)
        (0, 1) => intra_era_hardfork(current, to),
        // Protocol version 2 transitions from Byron to Shelley
        (1, 2) => from_shelley_genesis(&genesis.shelley),
        // Two intra-era hard forks, named Allegra (3) and Mary (4); we don't have separate types
        // for these eras
        (2, 3) => intra_era_hardfork(current, to),
        (3, 4) => intra_era_hardfork(current, to),
        // Protocol version 5 transitions from Shelley (Mary, technically) to Alonzo
        (4, 5) => into_alonzo(current, &genesis.alonzo),
        // One intra-era hard-fork in alonzo at protocol version 6
        (5, 6) => intra_era_hardfork(current, to),
        // Protocol version 7 transitions from Alonzo to Babbage
        (6, 7) => into_babbage(current, &genesis.alonzo),
        // One intra-era hard-fork in babbage at protocol version 8
        (7, 8) => intra_era_hardfork(current, to),
        // Protocol version 9 transitions from Babbage to Conway
        (8, 9) => into_conway(current, &genesis.alonzo, &genesis.conway),
        // One intra-era hard-fork in conway at protocol version 10
        (9, 10) => intra_era_hardfork(current, to),
        // Van Rossem: intra-era hard-fork to protocol version 11
        (10, 11) => intra_era_hardfork(current, to),
        // Protocol version 12 transitions from Conway to Dijkstra
        (11, 12) => into_dijkstra(current, genesis),
        (from, to) => {
            unimplemented!("don't know how to bump from version {from} to {to} (#1033)",)
        }
    }
}

pub fn force_pparams_version(
    initial: &PParamsSet,
    genesis: &Genesis,
    from: u16,
    to: u16,
) -> Result<PParamsSet, BrokenInvariant> {
    let mut pparams = initial.clone();

    for from in from..to {
        pparams = migrate_pparams_version(from, from + 1, &pparams, genesis);
    }

    Ok(pparams)
}

pub struct ProtocolConstants {
    pub epoch_length: u64,
    pub slot_length: u64,
}

pub fn protocol_constants(version: u16, genesis: &Genesis) -> ProtocolConstants {
    match version {
        x if x < 2 => byron_constants(&genesis.byron),
        _ => ProtocolConstants {
            epoch_length: genesis.shelley.epoch_length.unwrap_or_default() as u64,
            slot_length: genesis.shelley.slot_length.unwrap_or_default() as u64,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::load_genesis;

    #[test]
    fn force_pparams_to_van_rossem() {
        let path = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("test_data")
            .join("mainnet")
            .join("genesis");

        let genesis = load_genesis(&path);
        let initial = from_byron_genesis(&genesis.byron);
        let result = force_pparams_version(&initial, &genesis, 0, 11).unwrap();

        assert_eq!(result.protocol_major(), Some(11));
    }

    fn proto_devnet_byron() -> byron::GenesisFile {
        let path = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("test_data")
            .join("proto-devnet")
            .join("genesis")
            .join("byron.json");

        byron::from_file(&path).unwrap()
    }

    /// A 20 second Byron slot keeps the 21600 slot mainnet epoch.
    #[test]
    fn a_mainnet_byron_epoch_is_ten_k_slots_of_twenty_seconds() {
        let pparams = from_byron_genesis(&mainnet_genesis().byron);

        assert_eq!(pparams.epoch_length_or_default(), 21600);
        assert_eq!(pparams.slot_length_or_default(), 20);
    }

    /// A 250 millisecond Byron slot with k 40 gives a 400 slot epoch.
    #[test]
    fn a_sub_second_byron_slot_gives_a_ten_k_slot_epoch() {
        let pparams = from_byron_genesis(&proto_devnet_byron());

        assert_eq!(pparams.epoch_length_or_default(), 400);
    }

    /// The proto-devnet genesis set loads and its parameters step to version 12.
    #[test]
    fn the_proto_devnet_genesis_set_loads_and_steps_to_dijkstra() {
        let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let genesis = load_genesis(&dir.join("test_data").join("proto-devnet").join("genesis"))
            .with_dijkstra(
                dir.join("..")
                    .join("core")
                    .join("test_data")
                    .join("proto-devnet")
                    .join("dijkstra-genesis.json"),
            )
            .unwrap();

        let initial = from_byron_genesis(&genesis.byron);
        let result = force_pparams_version(&initial, &genesis, 0, 12).unwrap();

        assert_eq!(result.protocol_major(), Some(12));
        assert_eq!(result.epoch_length_or_default(), 3600);
        assert_eq!(result.slot_length_or_default(), 1);
    }

    fn mainnet_genesis() -> dolos_core::Genesis {
        let path = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("test_data")
            .join("mainnet")
            .join("genesis");

        load_genesis(&path)
    }

    /// The mainnet genesis with the proto-devnet Dijkstra file attached.
    ///
    /// The Dijkstra file is the one a proto-devnet network is governed by, copied
    /// byte for byte, so a cost model read out of it is the one the node runs
    /// and not one chosen for a test.
    fn genesis_with_dijkstra() -> dolos_core::Genesis {
        let path = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("..")
            .join("core")
            .join("test_data")
            .join("proto-devnet")
            .join("dijkstra-genesis.json");

        mainnet_genesis().with_dijkstra(path).unwrap()
    }

    /// The step to version 12 changes the version and adds the PlutusV4 cost
    /// model and the Dijkstra parameters, keeping every Conway parameter.
    #[test]
    fn force_pparams_to_dijkstra() {
        let genesis = genesis_with_dijkstra();
        let initial = from_byron_genesis(&genesis.byron);

        let at_eleven = force_pparams_version(&initial, &genesis, 0, 11).unwrap();
        let at_twelve = force_pparams_version(&initial, &genesis, 0, 12).unwrap();

        assert_eq!(at_twelve.protocol_major(), Some(12));

        let mut unknown = at_eleven.cost_models_unknown_or_default();
        unknown.insert(
            3,
            genesis
                .dijkstra
                .as_ref()
                .unwrap()
                .plutus_v4_cost_model
                .clone(),
        );

        let expected = genesis_dijkstra_parameters().into_iter().fold(
            at_eleven
                .with(PParamValue::ProtocolVersion((12, 0)))
                .with(PParamValue::CostModelsUnknown(unknown)),
            |set, value| set.with(value),
        );

        assert_eq!(at_twelve, expected);
    }

    /// The cost model the Dijkstra genesis declares for PlutusV4 is in the
    /// parameter set, under the key the cost model map names for that language,
    /// with the chain's own length, first entry, last entry and negative entry.
    #[test]
    fn the_dijkstra_step_carries_the_genesis_plutus_v4_cost_model() {
        let genesis = genesis_with_dijkstra();
        let initial = from_byron_genesis(&genesis.byron);

        let at_twelve = force_pparams_version(&initial, &genesis, 0, 12).unwrap();
        let carried = at_twelve.cost_models_unknown_or_default();

        // Key 3 is what the Dijkstra cost model map names for PlutusV4.
        let model = carried
            .get(&3)
            .expect("no cost model under the PlutusV4 key");

        assert_eq!(model.len(), 251);
        assert_eq!(model[0], 100788);
        assert_eq!(model[52], -900);
        assert_eq!(model[250], 1);
        assert_eq!(
            model,
            &genesis.dijkstra.as_ref().unwrap().plutus_v4_cost_model
        );
    }

    /// A set that stops in Conway has no cost model under the PlutusV4 key.
    #[test]
    fn the_step_before_dijkstra_carries_no_plutus_v4_cost_model() {
        let genesis = genesis_with_dijkstra();
        let initial = from_byron_genesis(&genesis.byron);

        let at_eleven = force_pparams_version(&initial, &genesis, 0, 11).unwrap();

        assert_eq!(at_eleven.cost_models_unknown_or_default().get(&3), None);
    }

    /// The Dijkstra step adds the one key and changes no other cost model.
    #[test]
    fn the_dijkstra_step_adds_one_cost_model_key_and_changes_no_other() {
        let genesis = genesis_with_dijkstra();
        let initial = from_byron_genesis(&genesis.byron);

        let at_eleven = force_pparams_version(&initial, &genesis, 0, 11).unwrap();
        let at_twelve = force_pparams_version(&initial, &genesis, 0, 12).unwrap();

        let before = at_eleven.cost_models_unknown_or_default();
        let after = at_twelve.cost_models_unknown_or_default();

        let added: Vec<u64> = after
            .keys()
            .filter(|key| !before.contains_key(key))
            .copied()
            .collect();
        assert_eq!(added, vec![3]);

        for (key, model) in before.iter() {
            assert_eq!(after.get(key), Some(model), "cost model {key} changed");
        }

        assert_eq!(
            at_twelve.cost_models_plutus_v1(),
            at_eleven.cost_models_plutus_v1()
        );
        assert_eq!(
            at_twelve.cost_models_plutus_v2(),
            at_eleven.cost_models_plutus_v2()
        );
        assert_eq!(
            at_twelve.cost_models_plutus_v3(),
            at_eleven.cost_models_plutus_v3()
        );
    }

    /// A configuration with no Dijkstra genesis file stops at the step to
    /// version 12.
    ///
    /// The PlutusV4 cost model is declared by that file and by nothing on the
    /// chain, so a follower without it would validate a PlutusV4 script against
    /// no cost model at all.
    #[test]
    #[should_panic(expected = "dijkstra genesis file")]
    fn a_dijkstra_step_without_the_genesis_file_stops() {
        let genesis = mainnet_genesis();
        let initial = from_byron_genesis(&genesis.byron);

        let _ = force_pparams_version(&initial, &genesis, 0, 12);
    }

    /// The values the test Dijkstra genesis file declares for the parameters
    /// the Dijkstra era added.
    fn genesis_dijkstra_parameters() -> Vec<Val> {
        let ratio = |numerator, denominator| pallas::ledger::primitives::RationalNumber {
            numerator,
            denominator,
        };

        vec![
            Val::MaxRefScriptSizePerBlock(1048576),
            Val::MaxRefScriptSizePerTx(204800),
            Val::RefScriptCostStride(25600),
            Val::RefScriptCostMultiplier(ratio(6, 5)),
            Val::MaxPledgeLeverage(None),
            Val::MinPoolMargin(ratio(3, 200)),
            Val::LeiosAnnouncementPeriodLength(1000),
            Val::LeiosVotePeriodLength(4000),
            Val::LeiosDiffusionPeriodLength(7000),
            Val::LeiosCommitteeSize(900),
            Val::LeiosQuorumStakeThreshold(ratio(3, 4)),
            Val::MaxEndorserBlockReferencesSize(100000),
            Val::MaxEndorserBlockTxsSize(1000000),
            Val::MaxEndorserBlockExUnits(ExUnits {
                mem: 310000000,
                steps: 100000000000,
            }),
            Val::MaxRefScriptSizePerEndorserBlock(4000000),
            Val::PerasMinCandidateBlockAge(0),
            Val::PerasHealingFactor(ratio(1, 1)),
            Val::PerasCertBoost(0),
            Val::PerasTargetCommitteeSize(0),
            Val::PerasBootstrapRound(None),
            Val::PerasQuorumThresholdSafetyMargin(ratio(0, 1)),
            Val::RefInputsCostPerMultiAssetPolicy(0),
            Val::RefInputsCostPerDatumByte(0),
        ]
    }

    /// The step to version 12 seeds each Dijkstra parameter with the value the
    /// genesis file declares.
    #[test]
    fn the_dijkstra_step_seeds_every_dijkstra_parameter_from_the_genesis_file() {
        let genesis = genesis_with_dijkstra();
        let initial = from_byron_genesis(&genesis.byron);

        let at_twelve = force_pparams_version(&initial, &genesis, 0, 12).unwrap();

        let seeded: Vec<Option<Val>> = genesis_dijkstra_parameters()
            .iter()
            .map(|expected| at_twelve.get(expected.kind()).cloned())
            .collect();
        let expected: Vec<Option<Val>> = genesis_dijkstra_parameters()
            .into_iter()
            .map(Some)
            .collect();

        assert_eq!(seeded, expected);
    }

    /// The genesis file declares zero for most Peras and reference input
    /// parameters, so distinct values show that each one is seeded into its
    /// own kind.
    #[test]
    fn each_peras_and_reference_input_genesis_entry_is_seeded_in_its_own_kind() {
        let mut genesis = genesis_with_dijkstra();
        let file = genesis.dijkstra.as_mut().unwrap();
        file.peras_min_candidate_block_age = 31;
        file.peras_healing_factor = 3.5;
        file.peras_cert_boost = 13;
        file.peras_target_committee_size = 401;
        file.peras_bootstrap_round = Some(17);
        file.peras_quorum_threshold_safety_margin = 0.125;
        file.ref_inputs_cost_per_multi_asset_policy = 1003;
        file.ref_inputs_cost_per_datum_byte = 5;
        let initial = from_byron_genesis(&genesis.byron);

        let at_twelve = force_pparams_version(&initial, &genesis, 0, 12).unwrap();

        let ratio = |numerator, denominator| pallas::ledger::primitives::RationalNumber {
            numerator,
            denominator,
        };
        let expected = vec![
            Val::PerasMinCandidateBlockAge(31),
            Val::PerasHealingFactor(ratio(7, 2)),
            Val::PerasCertBoost(13),
            Val::PerasTargetCommitteeSize(401),
            Val::PerasBootstrapRound(Some(17)),
            Val::PerasQuorumThresholdSafetyMargin(ratio(1, 8)),
            Val::RefInputsCostPerMultiAssetPolicy(1003),
            Val::RefInputsCostPerDatumByte(5),
        ];
        let seeded: Vec<Val> = expected
            .iter()
            .filter_map(|value| at_twelve.get(value.kind()).cloned())
            .collect();

        assert_eq!(seeded, expected);
    }

    /// The step to version 11 holds no Dijkstra parameter.
    #[test]
    fn the_step_before_dijkstra_holds_no_dijkstra_parameter() {
        let genesis = genesis_with_dijkstra();
        let initial = from_byron_genesis(&genesis.byron);

        let at_eleven = force_pparams_version(&initial, &genesis, 0, 11).unwrap();

        let held: Vec<Val> = genesis_dijkstra_parameters()
            .iter()
            .filter_map(|expected| at_eleven.get(expected.kind()).cloned())
            .collect();

        assert_eq!(held, vec![]);
    }

    /// The must-not case for the one above. Adding a rule for 12 must not turn
    /// the ladder into something that accepts any bump at all, because a
    /// version Dolos has no rule for has to stop it rather than produce a
    /// parameter set that is quietly wrong.
    #[test]
    #[should_panic(expected = "don't know how to bump")]
    fn a_version_with_no_rule_is_still_refused() {
        let genesis = genesis_with_dijkstra();
        let initial = from_byron_genesis(&genesis.byron);

        let _ = force_pparams_version(&initial, &genesis, 0, 13);
    }

    /// The leios devnet genesis set and the parameters its node answers with.
    fn leios_devnet() -> (dolos_core::Genesis, serde_json::Value) {
        let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("test_data")
            .join("leios-devnet");

        let genesis = load_genesis(&dir.join("genesis"))
            .with_dijkstra(dir.join("genesis").join("dijkstra.json"))
            .unwrap();

        let answered = std::fs::read_to_string(dir.join("protocol-parameters.json")).unwrap();

        (genesis, serde_json::from_str(&answered).unwrap())
    }

    /// A genesis set whose alonzo file injects no PlutusV3 model takes the
    /// conway genesis model.
    #[test]
    fn a_genesis_set_with_no_injected_plutus_v3_takes_the_conway_genesis_model() {
        let genesis = mainnet_genesis();
        let initial = from_byron_genesis(&genesis.byron);

        let at_eleven = force_pparams_version(&initial, &genesis, 0, 11).unwrap();

        assert_eq!(
            at_eleven.cost_models_plutus_v3(),
            Some(genesis.conway.plutus_v3_cost_model.clone())
        );
    }

    /// The devnet genesis set steps to the cost models its node answers with,
    /// which for PlutusV3 is the model the alonzo file injects.
    #[test]
    fn the_devnet_genesis_set_steps_to_the_cost_models_its_node_answers_with() {
        let (genesis, answered) = leios_devnet();
        let initial = from_byron_genesis(&genesis.byron);

        let at_twelve = force_pparams_version(&initial, &genesis, 0, 12).unwrap();

        let answered = |language: &str| -> Vec<i64> {
            answered["costModels"][language]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_i64().unwrap())
                .collect()
        };

        assert_eq!(
            at_twelve.cost_models_plutus_v1(),
            Some(answered("PlutusV1"))
        );
        assert_eq!(
            at_twelve.cost_models_plutus_v2(),
            Some(answered("PlutusV2"))
        );
        assert_eq!(
            at_twelve.cost_models_plutus_v3().map(|model| model.len()),
            Some(answered("PlutusV3").len())
        );
        assert_eq!(
            at_twelve.cost_models_plutus_v3(),
            Some(answered("PlutusV3"))
        );
        assert_eq!(
            at_twelve.cost_models_unknown_or_default().get(&3),
            Some(&answered("PlutusV4"))
        );
    }

    /// A PlutusV4 model the alonzo file injects replaces the one the dijkstra
    /// genesis file declares.
    #[test]
    fn an_injected_plutus_v4_model_replaces_the_dijkstra_genesis_model() {
        let (mut genesis, _) = leios_devnet();
        let injected: Vec<i64> = (1..=251).collect();

        let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let file = dir
            .join("test_data")
            .join("leios-devnet")
            .join("genesis")
            .join("alonzo.json");
        let mut alonzo: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
        alonzo["extraConfig"]["costModels"]["PlutusV4"] = serde_json::json!(injected);
        genesis.alonzo = serde_json::from_value(alonzo).unwrap();

        let initial = from_byron_genesis(&genesis.byron);
        let at_twelve = force_pparams_version(&initial, &genesis, 0, 12).unwrap();

        assert_eq!(
            at_twelve.cost_models_unknown_or_default().get(&3),
            Some(&injected)
        );
    }
}
