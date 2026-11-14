//! The Dijkstra ledger state a Dolos store holds.

use std::collections::BTreeMap;

use dolos_core::{ChainError, Domain, EntityKey, StateStore as _};
use pallas::{
    codec::minicbor,
    crypto::hash::Hash,
    ledger::primitives::{
        conway::{DRep, GovActionId},
        Coin, Epoch, StakeCredential,
    },
};
use pallas_validate::utils::{
    dijkstra_state::{
        DijkstraAccount, DijkstraCommitteeAuth, DijkstraDRep, DijkstraGovRoots,
        DijkstraLedgerState, DijkstraPool, DijkstraProposal, DijkstraProposalAction,
    },
    ValidationError,
};

use crate::model::{
    drep_to_entity_key, AccountState, CommitteeAuthorization, DRepState, FixedNamespace as _,
    GovState, PParamKind, PoolState, ProposalAction, ProposalState,
};

/// The parameters whose change the stake pools vote on.
const SECURITY_GROUP: [PParamKind; 31] = [
    PParamKind::MinFeeA,
    PParamKind::MinFeeB,
    PParamKind::MaxBlockBodySize,
    PParamKind::MaxTransactionSize,
    PParamKind::MaxBlockHeaderSize,
    PParamKind::AdaPerUtxoByte,
    PParamKind::MaxBlockExUnits,
    PParamKind::MaxValueSize,
    PParamKind::GovernanceActionDeposit,
    PParamKind::MinFeeRefScriptCostPerByte,
    PParamKind::MaxRefScriptSizePerBlock,
    PParamKind::MaxRefScriptSizePerTx,
    PParamKind::RefScriptCostStride,
    PParamKind::RefScriptCostMultiplier,
    PParamKind::LeiosAnnouncementPeriodLength,
    PParamKind::LeiosVotePeriodLength,
    PParamKind::LeiosDiffusionPeriodLength,
    PParamKind::LeiosCommitteeSize,
    PParamKind::LeiosQuorumStakeThreshold,
    PParamKind::MaxEndorserBlockReferencesSize,
    PParamKind::MaxEndorserBlockTxsSize,
    PParamKind::MaxEndorserBlockExUnits,
    PParamKind::MaxRefScriptSizePerEndorserBlock,
    PParamKind::PerasMinCandidateBlockAge,
    PParamKind::PerasHealingFactor,
    PParamKind::PerasCertBoost,
    PParamKind::PerasTargetCommitteeSize,
    PParamKind::PerasBootstrapRound,
    PParamKind::PerasQuorumThresholdSafetyMargin,
    PParamKind::RefInputsCostPerMultiAssetPolicy,
    PParamKind::RefInputsCostPerDatumByte,
];

fn unavailable(reason: impl std::fmt::Display) -> ValidationError {
    ValidationError::DijkstraLedgerStateUnavailable(reason.to_string())
}

/// The Dijkstra ledger state of a Dolos store in its current epoch.
pub(crate) struct StoredLedger<'a, D: Domain> {
    state: &'a D::State,
    epoch: Epoch,
    treasury: Coin,
    gov: GovState,
}

impl<'a, D: Domain> StoredLedger<'a, D> {
    pub(crate) fn new(state: &'a D::State) -> Result<Self, ChainError> {
        let epoch = crate::load_epoch::<D>(state)?;
        Ok(StoredLedger {
            state,
            epoch: epoch.number,
            treasury: epoch.initial_pots.treasury,
            gov: crate::read_singleton::<D, GovState>(state)?.unwrap_or_default(),
        })
    }

    fn pools(&self) -> Result<Vec<PoolState>, ValidationError> {
        self.state
            .iter_entities_typed::<PoolState>(PoolState::NS, None)
            .map_err(unavailable)?
            .map(|record| record.map(|(_, pool)| pool).map_err(unavailable))
            .collect()
    }
}

/// The balance a registered account holds in `epoch`.
pub(crate) fn account_balance(
    account: &AccountState,
    credential: &StakeCredential,
    epoch: Epoch,
) -> Result<Coin, ValidationError> {
    if account.credential != *credential {
        return Err(unavailable("stored credential does not match account key"));
    }
    if !account.stake.is_at_epoch(epoch) {
        return Err(unavailable("account snapshot does not match current epoch"));
    }
    let stake = account
        .stake
        .live()
        .ok_or_else(|| unavailable("registered account has no live balance"))?;
    stake
        .rewards_sum
        .checked_sub(stake.withdrawals_sum)
        .ok_or_else(|| unavailable("recorded withdrawals exceed rewards"))
}

/// The registered pool a stored pool is, by the VRF key hashes of its live and
/// scheduled parameters.
pub(crate) fn registered_pool(pool: &PoolState) -> Option<DijkstraPool> {
    let live = pool.snapshot.live().filter(|x| !x.is_retired)?;
    Some(DijkstraPool {
        vrf: live.params.vrf_keyhash,
        future_vrf: pool.snapshot.next().map(|x| x.params.vrf_keyhash),
    })
}

/// What later proposals and votes read of a stored governance action.
pub(crate) fn proposal_action(
    action: &ProposalAction,
) -> Result<DijkstraProposalAction, ValidationError> {
    Ok(match action {
        ProposalAction::ParamChange(set) => DijkstraProposalAction::ParameterChange {
            security_relevant: set.iter().any(|x| SECURITY_GROUP.contains(&x.kind())),
        },
        ProposalAction::HardFork(version) => {
            DijkstraProposalAction::HardForkInitiation { version: *version }
        }
        ProposalAction::TreasuryWithdrawal(_) => DijkstraProposalAction::TreasuryWithdrawals,
        ProposalAction::Other => return Err(unavailable("proposal stored without its action")),
        ProposalAction::NoConfidence => DijkstraProposalAction::NoConfidence,
        ProposalAction::UpdateCommittee { to_add, .. } => DijkstraProposalAction::UpdateCommittee {
            added: to_add.iter().map(|(member, _)| member.clone()).collect(),
        },
        ProposalAction::NewConstitution { .. } => DijkstraProposalAction::NewConstitution,
        ProposalAction::Info => DijkstraProposalAction::Information,
    })
}

impl<D: Domain> DijkstraLedgerState for StoredLedger<'_, D> {
    fn epoch(&self) -> Epoch {
        self.epoch
    }

    fn treasury(&self) -> Result<Coin, ValidationError> {
        Ok(self.treasury)
    }

    fn account(
        &self,
        credential: &StakeCredential,
    ) -> Result<Option<DijkstraAccount>, ValidationError> {
        let key = EntityKey::from(minicbor::to_vec(credential).map_err(unavailable)?);
        let account = self
            .state
            .read_entity_typed::<AccountState>(AccountState::NS, &key)
            .map_err(unavailable)?;
        match account.filter(AccountState::is_registered) {
            Some(account) => Ok(Some(DijkstraAccount {
                balance: account_balance(&account, credential, self.epoch)?,
                deposit: account
                    .deposit
                    .ok_or_else(|| unavailable("account stored without its deposit"))?,
            })),
            None => Ok(None),
        }
    }

    fn pool(&self, operator: &Hash<28>) -> Result<Option<DijkstraPool>, ValidationError> {
        let pool = self
            .state
            .read_entity_typed::<PoolState>(PoolState::NS, &EntityKey::from(operator.as_slice()))
            .map_err(unavailable)?;
        Ok(pool.as_ref().and_then(registered_pool))
    }

    fn vrf_count(&self, vrf: &Hash<32>) -> Result<u64, ValidationError> {
        Ok(self
            .pools()?
            .iter()
            .filter_map(registered_pool)
            .filter(|pool| pool.vrf == *vrf || pool.future_vrf == Some(*vrf))
            .count() as u64)
    }

    fn drep(&self, credential: &StakeCredential) -> Result<Option<DijkstraDRep>, ValidationError> {
        let identifier = match credential {
            StakeCredential::AddrKeyhash(hash) => DRep::Key(*hash),
            StakeCredential::ScriptHash(hash) => DRep::Script(*hash),
        };
        let drep = self
            .state
            .read_entity_typed::<DRepState>(DRepState::NS, &drep_to_entity_key(&identifier))
            .map_err(unavailable)?;
        Ok(drep
            .filter(|x| x.registered_at.is_some() && !x.is_unregistered())
            .map(|x| DijkstraDRep { deposit: x.deposit }))
    }

    fn committee(&self) -> Result<Option<BTreeMap<StakeCredential, Epoch>>, ValidationError> {
        Ok(self.gov.committee.as_ref().map(|x| x.members.clone()))
    }

    fn committee_auths(
        &self,
    ) -> Result<BTreeMap<StakeCredential, DijkstraCommitteeAuth>, ValidationError> {
        Ok(self
            .gov
            .committee_auths
            .iter()
            .filter_map(|(cold, history)| {
                let auth = match &history.last()?.1 {
                    CommitteeAuthorization::HotCredential(hot) => {
                        DijkstraCommitteeAuth::Hot(hot.clone())
                    }
                    CommitteeAuthorization::Resigned(_) => DijkstraCommitteeAuth::Resigned,
                };
                Some((cold.clone(), auth))
            })
            .collect())
    }

    fn proposals(&self) -> Result<Vec<(GovActionId, DijkstraProposal)>, ValidationError> {
        let mut proposals = Vec::new();
        let stored = self
            .state
            .iter_entities_typed::<ProposalState>(ProposalState::NS, None)
            .map_err(unavailable)?;
        for record in stored {
            let (_, proposal) = record.map_err(unavailable)?;
            if !proposal.is_unresolved_at_close(self.epoch) {
                continue;
            }
            let expires_after = proposal
                .max_epoch
                .ok_or_else(|| unavailable("proposal stored without its expiry"))?;
            let action = proposal_action(&proposal.action)?;
            proposals.push((
                proposal.gov_action_id(),
                DijkstraProposal {
                    action,
                    expires_after,
                },
            ));
        }
        Ok(proposals)
    }

    fn roots(&self) -> Result<DijkstraGovRoots, ValidationError> {
        let roots = &self.gov.prev_gov_action_ids;
        Ok(DijkstraGovRoots {
            parameter_change: roots.pparam_update.clone(),
            hard_fork: roots.hard_fork.clone(),
            committee: roots.committee.clone(),
            constitution: roots.constitution.clone(),
        })
    }

    fn guardrail_script(&self) -> Result<Option<Hash<28>>, ValidationError> {
        Ok(self
            .gov
            .constitution
            .as_ref()
            .and_then(|x| x.guardrail_script))
    }
}
