use std::collections::BTreeMap;

use dolos_cardano::model::PParamValue as P;
use ledger_oracle_judge::{
    compare::Tally,
    judgement::Version,
    params::{first_difference, Canon},
    record::PParams,
};
use pallas::ledger::primitives::{
    conway::{DRepVotingThresholds, PoolVotingThresholds},
    ExUnitPrices, ExUnits, RationalNumber,
};

use super::super::spec::cardano as u5c;

/// The pool voting threshold names in the order Dolos serves them.
const POOL_THRESHOLDS: [&str; 5] = [
    "motionNoConfidence",
    "committeeNormal",
    "committeeNoConfidence",
    "hardForkInitiation",
    "ppSecurityGroup",
];

/// The DRep voting threshold names in the order Dolos serves them.
const DREP_THRESHOLDS: [&str; 10] = [
    "motionNoConfidence",
    "committeeNormal",
    "committeeNoConfidence",
    "updateToConstitution",
    "hardForkInitiation",
    "ppNetworkGroup",
    "ppEconomicGroup",
    "ppTechnicalGroup",
    "ppGovGroup",
    "treasuryWithdrawal",
];

/// The cost model language names in the order of their ledger language index.
const LANGUAGES: [&str; 4] = ["PlutusV1", "PlutusV2", "PlutusV3", "PlutusV4"];

/// The Dolos updates a ledger parameter value seeds.
type SeedFn = fn(&Canon) -> Vec<P>;

/// The ledger parameter value ReadParams serves.
type ServedFn = fn(&u5c::PParams) -> Canon;

/// How one ledger parameter is seeded into Dolos and read back from ReadParams.
struct Param {
    key: &'static str,
    seed: SeedFn,
    served: ServedFn,
}

impl Param {
    const fn new(key: &'static str, seed: SeedFn, served: ServedFn) -> Param {
        Param { key, seed, served }
    }
}

fn int(c: &Canon) -> u64 {
    match c {
        Canon::Int(n) => u64::try_from(*n).expect("a ledger count fits a u64"),
        other => panic!("PARAMS expected an integer, found {other}"),
    }
}

fn small(c: &Canon) -> u32 {
    u32::try_from(int(c)).expect("a ledger count fits a u32")
}

fn ratio(c: &Canon) -> RationalNumber {
    let (numerator, denominator) = match c {
        Canon::Int(n) => (*n, 1),
        Canon::Ratio(n, d) => (*n, *d),
        other => panic!("PARAMS expected a ratio, found {other}"),
    };
    RationalNumber {
        numerator: u64::try_from(numerator).expect("a ledger ratio is not negative"),
        denominator: u64::try_from(denominator).expect("a ledger denominator fits a u64"),
    }
}

fn field<'a>(c: &'a Canon, name: &str) -> &'a Canon {
    match c {
        Canon::Map(fields) => fields
            .get(name)
            .unwrap_or_else(|| panic!("PARAMS field {name} missing from {c}")),
        other => panic!("PARAMS expected a map with {name}, found {other}"),
    }
}

fn units(c: &Canon) -> ExUnits {
    ExUnits {
        mem: int(field(c, "memory")),
        steps: int(field(c, "steps")),
    }
}

fn costs(c: &Canon) -> Vec<i64> {
    match c {
        Canon::List(items) => items
            .iter()
            .map(|i| match i {
                Canon::Int(n) => i64::try_from(*n).expect("a cost model entry fits an i64"),
                other => panic!("PARAMS expected a cost model entry, found {other}"),
            })
            .collect(),
        other => panic!("PARAMS expected a cost model, found {other}"),
    }
}

fn cost_models(c: &Canon) -> Vec<P> {
    let Canon::Map(models) = c else {
        panic!("PARAMS expected cost models, found {c}")
    };
    let mut out = Vec::new();
    let mut unknown = BTreeMap::new();
    for (name, model) in models {
        match name.as_str() {
            "PlutusV1" => out.push(P::CostModelsPlutusV1(costs(model))),
            "PlutusV2" => out.push(P::CostModelsPlutusV2(costs(model))),
            "PlutusV3" => out.push(P::CostModelsPlutusV3(costs(model))),
            other => {
                let index = LANGUAGES
                    .iter()
                    .position(|l| *l == other)
                    .unwrap_or_else(|| panic!("PARAMS unmapped key=costModels.{other}"));
                unknown.insert(index as u64, costs(model));
            }
        }
    }
    out.push(P::CostModelsUnknown(unknown));
    out
}

fn drep_thresholds(c: &Canon) -> Vec<P> {
    let t = |name| ratio(field(c, name));
    vec![P::DrepVotingThresholds(DRepVotingThresholds {
        motion_no_confidence: t("motionNoConfidence"),
        committee_normal: t("committeeNormal"),
        committee_no_confidence: t("committeeNoConfidence"),
        update_constitution: t("updateToConstitution"),
        hard_fork_initiation: t("hardForkInitiation"),
        pp_network_group: t("ppNetworkGroup"),
        pp_economic_group: t("ppEconomicGroup"),
        pp_technical_group: t("ppTechnicalGroup"),
        pp_governance_group: t("ppGovGroup"),
        treasury_withdrawal: t("treasuryWithdrawal"),
    })]
}

fn pool_thresholds(c: &Canon) -> Vec<P> {
    let t = |name| ratio(field(c, name));
    vec![P::PoolVotingThresholds(PoolVotingThresholds {
        motion_no_confidence: t("motionNoConfidence"),
        committee_normal: t("committeeNormal"),
        committee_no_confidence: t("committeeNoConfidence"),
        hard_fork_initiation: t("hardForkInitiation"),
        security_voting_threshold: t("ppSecurityGroup"),
    })]
}

fn n(value: u64) -> Canon {
    Canon::Int(value.into())
}

fn big(b: &Option<u5c::BigInt>) -> Canon {
    let magnitude = |bytes: &[u8]| {
        bytes
            .iter()
            .fold(0i128, |acc, b| acc * 256 + i128::from(*b))
    };
    match b.as_ref().and_then(|b| b.big_int.as_ref()) {
        Some(u5c::big_int::BigInt::Int(v)) => Canon::Int((*v).into()),
        Some(u5c::big_int::BigInt::BigUInt(bytes)) => Canon::Int(magnitude(bytes)),
        Some(u5c::big_int::BigInt::BigNInt(bytes)) => Canon::Int(-1 - magnitude(bytes)),
        None => Canon::Null,
    }
}

fn rat(r: &Option<u5c::RationalNumber>) -> Canon {
    r.as_ref()
        .and_then(|r| Canon::ratio(r.numerator.into(), r.denominator.into()))
        .unwrap_or(Canon::Null)
}

fn opt(value: Option<u64>) -> Canon {
    value.map_or(Canon::Null, n)
}

fn ex(u: &Option<u5c::ExUnits>) -> Canon {
    match u {
        Some(u) => Canon::Map(
            [
                ("memory".to_string(), n(u.memory)),
                ("steps".to_string(), n(u.steps)),
            ]
            .into(),
        ),
        None => Canon::Null,
    }
}

fn thresholds(t: &Option<u5c::VotingThresholds>, names: &[&str]) -> Canon {
    match t {
        Some(t) if t.thresholds.len() == names.len() => Canon::Map(
            names
                .iter()
                .zip(&t.thresholds)
                .map(|(name, r)| (name.to_string(), rat(&Some(r.clone()))))
                .collect(),
        ),
        Some(t) => Canon::List(t.thresholds.iter().map(|r| rat(&Some(r.clone()))).collect()),
        None => Canon::Null,
    }
}

fn served_models(m: &Option<u5c::CostModels>) -> Canon {
    let Some(m) = m else { return Canon::Null };
    let list = |c: &Option<u5c::CostModel>| {
        c.as_ref()
            .map(|c| Canon::List(c.values.iter().map(|v| Canon::Int((*v).into())).collect()))
    };
    Canon::Map(
        LANGUAGES
            .iter()
            .zip([&m.plutus_v1, &m.plutus_v2, &m.plutus_v3, &m.plutus_v4])
            .filter_map(|(name, c)| Some((name.to_string(), list(c)?)))
            .collect(),
    )
}

fn served_prices(p: &u5c::PParams) -> Canon {
    match &p.prices {
        Some(x) => Canon::Map(
            [
                ("priceMemory".to_string(), rat(&x.memory)),
                ("priceSteps".to_string(), rat(&x.steps)),
            ]
            .into(),
        ),
        None => Canon::Null,
    }
}

fn served_version(p: &u5c::PParams) -> Canon {
    match &p.protocol_version {
        Some(v) => Canon::Map(
            [
                ("major".to_string(), n(v.major.into())),
                ("minor".to_string(), n(v.minor.into())),
            ]
            .into(),
        ),
        None => Canon::Null,
    }
}

/// Builds the parameter table from each key's seed and served forms.
macro_rules! params {
    ($($key:literal => ($seed:expr, $served:expr $(,)?)),* $(,)?) => {
        [$(Param::new($key, $seed, $served)),*]
    };
}

/// Each ledger parameter with its seed and served forms.
static PARAMS: [Param; 54] = params! {
        "collateralPercentage" => (
            |c| vec![P::CollateralPercentage(small(c))],
            |p| n(p.collateral_percentage),
        ),
        "committeeMaxTermLength" => (
            |c| vec![P::CommitteeTermLimit(int(c))],
            |p| n(p.committee_term_limit),
        ),
        "committeeMinSize" => (
            |c| vec![P::MinCommitteeSize(int(c))],
            |p| n(p.min_committee_size.into()),
        ),
        "costModels" => (cost_models, |p| served_models(&p.cost_models)),
        "dRepActivity" => (
            |c| vec![P::DrepInactivityPeriod(int(c))],
            |p| n(p.drep_inactivity_period),
        ),
        "dRepDeposit" => (|c| vec![P::DrepDeposit(int(c))], |p| big(&p.drep_deposit)),
        "dRepVotingThresholds" => (drep_thresholds, |p| {
            thresholds(&p.drep_voting_thresholds, &DREP_THRESHOLDS)
        }),
        "executionUnitPrices" => (
            |c| {
                vec![P::ExecutionCosts(ExUnitPrices {
                    mem_price: ratio(field(c, "priceMemory")),
                    step_price: ratio(field(c, "priceSteps")),
                })]
            },
            served_prices,
        ),
        "govActionDeposit" => (
            |c| vec![P::GovernanceActionDeposit(int(c))],
            |p| big(&p.governance_action_deposit),
        ),
        "govActionLifetime" => (
            |c| vec![P::GovernanceActionValidityPeriod(int(c))],
            |p| n(p.governance_action_validity_period),
        ),
        "leiosAnnouncementPeriodLength" => (
            |c| vec![P::LeiosAnnouncementPeriodLength(int(c))],
            |p| opt(p.leios_announcement_period_length),
        ),
        "leiosCommitteeSize" => (
            |c| vec![P::LeiosCommitteeSize(int(c))],
            |p| opt(p.leios_committee_size.map(u64::from)),
        ),
        "leiosDiffusionPeriodLength" => (
            |c| vec![P::LeiosDiffusionPeriodLength(int(c))],
            |p| opt(p.leios_diffusion_period_length),
        ),
        "leiosQuorumStakeThreshold" => (
            |c| vec![P::LeiosQuorumStakeThreshold(ratio(c))],
            |p| rat(&p.leios_quorum_stake_threshold),
        ),
        "leiosVotePeriodLength" => (
            |c| vec![P::LeiosVotePeriodLength(int(c))],
            |p| opt(p.leios_vote_period_length),
        ),
        "maxBlockBodySize" => (
            |c| vec![P::MaxBlockBodySize(int(c))],
            |p| n(p.max_block_body_size),
        ),
        "maxBlockExecutionUnits" => (
            |c| vec![P::MaxBlockExUnits(units(c))],
            |p| ex(&p.max_execution_units_per_block),
        ),
        "maxBlockHeaderSize" => (
            |c| vec![P::MaxBlockHeaderSize(int(c))],
            |p| n(p.max_block_header_size),
        ),
        "maxCollateralInputs" => (
            |c| vec![P::MaxCollateralInputs(small(c))],
            |p| n(p.max_collateral_inputs),
        ),
        "maxEndorserBlockExecutionUnits" => (
            |c| vec![P::MaxEndorserBlockExUnits(units(c))],
            |p| ex(&p.max_endorser_block_execution_units),
        ),
        "maxEndorserBlockReferencesSize" => (
            |c| vec![P::MaxEndorserBlockReferencesSize(int(c))],
            |p| opt(p.max_endorser_block_references_size),
        ),
        "maxEndorserBlockTxsSize" => (
            |c| vec![P::MaxEndorserBlockTxsSize(int(c))],
            |p| opt(p.max_endorser_block_txs_size),
        ),
        "maxPledgeLeverage" => (
            |c| {
                vec![P::MaxPledgeLeverage(match c {
                    Canon::Null => None,
                    other => Some(ratio(other)),
                })]
            },
            |p| rat(&p.max_pledge_leverage),
        ),
        "maxRefScriptSizePerBlock" => (
            |c| vec![P::MaxRefScriptSizePerBlock(int(c))],
            |p| opt(p.max_ref_script_size_per_block),
        ),
        "maxRefScriptSizePerEndorserBlock" => (
            |c| vec![P::MaxRefScriptSizePerEndorserBlock(int(c))],
            |p| opt(p.max_ref_script_size_per_endorser_block),
        ),
        "maxRefScriptSizePerTx" => (
            |c| vec![P::MaxRefScriptSizePerTx(int(c))],
            |p| opt(p.max_ref_script_size_per_tx),
        ),
        "maxTxExecutionUnits" => (
            |c| vec![P::MaxTxExUnits(units(c))],
            |p| ex(&p.max_execution_units_per_transaction),
        ),
        "maxTxSize" => (
            |c| vec![P::MaxTransactionSize(int(c))],
            |p| n(p.max_tx_size),
        ),
        "maxValueSize" => (|c| vec![P::MaxValueSize(small(c))], |p| n(p.max_value_size)),
        "minFeeRefScriptCostPerByte" => (
            |c| vec![P::MinFeeRefScriptCostPerByte(ratio(c))],
            |p| rat(&p.min_fee_script_ref_cost_per_byte),
        ),
        "minPoolCost" => (|c| vec![P::MinPoolCost(int(c))], |p| big(&p.min_pool_cost)),
        "minPoolMargin" => (
            |c| vec![P::MinPoolMargin(ratio(c))],
            |p| rat(&p.min_pool_margin),
        ),
        "monetaryExpansion" => (
            |c| vec![P::ExpansionRate(ratio(c))],
            |p| rat(&p.monetary_expansion),
        ),
        "perasBootstrapRound" => (
            |c| {
                vec![P::PerasBootstrapRound(match c {
                    Canon::Null => None,
                    other => Some(int(other)),
                })]
            },
            |p| opt(p.peras_bootstrap_round),
        ),
        "perasCertBoost" => (
            |c| vec![P::PerasCertBoost(int(c))],
            |p| opt(p.peras_cert_boost.map(u64::from)),
        ),
        "perasHealingFactor" => (
            |c| vec![P::PerasHealingFactor(ratio(c))],
            |p| rat(&p.peras_healing_factor),
        ),
        "perasMinCandidateBlockAge" => (
            |c| vec![P::PerasMinCandidateBlockAge(int(c))],
            |p| opt(p.peras_min_candidate_block_age),
        ),
        "perasQuorumThresholdSafetyMargin" => (
            |c| vec![P::PerasQuorumThresholdSafetyMargin(ratio(c))],
            |p| rat(&p.peras_quorum_threshold_safety_margin),
        ),
        "perasTargetCommitteeSize" => (
            |c| vec![P::PerasTargetCommitteeSize(int(c))],
            |p| opt(p.peras_target_committee_size.map(u64::from)),
        ),
        "poolPledgeInfluence" => (
            |c| vec![P::PoolPledgeInfluence(ratio(c))],
            |p| rat(&p.pool_influence),
        ),
        "poolRetireMaxEpoch" => (
            |c| vec![P::MaximumEpoch(int(c))],
            |p| n(p.pool_retirement_epoch_bound),
        ),
        "poolVotingThresholds" => (pool_thresholds, |p| {
            thresholds(&p.pool_voting_thresholds, &POOL_THRESHOLDS)
        }),
        "protocolVersion" => (
            |c| {
                vec![P::ProtocolVersion((
                    int(field(c, "major")),
                    int(field(c, "minor")),
                ))]
            },
            served_version,
        ),
        "refInputsCostPerDatumByte" => (
            |c| vec![P::RefInputsCostPerDatumByte(int(c))],
            |p| big(&p.ref_inputs_cost_per_datum_byte),
        ),
        "refInputsCostPerMultiAssetPolicy" => (
            |c| vec![P::RefInputsCostPerMultiAssetPolicy(int(c))],
            |p| big(&p.ref_inputs_cost_per_multi_asset_policy),
        ),
        "refScriptCostMultiplier" => (
            |c| vec![P::RefScriptCostMultiplier(ratio(c))],
            |p| rat(&p.ref_script_cost_multiplier),
        ),
        "refScriptCostStride" => (
            |c| vec![P::RefScriptCostStride(int(c))],
            |p| opt(p.ref_script_cost_stride),
        ),
        "stakeAddressDeposit" => (
            |c| vec![P::KeyDeposit(int(c))],
            |p| big(&p.stake_key_deposit),
        ),
        "stakePoolDeposit" => (|c| vec![P::PoolDeposit(int(c))], |p| big(&p.pool_deposit)),
        "stakePoolTargetNum" => (
            |c| vec![P::DesiredNumberOfStakePools(small(c))],
            |p| n(p.desired_number_of_pools),
        ),
        "treasuryCut" => (
            |c| vec![P::TreasuryGrowthRate(ratio(c))],
            |p| rat(&p.treasury_expansion),
        ),
        "txFeeFixed" => (|c| vec![P::MinFeeB(int(c))], |p| big(&p.min_fee_constant)),
        "txFeePerByte" => (
            |c| vec![P::MinFeeA(int(c))],
            |p| big(&p.min_fee_coefficient),
        ),
        "utxoCostPerByte" => (
            |c| vec![P::AdaPerUtxoByte(int(c))],
            |p| big(&p.coins_per_utxo_byte),
        ),
};

/// The seed and served forms of a ledger parameter, none when the key is unmapped.
fn param(key: &str) -> Option<&'static Param> {
    PARAMS.iter().find(|p| p.key == key)
}

/// The fields of a latest version's message that the parameters message of `version` states.
fn narrowed(full: &u5c::PParams, version: Version) -> u5c::PParams {
    use crate::serve::grpc::{v1alpha, v1beta};
    use prost::Message as _;
    let bytes = full.encode_to_vec();
    let older = "a latest parameters message decodes as an older one";
    match version {
        Version::V1 => full.clone(),
        Version::V1beta => {
            as_latest(&v1beta::spec::cardano::PParams::decode(bytes.as_slice()).expect(older))
        }
        Version::V1alpha => {
            as_latest(&v1alpha::spec::cardano::PParams::decode(bytes.as_slice()).expect(older))
        }
    }
}

/// Parameters of the latest version's message with every field stated and none at its default.
fn every_field_stated() -> u5c::PParams {
    let big = || {
        Some(u5c::BigInt {
            big_int: Some(u5c::big_int::BigInt::Int(7)),
        })
    };
    let rat = || {
        Some(u5c::RationalNumber {
            numerator: 3,
            denominator: 7,
        })
    };
    let units = || {
        Some(u5c::ExUnits {
            steps: 7,
            memory: 9,
        })
    };
    let thresholds = |count| {
        Some(u5c::VotingThresholds {
            thresholds: vec![rat().unwrap(); count],
        })
    };
    let model = || Some(u5c::CostModel { values: vec![7] });
    u5c::PParams {
        coins_per_utxo_byte: big(),
        max_tx_size: 7,
        min_fee_coefficient: big(),
        min_fee_constant: big(),
        max_block_body_size: 7,
        max_block_header_size: 7,
        stake_key_deposit: big(),
        pool_deposit: big(),
        pool_retirement_epoch_bound: 7,
        desired_number_of_pools: 7,
        pool_influence: rat(),
        monetary_expansion: rat(),
        treasury_expansion: rat(),
        min_pool_cost: big(),
        protocol_version: Some(u5c::ProtocolVersion { major: 7, minor: 1 }),
        max_value_size: 7,
        collateral_percentage: 7,
        max_collateral_inputs: 7,
        cost_models: Some(u5c::CostModels {
            plutus_v1: model(),
            plutus_v2: model(),
            plutus_v3: model(),
            plutus_v4: model(),
        }),
        prices: Some(u5c::ExPrices {
            steps: rat(),
            memory: rat(),
        }),
        max_execution_units_per_transaction: units(),
        max_execution_units_per_block: units(),
        min_fee_script_ref_cost_per_byte: rat(),
        pool_voting_thresholds: thresholds(POOL_THRESHOLDS.len()),
        drep_voting_thresholds: thresholds(DREP_THRESHOLDS.len()),
        min_committee_size: 7,
        committee_term_limit: 7,
        governance_action_validity_period: 7,
        governance_action_deposit: big(),
        drep_deposit: big(),
        drep_inactivity_period: 7,
        max_ref_script_size_per_block: Some(7),
        max_ref_script_size_per_tx: Some(7),
        ref_script_cost_stride: Some(7),
        ref_script_cost_multiplier: rat(),
        max_pledge_leverage: rat(),
        min_pool_margin: rat(),
        leios_announcement_period_length: Some(7),
        leios_vote_period_length: Some(7),
        leios_diffusion_period_length: Some(7),
        leios_committee_size: Some(7),
        leios_quorum_stake_threshold: rat(),
        max_endorser_block_references_size: Some(7),
        max_endorser_block_txs_size: Some(7),
        max_endorser_block_execution_units: units(),
        max_ref_script_size_per_endorser_block: Some(7),
        peras_min_candidate_block_age: Some(7),
        peras_healing_factor: rat(),
        peras_cert_boost: Some(7),
        peras_target_committee_size: Some(7),
        peras_bootstrap_round: Some(7),
        peras_quorum_threshold_safety_margin: rat(),
        ref_inputs_cost_per_multi_asset_policy: big(),
        ref_inputs_cost_per_datum_byte: big(),
    }
}

/// The parameters an older version's message states, read as the latest version's message whose fields it is a prefix of.
pub(super) fn as_latest(served: &impl prost::Message) -> u5c::PParams {
    prost::Message::decode(served.encode_to_vec().as_slice())
        .expect("an older parameters message decodes as the latest")
}

fn canon(key: &str, value: &serde_json::Value) -> Result<Canon, String> {
    Canon::of_json(key, value).map_err(|e| e.to_string())
}

/// The Dolos parameter values that seed a run, or the first key with no mapping or no exact form.
pub(super) fn seed_values(pparams: &PParams) -> Result<Vec<P>, String> {
    let mut out = Vec::new();
    for (key, value) in &pparams.0 {
        let param = param(key).ok_or_else(|| format!("PARAMS unmapped key={key}"))?;
        out.extend((param.seed)(&canon(key, value)?));
    }
    Ok(out)
}

/// Counts each ledger parameter of a run that the parameters message of `version` states against the value Dolos serves for it.
pub(super) fn compare<F>(
    pparams: &PParams,
    served: &u5c::PParams,
    version: Version,
    tally: &mut Tally<F>,
    describe: impl Fn(&str, String) -> F,
) {
    let full = every_field_stated();
    let stated = narrowed(&full, version);
    for (key, value) in &pparams.0 {
        let param = param(key).unwrap_or_else(|| panic!("PARAMS unmapped key={key}"));
        if (param.served)(&full) != (param.served)(&stated) {
            continue;
        }
        let ledger = canon(key, value).unwrap_or_else(|e| panic!("{e}"));
        let difference = first_difference(key, &ledger, &(param.served)(served));
        tally.add(difference.is_none(), || {
            describe(key, difference.unwrap_or_default())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_older_version_states_the_parameters_its_message_has_fields_for() {
        let full = every_field_stated();
        let mut stated = BTreeMap::new();
        for version in Version::ALL {
            let narrowed = narrowed(&full, version);
            for p in &PARAMS {
                let kept = (p.served)(&full) == (p.served)(&narrowed);
                *stated.entry((version, kept)).or_insert(0) += 1;
            }
        }
        let counts: Vec<_> = stated.into_iter().collect();
        assert_eq!(
            counts,
            [
                ((Version::V1, true), 54),
                ((Version::V1beta, false), 23),
                ((Version::V1beta, true), 31),
                ((Version::V1alpha, false), 23),
                ((Version::V1alpha, true), 31),
            ]
        );
    }

    #[test]
    fn each_parameter_key_is_listed_once() {
        let keys: std::collections::BTreeSet<&str> = PARAMS.iter().map(|p| p.key).collect();
        assert_eq!(keys.len(), PARAMS.len());
    }

    #[test]
    fn an_unmapped_key_fails_the_seed() {
        let mut map = serde_json::Map::new();
        map.insert("maxTxSize".into(), 16384.into());
        assert!(seed_values(&PParams(map.clone())).is_ok());
        map.insert("notAParameter".into(), 1.into());
        assert_eq!(
            seed_values(&PParams(map)).err().as_deref(),
            Some("PARAMS unmapped key=notAParameter")
        );
    }

    /// The thresholds named by `names`, each the ratio of its position in the
    /// list.
    fn numbered(names: &[&str]) -> Canon {
        let at = |i: usize| Canon::Ratio(i as i128 + 1, 1009);
        Canon::Map(
            names
                .iter()
                .enumerate()
                .map(|(i, n)| (n.to_string(), at(i)))
                .collect(),
        )
    }

    /// The numerators of thresholds seeded from `numbered`, which give each
    /// one's position.
    fn positions(ratios: &[&RationalNumber]) -> Vec<u64> {
        ratios.iter().map(|r| r.numerator).collect()
    }

    #[test]
    fn the_drep_threshold_names_are_the_ledger_fields_in_order() {
        let [P::DrepVotingThresholds(t)] = &drep_thresholds(&numbered(&DREP_THRESHOLDS))[..] else {
            panic!("one DRep threshold update")
        };
        let DRepVotingThresholds {
            motion_no_confidence,
            committee_normal,
            committee_no_confidence,
            update_constitution,
            hard_fork_initiation,
            pp_network_group,
            pp_economic_group,
            pp_technical_group,
            pp_governance_group,
            treasury_withdrawal,
        } = t;
        let fields = [
            motion_no_confidence,
            committee_normal,
            committee_no_confidence,
            update_constitution,
            hard_fork_initiation,
            pp_network_group,
            pp_economic_group,
            pp_technical_group,
            pp_governance_group,
            treasury_withdrawal,
        ];
        assert_eq!(positions(&fields), (1..=10).collect::<Vec<_>>());
    }

    #[test]
    fn the_pool_threshold_names_are_the_ledger_fields_in_order() {
        let [P::PoolVotingThresholds(t)] = &pool_thresholds(&numbered(&POOL_THRESHOLDS))[..] else {
            panic!("one pool threshold update")
        };
        let PoolVotingThresholds {
            motion_no_confidence,
            committee_normal,
            committee_no_confidence,
            hard_fork_initiation,
            security_voting_threshold,
        } = t;
        let fields = [
            motion_no_confidence,
            committee_normal,
            committee_no_confidence,
            hard_fork_initiation,
            security_voting_threshold,
        ];
        assert_eq!(positions(&fields), (1..=5).collect::<Vec<_>>());
    }

    #[test]
    fn the_language_names_are_the_ledger_languages_by_index() {
        use pallas::{codec::minicbor, ledger::primitives::dijkstra::Language};
        let name = |language: &Language| match language {
            Language::PlutusV1 => "PlutusV1",
            Language::PlutusV2 => "PlutusV2",
            Language::PlutusV3 => "PlutusV3",
            Language::PlutusV4 => "PlutusV4",
        };
        let decoded: Vec<(usize, &str)> = (0u8..=255)
            .filter_map(|n| {
                let language = minicbor::decode::<Language>(&minicbor::to_vec(n).unwrap()).ok()?;
                Some((usize::from(n), name(&language)))
            })
            .collect();
        let listed: Vec<(usize, &str)> = LANGUAGES.iter().copied().enumerate().collect();
        assert_eq!(decoded, listed);
    }
}
