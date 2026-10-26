use crate::model::FixedNamespace;
use std::borrow::Cow;

use dolos_core::{
    ChainError, ChainPoint, Domain, EraCbor, Genesis, MempoolAwareUtxoStore, MempoolStore,
    MempoolTx, StateStore,
};

use pallas::ledger::{
    primitives::{NetworkId, TransactionInput},
    traverse::{Era, MultiEraInput, MultiEraOutput, MultiEraTx},
};
use pallas_validate::utils::{
    CertState, DijkstraProtParams, DijkstraRegistrationState, MultiEraProtocolParameters,
};
use tracing::debug;

pub fn validate_tx<D: Domain>(
    cbor: &[u8],
    utxos: &MempoolAwareUtxoStore<D>,
    tip: Option<ChainPoint>,
    genesis: &Genesis,
) -> Result<MempoolTx, ChainError> {
    let active = crate::load_effective_pparams::<D>(utxos.state())?;
    let era = submission_era(active.ensure_protocol_version()?)?;
    let tx = decode_submission(era, cbor)?;
    let hash = tx.hash();
    let pparams = validation_parameters(&active)?;

    let network_id = match genesis.shelley.network_id.as_ref() {
        Some(network) => match network.as_str() {
            "Mainnet" => Some(NetworkId::Mainnet.into()),
            "Testnet" => Some(NetworkId::Testnet.into()),
            _ => None,
        },
        None => None,
    }
    .ok_or_else(|| ChainError::GenesisFieldMissing("network id".into()))?;

    let env = pallas_validate::utils::Environment {
        prot_params: pparams,
        prot_magic: genesis
            .shelley
            .network_magic
            .ok_or_else(|| ChainError::GenesisFieldMissing("network magic".into()))?,
        block_slot: tip.ok_or(ChainError::NoActiveEpoch)?.slot(),
        network_id,
        acnt: if era == Era::Dijkstra {
            None
        } else {
            Some(pallas_validate::utils::AccountState::default())
        },
    };

    let input_refs = tx.requires().iter().map(From::from).collect();

    let utxos_matches = utxos.get_utxos(input_refs)?;

    let mut pallas_utxos = pallas_validate::utils::UTxOs::new();

    for (txoref, eracbor) in utxos_matches.iter() {
        let tx_in = TransactionInput {
            transaction_id: txoref.0,
            index: txoref.1.into(),
        };

        let input = MultiEraInput::AlonzoCompatible(<Box<Cow<'_, TransactionInput>>>::from(
            Cow::Owned(tx_in),
        ));

        let eracbor = eracbor.as_ref();

        let output = MultiEraOutput::try_from(eracbor)?;

        pallas_utxos.insert(input, output);
    }

    let mut cert_state = certificate_state(&tx, utxos)?;
    pallas_validate::phase1::validate_tx(&tx, 0, &env, &pallas_utxos, &mut cert_state)?;
    if era == Era::Dijkstra {
        check_dijkstra_withdrawal_reservations(&cert_state, utxos)?;
    }

    let report = evaluate_decoded::<D>(&tx, utxos, &env.prot_params, false)?;

    for eval in report.iter() {
        if !eval.success {
            return Err(ChainError::Phase2ValidationRejected(eval.logs.clone()));
        }
    }

    debug!(
        phase1 = true,
        phase2 = true,
        redeemer_count = report.len(),
        "tx validated"
    );

    let era = u16::from(tx.era());
    let payload = EraCbor(era, cbor.into());

    let tx = MempoolTx::new(hash, payload, report);

    Ok(tx)
}

pub fn evaluate_tx<D: Domain>(
    cbor: &[u8],
    utxos: &MempoolAwareUtxoStore<D>,
) -> Result<pallas_validate::phase2::EvalReport, ChainError> {
    let active = crate::load_effective_pparams::<D>(utxos.state())?;
    let tx = decode_submission(submission_era(active.ensure_protocol_version()?)?, cbor)?;
    let pparams = validation_parameters(&active)?;
    evaluate_decoded::<D>(&tx, utxos, &pparams, false)
}

/// Estimate native script costs independently of signatures and declared budgets.
/// Earlier eras retain their existing phase-two evaluation behavior. Successful
/// per-script estimates may sum above the transaction limit; admission still
/// requires phase one and budget-enforcing phase two through `validate_tx`.
pub fn estimate_tx<D: Domain>(
    cbor: &[u8],
    utxos: &MempoolAwareUtxoStore<D>,
) -> Result<pallas_validate::phase2::EvalReport, ChainError> {
    let active = crate::load_effective_pparams::<D>(utxos.state())?;
    let tx = decode_submission(submission_era(active.ensure_protocol_version()?)?, cbor)?;
    let pparams = validation_parameters(&active)?;
    evaluate_decoded::<D>(&tx, utxos, &pparams, true)
}

fn evaluate_decoded<D: Domain>(
    tx: &MultiEraTx<'_>,
    utxos: &MempoolAwareUtxoStore<D>,
    pparams: &MultiEraProtocolParameters,
    estimate: bool,
) -> Result<pallas_validate::phase2::EvalReport, ChainError> {
    if !requests_scripts(tx) {
        return Ok(vec![]);
    }

    use dolos_core::TxoRef;

    let eras = crate::eras::load_era_summary::<D>(utxos.state())?;

    // `eras` keeps slot_length/timestamp in seconds; the Plutus ScriptContext
    // needs POSIXTime in milliseconds. The conversion lives in the helper.
    let slot_config = eras.to_pallas_slot_config();

    let input_refs = tx.requires().iter().map(From::from).collect();

    let utxos: pallas_validate::utils::UtxoMap = utxos
        .get_utxos(input_refs)?
        .into_iter()
        .map(|(TxoRef(a, b), eracbor)| {
            let era = eracbor.era().try_into()?;

            Ok::<_, ChainError>((
                pallas_validate::utils::TxoRef::from((a, b)),
                pallas_validate::utils::EraCbor::from((era, eracbor.cbor().into())),
            ))
        })
        .collect::<Result<_, _>>()?;

    let report = if estimate && tx.era() == Era::Dijkstra {
        pallas_validate::phase2::estimate_tx(tx, pparams, &utxos, &slot_config)
    } else {
        pallas_validate::phase2::evaluate_tx(tx, pparams, &utxos, &slot_config)
    }
    .map_err(|e| ChainError::Phase2EvaluationError(e.to_string()))?;

    Ok(report)
}

/// Whether phase two has anything to run: a redeemer at the top level or in a
/// sub-transaction. Pallas's Dijkstra evaluator refuses every batch, so a
/// key-only batch must not reach it.
fn requests_scripts(tx: &MultiEraTx<'_>) -> bool {
    !tx.redeemers().is_empty()
        || tx.as_dijkstra().is_some_and(|tx| {
            tx.transaction_body
                .sub_transactions
                .iter()
                .flat_map(|subs| subs.iter())
                .any(|sub| sub.transaction_witness_set.redeemer.is_some())
        })
}

/// Submission has no era tag in its signed envelope. Only known protocol
/// versions select an era; unknown future versions must not inherit this
/// subset.
pub fn submission_era(protocol: (u64, u64)) -> Result<Era, ChainError> {
    match protocol {
        (0..=1, _) => Ok(Era::Byron),
        (2, _) => Ok(Era::Shelley),
        (3, _) => Ok(Era::Allegra),
        (4, _) => Ok(Era::Mary),
        (5..=6, _) => Ok(Era::Alonzo),
        (7..=8, _) => Ok(Era::Babbage),
        (9..=11, _) => Ok(Era::Conway),
        (12, 0) => Ok(Era::Dijkstra),
        _ => Err(ChainError::InvalidConfig(format!(
            "unsupported submission protocol {}.{}",
            protocol.0, protocol.1
        ))),
    }
}

/// The protocol parameters of the active set in the era its protocol version
/// names.
pub fn validation_parameters(
    pp: &crate::model::PParamsSet,
) -> Result<MultiEraProtocolParameters, ChainError> {
    let protocol = pp.ensure_protocol_version()?;
    if submission_era(protocol)? != Era::Dijkstra {
        // Preserve the existing earlier-era parameter projection.
        return Ok(crate::utils::pparams_to_pallas(pp));
    }
    let missing = |name: &str| ChainError::PParamsNotFound(name.into());
    let u32_param = |x: u64, name: &str| {
        u32::try_from(x).map_err(|_| ChainError::InvalidConfig(format!("{name} exceeds u32")))
    };
    let u16_param = |x: u64, name: &str| {
        u16::try_from(x).map_err(|_| ChainError::InvalidConfig(format!("{name} exceeds u16")))
    };
    let cost_models = pp.cost_models_for_script_languages();
    let mut unknown = cost_models.unknown;
    let plutus_v4 = unknown.remove(&crate::pallas_extras::PLUTUS_V4_COST_MODEL_KEY);
    Ok(MultiEraProtocolParameters::Dijkstra(DijkstraProtParams {
        protocol_version: protocol,
        system_start: chrono::DateTime::from_timestamp(pp.ensure_system_start()? as i64, 0)
            .ok_or_else(|| ChainError::InvalidConfig("invalid system start".into()))?
            .fixed_offset(),
        epoch_length: pp.ensure_epoch_length()?,
        slot_length: pp.ensure_slot_length()?,
        minfee_a: u32_param(
            pp.min_fee_a().ok_or_else(|| missing("min fee a"))?,
            "min fee a",
        )?,
        minfee_b: u32_param(
            pp.min_fee_b().ok_or_else(|| missing("min fee b"))?,
            "min fee b",
        )?,
        max_transaction_size: u32_param(
            pp.max_transaction_size()
                .ok_or_else(|| missing("max transaction size"))?,
            "max transaction size",
        )?,
        ada_per_utxo_byte: pp
            .ada_per_utxo_byte()
            .ok_or_else(|| missing("ada per utxo byte"))?,
        max_value_size: pp
            .max_value_size()
            .ok_or_else(|| missing("max value size"))?,
        key_deposit: pp.key_deposit().ok_or_else(|| missing("key deposit"))?,
        max_block_body_size: u32_param(pp.max_block_body_size_or_default(), "max block body size")?,
        max_block_header_size: u32_param(
            pp.max_block_header_size_or_default(),
            "max block header size",
        )?,
        pool_deposit: pp.pool_deposit_or_default(),
        desired_number_of_stake_pools: pp.desired_number_of_stake_pools_or_default(),
        min_pool_cost: pp.min_pool_cost_or_default(),
        cost_models_for_script_languages: pallas::ledger::primitives::dijkstra::CostModels {
            plutus_v1: cost_models.plutus_v1,
            plutus_v2: cost_models.plutus_v2,
            plutus_v3: cost_models.plutus_v3,
            plutus_v4,
            unknown: unknown.into_iter().collect(),
        },
        execution_costs: pp
            .execution_costs()
            .ok_or_else(|| missing("execution costs"))?,
        max_tx_ex_units: pp
            .max_tx_ex_units()
            .ok_or_else(|| missing("max tx ex units"))?,
        max_block_ex_units: pp.max_block_ex_units_or_default(),
        collateral_percentage: pp
            .collateral_percentage()
            .ok_or_else(|| missing("collateral percentage"))?,
        max_collateral_inputs: pp
            .max_collateral_inputs()
            .ok_or_else(|| missing("max collateral inputs"))?,
        expansion_rate: pp.expansion_rate_or_default(),
        treasury_growth_rate: pp.treasury_growth_rate_or_default(),
        maximum_epoch: pp.maximum_epoch_or_default(),
        pool_pledge_influence: pp.pool_pledge_influence_or_default(),
        pool_voting_thresholds: pp.pool_voting_thresholds_or_default(),
        drep_voting_thresholds: pp.drep_voting_thresholds_or_default(),
        min_committee_size: pp.min_committee_size_or_default(),
        committee_term_limit: pp.committee_term_limit_or_default(),
        governance_action_validity_period: pp.governance_action_validity_period_or_default(),
        governance_action_deposit: pp.governance_action_deposit_or_default(),
        drep_deposit: pp.drep_deposit_or_default(),
        drep_inactivity_period: pp.drep_inactivity_period_or_default(),
        minfee_refscript_cost_per_byte: pp
            .min_fee_ref_script_cost_per_byte()
            .ok_or_else(|| missing("reference script cost"))?,
        max_ref_script_size_per_block: u32_param(
            pp.max_ref_script_size_per_block()
                .ok_or_else(|| missing("max reference script size per block"))?,
            "max reference script size per block",
        )?,
        max_ref_script_size_per_tx: u32_param(
            pp.max_ref_script_size_per_tx()
                .ok_or_else(|| missing("max reference script size"))?,
            "max reference script size",
        )?,
        ref_script_cost_stride: u32_param(
            pp.ref_script_cost_stride()
                .ok_or_else(|| missing("reference script cost stride"))?,
            "reference script cost stride",
        )?,
        ref_script_cost_multiplier: pp
            .ref_script_cost_multiplier()
            .ok_or_else(|| missing("reference script cost multiplier"))?,
        max_pledge_leverage: pp
            .max_pledge_leverage()
            .ok_or_else(|| missing("max pledge leverage"))?,
        min_pool_margin: pp
            .min_pool_margin()
            .ok_or_else(|| missing("min pool margin"))?,
        leios_announcement_period_length: u32_param(
            pp.leios_announcement_period_length()
                .ok_or_else(|| missing("leios announcement period length"))?,
            "leios announcement period length",
        )?,
        leios_vote_period_length: u32_param(
            pp.leios_vote_period_length()
                .ok_or_else(|| missing("leios vote period length"))?,
            "leios vote period length",
        )?,
        leios_diffusion_period_length: u32_param(
            pp.leios_diffusion_period_length()
                .ok_or_else(|| missing("leios diffusion period length"))?,
            "leios diffusion period length",
        )?,
        leios_committee_size: u16_param(
            pp.leios_committee_size()
                .ok_or_else(|| missing("leios committee size"))?,
            "leios committee size",
        )?,
        leios_quorum_stake_threshold: pp
            .leios_quorum_stake_threshold()
            .ok_or_else(|| missing("leios quorum stake threshold"))?,
        max_endorser_block_references_size: u32_param(
            pp.max_endorser_block_references_size()
                .ok_or_else(|| missing("max endorser block references size"))?,
            "max endorser block references size",
        )?,
        max_endorser_block_txs_size: u32_param(
            pp.max_endorser_block_txs_size()
                .ok_or_else(|| missing("max endorser block txs size"))?,
            "max endorser block txs size",
        )?,
        max_endorser_block_ex_units: pp
            .max_endorser_block_ex_units()
            .ok_or_else(|| missing("max endorser block ex units"))?,
        max_ref_script_size_per_endorser_block: u32_param(
            pp.max_ref_script_size_per_endorser_block()
                .ok_or_else(|| missing("max reference script size per endorser block"))?,
            "max reference script size per endorser block",
        )?,
        peras_min_candidate_block_age: u32_param(
            pp.peras_min_candidate_block_age()
                .ok_or_else(|| missing("peras min candidate block age"))?,
            "peras min candidate block age",
        )?,
        peras_healing_factor: pp
            .peras_healing_factor()
            .ok_or_else(|| missing("peras healing factor"))?,
        peras_cert_boost: u16_param(
            pp.peras_cert_boost()
                .ok_or_else(|| missing("peras cert boost"))?,
            "peras cert boost",
        )?,
        peras_target_committee_size: u16_param(
            pp.peras_target_committee_size()
                .ok_or_else(|| missing("peras target committee size"))?,
            "peras target committee size",
        )?,
        peras_bootstrap_round: pp
            .peras_bootstrap_round()
            .ok_or_else(|| missing("peras bootstrap round"))?
            .map(|x| u32_param(x, "peras bootstrap round"))
            .transpose()?,
        peras_quorum_threshold_safety_margin: pp
            .peras_quorum_threshold_safety_margin()
            .ok_or_else(|| missing("peras quorum threshold safety margin"))?,
        ref_inputs_cost_per_multi_asset_policy: pp
            .ref_inputs_cost_per_multi_asset_policy()
            .ok_or_else(|| missing("reference inputs cost per multi asset policy"))?,
        ref_inputs_cost_per_datum_byte: pp
            .ref_inputs_cost_per_datum_byte()
            .ok_or_else(|| missing("reference inputs cost per datum byte"))?,
    }))
}

fn certificate_state<D: Domain>(
    tx: &MultiEraTx<'_>,
    utxos: &MempoolAwareUtxoStore<D>,
) -> Result<CertState, ChainError> {
    use crate::model::AccountState;
    use pallas::ledger::primitives::dijkstra::Certificate;
    let mut state = CertState::default();
    if let Some(native) = tx.as_dijkstra() {
        if let Some(withdrawals) = &native.transaction_body.withdrawals {
            let epoch = crate::load_epoch::<D>(utxos.state())?.number;
            for raw in withdrawals.keys() {
                let credential = withdrawal_credential(raw)?;
                let key = pallas::codec::minicbor::to_vec(&credential).unwrap();
                let account = utxos
                    .state()
                    .read_entity_typed::<AccountState>(AccountState::NS, &key.into())?;
                let unavailable = |reason| {
                    pallas_validate::utils::ValidationError::DijkstraAccountStateUnavailable(
                        format!("{}: {reason}", hex::encode(raw.as_slice())),
                    )
                };
                let balance = match account {
                    None => None, // Absence in the synced namespace means unregistered.
                    Some(account) => {
                        if account.credential != credential {
                            return Err(unavailable(
                                "stored credential does not match account key",
                            )
                            .into());
                        }
                        if account.is_registered() {
                            if !account.stake.is_at_epoch(epoch) {
                                return Err(unavailable(
                                    "account snapshot does not match current epoch",
                                )
                                .into());
                            }
                            let stake = account.stake.live().ok_or_else(|| {
                                unavailable("registered account has no live balance")
                            })?;
                            Some(stake.rewards_sum.checked_sub(stake.withdrawals_sum).ok_or_else(|| {
                                unavailable("recorded withdrawals exceed rewards; account state is inconsistent")
                            })?)
                        } else {
                            None
                        }
                    }
                };
                state.dijkstra_account_balances.insert(credential, balance);
            }
        }
        for cert in native.transaction_body.certificates.iter().flatten() {
            if let Certificate::Reg(credential, _) = cert {
                // The synced account namespace is authoritative, including
                // absence. Only the transaction-local state is
                // changed by phase one.
                let key = pallas::codec::minicbor::to_vec(credential).unwrap();
                let account = utxos
                    .state()
                    .read_entity_typed::<AccountState>(AccountState::NS, &key.into())?;
                let registered = account.is_some_and(|a| a.is_registered());
                state.dijkstra_registrations.insert(
                    credential.clone(),
                    if registered {
                        DijkstraRegistrationState::Registered
                    } else {
                        DijkstraRegistrationState::Unregistered
                    },
                );
            }
        }
        // Pending accepted registrations also reserve their credentials. Do not
        // let two otherwise independent transactions register the same account.
        let mut pending = utxos.mempool().peek_pending();
        pending.extend(utxos.mempool().peek_inflight());
        for entry in pending {
            let pending_tx = MultiEraTx::try_from(&entry.payload)?;
            for cert in pending_tx.certs() {
                if let Some(credential) = crate::pallas_extras::cert_as_stake_registration(&cert) {
                    if let Some(value) = state.dijkstra_registrations.get_mut(&credential) {
                        *value = DijkstraRegistrationState::Registered;
                    }
                }
            }
        }
    }
    Ok(state)
}

fn withdrawal_credential(
    raw: &[u8],
) -> Result<pallas::ledger::primitives::StakeCredential, ChainError> {
    let pallas::ledger::addresses::Address::Stake(address) =
        pallas::ledger::addresses::Address::from_bytes(raw)?
    else {
        return Err(
            pallas_validate::utils::ValidationError::DijkstraInvalidWithdrawal(
                "expected a reward account address",
            )
            .into(),
        );
    };
    Ok(crate::pallas_extras::stake_address_to_cred(&address))
}

fn check_dijkstra_withdrawal_reservations<D: Domain>(
    state: &CertState,
    utxos: &MempoolAwareUtxoStore<D>,
) -> Result<(), ChainError> {
    if state.dijkstra_account_balances.is_empty() {
        return Ok(());
    }
    // Phase one has already checked ledger semantics (including V3 draining)
    // against the original balances, and deducted this candidate's withdrawals
    // from its provisional state. Reserve pending withdrawals only afterward:
    // using reduced balances in phase one could incorrectly authorize a partial
    // V3 withdrawal. No provisional balances are written to the state store.
    let mut remaining = state.dijkstra_account_balances.clone();
    let mut pending = utxos.mempool().peek_pending();
    pending.extend(utxos.mempool().peek_inflight());
    // Relay can move a transaction between these two reads. Count it once.
    let mut seen = std::collections::HashSet::new();
    for entry in pending {
        if !seen.insert(entry.hash) {
            continue;
        }
        if !matches!(
            entry.stage,
            dolos_core::MempoolTxStage::Pending
                | dolos_core::MempoolTxStage::Propagated
                | dolos_core::MempoolTxStage::Acknowledged
        ) {
            continue; // Confirmed withdrawals are already reflected in ledger state.
        }
        let tx = MultiEraTx::try_from(&entry.payload)?;
        for (raw, amount) in tx.withdrawals().collect::<Vec<_>>() {
            let credential = withdrawal_credential(raw)?;
            if let Some(Some(balance)) = remaining.get_mut(&credential) {
                *balance = balance.checked_sub(amount).ok_or_else(|| {
                    ChainError::DijkstraWithdrawalConflict {
                        account: hex::encode(raw),
                    }
                })?;
            }
        }
    }
    Ok(())
}

fn decode_submission(era: Era, cbor: &[u8]) -> Result<MultiEraTx<'_>, ChainError> {
    let mut decoder = pallas::codec::minicbor::Decoder::new(cbor);
    decoder.skip()?;
    if decoder.position() != cbor.len() {
        return Err(
            pallas::codec::minicbor::decode::Error::message("trailing transaction bytes").into(),
        );
    }
    if era == Era::Dijkstra {
        // The traversal decoder also accepts block-only envelopes. A submitted
        // payload must satisfy the mempool rule, since these exact bytes relay.
        let _: pallas::ledger::primitives::dijkstra::MempoolTransaction =
            pallas::codec::minicbor::decode(cbor)?;
    }
    Ok(MultiEraTx::decode_for_era(era, cbor)?)
}

#[cfg(test)]
mod batch_tests {
    use super::*;

    /// A key-only batch carrying one sub-transaction, in the mempool form it was
    /// submitted in. Musashi included it as tx
    /// 9582ee5f1c8f9bb023d687bf36ba22d694c435c01f87ef8958b8f6c81d02c942.
    const KEY_ONLY_BATCH: &str = "83a500d9010281825820c88a71fd0cfaa30e320c737ac064b0fe30de68e9e6d25d922d64488806b80e4401018182581d60a776e47e31d0da1fc98f734cbc036cf9c216aae1b191f35754369f371b0000000252d82085021a0002a9e517d901028183a200d9010281825820c88a71fd0cfaa30e320c737ac064b0fe30de68e9e6d25d922d64488806b80e4400018182581d6021f213394ce9e6ddec232aa9821f6ae9c144d0fdac2853219450a3cd1a003d0900a100d90102818258200f89ac07c0ead276c043938fa4941579cee1ac298d69bdd99c2fbe7e36426375584094c1f7af30f7bb4a233d23e06f8d97cddea6f49bd0462928a10a3b1756623976a5144438ae01f8b34c7d754e6712b22efa3891e9ad3ef211c1fc45be48f7630af612d9010281825820c88a71fd0cfaa30e320c737ac064b0fe30de68e9e6d25d922d64488806b80e4400a100d90102818258206a37d23da740e3246ccdc1ba100e13c5d24d0f7d773effff507c7cb0d647f01f58405c8ba9c49e97b93624183a6a10f909888f25a7764f50044bf78c1ed872febba4e39a526471f0f263fe88167ddaea9e9600d9f349bd3917367a5ff8df272c7206f6";

    #[test]
    fn a_key_only_batch_requests_no_scripts() {
        let cbor = hex::decode(KEY_ONLY_BATCH).unwrap();
        let tx = decode_submission(Era::Dijkstra, &cbor).unwrap();
        let native = tx.as_dijkstra().unwrap();
        assert!(native.transaction_body.sub_transactions.is_some());
        assert!(!requests_scripts(&tx));
    }
}

#[cfg(test)]
mod parameter_tests {
    use super::*;
    use crate::model::{PParamValue as P, PParamsSet};
    use pallas::ledger::primitives::{
        conway::{DRepVotingThresholds, PoolVotingThresholds},
        dijkstra::CostModels,
        ExUnitPrices, ExUnits, RationalNumber,
    };

    fn r(numerator: u64, denominator: u64) -> RationalNumber {
        RationalNumber {
            numerator,
            denominator,
        }
    }

    fn pool_thresholds() -> PoolVotingThresholds {
        PoolVotingThresholds {
            motion_no_confidence: r(1, 51),
            committee_normal: r(1, 52),
            committee_no_confidence: r(1, 53),
            hard_fork_initiation: r(1, 54),
            security_voting_threshold: r(1, 55),
        }
    }

    fn drep_thresholds() -> DRepVotingThresholds {
        DRepVotingThresholds {
            motion_no_confidence: r(1, 61),
            committee_normal: r(1, 62),
            committee_no_confidence: r(1, 63),
            update_constitution: r(1, 64),
            hard_fork_initiation: r(1, 65),
            pp_network_group: r(1, 66),
            pp_economic_group: r(1, 67),
            pp_technical_group: r(1, 68),
            pp_governance_group: r(1, 69),
            treasury_withdrawal: r(1, 70),
        }
    }

    fn prices() -> ExUnitPrices {
        ExUnitPrices {
            mem_price: r(577, 10000),
            step_price: r(721, 10000000),
        }
    }

    fn active() -> PParamsSet {
        let mut pp = PParamsSet::default();
        for value in [
            P::SystemStart(1788739200),
            P::EpochLength(21600),
            P::SlotLength(1),
            P::MinFeeA(44),
            P::MinFeeB(155381),
            P::MaxBlockBodySize(90112),
            P::MaxTransactionSize(16384),
            P::MaxBlockHeaderSize(1100),
            P::KeyDeposit(2000000),
            P::PoolDeposit(500000000),
            P::DesiredNumberOfStakePools(150),
            P::ProtocolVersion((12, 0)),
            P::MinPoolCost(170000000),
            P::ExpansionRate(r(3, 1000)),
            P::TreasuryGrowthRate(r(1, 5)),
            P::MaximumEpoch(18),
            P::PoolPledgeInfluence(r(3, 10)),
            P::AdaPerUtxoByte(4310),
            P::ExecutionCosts(prices()),
            P::MaxTxExUnits(ExUnits {
                mem: 14000000,
                steps: 10000000000,
            }),
            P::MaxBlockExUnits(ExUnits {
                mem: 62000000,
                steps: 20000000000,
            }),
            P::MaxValueSize(5000),
            P::CollateralPercentage(151),
            P::MaxCollateralInputs(3),
            P::PoolVotingThresholds(pool_thresholds()),
            P::DrepVotingThresholds(drep_thresholds()),
            P::MinCommitteeSize(7),
            P::CommitteeTermLimit(293),
            P::GovernanceActionValidityPeriod(120),
            P::GovernanceActionDeposit(100000000000),
            P::DrepDeposit(500000001),
            P::DrepInactivityPeriod(20),
            P::MinFeeRefScriptCostPerByte(r(15, 1)),
            P::CostModelsPlutusV1(vec![101, 102]),
            P::CostModelsPlutusV2(vec![201, 202]),
            P::CostModelsPlutusV3(vec![301, 302]),
            P::CostModelsUnknown(std::collections::BTreeMap::from([(
                crate::pallas_extras::PLUTUS_V4_COST_MODEL_KEY,
                vec![411, -412],
            )])),
            P::MaxRefScriptSizePerBlock(1048577),
            P::MaxRefScriptSizePerTx(204801),
            P::RefScriptCostStride(25601),
            P::RefScriptCostMultiplier(r(5, 4)),
            P::MaxPledgeLeverage(Some(r(39, 1))),
            P::MinPoolMargin(r(1, 50)),
            P::LeiosAnnouncementPeriodLength(1001),
            P::LeiosVotePeriodLength(4001),
            P::LeiosDiffusionPeriodLength(7001),
            P::LeiosCommitteeSize(901),
            P::LeiosQuorumStakeThreshold(r(4, 5)),
            P::MaxEndorserBlockReferencesSize(150000),
            P::MaxEndorserBlockTxsSize(1500000),
            P::MaxEndorserBlockExUnits(ExUnits {
                mem: 465000000,
                steps: 100000000001,
            }),
            P::MaxRefScriptSizePerEndorserBlock(6000000),
            P::PerasMinCandidateBlockAge(31),
            P::PerasHealingFactor(r(7, 2)),
            P::PerasCertBoost(13),
            P::PerasTargetCommitteeSize(401),
            P::PerasBootstrapRound(Some(17)),
            P::PerasQuorumThresholdSafetyMargin(r(1, 8)),
            P::RefInputsCostPerMultiAssetPolicy(1003),
            P::RefInputsCostPerDatumByte(7),
        ] {
            pp.set(value);
        }
        pp
    }

    #[test]
    fn every_dijkstra_parameter_comes_from_the_active_set() {
        let MultiEraProtocolParameters::Dijkstra(params) =
            validation_parameters(&active()).unwrap()
        else {
            panic!("a Dijkstra parameter set");
        };
        let expected = DijkstraProtParams {
            system_start: chrono::DateTime::from_timestamp(1788739200, 0)
                .unwrap()
                .fixed_offset(),
            epoch_length: 21600,
            slot_length: 1,
            minfee_a: 44,
            minfee_b: 155381,
            max_block_body_size: 90112,
            max_transaction_size: 16384,
            max_block_header_size: 1100,
            key_deposit: 2000000,
            pool_deposit: 500000000,
            desired_number_of_stake_pools: 150,
            protocol_version: (12, 0),
            min_pool_cost: 170000000,
            ada_per_utxo_byte: 4310,
            cost_models_for_script_languages: CostModels {
                plutus_v1: Some(vec![101, 102]),
                plutus_v2: Some(vec![201, 202]),
                plutus_v3: Some(vec![301, 302]),
                plutus_v4: Some(vec![411, -412]),
                unknown: Default::default(),
            },
            execution_costs: prices(),
            max_tx_ex_units: ExUnits {
                mem: 14000000,
                steps: 10000000000,
            },
            max_block_ex_units: ExUnits {
                mem: 62000000,
                steps: 20000000000,
            },
            max_value_size: 5000,
            collateral_percentage: 151,
            max_collateral_inputs: 3,
            expansion_rate: r(3, 1000),
            treasury_growth_rate: r(1, 5),
            maximum_epoch: 18,
            pool_pledge_influence: r(3, 10),
            pool_voting_thresholds: pool_thresholds(),
            drep_voting_thresholds: drep_thresholds(),
            min_committee_size: 7,
            committee_term_limit: 293,
            governance_action_validity_period: 120,
            governance_action_deposit: 100000000000,
            drep_deposit: 500000001,
            drep_inactivity_period: 20,
            minfee_refscript_cost_per_byte: r(15, 1),
            max_ref_script_size_per_block: 1048577,
            max_ref_script_size_per_tx: 204801,
            ref_script_cost_stride: 25601,
            ref_script_cost_multiplier: r(5, 4),
            max_pledge_leverage: Some(r(39, 1)),
            min_pool_margin: r(1, 50),
            leios_announcement_period_length: 1001,
            leios_vote_period_length: 4001,
            leios_diffusion_period_length: 7001,
            leios_committee_size: 901,
            leios_quorum_stake_threshold: r(4, 5),
            max_endorser_block_references_size: 150000,
            max_endorser_block_txs_size: 1500000,
            max_endorser_block_ex_units: ExUnits {
                mem: 465000000,
                steps: 100000000001,
            },
            max_ref_script_size_per_endorser_block: 6000000,
            peras_min_candidate_block_age: 31,
            peras_healing_factor: r(7, 2),
            peras_cert_boost: 13,
            peras_target_committee_size: 401,
            peras_bootstrap_round: Some(17),
            peras_quorum_threshold_safety_margin: r(1, 8),
            ref_inputs_cost_per_multi_asset_policy: 1003,
            ref_inputs_cost_per_datum_byte: 7,
        };
        assert_eq!(format!("{params:?}"), format!("{expected:?}"));
    }

    #[test]
    fn a_set_with_no_peras_bootstrap_round_leaves_it_absent() {
        let set = active().with(P::PerasBootstrapRound(None));
        let MultiEraProtocolParameters::Dijkstra(params) = validation_parameters(&set).unwrap()
        else {
            panic!("a Dijkstra parameter set");
        };
        assert_eq!(params.peras_bootstrap_round, None);
    }

    #[test]
    fn a_peras_cert_boost_beyond_u16_is_refused() {
        let set = active().with(P::PerasCertBoost(65536));
        let refused = validation_parameters(&set).unwrap_err();
        assert!(matches!(
            refused,
            ChainError::InvalidConfig(x) if x == "peras cert boost exceeds u16"
        ));
    }

    #[test]
    fn a_set_with_no_pledge_leverage_cap_leaves_it_absent() {
        let set = active().with(P::MaxPledgeLeverage(None));
        let MultiEraProtocolParameters::Dijkstra(params) = validation_parameters(&set).unwrap()
        else {
            panic!("a Dijkstra parameter set");
        };
        assert_eq!(params.max_pledge_leverage, None);
    }

    /// A set without one of the Dijkstra parameters is refused with that
    /// parameter named.
    #[test]
    fn a_set_missing_a_dijkstra_parameter_is_refused() {
        let mut set = active();
        set.clear(crate::model::PParamKind::MaxEndorserBlockTxsSize);
        let refused = validation_parameters(&set).unwrap_err();
        assert!(
            matches!(&refused, ChainError::PParamsNotFound(x) if x == "max endorser block txs size"),
            "{refused:?}"
        );
    }

    #[test]
    fn a_committee_size_beyond_u16_is_refused() {
        let set = active().with(P::LeiosCommitteeSize(65536));
        let refused = validation_parameters(&set).unwrap_err();
        assert!(matches!(
            refused,
            ChainError::InvalidConfig(x) if x == "leios committee size exceeds u16"
        ));
    }
}
