use dolos_cardano::indexes::CardanoStateIndexExt;
use itertools::Itertools as _;
use pallas::interop::utxorpc::v1beta::spec::query::any_utxo_pattern::UtxoPattern;
use pallas::interop::utxorpc::v1beta::{self as interop, spec as u5c};
use pallas::interop::utxorpc::LedgerContext;
use pallas::ledger::traverse::{MultiEraBlock, MultiEraOutput};
use std::collections::HashSet;
use tonic::{Request, Response, Status};
use tracing::{info, warn};

use crate::prelude::*;
use crate::serve::grpc::block_refs::BlockRefData;
use crate::serve::grpc::masking::apply_mask;
use dolos_cardano::indexes::AsyncCardanoQueryExt;

fn to_chain_point(data: &BlockRefData) -> u5c::query::ChainPoint {
    u5c::query::ChainPoint {
        slot: data.slot,
        hash: data.hash.to_vec().into(),
        height: data.height,
        timestamp: data.timestamp,
    }
}

pub fn point_to_u5c<T: LedgerContext>(_ledger: &T, point: &ChainPoint) -> u5c::query::ChainPoint {
    u5c::query::ChainPoint {
        slot: point.slot(),
        hash: point.hash().map(|h| h.to_vec()).unwrap_or_default().into(),
        ..Default::default()
    }
}

pub struct QueryServiceImpl<D>
where
    D: Domain + LedgerContext,
{
    domain: D,
    mapper: interop::Mapper<D>,
}

impl<D> QueryServiceImpl<D>
where
    D: Domain + LedgerContext,
{
    pub fn new(domain: D) -> Self {
        let mapper = interop::Mapper::new(domain.clone());

        Self { domain, mapper }
    }
}

fn into_status(err: impl std::error::Error) -> Status {
    Status::internal(err.to_string())
}

/// Builds the protocol parameters a client is served for the current chain.
fn map_live_params<C: LedgerContext>(
    mapper: &interop::Mapper<C>,
    pparams: &dolos_cardano::PParamsSet,
) -> Result<u5c::cardano::PParams, ChainError> {
    // The era mapping sets no retirement epoch bound, so the reply would carry a
    // zero that a client cannot tell from a bound of zero epochs.
    let bound = pparams
        .maximum_epoch()
        .ok_or_else(|| ChainError::PParamsNotFound("MaximumEpoch".to_string()))?;

    let mut mapped = mapper.map_pparams(dolos_cardano::utils::pparams_to_pallas(pparams));

    mapped.pool_retirement_epoch_bound = bound;

    // The Conway cost model type names three languages and reads the PlutusV4
    // vector into a wildcard map, which the era mapping drops, so a client
    // pricing a PlutusV4 script would be served no model for it.
    if let Some(values) = plutus_v4_cost_model(pparams) {
        mapped
            .cost_models
            .get_or_insert_with(Default::default)
            .plutus_v4 = Some(u5c::cardano::CostModel { values });
    }

    Ok(mapped)
}

/// The PlutusV4 cost model the parameters carry, at key 3 of the wildcard map.
fn plutus_v4_cost_model(pparams: &dolos_cardano::PParamsSet) -> Option<Vec<i64>> {
    pparams
        .cost_models_unknown()?
        .get(&dolos_cardano::pallas_extras::PLUTUS_V4_COST_MODEL_KEY)
        .cloned()
}

trait IntoSet {
    fn into_set<S: CardanoStateIndexExt>(self, state: &S) -> Result<HashSet<TxoRef>, Status>;
}

fn intersect<S: CardanoStateIndexExt>(
    state: &S,
    a: impl IntoSet,
    b: impl IntoSet,
) -> Result<HashSet<TxoRef>, Status> {
    let a = a.into_set(state)?;
    let b = b.into_set(state)?;

    Ok(a.intersection(&b).cloned().collect())
}

struct ByAddressQuery(bytes::Bytes);

impl ByAddressQuery {
    fn maybe_from(data: bytes::Bytes) -> Option<Self> {
        if data.is_empty() {
            return None;
        }

        Some(Self(data))
    }
}

impl IntoSet for ByAddressQuery {
    fn into_set<S: CardanoStateIndexExt>(self, state: &S) -> Result<HashSet<TxoRef>, Status> {
        state.utxos_by_address(&self.0).map_err(into_status)
    }
}

struct ByPaymentQuery(bytes::Bytes);

impl ByPaymentQuery {
    fn maybe_from(data: bytes::Bytes) -> Option<Self> {
        if data.is_empty() {
            return None;
        }

        Some(Self(data))
    }
}

impl IntoSet for ByPaymentQuery {
    fn into_set<S: CardanoStateIndexExt>(self, state: &S) -> Result<HashSet<TxoRef>, Status> {
        state.utxos_by_payment(&self.0).map_err(into_status)
    }
}

struct ByDelegationQuery(bytes::Bytes);

impl ByDelegationQuery {
    fn maybe_from(data: bytes::Bytes) -> Option<Self> {
        if data.is_empty() {
            return None;
        }

        Some(Self(data))
    }
}

impl IntoSet for ByDelegationQuery {
    fn into_set<S: CardanoStateIndexExt>(self, state: &S) -> Result<HashSet<TxoRef>, Status> {
        state.utxos_by_stake(&self.0).map_err(into_status)
    }
}

impl IntoSet for u5c::cardano::AddressPattern {
    fn into_set<S: CardanoStateIndexExt>(self, state: &S) -> Result<HashSet<TxoRef>, Status> {
        let exact = ByAddressQuery::maybe_from(self.exact_address);
        let payment = ByPaymentQuery::maybe_from(self.payment_part);
        let delegation = ByDelegationQuery::maybe_from(self.delegation_part);

        match (exact, payment, delegation) {
            (Some(x), None, None) => x.into_set(state),
            (None, Some(x), None) => x.into_set(state),
            (None, None, Some(x)) => x.into_set(state),
            (None, Some(a), Some(b)) => intersect(state, a, b),
            (None, None, None) => Ok(HashSet::default()),
            _ => Err(Status::invalid_argument("conflicting address criteria")),
        }
    }
}

struct ByPolicyQuery(bytes::Bytes);

impl ByPolicyQuery {
    fn maybe_from(data: bytes::Bytes) -> Option<Self> {
        if data.is_empty() {
            return None;
        }

        Some(Self(data))
    }
}

impl IntoSet for ByPolicyQuery {
    fn into_set<S: CardanoStateIndexExt>(self, state: &S) -> Result<HashSet<TxoRef>, Status> {
        state.utxos_by_policy(&self.0).map_err(into_status)
    }
}

struct ByAssetQuery(bytes::Bytes);

impl ByAssetQuery {
    fn maybe_from(data: bytes::Bytes) -> Option<Self> {
        if data.is_empty() {
            return None;
        }

        Some(Self(data))
    }
}

impl IntoSet for ByAssetQuery {
    fn into_set<S: CardanoStateIndexExt>(self, state: &S) -> Result<HashSet<TxoRef>, Status> {
        state.utxos_by_asset(&self.0).map_err(into_status)
    }
}

impl IntoSet for u5c::cardano::AssetPattern {
    fn into_set<S: CardanoStateIndexExt>(self, state: &S) -> Result<HashSet<TxoRef>, Status> {
        let by_policy = ByPolicyQuery::maybe_from(self.policy_id.clone());
        let by_asset = ByAssetQuery::maybe_from(self.asset_name.clone());

        match (by_policy, by_asset) {
            (Some(_), Some(_)) => {
                let mut subject = self.policy_id.to_vec();
                subject.extend_from_slice(&self.asset_name);
                ByAssetQuery(bytes::Bytes::from(subject)).into_set(state)
            }
            (Some(x), None) => x.into_set(state),
            (None, Some(_)) => Err(Status::invalid_argument(
                "asset name query requires a policy_id",
            )),
            (None, None) => Ok(HashSet::default()),
        }
    }
}

impl IntoSet for u5c::cardano::TxOutputPattern {
    fn into_set<S: CardanoStateIndexExt>(self, state: &S) -> Result<HashSet<TxoRef>, Status> {
        match (self.address, self.asset) {
            (None, Some(x)) => x.into_set(state),
            (Some(x), None) => x.into_set(state),
            (Some(a), Some(b)) => intersect(state, a, b),
            (None, None) => Ok(HashSet::default()),
        }
    }
}

impl IntoSet for u5c::query::AnyUtxoPattern {
    fn into_set<S: CardanoStateIndexExt>(self, state: &S) -> Result<HashSet<TxoRef>, Status> {
        match self.utxo_pattern {
            Some(UtxoPattern::Cardano(x)) => x.into_set(state),
            _ => Ok(HashSet::new()),
        }
    }
}

fn from_u5c_txoref(txo: u5c::query::TxoRef) -> Result<TxoRef, Status> {
    let hash = crate::serve::grpc::convert::bytes_to_hash32(&txo.hash)?;
    Ok(TxoRef(hash, txo.index))
}

fn u64_to_bigint(value: u64) -> Option<u5c::cardano::BigInt> {
    if value <= i64::MAX as u64 {
        Some(u5c::cardano::BigInt {
            big_int: Some(u5c::cardano::big_int::BigInt::Int(value as i64)),
        })
    } else {
        Some(u5c::cardano::BigInt {
            big_int: Some(u5c::cardano::big_int::BigInt::BigUInt(
                value.to_be_bytes().to_vec().into(),
            )),
        })
    }
}

fn rational_to_u5c(numerator: u64, denominator: u64) -> u5c::cardano::RationalNumber {
    u5c::cardano::RationalNumber {
        numerator: numerator as i32,
        denominator: denominator as u32,
    }
}

fn float_to_u5c_rational(value: f32) -> u5c::cardano::RationalNumber {
    let value = dolos_cardano::utils::float_to_rational(value);
    rational_to_u5c(value.numerator, value.denominator)
}

fn map_execution_prices(
    value: &pallas::interop::hardano::configs::alonzo::ExecutionPrices,
) -> u5c::cardano::ExPrices {
    let value: pallas::ledger::primitives::alonzo::ExUnitPrices = value.clone().into();

    u5c::cardano::ExPrices {
        steps: Some(rational_to_u5c(
            value.step_price.numerator,
            value.step_price.denominator,
        )),
        memory: Some(rational_to_u5c(
            value.mem_price.numerator,
            value.mem_price.denominator,
        )),
    }
}

fn map_execution_units(
    value: &pallas::interop::hardano::configs::alonzo::ExUnits,
) -> u5c::cardano::ExUnits {
    u5c::cardano::ExUnits {
        steps: value.ex_units_steps,
        memory: value.ex_units_mem,
    }
}

fn map_cost_models(
    genesis: &Genesis,
) -> (
    Option<u5c::cardano::CostModels>,
    Option<u5c::cardano::CostModelMap>,
) {
    use pallas::interop::hardano::configs::alonzo::Language;

    let plutus_v1 = genesis
        .alonzo
        .cost_models
        .get(&Language::PlutusV1)
        .cloned()
        .map(Vec::<i64>::from)
        .map(|values| u5c::cardano::CostModel { values });

    let plutus_v2 = genesis
        .alonzo
        .cost_models
        .get(&Language::PlutusV2)
        .cloned()
        .map(Vec::<i64>::from)
        .map(|values| u5c::cardano::CostModel { values });

    let plutus_v3 =
        (!genesis.conway.plutus_v3_cost_model.is_empty()).then(|| u5c::cardano::CostModel {
            values: genesis.conway.plutus_v3_cost_model.clone(),
        });

    // The PlutusV4 model is declared by the Dijkstra genesis file, which a
    // configuration for an earlier network has no path for.
    let plutus_v4 = genesis
        .dijkstra
        .as_ref()
        .filter(|x| !x.plutus_v4_cost_model.is_empty())
        .map(|x| u5c::cardano::CostModel {
            values: x.plutus_v4_cost_model.clone(),
        });

    let cost_models = u5c::cardano::CostModels {
        plutus_v1: plutus_v1.clone(),
        plutus_v2: plutus_v2.clone(),
        plutus_v3: plutus_v3.clone(),
        plutus_v4: plutus_v4.clone(),
    };

    let cost_model_map = u5c::cardano::CostModelMap {
        plutus_v1,
        plutus_v2,
        plutus_v3,
        plutus_v4,
    };

    (
        Some(cost_models).filter(|x| {
            x.plutus_v1.is_some()
                || x.plutus_v2.is_some()
                || x.plutus_v3.is_some()
                || x.plutus_v4.is_some()
        }),
        Some(cost_model_map).filter(|x| {
            x.plutus_v1.is_some()
                || x.plutus_v2.is_some()
                || x.plutus_v3.is_some()
                || x.plutus_v4.is_some()
        }),
    )
}

fn map_genesis_protocol_params(genesis: &Genesis) -> u5c::cardano::PParams {
    let shelley = &genesis.shelley.protocol_params;
    let (cost_models, _) = map_cost_models(genesis);

    u5c::cardano::PParams {
        max_tx_size: shelley.max_tx_size.into(),
        min_fee_coefficient: u64_to_bigint(shelley.min_fee_a.into()),
        min_fee_constant: u64_to_bigint(shelley.min_fee_b.into()),
        max_block_body_size: shelley.max_block_body_size.into(),
        max_block_header_size: shelley.max_block_header_size.into(),
        stake_key_deposit: u64_to_bigint(shelley.key_deposit),
        pool_deposit: u64_to_bigint(shelley.pool_deposit),
        pool_retirement_epoch_bound: shelley.e_max,
        desired_number_of_pools: shelley.n_opt.into(),
        pool_influence: Some(rational_to_u5c(
            shelley.a0.numerator,
            shelley.a0.denominator,
        )),
        monetary_expansion: Some(rational_to_u5c(
            shelley.rho.numerator,
            shelley.rho.denominator,
        )),
        treasury_expansion: Some(rational_to_u5c(
            shelley.tau.numerator,
            shelley.tau.denominator,
        )),
        min_pool_cost: u64_to_bigint(shelley.min_pool_cost),
        protocol_version: Some(u5c::cardano::ProtocolVersion {
            major: shelley.protocol_version.major as u32,
            minor: shelley.protocol_version.minor as u32,
        }),
        max_value_size: genesis.alonzo.max_value_size.into(),
        collateral_percentage: genesis.alonzo.collateral_percentage.into(),
        max_collateral_inputs: genesis.alonzo.max_collateral_inputs.into(),
        cost_models,
        prices: Some(map_execution_prices(&genesis.alonzo.execution_prices)),
        max_execution_units_per_transaction: Some(map_execution_units(
            &genesis.alonzo.max_tx_ex_units,
        )),
        max_execution_units_per_block: Some(map_execution_units(
            &genesis.alonzo.max_block_ex_units,
        )),
        min_fee_script_ref_cost_per_byte: Some(rational_to_u5c(
            genesis.conway.min_fee_ref_script_cost_per_byte,
            1,
        )),
        pool_voting_thresholds: Some(u5c::cardano::VotingThresholds {
            thresholds: vec![
                float_to_u5c_rational(genesis.conway.pool_voting_thresholds.motion_no_confidence),
                float_to_u5c_rational(genesis.conway.pool_voting_thresholds.committee_normal),
                float_to_u5c_rational(
                    genesis
                        .conway
                        .pool_voting_thresholds
                        .committee_no_confidence,
                ),
                float_to_u5c_rational(genesis.conway.pool_voting_thresholds.hard_fork_initiation),
                float_to_u5c_rational(genesis.conway.pool_voting_thresholds.pp_security_group),
            ],
        }),
        drep_voting_thresholds: Some(u5c::cardano::VotingThresholds {
            thresholds: vec![
                float_to_u5c_rational(genesis.conway.d_rep_voting_thresholds.motion_no_confidence),
                float_to_u5c_rational(genesis.conway.d_rep_voting_thresholds.committee_normal),
                float_to_u5c_rational(
                    genesis
                        .conway
                        .d_rep_voting_thresholds
                        .committee_no_confidence,
                ),
                float_to_u5c_rational(
                    genesis
                        .conway
                        .d_rep_voting_thresholds
                        .update_to_constitution,
                ),
                float_to_u5c_rational(genesis.conway.d_rep_voting_thresholds.hard_fork_initiation),
                float_to_u5c_rational(genesis.conway.d_rep_voting_thresholds.pp_network_group),
                float_to_u5c_rational(genesis.conway.d_rep_voting_thresholds.pp_economic_group),
                float_to_u5c_rational(genesis.conway.d_rep_voting_thresholds.pp_technical_group),
                float_to_u5c_rational(genesis.conway.d_rep_voting_thresholds.pp_gov_group),
                float_to_u5c_rational(genesis.conway.d_rep_voting_thresholds.treasury_withdrawal),
            ],
        }),
        min_committee_size: genesis.conway.committee_min_size as u32,
        committee_term_limit: genesis.conway.committee_max_term_length.into(),
        governance_action_validity_period: genesis.conway.gov_action_lifetime.into(),
        governance_action_deposit: u64_to_bigint(genesis.conway.gov_action_deposit),
        drep_deposit: u64_to_bigint(genesis.conway.d_rep_deposit),
        drep_inactivity_period: genesis.conway.d_rep_activity.into(),
        ..Default::default()
    }
}

fn caip2_from_genesis(genesis: &Genesis) -> Result<String, Status> {
    match genesis.shelley.network_magic {
        Some(764824073) => Ok("cardano:mainnet".into()),
        Some(1) => Ok("cardano:preprod".into()),
        Some(2) => Ok("cardano:preview".into()),
        Some(x) => Ok(format!("cardano:{x}")),
        None => Err(Status::internal("missing Cardano network magic")),
    }
}

fn map_cardano_genesis(genesis: &Genesis) -> Result<u5c::cardano::Genesis, Status> {
    let (_, cost_model_map) = map_cost_models(genesis);
    let constitution_anchor_hash = hex::decode(&genesis.conway.constitution.anchor.data_hash)
        .map_err(|e| Status::internal(format!("invalid constitution anchor hash: {e}")))?;
    let constitution_hash = genesis
        .conway
        .constitution
        .script
        .as_deref()
        .map(hex::decode)
        .transpose()
        .map_err(|e| Status::internal(format!("invalid constitution script hash: {e}")))?
        .unwrap_or_default();

    Ok(u5c::cardano::Genesis {
        avvm_distr: genesis.byron.avvm_distr.clone(),
        block_version_data: Some(u5c::cardano::BlockVersionData {
            script_version: genesis.byron.block_version_data.script_version.into(),
            slot_duration: genesis.byron.block_version_data.slot_duration.to_string(),
            max_block_size: genesis.byron.block_version_data.max_block_size.to_string(),
            max_header_size: genesis.byron.block_version_data.max_header_size.to_string(),
            max_tx_size: genesis.byron.block_version_data.max_tx_size.to_string(),
            max_proposal_size: genesis
                .byron
                .block_version_data
                .max_proposal_size
                .to_string(),
            mpc_thd: genesis.byron.block_version_data.mpc_thd.to_string(),
            heavy_del_thd: genesis.byron.block_version_data.heavy_del_thd.to_string(),
            update_vote_thd: genesis.byron.block_version_data.update_vote_thd.to_string(),
            update_proposal_thd: genesis
                .byron
                .block_version_data
                .update_proposal_thd
                .to_string(),
            update_implicit: genesis.byron.block_version_data.update_implicit.to_string(),
            softfork_rule: Some(u5c::cardano::SoftforkRule {
                init_thd: genesis
                    .byron
                    .block_version_data
                    .softfork_rule
                    .init_thd
                    .to_string(),
                min_thd: genesis
                    .byron
                    .block_version_data
                    .softfork_rule
                    .min_thd
                    .to_string(),
                thd_decrement: genesis
                    .byron
                    .block_version_data
                    .softfork_rule
                    .thd_decrement
                    .to_string(),
            }),
            tx_fee_policy: Some(u5c::cardano::TxFeePolicy {
                multiplier: genesis
                    .byron
                    .block_version_data
                    .tx_fee_policy
                    .multiplier
                    .to_string(),
                summand: genesis
                    .byron
                    .block_version_data
                    .tx_fee_policy
                    .summand
                    .to_string(),
            }),
            unlock_stake_epoch: genesis
                .byron
                .block_version_data
                .unlock_stake_epoch
                .to_string(),
        }),
        fts_seed: genesis.byron.fts_seed.clone().unwrap_or_default(),
        protocol_consts: Some(u5c::cardano::ProtocolConsts {
            k: genesis.byron.protocol_consts.k as u32,
            protocol_magic: genesis.byron.protocol_consts.protocol_magic,
            vss_max_ttl: genesis
                .byron
                .protocol_consts
                .vss_max_ttl
                .unwrap_or_default(),
            vss_min_ttl: genesis
                .byron
                .protocol_consts
                .vss_min_ttl
                .unwrap_or_default(),
        }),
        start_time: genesis.byron.start_time,
        boot_stakeholders: genesis
            .byron
            .boot_stakeholders
            .iter()
            .map(|(k, v)| (k.clone(), (*v).into()))
            .collect(),
        heavy_delegation: genesis
            .byron
            .heavy_delegation
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    u5c::cardano::HeavyDelegation {
                        cert: v.cert.clone(),
                        delegate_pk: v.delegate_pk.clone(),
                        issuer_pk: v.issuer_pk.clone(),
                        omega: 0,
                    },
                )
            })
            .collect(),
        non_avvm_balances: genesis.byron.non_avvm_balances.clone(),
        vss_certs: genesis
            .byron
            .vss_certs
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|(k, v)| {
                (
                    k,
                    u5c::cardano::VssCert {
                        expiry_epoch: v.expiry_epoch,
                        signature: v.signature,
                        signing_key: v.signing_key,
                        vss_key: v.vss_key,
                    },
                )
            })
            .collect(),
        active_slots_coeff: genesis
            .shelley
            .active_slots_coeff
            .map(float_to_u5c_rational),
        epoch_length: genesis.shelley.epoch_length.unwrap_or_default(),
        gen_delegs: genesis
            .shelley
            .gen_delegs
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|(k, v)| {
                (
                    k,
                    u5c::cardano::GenDelegs {
                        delegate: v.delegate.unwrap_or_default(),
                        vrf: v.vrf.unwrap_or_default(),
                    },
                )
            })
            .collect(),
        initial_funds: genesis
            .shelley
            .initial_funds
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|(k, v)| {
                (
                    k,
                    u64_to_bigint(v).expect("u64 genesis funds must map to bigint"),
                )
            })
            .collect(),
        max_kes_evolutions: genesis.shelley.max_kes_evolutions.unwrap_or_default(),
        max_lovelace_supply: genesis.shelley.max_lovelace_supply.and_then(u64_to_bigint),
        network_id: genesis.shelley.network_id.clone().unwrap_or_default(),
        network_magic: genesis.shelley.network_magic.unwrap_or_default(),
        protocol_params: Some(map_genesis_protocol_params(genesis)),
        security_param: genesis.shelley.security_param.unwrap_or_default(),
        slot_length: genesis.shelley.slot_length.unwrap_or_default(),
        slots_per_kes_period: genesis.shelley.slots_per_kes_period.unwrap_or_default(),
        system_start: genesis.shelley.system_start.clone().unwrap_or_default(),
        update_quorum: genesis.shelley.update_quorum.unwrap_or_default(),
        lovelace_per_utxo_word: u64_to_bigint(genesis.alonzo.lovelace_per_utxo_word),
        execution_prices: Some(map_execution_prices(&genesis.alonzo.execution_prices)),
        max_tx_ex_units: Some(map_execution_units(&genesis.alonzo.max_tx_ex_units)),
        max_block_ex_units: Some(map_execution_units(&genesis.alonzo.max_block_ex_units)),
        max_value_size: genesis.alonzo.max_value_size,
        collateral_percentage: genesis.alonzo.collateral_percentage,
        max_collateral_inputs: genesis.alonzo.max_collateral_inputs,
        cost_models: cost_model_map,
        committee: Some(u5c::cardano::Committee {
            members: genesis.conway.committee.members.clone(),
            threshold: Some(rational_to_u5c(
                genesis.conway.committee.threshold.numerator,
                genesis.conway.committee.threshold.denominator,
            )),
        }),
        constitution: Some(u5c::cardano::Constitution {
            anchor: Some(u5c::cardano::Anchor {
                url: genesis.conway.constitution.anchor.url.clone(),
                content_hash: constitution_anchor_hash.into(),
            }),
            hash: constitution_hash.into(),
        }),
        committee_min_size: genesis.conway.committee_min_size,
        committee_max_term_length: genesis.conway.committee_max_term_length.into(),
        gov_action_lifetime: genesis.conway.gov_action_lifetime.into(),
        gov_action_deposit: u64_to_bigint(genesis.conway.gov_action_deposit),
        drep_deposit: u64_to_bigint(genesis.conway.d_rep_deposit),
        drep_activity: genesis.conway.d_rep_activity.into(),
        min_fee_ref_script_cost_per_byte: Some(rational_to_u5c(
            genesis.conway.min_fee_ref_script_cost_per_byte,
            1,
        )),
        drep_voting_thresholds: Some(u5c::cardano::DRepVotingThresholds {
            motion_no_confidence: Some(float_to_u5c_rational(
                genesis.conway.d_rep_voting_thresholds.motion_no_confidence,
            )),
            committee_normal: Some(float_to_u5c_rational(
                genesis.conway.d_rep_voting_thresholds.committee_normal,
            )),
            committee_no_confidence: Some(float_to_u5c_rational(
                genesis
                    .conway
                    .d_rep_voting_thresholds
                    .committee_no_confidence,
            )),
            update_to_constitution: Some(float_to_u5c_rational(
                genesis
                    .conway
                    .d_rep_voting_thresholds
                    .update_to_constitution,
            )),
            hard_fork_initiation: Some(float_to_u5c_rational(
                genesis.conway.d_rep_voting_thresholds.hard_fork_initiation,
            )),
            pp_network_group: Some(float_to_u5c_rational(
                genesis.conway.d_rep_voting_thresholds.pp_network_group,
            )),
            pp_economic_group: Some(float_to_u5c_rational(
                genesis.conway.d_rep_voting_thresholds.pp_economic_group,
            )),
            pp_technical_group: Some(float_to_u5c_rational(
                genesis.conway.d_rep_voting_thresholds.pp_technical_group,
            )),
            pp_gov_group: Some(float_to_u5c_rational(
                genesis.conway.d_rep_voting_thresholds.pp_gov_group,
            )),
            treasury_withdrawal: Some(float_to_u5c_rational(
                genesis.conway.d_rep_voting_thresholds.treasury_withdrawal,
            )),
        }),
        pool_voting_thresholds: Some(u5c::cardano::PoolVotingThresholds {
            motion_no_confidence: Some(float_to_u5c_rational(
                genesis.conway.pool_voting_thresholds.motion_no_confidence,
            )),
            committee_normal: Some(float_to_u5c_rational(
                genesis.conway.pool_voting_thresholds.committee_normal,
            )),
            committee_no_confidence: Some(float_to_u5c_rational(
                genesis
                    .conway
                    .pool_voting_thresholds
                    .committee_no_confidence,
            )),
            hard_fork_initiation: Some(float_to_u5c_rational(
                genesis.conway.pool_voting_thresholds.hard_fork_initiation,
            )),
            pp_security_group: Some(float_to_u5c_rational(
                genesis.conway.pool_voting_thresholds.pp_security_group,
            )),
        }),
    })
}

fn map_era_boundary(boundary: &dolos_cardano::EraBoundary) -> u5c::cardano::EraBoundary {
    u5c::cardano::EraBoundary {
        time: boundary.timestamp.saturating_mul(1000),
        slot: boundary.slot,
        epoch: boundary.epoch,
    }
}

fn protocol_to_era_name(protocol: u16) -> &'static str {
    match protocol {
        0..=1 => "byron",
        2 => "shelley",
        3 => "allegra",
        4 => "mary",
        5..=6 => "alonzo",
        7..=8 => "babbage",
        9..=10 => "conway",
        _ => "unknown",
    }
}

fn map_era_summary(
    era: &dolos_cardano::EraSummary,
    active_protocol: u16,
    active_params: &u5c::cardano::PParams,
) -> u5c::cardano::EraSummary {
    u5c::cardano::EraSummary {
        name: protocol_to_era_name(era.protocol).into(),
        start: Some(map_era_boundary(&era.start)),
        end: era.end.as_ref().map(map_era_boundary),
        protocol_params: (era.protocol == active_protocol).then(|| active_params.clone()),
    }
}

async fn into_u5c_utxo<S: Domain + LedgerContext>(
    txo: &TxoRef,
    body: &EraCbor,
    mapper: &interop::Mapper<S>,
    domain: &S,
    block_ref: Option<u5c::query::ChainPoint>,
) -> Result<u5c::query::AnyUtxoData, Box<dyn std::error::Error>> {
    use pallas::ledger::primitives::conway::DatumOption;

    let query = dolos_core::AsyncQueryFacade::new(domain.clone());

    let parsed_output = MultiEraOutput::try_from(body)?;
    let mut parsed = mapper.map_tx_output(&parsed_output, None);

    // If the output has a datum hash, try to fetch the datum value from storage
    if let Some(DatumOption::Hash(datum_hash)) = parsed_output.datum() {
        match query.get_datum(&datum_hash).await {
            Ok(Some(datum_bytes)) => {
                // Decode the datum and update the parsed output
                match pallas::codec::minicbor::decode::<
                    pallas::ledger::primitives::conway::PlutusData,
                >(&datum_bytes)
                {
                    Ok(plutus_data) => {
                        // Update the datum field with both hash and payload
                        parsed.datum = Some(u5c::cardano::Datum {
                            hash: datum_hash.to_vec().into(),
                            payload: Some(mapper.map_plutus_datum(&plutus_data)),
                            original_cbor: Some(datum_bytes.into()),
                        });
                    }
                    Err(e) => {
                        warn!(
                            datum_hash = hex::encode(datum_hash),
                            error = %e,
                            "Failed to decode datum value from storage"
                        );
                    }
                }
            }
            Ok(None) => {
                warn!(
                    datum_hash = hex::encode(datum_hash),
                    txo_ref = format!("{}#{}", hex::encode(txo.0), txo.1),
                    "Datum value not found in storage for UTXO with datum hash"
                );
            }
            Err(e) => {
                warn!(
                    datum_hash = hex::encode(datum_hash),
                    error = %e,
                    "Error querying datum storage"
                );
            }
        }
    }

    Ok(u5c::query::AnyUtxoData {
        txo_ref: Some(u5c::query::TxoRef {
            hash: txo.0.to_vec().into(),
            index: txo.1,
        }),
        native_bytes: body.1.clone().into(),
        parsed_state: Some(u5c::query::any_utxo_data::ParsedState::Cardano(parsed)),
        block_ref,
    })
}

#[async_trait::async_trait]
impl<D> u5c::query::query_service_server::QueryService for QueryServiceImpl<D>
where
    D: Domain + LedgerContext,
{
    async fn read_params(
        &self,
        request: Request<u5c::query::ReadParamsRequest>,
    ) -> Result<Response<u5c::query::ReadParamsResponse>, Status> {
        let message = request.into_inner();

        info!("received new grpc query - read_params");

        let tip = self
            .domain
            .state()
            .read_cursor()
            .map_err(into_status)?
            .ok_or(Status::internal("Failed to find ledger tip"))?;

        let pparams = dolos_cardano::load_effective_pparams::<D>(self.domain.state())
            .map_err(|_| Status::internal("Failed to load current pparams"))?;

        let params = map_live_params(&self.mapper, &pparams).map_err(into_status)?;

        let mut response = u5c::query::ReadParamsResponse {
            values: Some(u5c::query::AnyChainParams {
                params: u5c::query::any_chain_params::Params::Cardano(params).into(),
            }),
            ledger_tip: Some(point_to_u5c(&self.domain, &tip)),
        };

        if let Some(mask) = message.field_mask {
            response = apply_mask(response, mask.paths)
                .map_err(|_| Status::internal("Failed to apply field mask"))?
        }

        Ok(Response::new(response))
    }

    async fn read_data(
        &self,
        request: Request<u5c::query::ReadDataRequest>,
    ) -> Result<Response<u5c::query::ReadDataResponse>, Status> {
        let _message = request.into_inner();

        info!("received new grpc query - read_data");

        todo!()
    }

    async fn read_utxos(
        &self,
        request: Request<u5c::query::ReadUtxosRequest>,
    ) -> Result<Response<u5c::query::ReadUtxosResponse>, Status> {
        let message = request.into_inner();

        info!("received new grpc query - read_utxos");

        let keys: Vec<_> = message
            .keys
            .into_iter()
            .map(from_u5c_txoref)
            .try_collect()?;

        let utxos = StateStore::get_utxos(self.domain.state(), keys)
            .map_err(|e| Status::internal(e.to_string()))?;

        let block_refs =
            crate::serve::grpc::block_refs::fetch_block_refs(&self.domain, utxos.keys()).await?;

        let mut items = Vec::new();
        for (k, v) in utxos.iter() {
            let block_ref = block_refs.get(&k.0).map(to_chain_point);
            items.push(
                into_u5c_utxo(k, v, &self.mapper, &self.domain, block_ref)
                    .await
                    .map_err(|e| Status::internal(e.to_string()))?,
            );
        }

        let cursor = self
            .domain
            .state()
            .read_cursor()
            .map_err(|e| Status::internal(e.to_string()))?
            .as_ref()
            .map(|p| point_to_u5c(&self.domain, p));

        Ok(Response::new(u5c::query::ReadUtxosResponse {
            items,
            ledger_tip: cursor,
        }))
    }

    async fn search_utxos(
        &self,
        request: Request<u5c::query::SearchUtxosRequest>,
    ) -> Result<Response<u5c::query::SearchUtxosResponse>, Status> {
        let message = request.into_inner();

        info!("received new grpc query - search_utxos");

        let set = match message.predicate {
            Some(x) => match x.r#match {
                Some(x) => x.into_set(self.domain.state())?,
                _ => {
                    return Err(Status::invalid_argument(
                        "only 'match' predicate is supported by Dolos",
                    ));
                }
            },
            _ => {
                return Err(Status::invalid_argument(
                    "criteria too broad, narrow it down",
                ));
            }
        };

        let utxos = StateStore::get_utxos(self.domain.state(), set.into_iter().collect_vec())
            .map_err(|e| Status::internal(e.to_string()))?;

        let block_refs =
            crate::serve::grpc::block_refs::fetch_block_refs(&self.domain, utxos.keys()).await?;

        let mut items = Vec::new();
        for (k, v) in utxos.iter() {
            let block_ref = block_refs.get(&k.0).map(to_chain_point);
            items.push(
                into_u5c_utxo(k, v, &self.mapper, &self.domain, block_ref)
                    .await
                    .map_err(|e| Status::internal(e.to_string()))?,
            );
        }

        let cursor = self
            .domain
            .state()
            .read_cursor()
            .map_err(|e| Status::internal(e.to_string()))?
            .as_ref()
            .map(|p| point_to_u5c(&self.domain, p));

        Ok(Response::new(u5c::query::SearchUtxosResponse {
            items,
            ledger_tip: cursor,
            next_token: String::default(),
        }))
    }

    async fn read_tx(
        &self,
        request: Request<u5c::query::ReadTxRequest>,
    ) -> Result<Response<u5c::query::ReadTxResponse>, Status> {
        let message = request.into_inner();

        info!("received new grpc query - read_tx");

        let tx_hash = message.hash;

        let query = dolos_core::AsyncQueryFacade::new(self.domain.clone());
        let (block_bytes, _) = query
            .block_by_tx_hash(tx_hash.to_vec())
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .ok_or_else(|| Status::not_found("tx hash not found"))?;

        let block = MultiEraBlock::decode(&block_bytes)
            .map_err(|e| Status::internal(format!("failed to decode block: {e}")))?;

        let (_, tx) = dolos_core::tx_by_hash(&block, &tx_hash)
            .ok_or_else(|| Status::not_found("tx hash not found"))?;

        let native_bytes = tx.encode().into();

        let cursor = self
            .domain
            .state()
            .read_cursor()
            .map_err(|e| Status::internal(e.to_string()))?
            .as_ref()
            .map(|p| point_to_u5c(&self.domain, p));

        let mut response = u5c::query::ReadTxResponse {
            tx: Some(u5c::query::AnyChainTx {
                native_bytes,
                block_ref: Some(u5c::query::ChainPoint {
                    slot: block.slot(),
                    hash: block.hash().to_vec().into(),
                    height: block.header().number(),
                    timestamp: self
                        .domain
                        .get_slot_timestamp(block.slot())
                        .map(|s| s * 1000)
                        .unwrap_or(0),
                }),
                chain: Some(u5c::query::any_chain_tx::Chain::Cardano(
                    self.mapper.map_tx(&tx),
                )),
            }),
            ledger_tip: cursor,
        };

        if let Some(mask) = message.field_mask {
            response = apply_mask(response, mask.paths)
                .map_err(|e| Status::internal(format!("failed to apply field mask: {e}")))?;
        }

        Ok(Response::new(response))
    }

    async fn read_genesis(
        &self,
        request: Request<u5c::query::ReadGenesisRequest>,
    ) -> Result<Response<u5c::query::ReadGenesisResponse>, Status> {
        let message = request.into_inner();

        info!("received new grpc query - read_genesis");

        let genesis = self.domain.genesis();

        let mut response = u5c::query::ReadGenesisResponse {
            genesis: genesis.shelley_hash.to_vec().into(),
            caip2: caip2_from_genesis(&genesis)?,
            config: Some(u5c::query::read_genesis_response::Config::Cardano(
                map_cardano_genesis(&genesis)?,
            )),
        };

        if let Some(mask) = message.field_mask {
            response = apply_mask(response, mask.paths)
                .map_err(|e| Status::internal(format!("failed to apply field mask: {e}")))?;
        }

        Ok(Response::new(response))
    }

    async fn read_era_summary(
        &self,
        request: Request<u5c::query::ReadEraSummaryRequest>,
    ) -> Result<Response<u5c::query::ReadEraSummaryResponse>, Status> {
        let message = request.into_inner();

        info!("received new grpc query - read_era_summary");

        let chain_summary = dolos_cardano::load_era_summary::<D>(self.domain.state())
            .map_err(|e| Status::internal(format!("failed to load era summary: {e}")))?;

        let active_pparams = dolos_cardano::load_effective_pparams::<D>(self.domain.state())
            .map_err(|e| Status::internal(format!("failed to load current pparams: {e}")))?;
        let active_protocol = active_pparams.protocol_major_or_default();
        let active_params = self
            .mapper
            .map_pparams(dolos_cardano::utils::pparams_to_pallas(&active_pparams));

        let summaries = chain_summary
            .iter_all()
            .map(|era| map_era_summary(era, active_protocol, &active_params))
            .collect();

        let mut response = u5c::query::ReadEraSummaryResponse {
            summary: Some(u5c::query::read_era_summary_response::Summary::Cardano(
                u5c::cardano::EraSummaries { summaries },
            )),
        };

        if let Some(mask) = message.field_mask {
            response = apply_mask(response, mask.paths)
                .map_err(|e| Status::internal(format!("failed to apply field mask: {e}")))?;
        }

        Ok(Response::new(response))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dolos_testing::toy_domain::ToyDomain;
    use pallas::interop::utxorpc::v1beta::spec::query::query_service_server::QueryService;

    use super::*;

    #[test]
    fn maps_known_networks_to_caip2() {
        let mainnet = dolos_cardano::include::mainnet::load();
        let preprod = dolos_cardano::include::preprod::load();
        let preview = dolos_cardano::include::preview::load();

        assert_eq!(caip2_from_genesis(&mainnet).unwrap(), "cardano:mainnet");
        assert_eq!(caip2_from_genesis(&preprod).unwrap(), "cardano:preprod");
        assert_eq!(caip2_from_genesis(&preview).unwrap(), "cardano:preview");
    }

    #[test]
    fn falls_back_to_network_magic_for_unknown_caip2() {
        let mut genesis = dolos_cardano::include::preview::load();
        genesis.shelley.network_magic = Some(42);

        assert_eq!(caip2_from_genesis(&genesis).unwrap(), "cardano:42");
    }

    #[test]
    fn maps_representative_genesis_fields() {
        let genesis = dolos_cardano::include::preview::load();
        let mapped = map_cardano_genesis(&genesis).unwrap();
        let expected_anchor_hash =
            hex::decode(&genesis.conway.constitution.anchor.data_hash).unwrap();
        let expected_constitution_hash = hex::decode(
            genesis
                .conway
                .constitution
                .script
                .as_deref()
                .expect("preview genesis includes a constitution script"),
        )
        .unwrap();

        assert_eq!(mapped.network_magic, 2);
        assert_eq!(mapped.epoch_length, genesis.shelley.epoch_length.unwrap());
        assert_eq!(mapped.slot_length, genesis.shelley.slot_length.unwrap());
        assert_eq!(mapped.system_start, genesis.shelley.system_start.unwrap());
        assert!(mapped.protocol_params.is_some());
        assert!(mapped.execution_prices.is_some());
        assert!(mapped.cost_models.is_some());

        let constitution = mapped.constitution.expect("missing constitution");
        let anchor = constitution.anchor.expect("missing constitution anchor");

        assert_eq!(
            anchor.content_hash.as_ref(),
            expected_anchor_hash.as_slice()
        );
        assert_eq!(
            constitution.hash.as_ref(),
            expected_constitution_hash.as_slice()
        );
        assert_ne!(anchor.content_hash.as_ref(), constitution.hash.as_ref());
    }

    /// The Musashi node's own Dijkstra genesis, copied byte for byte, so a cost
    /// model read out of it is the one the chain is governed by.
    fn musashi_dijkstra() -> dolos_core::dijkstra::GenesisFile {
        let path = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("crates")
            .join("core")
            .join("test_data")
            .join("musashi")
            .join("dijkstra-genesis.json");

        dolos_core::dijkstra::from_file(path).unwrap()
    }

    /// MUST FIRE: the PlutusV4 cost model a Dijkstra genesis declares is
    /// reported, in the cost model message and in the map.
    ///
    /// The length and the one negative cost are the chain's own numbers, so a
    /// reply carrying some other vector fails here.
    #[test]
    fn a_dijkstra_genesis_reports_its_plutus_v4_cost_model() {
        let mut genesis = dolos_cardano::include::preview::load();
        genesis.dijkstra = Some(musashi_dijkstra());

        let (models, map) = map_cost_models(&genesis);

        let models = models.expect("no cost models");
        let map = map.expect("no cost model map");

        assert_eq!(models.plutus_v4.as_ref().map(|x| x.values.len()), Some(251));
        assert_eq!(map.plutus_v4.as_ref().map(|x| x.values.len()), Some(251));
        assert_eq!(models.plutus_v4.as_ref().unwrap().values[52], -900);
        assert_eq!(models.plutus_v3, map.plutus_v3);
    }

    /// MUST NOT FIRE: a genesis with no Dijkstra file reports no PlutusV4 cost
    /// model, and still reports the models it does have.
    #[test]
    fn a_genesis_without_the_dijkstra_file_reports_no_plutus_v4_cost_model() {
        let genesis = dolos_cardano::include::preview::load();

        let (models, map) = map_cost_models(&genesis);

        let models = models.expect("no cost models");

        assert_eq!(models.plutus_v4, None);
        assert!(models.plutus_v3.is_some());
        assert_eq!(map.expect("no cost model map").plutus_v4, None);
    }

    /// A sub transaction's hash is answered with the sub transaction's bytes,
    /// and its parent's hash with bytes that hash to the parent, both at
    /// the block's slot.
    #[tokio::test]
    async fn read_tx_answers_a_sub_transaction_by_its_own_hash() {
        let fixture = crate::tests::musashi::sub_transaction_block();
        let service = QueryServiceImpl::new(fixture.domain.clone());

        let mut answers = vec![];

        for hash in [fixture.sub, fixture.parent] {
            let request = u5c::query::ReadTxRequest {
                hash: hash.to_vec().into(),
                ..Default::default()
            };

            let tx = QueryService::read_tx(&service, Request::new(request))
                .await
                .unwrap()
                .into_inner()
                .tx
                .unwrap();

            answers.push((tx.native_bytes.to_vec(), tx.block_ref.map(|x| x.slot)));
        }

        let parent = pallas::ledger::traverse::MultiEraTx::decode_for_era(
            pallas::ledger::traverse::Era::Dijkstra,
            &answers[1].0,
        )
        .unwrap()
        .hash();

        assert_eq!(
            (answers[0].clone(), answers[1].1, parent),
            (
                (fixture.sub_bytes, Some(fixture.slot)),
                Some(fixture.slot),
                fixture.parent
            )
        );
    }

    #[tokio::test]
    async fn read_genesis_applies_field_mask() {
        let domain = ToyDomain::new_with_genesis(
            Arc::new(dolos_cardano::include::preview::load()),
            None,
            None,
        );
        let service = QueryServiceImpl::new(domain);
        let mut request = u5c::query::ReadGenesisRequest {
            field_mask: Some(Default::default()),
        };
        request.field_mask.as_mut().unwrap().paths = vec!["caip2".into()];

        let response = QueryService::read_genesis(&service, Request::new(request))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(response.caip2, "cardano:preview");
        assert!(response.genesis.is_empty());
        assert!(response.config.is_none());
    }

    #[tokio::test]
    async fn read_genesis_returns_hash_and_config() {
        let genesis = Arc::new(dolos_cardano::include::preprod::load());
        let expected_hash = genesis.shelley_hash.to_vec();
        let domain = ToyDomain::new_with_genesis(genesis, None, None);
        let service = QueryServiceImpl::new(domain);

        let response = QueryService::read_genesis(
            &service,
            Request::new(u5c::query::ReadGenesisRequest { field_mask: None }),
        )
        .await
        .unwrap()
        .into_inner();

        assert_eq!(response.genesis.as_ref(), expected_hash.as_slice());
        assert_eq!(response.caip2, "cardano:preprod");

        match response.config {
            Some(u5c::query::read_genesis_response::Config::Cardano(cardano)) => {
                assert_eq!(cardano.network_magic, 1);
                assert!(cardano.protocol_params.unwrap().max_value_size > 0);
            }
            _ => panic!("missing cardano genesis config"),
        }
    }

    #[test]
    fn maps_protocols_to_era_names() {
        assert_eq!(protocol_to_era_name(0), "byron");
        assert_eq!(protocol_to_era_name(2), "shelley");
        assert_eq!(protocol_to_era_name(3), "allegra");
        assert_eq!(protocol_to_era_name(4), "mary");
        assert_eq!(protocol_to_era_name(6), "alonzo");
        assert_eq!(protocol_to_era_name(8), "babbage");
        assert_eq!(protocol_to_era_name(10), "conway");
        assert_eq!(protocol_to_era_name(42), "unknown");
    }

    #[tokio::test]
    async fn read_era_summary_returns_active_era_params_only() {
        let domain = ToyDomain::new_with_genesis(
            Arc::new(dolos_cardano::include::preview::load()),
            None,
            None,
        );
        let service = QueryServiceImpl::new(domain);

        let response = QueryService::read_era_summary(
            &service,
            Request::new(u5c::query::ReadEraSummaryRequest { field_mask: None }),
        )
        .await
        .unwrap()
        .into_inner();

        let summaries = match response.summary {
            Some(u5c::query::read_era_summary_response::Summary::Cardano(cardano)) => {
                cardano.summaries
            }
            _ => panic!("missing cardano era summaries"),
        };

        assert!(!summaries.is_empty());

        let active_with_params = summaries
            .iter()
            .filter(|x| x.protocol_params.is_some())
            .count();
        assert_eq!(active_with_params, 1);

        let active = summaries
            .iter()
            .find(|x| x.protocol_params.is_some())
            .expect("expected active era with protocol params");

        assert_eq!(active.name, "alonzo");
        assert!(active.start.is_some());
    }

    #[tokio::test]
    async fn read_era_summary_returns_boundary_time_in_milliseconds() {
        let domain = ToyDomain::new_with_genesis(
            Arc::new(dolos_cardano::include::preview::load()),
            None,
            None,
        );
        let service = QueryServiceImpl::new(domain);

        let response = QueryService::read_era_summary(
            &service,
            Request::new(u5c::query::ReadEraSummaryRequest { field_mask: None }),
        )
        .await
        .unwrap()
        .into_inner();

        let summaries = match response.summary {
            Some(u5c::query::read_era_summary_response::Summary::Cardano(cardano)) => {
                cardano.summaries
            }
            _ => panic!("missing cardano era summaries"),
        };

        let first = summaries
            .first()
            .expect("expected at least one era summary");
        let start = first.start.as_ref().expect("expected era start");

        assert_eq!(start.slot, 0);
        assert_eq!(start.time % 1000, 0);
        assert!(start.time >= 1_666_656_000_000);
    }

    #[tokio::test]
    async fn read_era_summary_applies_field_mask() {
        let domain = ToyDomain::new_with_genesis(
            Arc::new(dolos_cardano::include::preview::load()),
            None,
            None,
        );
        let service = QueryServiceImpl::new(domain);
        let mut request = u5c::query::ReadEraSummaryRequest {
            field_mask: Some(Default::default()),
        };
        request.field_mask.as_mut().unwrap().paths = vec!["cardano.summaries".into()];

        let response = QueryService::read_era_summary(&service, Request::new(request))
            .await
            .unwrap()
            .into_inner();

        match response.summary {
            Some(u5c::query::read_era_summary_response::Summary::Cardano(cardano)) => {
                assert!(!cardano.summaries.is_empty());
            }
            _ => panic!("missing cardano era summaries"),
        }
    }
}

#[cfg(test)]
mod live_params_tests {
    use dolos_cardano::model::PParamValue as Val;
    use dolos_cardano::utils::float_to_rational;
    use dolos_cardano::PParamsSet;
    use dolos_testing::toy_domain::ToyDomain;
    use pallas::ledger::primitives::conway::{DRepVotingThresholds, PoolVotingThresholds};
    use pallas::ledger::primitives::{ExUnitPrices, ExUnits, RationalNumber};
    use serde_json::Value;

    use super::*;

    /// The protocol parameters the Musashi node answers for the chain it is
    /// running, read with the cli.
    fn node() -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("crates/core/test_data/musashi/protocol-parameters.json");

        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    fn whole(node: &Value, key: &str) -> u64 {
        node.get(key)
            .unwrap_or_else(|| panic!("the node parameters name no {key}"))
            .as_u64()
            .unwrap_or_else(|| panic!("{key} is not a whole number"))
    }

    fn decimal(node: &Value, key: &str) -> f64 {
        node.get(key)
            .unwrap_or_else(|| panic!("the node parameters name no {key}"))
            .as_f64()
            .unwrap_or_else(|| panic!("{key} is not a number"))
    }

    fn nested(node: &Value, key: &str, inner: &str) -> Value {
        node.get(key)
            .unwrap_or_else(|| panic!("the node parameters name no {key}"))
            .get(inner)
            .unwrap_or_else(|| panic!("{key} names no {inner}"))
            .clone()
    }

    fn nested_whole(node: &Value, key: &str, inner: &str) -> u64 {
        nested(node, key, inner)
            .as_u64()
            .unwrap_or_else(|| panic!("{key}.{inner} is not a whole number"))
    }

    fn nested_ratio(node: &Value, key: &str, inner: &str) -> RationalNumber {
        let value = nested(node, key, inner)
            .as_f64()
            .unwrap_or_else(|| panic!("{key}.{inner} is not a number"));

        float_to_rational(value as f32)
    }

    fn ratio(node: &Value, key: &str) -> RationalNumber {
        float_to_rational(decimal(node, key) as f32)
    }

    fn model(node: &Value, language: &str) -> Vec<i64> {
        nested(node, "costModels", language)
            .as_array()
            .unwrap_or_else(|| panic!("the {language} cost model is not a list"))
            .iter()
            .map(|x| {
                x.as_i64()
                    .unwrap_or_else(|| panic!("a {language} cost is not a whole number"))
            })
            .collect()
    }

    fn ex_units(node: &Value, key: &str) -> ExUnits {
        ExUnits {
            mem: nested_whole(node, key, "memory"),
            steps: nested_whole(node, key, "steps"),
        }
    }

    /// The parameter set the ledger holds for the chain the node is running,
    /// with every value taken from the node's own answer.
    fn live_set(node: &Value) -> PParamsSet {
        PParamsSet::default()
            .with(Val::MinFeeA(whole(node, "txFeePerByte")))
            .with(Val::MinFeeB(whole(node, "txFeeFixed")))
            .with(Val::MaxBlockBodySize(whole(node, "maxBlockBodySize")))
            .with(Val::MaxTransactionSize(whole(node, "maxTxSize")))
            .with(Val::MaxBlockHeaderSize(whole(node, "maxBlockHeaderSize")))
            .with(Val::KeyDeposit(whole(node, "stakeAddressDeposit")))
            .with(Val::PoolDeposit(whole(node, "stakePoolDeposit")))
            .with(Val::DesiredNumberOfStakePools(
                whole(node, "stakePoolTargetNum") as u32,
            ))
            .with(Val::ProtocolVersion((
                nested_whole(node, "protocolVersion", "major"),
                nested_whole(node, "protocolVersion", "minor"),
            )))
            .with(Val::MinPoolCost(whole(node, "minPoolCost")))
            .with(Val::ExpansionRate(ratio(node, "monetaryExpansion")))
            .with(Val::TreasuryGrowthRate(ratio(node, "treasuryCut")))
            .with(Val::MaximumEpoch(whole(node, "poolRetireMaxEpoch")))
            .with(Val::PoolPledgeInfluence(ratio(node, "poolPledgeInfluence")))
            .with(Val::AdaPerUtxoByte(whole(node, "utxoCostPerByte")))
            .with(Val::ExecutionCosts(ExUnitPrices {
                mem_price: nested_ratio(node, "executionUnitPrices", "priceMemory"),
                step_price: nested_ratio(node, "executionUnitPrices", "priceSteps"),
            }))
            .with(Val::MaxTxExUnits(ex_units(node, "maxTxExecutionUnits")))
            .with(Val::MaxBlockExUnits(ex_units(
                node,
                "maxBlockExecutionUnits",
            )))
            .with(Val::MaxValueSize(whole(node, "maxValueSize") as u32))
            .with(Val::CollateralPercentage(
                whole(node, "collateralPercentage") as u32,
            ))
            .with(Val::MaxCollateralInputs(
                whole(node, "maxCollateralInputs") as u32
            ))
            .with(Val::PoolVotingThresholds(PoolVotingThresholds {
                motion_no_confidence: nested_ratio(
                    node,
                    "poolVotingThresholds",
                    "motionNoConfidence",
                ),
                committee_normal: nested_ratio(node, "poolVotingThresholds", "committeeNormal"),
                committee_no_confidence: nested_ratio(
                    node,
                    "poolVotingThresholds",
                    "committeeNoConfidence",
                ),
                hard_fork_initiation: nested_ratio(
                    node,
                    "poolVotingThresholds",
                    "hardForkInitiation",
                ),
                security_voting_threshold: nested_ratio(
                    node,
                    "poolVotingThresholds",
                    "ppSecurityGroup",
                ),
            }))
            .with(Val::DrepVotingThresholds(DRepVotingThresholds {
                motion_no_confidence: nested_ratio(
                    node,
                    "dRepVotingThresholds",
                    "motionNoConfidence",
                ),
                committee_normal: nested_ratio(node, "dRepVotingThresholds", "committeeNormal"),
                committee_no_confidence: nested_ratio(
                    node,
                    "dRepVotingThresholds",
                    "committeeNoConfidence",
                ),
                update_constitution: nested_ratio(
                    node,
                    "dRepVotingThresholds",
                    "updateToConstitution",
                ),
                hard_fork_initiation: nested_ratio(
                    node,
                    "dRepVotingThresholds",
                    "hardForkInitiation",
                ),
                pp_network_group: nested_ratio(node, "dRepVotingThresholds", "ppNetworkGroup"),
                pp_economic_group: nested_ratio(node, "dRepVotingThresholds", "ppEconomicGroup"),
                pp_technical_group: nested_ratio(node, "dRepVotingThresholds", "ppTechnicalGroup"),
                pp_governance_group: nested_ratio(node, "dRepVotingThresholds", "ppGovGroup"),
                treasury_withdrawal: nested_ratio(
                    node,
                    "dRepVotingThresholds",
                    "treasuryWithdrawal",
                ),
            }))
            .with(Val::MinCommitteeSize(whole(node, "committeeMinSize")))
            .with(Val::CommitteeTermLimit(whole(
                node,
                "committeeMaxTermLength",
            )))
            .with(Val::GovernanceActionValidityPeriod(whole(
                node,
                "govActionLifetime",
            )))
            .with(Val::GovernanceActionDeposit(whole(
                node,
                "govActionDeposit",
            )))
            .with(Val::DrepDeposit(whole(node, "dRepDeposit")))
            .with(Val::DrepInactivityPeriod(whole(node, "dRepActivity")))
            .with(Val::MinFeeRefScriptCostPerByte(ratio(
                node,
                "minFeeRefScriptCostPerByte",
            )))
            .with(Val::CostModelsPlutusV1(model(node, "PlutusV1")))
            .with(Val::CostModelsPlutusV2(model(node, "PlutusV2")))
            .with(Val::CostModelsPlutusV3(model(node, "PlutusV3")))
            // The Conway cost model type names three languages, so the chain
            // carries the PlutusV4 vector under key 3 of the wildcard map.
            .with(Val::CostModelsUnknown(std::collections::BTreeMap::from([
                (
                    dolos_cardano::pallas_extras::PLUTUS_V4_COST_MODEL_KEY,
                    model(node, "PlutusV4"),
                ),
            ])))
    }

    fn serve(set: &PParamsSet) -> u5c::cardano::PParams {
        let mapper = interop::Mapper::new(ToyDomain::new(None, None));

        map_live_params(&mapper, set).unwrap()
    }

    fn show_ratio(x: &RationalNumber) -> String {
        format!("{}/{}", x.numerator, x.denominator)
    }

    fn show_served_ratio(x: &Option<u5c::cardano::RationalNumber>) -> String {
        match x {
            Some(x) => format!("{}/{}", x.numerator, x.denominator),
            None => "no value".to_string(),
        }
    }

    fn show_served_number(x: &Option<u5c::cardano::BigInt>) -> String {
        match x.as_ref().and_then(|x| x.big_int.as_ref()) {
            Some(u5c::cardano::big_int::BigInt::Int(v)) => v.to_string(),
            Some(_) => "outside the int64 range".to_string(),
            None => "no value".to_string(),
        }
    }

    fn show_served_units(x: &Option<u5c::cardano::ExUnits>) -> String {
        match x {
            Some(x) => format!("{} memory, {} steps", x.memory, x.steps),
            None => "no value".to_string(),
        }
    }

    fn show_units(x: &ExUnits) -> String {
        format!("{} memory, {} steps", x.mem, x.steps)
    }

    fn show_served_thresholds(x: &Option<u5c::cardano::VotingThresholds>) -> String {
        match x {
            Some(x) => x
                .thresholds
                .iter()
                .map(|r| format!("{}/{}", r.numerator, r.denominator))
                .collect::<Vec<_>>()
                .join(" "),
            None => "no value".to_string(),
        }
    }

    fn show_thresholds(x: &[RationalNumber]) -> String {
        x.iter().map(show_ratio).collect::<Vec<_>>().join(" ")
    }

    fn show_served_model(x: &Option<u5c::cardano::CostModel>) -> String {
        match x {
            Some(x) => format!(
                "{} entries starting {:?} ending {:?}",
                x.values.len(),
                x.values.first(),
                x.values.last()
            ),
            None => "no value".to_string(),
        }
    }

    fn show_model(x: &[i64]) -> String {
        format!(
            "{} entries starting {:?} ending {:?}",
            x.len(),
            x.first(),
            x.last()
        )
    }

    /// Every served parameter, rendered so a mismatch names the field and both
    /// values. The PlutusV4 cost model the node also reports is left out, since
    /// no path puts it in the parameter set this reads.
    fn served_fields(p: &u5c::cardano::PParams) -> Vec<(&'static str, String)> {
        let models = p.cost_models.clone().unwrap_or_default();

        vec![
            (
                "coins_per_utxo_byte",
                show_served_number(&p.coins_per_utxo_byte),
            ),
            ("max_tx_size", p.max_tx_size.to_string()),
            (
                "min_fee_coefficient",
                show_served_number(&p.min_fee_coefficient),
            ),
            ("min_fee_constant", show_served_number(&p.min_fee_constant)),
            ("max_block_body_size", p.max_block_body_size.to_string()),
            ("max_block_header_size", p.max_block_header_size.to_string()),
            (
                "stake_key_deposit",
                show_served_number(&p.stake_key_deposit),
            ),
            ("pool_deposit", show_served_number(&p.pool_deposit)),
            (
                "pool_retirement_epoch_bound",
                p.pool_retirement_epoch_bound.to_string(),
            ),
            (
                "desired_number_of_pools",
                p.desired_number_of_pools.to_string(),
            ),
            ("pool_influence", show_served_ratio(&p.pool_influence)),
            (
                "monetary_expansion",
                show_served_ratio(&p.monetary_expansion),
            ),
            (
                "treasury_expansion",
                show_served_ratio(&p.treasury_expansion),
            ),
            ("min_pool_cost", show_served_number(&p.min_pool_cost)),
            (
                "protocol_version",
                p.protocol_version
                    .as_ref()
                    .map(|v| format!("{}.{}", v.major, v.minor))
                    .unwrap_or_else(|| "no value".to_string()),
            ),
            ("max_value_size", p.max_value_size.to_string()),
            ("collateral_percentage", p.collateral_percentage.to_string()),
            ("max_collateral_inputs", p.max_collateral_inputs.to_string()),
            (
                "cost_models.plutus_v1",
                show_served_model(&models.plutus_v1),
            ),
            (
                "cost_models.plutus_v2",
                show_served_model(&models.plutus_v2),
            ),
            (
                "cost_models.plutus_v3",
                show_served_model(&models.plutus_v3),
            ),
            (
                "cost_models.plutus_v4",
                show_served_model(&models.plutus_v4),
            ),
            (
                "prices.memory",
                show_served_ratio(&p.prices.as_ref().and_then(|x| x.memory.clone())),
            ),
            (
                "prices.steps",
                show_served_ratio(&p.prices.as_ref().and_then(|x| x.steps.clone())),
            ),
            (
                "max_execution_units_per_transaction",
                show_served_units(&p.max_execution_units_per_transaction),
            ),
            (
                "max_execution_units_per_block",
                show_served_units(&p.max_execution_units_per_block),
            ),
            (
                "min_fee_script_ref_cost_per_byte",
                show_served_ratio(&p.min_fee_script_ref_cost_per_byte),
            ),
            (
                "pool_voting_thresholds",
                show_served_thresholds(&p.pool_voting_thresholds),
            ),
            (
                "drep_voting_thresholds",
                show_served_thresholds(&p.drep_voting_thresholds),
            ),
            ("min_committee_size", p.min_committee_size.to_string()),
            ("committee_term_limit", p.committee_term_limit.to_string()),
            (
                "governance_action_validity_period",
                p.governance_action_validity_period.to_string(),
            ),
            (
                "governance_action_deposit",
                show_served_number(&p.governance_action_deposit),
            ),
            ("drep_deposit", show_served_number(&p.drep_deposit)),
            (
                "drep_inactivity_period",
                p.drep_inactivity_period.to_string(),
            ),
        ]
    }

    /// The same fields, rendered from the node's own answer.
    fn node_fields(node: &Value) -> Vec<(&'static str, String)> {
        let pool = [
            "motionNoConfidence",
            "committeeNormal",
            "committeeNoConfidence",
            "hardForkInitiation",
            "ppSecurityGroup",
        ]
        .map(|k| nested_ratio(node, "poolVotingThresholds", k));

        let drep = [
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
        ]
        .map(|k| nested_ratio(node, "dRepVotingThresholds", k));

        vec![
            (
                "coins_per_utxo_byte",
                whole(node, "utxoCostPerByte").to_string(),
            ),
            ("max_tx_size", whole(node, "maxTxSize").to_string()),
            (
                "min_fee_coefficient",
                whole(node, "txFeePerByte").to_string(),
            ),
            ("min_fee_constant", whole(node, "txFeeFixed").to_string()),
            (
                "max_block_body_size",
                whole(node, "maxBlockBodySize").to_string(),
            ),
            (
                "max_block_header_size",
                whole(node, "maxBlockHeaderSize").to_string(),
            ),
            (
                "stake_key_deposit",
                whole(node, "stakeAddressDeposit").to_string(),
            ),
            ("pool_deposit", whole(node, "stakePoolDeposit").to_string()),
            (
                "pool_retirement_epoch_bound",
                whole(node, "poolRetireMaxEpoch").to_string(),
            ),
            (
                "desired_number_of_pools",
                whole(node, "stakePoolTargetNum").to_string(),
            ),
            (
                "pool_influence",
                show_ratio(&ratio(node, "poolPledgeInfluence")),
            ),
            (
                "monetary_expansion",
                show_ratio(&ratio(node, "monetaryExpansion")),
            ),
            (
                "treasury_expansion",
                show_ratio(&ratio(node, "treasuryCut")),
            ),
            ("min_pool_cost", whole(node, "minPoolCost").to_string()),
            (
                "protocol_version",
                format!(
                    "{}.{}",
                    nested_whole(node, "protocolVersion", "major"),
                    nested_whole(node, "protocolVersion", "minor")
                ),
            ),
            ("max_value_size", whole(node, "maxValueSize").to_string()),
            (
                "collateral_percentage",
                whole(node, "collateralPercentage").to_string(),
            ),
            (
                "max_collateral_inputs",
                whole(node, "maxCollateralInputs").to_string(),
            ),
            (
                "cost_models.plutus_v1",
                show_model(&model(node, "PlutusV1")),
            ),
            (
                "cost_models.plutus_v2",
                show_model(&model(node, "PlutusV2")),
            ),
            (
                "cost_models.plutus_v3",
                show_model(&model(node, "PlutusV3")),
            ),
            (
                "cost_models.plutus_v4",
                show_model(&model(node, "PlutusV4")),
            ),
            (
                "prices.memory",
                show_ratio(&nested_ratio(node, "executionUnitPrices", "priceMemory")),
            ),
            (
                "prices.steps",
                show_ratio(&nested_ratio(node, "executionUnitPrices", "priceSteps")),
            ),
            (
                "max_execution_units_per_transaction",
                show_units(&ex_units(node, "maxTxExecutionUnits")),
            ),
            (
                "max_execution_units_per_block",
                show_units(&ex_units(node, "maxBlockExecutionUnits")),
            ),
            (
                "min_fee_script_ref_cost_per_byte",
                show_ratio(&ratio(node, "minFeeRefScriptCostPerByte")),
            ),
            ("pool_voting_thresholds", show_thresholds(&pool)),
            ("drep_voting_thresholds", show_thresholds(&drep)),
            (
                "min_committee_size",
                whole(node, "committeeMinSize").to_string(),
            ),
            (
                "committee_term_limit",
                whole(node, "committeeMaxTermLength").to_string(),
            ),
            (
                "governance_action_validity_period",
                whole(node, "govActionLifetime").to_string(),
            ),
            (
                "governance_action_deposit",
                whole(node, "govActionDeposit").to_string(),
            ),
            ("drep_deposit", whole(node, "dRepDeposit").to_string()),
            (
                "drep_inactivity_period",
                whole(node, "dRepActivity").to_string(),
            ),
        ]
    }

    #[test]
    fn the_served_pool_retirement_epoch_bound_is_the_node_value() {
        let node = node();

        assert_eq!(whole(&node, "poolRetireMaxEpoch"), 18);
        assert_eq!(serve(&live_set(&node)).pool_retirement_epoch_bound, 18);
    }

    #[test]
    fn every_served_parameter_is_the_node_value() {
        let node = node();
        let served = serve(&live_set(&node));

        assert_eq!(served_fields(&served), node_fields(&node));
    }

    #[test]
    fn the_served_cost_models_are_the_node_vectors() {
        let node = node();
        let served = serve(&live_set(&node));
        let models = served.cost_models.unwrap();

        assert_eq!(models.plutus_v1.unwrap().values, model(&node, "PlutusV1"));
        assert_eq!(models.plutus_v2.unwrap().values, model(&node, "PlutusV2"));
        assert_eq!(models.plutus_v3.unwrap().values, model(&node, "PlutusV3"));
        assert_eq!(models.plutus_v4.unwrap().values, model(&node, "PlutusV4"));
    }

    /// The must-not case. A set that names no PlutusV4 vector has to answer
    /// none, or every chain would read as one that priced PlutusV4.
    #[test]
    fn a_set_with_no_plutus_v4_model_serves_none() {
        let node = node();
        let mut set = live_set(&node);
        set.clear(dolos_cardano::model::PParamKind::CostModelsUnknown);

        let models = serve(&set).cost_models.unwrap_or_default();

        assert!(models.plutus_v4.is_none());
    }

    #[test]
    fn a_set_with_no_retirement_bound_is_refused() {
        let node = node();
        let mut set = live_set(&node);
        set.clear(dolos_cardano::model::PParamKind::MaximumEpoch);

        let mapper = interop::Mapper::new(ToyDomain::new(None, None));
        let error = map_live_params(&mapper, &set).unwrap_err();

        assert!(
            error.to_string().contains("MaximumEpoch"),
            "the refusal names no parameter: {error}"
        );
    }

    #[test]
    fn the_retirement_bound_and_the_plutus_v4_model_are_the_fields_the_era_mapping_leaves_behind() {
        let node = node();
        let set = live_set(&node);

        let mapper = interop::Mapper::new(ToyDomain::new(None, None));
        let era_only = mapper.map_pparams(dolos_cardano::utils::pparams_to_pallas(&set));
        let served = map_live_params(&mapper, &set).unwrap();

        let differing: Vec<&'static str> = served_fields(&served)
            .into_iter()
            .zip(served_fields(&era_only))
            .filter(|((_, a), (_, b))| a != b)
            .map(|((name, _), _)| name)
            .collect();

        assert_eq!(
            differing,
            vec!["pool_retirement_epoch_bound", "cost_models.plutus_v4"]
        );
    }
}
