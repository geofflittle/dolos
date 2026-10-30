use crate::model::FixedNamespace;
use std::borrow::Cow;

use dolos_core::{
    ChainError, ChainPoint, Domain, EraCbor, Genesis, MempoolAwareUtxoStore, MempoolStore,
    MempoolTx, StateStore, UtxoMap,
};

use pallas::ledger::{
    primitives::{NetworkId, TransactionInput},
    traverse::{Era, MultiEraInput, MultiEraOutput, MultiEraTx},
};
use pallas_validate::phase2::{script_context::SlotConfig, tx::TxEvalResult, Builtin};
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

    let utxos_matches = required_utxos(&tx, utxos)?;
    let pallas_utxos = phase_one_utxos(&utxos_matches)?;

    let mut cert_state = certificate_state(&tx, utxos)?;
    pallas_validate::phase1::validate_tx(&tx, 0, &env, &pallas_utxos, &mut cert_state)?;

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

/// The outputs the inputs of a transaction name, from the state and the mempool.
pub fn required_utxos<D: Domain>(
    tx: &MultiEraTx<'_>,
    utxos: &MempoolAwareUtxoStore<D>,
) -> Result<UtxoMap, ChainError> {
    Ok(utxos.get_utxos(tx.requires().iter().map(From::from).collect())?)
}

/// The phase one view of resolved outputs.
pub fn phase_one_utxos(matches: &UtxoMap) -> Result<pallas_validate::utils::UTxOs<'_>, ChainError> {
    let mut pallas_utxos = pallas_validate::utils::UTxOs::new();

    for (txoref, eracbor) in matches.iter() {
        let tx_in = TransactionInput {
            transaction_id: txoref.0,
            index: txoref.1.into(),
        };

        let input = MultiEraInput::AlonzoCompatible(<Box<Cow<'_, TransactionInput>>>::from(
            Cow::Owned(tx_in),
        ));

        let output = MultiEraOutput::try_from(eracbor.as_ref())?;

        pallas_utxos.insert(input, output);
    }

    Ok(pallas_utxos)
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

/// [`estimate_tx`] of a Dijkstra transaction, with the builtins each redeemer's evaluation called in order of first call.
pub fn estimate_tx_calls<D: Domain>(
    cbor: &[u8],
    utxos: &MempoolAwareUtxoStore<D>,
) -> Result<Vec<(TxEvalResult, Vec<Builtin>)>, ChainError> {
    let active = crate::load_effective_pparams::<D>(utxos.state())?;
    let tx = decode_submission(submission_era(active.ensure_protocol_version()?)?, cbor)?;
    let pparams = validation_parameters(&active)?;
    estimate_decoded::<D>(&tx, utxos, &pparams)
}

/// The minimum fee of a Dijkstra transaction under the active parameters, with the redeemers of its top level and sub transactions priced together, none outside Dijkstra.
pub fn min_fee<D: Domain>(
    cbor: &[u8],
    utxos: &MempoolAwareUtxoStore<D>,
) -> Result<Option<u64>, ChainError> {
    let active = crate::load_effective_pparams::<D>(utxos.state())?;
    let tx = decode_submission(submission_era(active.ensure_protocol_version()?)?, cbor)?;
    let (MultiEraProtocolParameters::Dijkstra(pp), MultiEraTx::Dijkstra(dijkstra)) =
        (validation_parameters(&active)?, &tx)
    else {
        return Ok(None);
    };
    let matches = required_utxos(&tx, utxos)?;
    let resolved = phase_one_utxos(&matches)?;
    Ok(Some(pallas_validate::phase1::dijkstra::dijkstra_min_fee(
        dijkstra, &resolved, &pp,
    )?))
}

fn evaluate_decoded<D: Domain>(
    tx: &MultiEraTx<'_>,
    utxos: &MempoolAwareUtxoStore<D>,
    pparams: &MultiEraProtocolParameters,
    estimate: bool,
) -> Result<pallas_validate::phase2::EvalReport, ChainError> {
    if estimate && tx.era() == Era::Dijkstra {
        let estimates = estimate_decoded::<D>(tx, utxos, pparams)?;
        return Ok(estimates.into_iter().map(|(result, _)| result).collect());
    }
    if !requests_scripts(tx) {
        return Ok(vec![]);
    }
    let (utxos, slot_config) = evaluation_inputs::<D>(tx, utxos)?;
    pallas_validate::phase2::evaluate_tx(tx, pparams, &utxos, &slot_config)
        .map_err(evaluation_error)
}

fn estimate_decoded<D: Domain>(
    tx: &MultiEraTx<'_>,
    utxos: &MempoolAwareUtxoStore<D>,
    pparams: &MultiEraProtocolParameters,
) -> Result<Vec<(TxEvalResult, Vec<Builtin>)>, ChainError> {
    if !requests_scripts(tx) {
        return Ok(vec![]);
    }
    let (utxos, slot_config) = evaluation_inputs::<D>(tx, utxos)?;
    pallas_validate::phase2::estimate_tx_calls(tx, pparams, &utxos, &slot_config)
        .map_err(evaluation_error)
}

/// The resolved inputs and the slot configuration a phase two evaluation reads.
fn evaluation_inputs<D: Domain>(
    tx: &MultiEraTx<'_>,
    utxos: &MempoolAwareUtxoStore<D>,
) -> Result<(pallas_validate::utils::UtxoMap, SlotConfig), ChainError> {
    use dolos_core::TxoRef;

    let eras = crate::eras::load_era_summary::<D>(utxos.state())?;

    // `eras` keeps slot_length/timestamp in seconds; the Plutus ScriptContext
    // needs POSIXTime in milliseconds. The conversion lives in the helper.
    let slot_config = eras.to_pallas_slot_config();

    let utxos: pallas_validate::utils::UtxoMap = required_utxos(tx, utxos)?
        .into_iter()
        .map(|(TxoRef(a, b), eracbor)| {
            let era = eracbor.era().try_into()?;

            Ok::<_, ChainError>((
                pallas_validate::utils::TxoRef::from((a, b)),
                pallas_validate::utils::EraCbor::from((era, eracbor.cbor().into())),
            ))
        })
        .collect::<Result<_, _>>()?;

    Ok((utxos, slot_config))
}

/// The chain error for a phase two evaluation pallas could not complete.
fn evaluation_error(error: pallas_validate::phase2::error::Error) -> ChainError {
    match error {
        pallas_validate::phase2::error::Error::DijkstraUnsupported(unsupported) => {
            ChainError::Phase2EvaluationUnsupported(unsupported)
        }
        error => ChainError::Phase2EvaluationError(error.to_string()),
    }
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
        let unconfirmed = unconfirmed_txs(utxos);
        let unconfirmed = unconfirmed
            .iter()
            .map(|entry| MultiEraTx::try_from(&entry.payload))
            .collect::<Result<Vec<_>, _>>()?;
        let pending = pending_accounts(&unconfirmed)?;
        if let Some(withdrawals) = &native.transaction_body.withdrawals {
            let epoch = crate::load_epoch::<D>(utxos.state())?.number;
            for raw in withdrawals.keys() {
                let credential = reward_account_credential(raw)?;
                let key = pallas::codec::minicbor::to_vec(&credential).unwrap();
                let account = utxos
                    .state()
                    .read_entity_typed::<AccountState>(AccountState::NS, &key.into())?;
                let balance = withdrawal_balance(
                    raw,
                    &credential,
                    account,
                    epoch,
                    pending.get(&credential).copied(),
                )?;
                state.dijkstra_account_balances.insert(credential, balance);
            }
        }
        for cert in native.transaction_body.certificates.iter().flatten() {
            if let Certificate::Reg(credential, _) = cert {
                let registered = match pending.get(credential).and_then(|x| x.registration) {
                    Some(pending) => pending == PendingRegistration::Registered,
                    None => {
                        let key = pallas::codec::minicbor::to_vec(credential).unwrap();
                        utxos
                            .state()
                            .read_entity_typed::<AccountState>(AccountState::NS, &key.into())?
                            .is_some_and(|a| a.is_registered())
                    }
                };
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
    }
    Ok(state)
}

/// The registration the last certificate naming a credential leaves it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingRegistration {
    Registered,
    Deregistered,
}

/// What unconfirmed transactions do to one account, the registration their last certificate naming it leaves and the coin they deposit net of withdrawals after that certificate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PendingAccount {
    registration: Option<PendingRegistration>,
    net_deposits: i128,
}

/// What `txs` do to each account they name, applied in order.
fn pending_accounts(
    txs: &[MultiEraTx<'_>],
) -> Result<
    std::collections::HashMap<pallas::ledger::primitives::StakeCredential, PendingAccount>,
    ChainError,
> {
    use crate::pallas_extras::{
        cert_as_stake_deregistration, cert_as_stake_registration, tx_direct_deposits,
    };
    let mut pending: std::collections::HashMap<_, PendingAccount> =
        std::collections::HashMap::new();
    for tx in txs {
        crate::pallas_extras::for_each_applied_tx(tx, |tx| {
            // The ledger applies withdrawals, then certificates, then direct deposits.
            for (raw, amount) in tx.withdrawals().collect::<Vec<_>>() {
                let account = pending.entry(reward_account_credential(raw)?).or_default();
                account.net_deposits -= i128::from(amount);
            }
            for cert in tx.certs() {
                if let Some(credential) = cert_as_stake_registration(&cert) {
                    let registration = Some(PendingRegistration::Registered);
                    pending.insert(
                        credential,
                        PendingAccount {
                            registration,
                            net_deposits: 0,
                        },
                    );
                }
                if let Some(credential) = cert_as_stake_deregistration(&cert) {
                    let registration = Some(PendingRegistration::Deregistered);
                    pending.insert(
                        credential,
                        PendingAccount {
                            registration,
                            net_deposits: 0,
                        },
                    );
                }
            }
            for (raw, amount) in tx_direct_deposits(tx) {
                let account = pending.entry(reward_account_credential(raw)?).or_default();
                account.net_deposits += i128::from(amount);
            }
            Ok::<_, ChainError>(())
        })?;
    }
    Ok(pending)
}

/// The balance a withdrawal from the account may draw, `None` when the account is not registered.
fn withdrawal_balance(
    raw: &[u8],
    credential: &pallas::ledger::primitives::StakeCredential,
    account: Option<crate::model::AccountState>,
    epoch: u64,
    pending: Option<PendingAccount>,
) -> Result<Option<u64>, ChainError> {
    let pending = pending.unwrap_or_default();
    let balance = match pending.registration {
        Some(PendingRegistration::Registered) => Some(0),
        Some(PendingRegistration::Deregistered) => None,
        None => synced_balance(raw, credential, account, epoch)?,
    };
    balance
        .map(|balance| {
            u64::try_from(i128::from(balance) + pending.net_deposits).map_err(|_| {
                ChainError::DijkstraWithdrawalConflict {
                    account: hex::encode(raw),
                }
            })
        })
        .transpose()
}

/// The balance the synced state holds for the account, `None` when the account is not registered.
fn synced_balance(
    raw: &[u8],
    credential: &pallas::ledger::primitives::StakeCredential,
    account: Option<crate::model::AccountState>,
    epoch: u64,
) -> Result<Option<u64>, ChainError> {
    let unavailable = |reason| {
        pallas_validate::utils::ValidationError::DijkstraAccountStateUnavailable(format!(
            "{}: {reason}",
            hex::encode(raw)
        ))
    };
    let balance = match account {
        None => None, // Absence in the synced namespace means unregistered.
        Some(account) => {
            if account.credential != *credential {
                return Err(unavailable("stored credential does not match account key").into());
            }
            if account.is_registered() {
                if !account.stake.is_at_epoch(epoch) {
                    return Err(unavailable("account snapshot does not match current epoch").into());
                }
                let stake = account
                    .stake
                    .live()
                    .ok_or_else(|| unavailable("registered account has no live balance"))?;
                Some(
                    stake
                        .rewards_sum
                        .checked_sub(stake.withdrawals_sum)
                        .ok_or_else(|| {
                            unavailable(
                                "recorded withdrawals exceed rewards; account state is inconsistent",
                            )
                        })?,
                )
            } else {
                None
            }
        }
    };
    Ok(balance)
}

/// The mempool transactions, inflight before pending, each once.
fn unconfirmed_txs<D: Domain>(utxos: &MempoolAwareUtxoStore<D>) -> Vec<MempoolTx> {
    let pending = utxos.pending();
    let inflight = utxos.mempool().peek_inflight();
    // Relay can move a transaction between these two reads. Count it once.
    let mut seen = std::collections::HashSet::new();
    inflight
        .into_iter()
        .chain(pending)
        .filter(|entry| seen.insert(entry.hash))
        .filter(|entry| {
            matches!(
                entry.stage,
                dolos_core::MempoolTxStage::Pending
                    | dolos_core::MempoolTxStage::Propagated
                    | dolos_core::MempoolTxStage::Acknowledged
            )
        })
        .collect()
}

fn reward_account_credential(
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
mod pending_tests {
    use super::*;
    use pallas::codec::minicbor;
    use pallas::crypto::hash::Hash;
    use pallas::ledger::primitives::{dijkstra, StakeCredential};

    fn credential(byte: u8) -> StakeCredential {
        StakeCredential::AddrKeyhash(Hash::new([byte; 28]))
    }

    /// A Dijkstra transaction in mempool form whose body carries these certificates.
    fn tx_with(certs: Vec<dijkstra::Certificate>) -> Vec<u8> {
        tx_moving(certs, &[], &[])
    }

    /// A Dijkstra transaction in mempool form with these certificates, withdrawals and direct deposits, each account named by its credential byte.
    fn tx_moving(
        certs: Vec<dijkstra::Certificate>,
        withdrawals: &[(u8, u64)],
        deposits: &[(u8, u64)],
    ) -> Vec<u8> {
        let present = [
            !certs.is_empty(),
            !withdrawals.is_empty(),
            !deposits.is_empty(),
        ];
        let mut e = minicbor::Encoder::new(Vec::new());
        e.array(3).unwrap();
        e.map(3 + present.iter().filter(|x| **x).count() as u64)
            .unwrap();
        e.u8(0).unwrap().array(1).unwrap().array(2).unwrap();
        e.bytes(&[0; 32]).unwrap().u8(0).unwrap();
        e.u8(1).unwrap().array(0).unwrap();
        e.u8(2).unwrap().u64(200_000).unwrap();
        if !certs.is_empty() {
            e.u8(4).unwrap();
            e.encode(dijkstra::NonEmptySet::from_vec(certs).unwrap())
                .unwrap();
        }
        for (key, accounts) in [(5, withdrawals), (25, deposits)] {
            if !accounts.is_empty() {
                e.u8(key).unwrap().map(accounts.len() as u64).unwrap();
                for (byte, amount) in accounts {
                    e.bytes(&reward_account(*byte))
                        .unwrap()
                        .u64(*amount)
                        .unwrap();
                }
            }
        }
        e.map(0).unwrap().null().unwrap();
        e.into_writer()
    }

    /// The testnet reward account of `credential(byte)`.
    fn reward_account(byte: u8) -> Vec<u8> {
        [vec![0xe0], vec![byte; 28]].concat()
    }

    fn reg(byte: u8) -> dijkstra::Certificate {
        dijkstra::Certificate::Reg(credential(byte), 2_000_000)
    }

    fn unreg(byte: u8) -> dijkstra::Certificate {
        dijkstra::Certificate::UnReg(credential(byte), 2_000_000)
    }

    fn accounts(txs: &[Vec<u8>]) -> std::collections::HashMap<StakeCredential, PendingAccount> {
        let txs: Vec<_> = txs
            .iter()
            .map(|cbor| MultiEraTx::decode_for_era(Era::Dijkstra, cbor).unwrap())
            .collect();
        pending_accounts(&txs).unwrap()
    }

    /// The registration `txs` leave the account of `credential(byte)` in.
    fn registration(txs: &[Vec<u8>], byte: u8) -> Option<PendingRegistration> {
        accounts(txs).get(&credential(byte))?.registration
    }

    /// The pending view of an account a certificate leaves in `registration` with no coin moved after it.
    fn certified(registration: PendingRegistration) -> Option<PendingAccount> {
        Some(PendingAccount {
            registration: Some(registration),
            net_deposits: 0,
        })
    }

    #[test]
    fn a_credential_no_pending_certificate_names_has_no_pending_registration() {
        let pending = accounts(&[tx_with(vec![reg(1)]), tx_with(vec![unreg(2)])]);
        assert_eq!(pending.get(&credential(3)), None);
        assert_eq!(pending.len(), 2);
    }

    #[test]
    fn the_last_pending_certificate_decides_the_registration() {
        let txs = [tx_with(vec![reg(1), unreg(2)]), tx_with(vec![unreg(1)])];
        assert_eq!(
            (registration(&txs, 1), registration(&txs, 2)),
            (
                Some(PendingRegistration::Deregistered),
                Some(PendingRegistration::Deregistered)
            )
        );

        let txs = [tx_with(vec![unreg(1)]), tx_with(vec![reg(1)])];
        assert_eq!(registration(&txs, 1), Some(PendingRegistration::Registered));
    }

    /// A synced account registered in epoch 7 with 5 lovelace of rewards and none withdrawn.
    fn registered(byte: u8) -> crate::model::AccountState {
        use crate::model::{EpochValue, Stake};
        crate::model::AccountState {
            registered_at: Some(0),
            stake: EpochValue::with_live(
                7,
                Stake {
                    rewards_sum: 5,
                    ..Default::default()
                },
            ),
            pool: EpochValue::new(7),
            drep: EpochValue::new(7),
            vote_delegated_at: None,
            deregistered_at: None,
            credential: credential(byte),
            retired_pool: None,
        }
    }

    #[test]
    fn a_synced_account_no_pending_certificate_names_keeps_its_balance() {
        let account = Some(registered(1));
        let balance = withdrawal_balance(&[0xe0], &credential(1), account, 7, None).unwrap();
        assert_eq!(balance, Some(5));
    }

    #[test]
    fn a_synced_account_a_pending_certificate_deregisters_has_no_balance() {
        let account = Some(registered(1));
        let pending = certified(PendingRegistration::Deregistered);
        let balance = withdrawal_balance(&[0xe0], &credential(1), account, 7, pending).unwrap();
        assert_eq!(balance, None);
    }

    #[test]
    fn an_account_neither_synced_nor_pending_has_no_balance() {
        let balance = withdrawal_balance(&[0xe0], &credential(1), None, 7, None).unwrap();
        assert_eq!(balance, None);
    }

    #[test]
    fn an_account_a_pending_certificate_deregisters_has_no_balance() {
        let pending = certified(PendingRegistration::Deregistered);
        let balance = withdrawal_balance(&[0xe0], &credential(1), None, 7, pending).unwrap();
        assert_eq!(balance, None);
    }

    #[test]
    fn an_account_a_pending_certificate_registers_has_a_zero_balance() {
        let pending = certified(PendingRegistration::Registered);
        let balance = withdrawal_balance(&[0xe0], &credential(1), None, 7, pending).unwrap();
        assert_eq!(balance, Some(0));
    }

    /// The balance a withdrawal from the synced account of `credential(1)` may draw once `txs` apply.
    fn balance_after(txs: &[Vec<u8>]) -> Result<Option<u64>, ChainError> {
        let pending = accounts(txs);
        withdrawal_balance(
            &reward_account(1),
            &credential(1),
            Some(registered(1)),
            7,
            pending.get(&credential(1)).copied(),
        )
    }

    #[test]
    fn pending_transactions_naming_other_accounts_keep_the_synced_balance() {
        let txs = [tx_moving(vec![], &[(2, 3)], &[(3, 4)])];
        assert_eq!(balance_after(&txs).unwrap(), Some(5));
    }

    #[test]
    fn a_pending_withdrawal_lowers_the_balance() {
        let txs = [tx_moving(vec![], &[(1, 2)], &[])];
        assert_eq!(balance_after(&txs).unwrap(), Some(3));
    }

    #[test]
    fn a_pending_direct_deposit_raises_the_balance() {
        let txs = [tx_moving(vec![], &[], &[(1, 4)])];
        assert_eq!(balance_after(&txs).unwrap(), Some(9));
    }

    #[test]
    fn a_pending_registration_keeps_only_what_moves_after_it() {
        let txs = [
            tx_moving(vec![unreg(1)], &[(1, 5)], &[]),
            tx_moving(vec![reg(1)], &[], &[(1, 3)]),
        ];
        assert_eq!(balance_after(&txs).unwrap(), Some(3));
    }

    #[test]
    fn pending_withdrawals_above_the_balance_conflict() {
        let txs = [tx_moving(vec![], &[(1, 6)], &[])];
        let error = balance_after(&txs).unwrap_err();
        assert!(
            matches!(error, ChainError::DijkstraWithdrawalConflict { .. }),
            "{error:?}"
        );
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

#[cfg(test)]
mod evaluation_tests {
    use super::*;
    use pallas_validate::phase2::error::{Error, Unsupported};

    #[test]
    fn an_evaluation_error_keeps_its_message() {
        let error = evaluation_error(Error::NoRedeemers);
        assert!(
            matches!(&error, ChainError::Phase2EvaluationError(x) if x == "can't eval without redeemers"),
            "{error:?}"
        );
    }

    #[test]
    fn an_unsupported_evaluation_keeps_its_label() {
        let unsupported = Unsupported("V3 builtin outside audited protocol-12 subset");
        let error = evaluation_error(Error::DijkstraUnsupported(unsupported));
        assert!(
            matches!(&error, ChainError::Phase2EvaluationUnsupported(x) if *x == unsupported),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            "couldn't evaluate phase-2 script: unsupported native Dijkstra evaluation: \
             V3 builtin outside audited protocol-12 subset"
        );
    }
}
