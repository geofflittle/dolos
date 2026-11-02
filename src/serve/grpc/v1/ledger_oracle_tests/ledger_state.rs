//! Writes the pools, DReps, committee and proposals the ledger ran a record from into Dolos state.

use std::collections::BTreeMap;

use dolos_cardano::{
    model::{
        drep_to_entity_key, Committee, CommitteeAuthorization, Constitution, DRepExpiry, DRepState,
        EpochValue, GovRoots, GovState, PParamsSet, PoolParams, PoolSnapshot, PoolState,
        ProposalAction, ProposalState,
    },
    FixedNamespace as _, SingletonEntity as _,
};
use dolos_core::{Domain as _, EntityKey, StateStore as _, StateWriter as _};
use ledger_oracle_judge::{
    accounts::Credential,
    state::{
        self, ActionId, Certs, CommitteeAuthorization as LedgerAuthorization, CredentialObject,
        Fraction, Gov, GovAction, RewardAccount,
    },
};
use pallas::ledger::{
    addresses::Network,
    primitives::{
        conway::{self, Anchor, GovActionId},
        dijkstra::BlsKey,
        RationalNumber, StakeCredential,
    },
};

use super::{params, seed::stake_credential};
use crate::adapters::DomainAdapter;

fn credential(c: CredentialObject) -> StakeCredential {
    stake_credential(&Credential::from(c))
}

fn ratio(f: Fraction) -> RationalNumber {
    RationalNumber {
        numerator: f.numerator,
        denominator: f.denominator,
    }
}

fn anchor(a: &state::Anchor) -> Anchor {
    Anchor {
        url: a.url.clone(),
        content_hash: a.data_hash.0.into(),
    }
}

fn action_id(id: ActionId) -> GovActionId {
    GovActionId {
        transaction_id: id.tx_id.0.into(),
        action_index: id.gov_action_ix,
    }
}

/// The reward address bytes of a credential on the network.
fn reward_address(credential: CredentialObject, network: Network) -> Vec<u8> {
    let (kind, hash) = match credential {
        CredentialObject::KeyHash(h) => (0xe0, h.0),
        CredentialObject::ScriptHash(h) => (0xf0, h.0),
    };
    [&[kind | network.value()][..], &hash[..]].concat()
}

/// The network a reward account names.
fn network(account: &RewardAccount) -> Network {
    match account.network.as_str() {
        "Mainnet" => Network::Mainnet,
        "Testnet" => Network::Testnet,
        other => panic!("STATE reward account network {other}"),
    }
}

fn pool_snapshot(params: PoolParams) -> PoolSnapshot {
    PoolSnapshot {
        is_retired: false,
        blocks_minted: 0,
        params,
        is_new: false,
    }
}

fn pool_params(pool: &state::StakePool, network: Network) -> PoolParams {
    PoolParams {
        vrf_keyhash: pool.sps_vrf.0.into(),
        pledge: pool.sps_pledge,
        cost: pool.sps_cost,
        margin: ratio(pool.sps_margin),
        reward_account: reward_address(pool.sps_account_id, network),
        pool_owners: pool.sps_owners.iter().map(|h| h.0.into()).collect(),
        relays: vec![],
        pool_metadata: None,
        bls_key: pool.sps_bls_key.as_ref().map(|k| BlsKey {
            bls_pubkey: k.bks_key.bls_pub_key.0.clone().into(),
            bls_possession_proof: k.bks_key.bls_possession_proof.0.clone().into(),
        }),
    }
}

fn future_params(future: &state::PoolParams) -> PoolParams {
    PoolParams {
        vrf_keyhash: future.vrf.0.into(),
        pledge: future.pledge,
        cost: future.cost,
        margin: ratio(future.margin),
        reward_account: reward_address(
            future.account_address.credential,
            network(&future.account_address),
        ),
        pool_owners: future.owners.iter().map(|h| h.0.into()).collect(),
        relays: vec![],
        pool_metadata: None,
        bls_key: None,
    }
}

fn pool_states(
    certs: &Certs,
    network: Network,
    slot: u64,
    epoch: u64,
) -> Vec<(EntityKey, PoolState)> {
    let pools = &certs.pstate;
    pools
        .stake_pools
        .iter()
        .map(|(id, pool)| {
            let mut snapshot =
                EpochValue::with_live(epoch, pool_snapshot(pool_params(pool, network)));
            if let Some(future) = pools.future_stake_pool_params.get(id) {
                snapshot.schedule(epoch, Some(pool_snapshot(future_params(future))));
            }
            let state = PoolState {
                operator: id.0.into(),
                snapshot,
                blocks_minted_total: 0,
                register_slot: slot,
                retiring_epoch: pools.retiring.get(id).copied(),
                deposit: pool.sps_deposit,
            };
            (EntityKey::from(id.0.as_slice()), state)
        })
        .collect()
}

fn drep_states(certs: &Certs, slot: u64, epoch: u64) -> Vec<(EntityKey, DRepState)> {
    certs
        .vstate
        .dreps
        .iter()
        .map(|(credential, drep)| {
            let identifier = match credential {
                Credential::Key(h) => conway::DRep::Key((*h).into()),
                Credential::Script(h) => conway::DRep::Script((*h).into()),
            };
            let mut state = DRepState::new(identifier.clone());
            state.registered_at = Some((slot, 0));
            state.voting_power = drep.deposit;
            state.deposit = drep.deposit;
            state.anchor = drep.anchor.as_ref().map(anchor);
            state.expiry = Some(DRepExpiry::new(drep.expiry, epoch));
            (drep_to_entity_key(&identifier), state)
        })
        .collect()
}

fn proposal_action(action: &GovAction) -> (ProposalAction, Option<GovActionId>) {
    let parent = |p: &Option<ActionId>| p.map(action_id);
    match action {
        GovAction::ParameterChange(prev, update, _) => {
            let mut set = PParamsSet::default();
            for value in params::seed_values(update).unwrap_or_else(|e| panic!("{e}")) {
                set.set(value);
            }
            (ProposalAction::ParamChange(set), parent(prev))
        }
        GovAction::HardForkInitiation(prev, version) => (
            ProposalAction::HardFork((version.major, version.minor)),
            parent(prev),
        ),
        GovAction::TreasuryWithdrawals(withdrawals, _) => (
            ProposalAction::TreasuryWithdrawal(
                withdrawals
                    .iter()
                    .map(|(account, coin)| (credential(account.credential), *coin))
                    .collect(),
            ),
            None,
        ),
        GovAction::NoConfidence(prev) => (ProposalAction::NoConfidence, parent(prev)),
        GovAction::UpdateCommittee(prev, removed, added, threshold) => (
            ProposalAction::UpdateCommittee {
                to_remove: removed.iter().map(|c| credential(*c)).collect(),
                to_add: added
                    .iter()
                    .map(|(c, epoch)| (stake_credential(c), *epoch))
                    .collect(),
                threshold: ratio(*threshold),
            },
            parent(prev),
        ),
        GovAction::NewConstitution(prev, constitution) => (
            ProposalAction::NewConstitution {
                anchor: anchor(&constitution.anchor),
                guardrail_script: constitution.script.map(|h| h.0.into()),
            },
            parent(prev),
        ),
        GovAction::InfoAction => (ProposalAction::Info, None),
    }
}

fn vote(v: state::Vote) -> conway::Vote {
    match v {
        state::Vote::VoteYes => conway::Vote::Yes,
        state::Vote::VoteNo => conway::Vote::No,
        state::Vote::Abstain => conway::Vote::Abstain,
    }
}

fn proposal_states(gov: &Gov, slot: u64) -> Vec<(EntityKey, ProposalState)> {
    gov.proposals
        .iter()
        .map(|p| {
            let procedure = &p.proposal_procedure;
            let (action, parent) = proposal_action(&procedure.gov_action);
            let history = |v: &state::Vote| vec![(slot, vote(*v))];
            let state = ProposalState {
                slot,
                tx: p.action_id.tx_id.0.into(),
                idx: p.action_id.gov_action_ix,
                purpose: action.purpose(),
                action,
                max_epoch: Some(p.expires_after),
                ratified_epoch: None,
                canceled_epoch: None,
                deposit: Some(procedure.deposit),
                reward_account: Some(credential(procedure.return_addr.credential)),
                proposed_in: Some(p.proposed_in),
                parent,
                anchor: Some(anchor(&procedure.anchor)),
                cc_votes: p
                    .committee_votes
                    .iter()
                    .map(|(c, v)| (stake_credential(c), history(v)))
                    .collect(),
                drep_votes: p
                    .drep_votes
                    .iter()
                    .map(|(c, v)| (stake_credential(c), history(v)))
                    .collect(),
                spo_votes: p
                    .stake_pool_votes
                    .iter()
                    .map(|(pool, v)| (pool.0.into(), history(v)))
                    .collect(),
            };
            (state.key(), state)
        })
        .collect()
}

/// The governance singleton with the ledger's committee, constitution, authorizations, roots and dormant epochs.
fn gov_state(mut current: GovState, certs: &Certs, gov: &Gov, slot: u64) -> GovState {
    current.committee = gov.committee.as_ref().map(|c| Committee {
        members: c
            .members
            .iter()
            .map(|(cold, epoch)| (stake_credential(cold), *epoch))
            .collect(),
        threshold: ratio(c.threshold),
    });
    current.constitution = Some(Constitution {
        anchor: anchor(&gov.constitution.anchor),
        guardrail_script: gov.constitution.script.map(|h| h.0.into()),
    });
    current.committee_auths = certs
        .vstate
        .committee_state
        .cs_committee_creds
        .iter()
        .map(|(cold, auth)| {
            let auth = match auth {
                LedgerAuthorization::CommitteeHotCredential(hot) => {
                    CommitteeAuthorization::HotCredential(credential(*hot))
                }
                LedgerAuthorization::CommitteeMemberResigned(a) => {
                    CommitteeAuthorization::Resigned(a.as_ref().map(anchor))
                }
            };
            (stake_credential(cold), vec![(slot, auth)])
        })
        .collect::<BTreeMap<_, _>>();
    let roots = gov.roots;
    current.prev_gov_action_ids = GovRoots {
        pparam_update: roots.pparam_update.map(action_id),
        hard_fork: roots.hard_fork.map(action_id),
        committee: roots.committee.map(action_id),
        constitution: roots.constitution.map(action_id),
    };
    current.num_dormant_epochs = certs.vstate.num_dormant_epochs;
    current
}

/// Writes the ledger's pools, DReps, committee, constitution and proposals at the slot of the epoch.
pub(super) fn seed(
    domain: &DomainAdapter,
    network: Network,
    slot: u64,
    epoch: u64,
    certs: &Certs,
    gov: &Gov,
) {
    let state = domain.state();
    let current = state
        .read_entity_typed::<GovState>(GovState::NS, &GovState::singleton_key())
        .unwrap()
        .expect("the store holds the governance singleton");
    let writer = state.start_writer().unwrap();
    for (key, pool) in pool_states(certs, network, slot, epoch) {
        writer.write_entity_typed(&key, &pool).unwrap();
    }
    for (key, drep) in drep_states(certs, slot, epoch) {
        writer.write_entity_typed(&key, &drep).unwrap();
    }
    for (key, proposal) in proposal_states(gov, slot) {
        writer.write_entity_typed(&key, &proposal).unwrap();
    }
    writer
        .write_entity_typed(
            &GovState::singleton_key(),
            &gov_state(current, certs, gov, slot),
        )
        .unwrap();
    writer.commit().unwrap();
}

#[cfg(test)]
mod tests {
    use ledger_oracle_judge::record::{CorpusKind, Record};

    use super::{
        super::{epoch_size, load, seed},
        *,
    };

    struct Read {
        pool: Option<PoolState>,
        drep: Option<DRepState>,
        proposal: Option<ProposalState>,
        gov: GovState,
    }

    /// What Dolos holds under the keys of `named` after seeding `seeded`.
    fn read_after_seeding(seeded: &Record, named: &Record) -> Read {
        let corpus = load(CorpusKind::Ic);
        let run = corpus.kept().next().unwrap();
        let genesis = seed::genesis(epoch_size(&corpus));
        let domain = seed::seed(
            &genesis,
            run.epoch().unwrap(),
            &[],
            run.seed(),
            &seeded.accounts_pre,
            seeded,
        )
        .domain;
        let state = domain.state();
        let pool = named.certs_pre.pstate.stake_pools.keys().next().unwrap();
        let drep = named.certs_pre.vstate.dreps.keys().next().unwrap();
        let drep = match drep {
            Credential::Key(h) => conway::DRep::Key((*h).into()),
            Credential::Script(h) => conway::DRep::Script((*h).into()),
        };
        let proposal = named.gov_pre.proposals[0].action_id;
        let proposal =
            ProposalState::build_entity_key(proposal.tx_id.0.into(), proposal.gov_action_ix);
        Read {
            pool: state
                .read_entity_typed(PoolState::NS, &EntityKey::from(pool.0.as_slice()))
                .unwrap(),
            drep: state
                .read_entity_typed(DRepState::NS, &drep_to_entity_key(&drep))
                .unwrap(),
            proposal: state
                .read_entity_typed(ProposalState::NS, &proposal)
                .unwrap(),
            gov: state
                .read_entity_typed(GovState::NS, &GovState::singleton_key())
                .unwrap()
                .unwrap(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "seeds from the committed ledger corpora, run in release"]
    async fn a_seeded_record_leaves_its_pools_dreps_proposals_and_committee_in_dolos_state() {
        let corpus = load(CorpusKind::Ic);
        let records = || corpus.kept().flat_map(|run| run.records());
        let full = records()
            .find(|r| {
                let (certs, gov) = (&r.certs_pre, &r.gov_pre);
                !certs.pstate.stake_pools.is_empty()
                    && !certs.vstate.dreps.is_empty()
                    && !certs.vstate.committee_state.cs_committee_creds.is_empty()
                    && !gov.proposals.is_empty()
            })
            .unwrap();
        let empty = records()
            .find(|r| {
                let (certs, gov) = (&r.certs_pre, &r.gov_pre);
                certs.pstate.stake_pools.is_empty()
                    && certs.vstate.dreps.is_empty()
                    && certs.vstate.committee_state.cs_committee_creds.is_empty()
                    && gov.proposals.is_empty()
            })
            .unwrap();

        let none = read_after_seeding(empty, full);
        assert!(none.pool.is_none() && none.drep.is_none() && none.proposal.is_none());
        assert!(none.gov.committee_auths.is_empty());

        let read = read_after_seeding(full, full);
        let (id, pool) = full.certs_pre.pstate.stake_pools.iter().next().unwrap();
        let held = read.pool.unwrap();
        assert_eq!(
            (held.operator.as_slice(), held.deposit),
            (&id.0[..], pool.sps_deposit)
        );
        let drep = full.certs_pre.vstate.dreps.values().next().unwrap();
        let held = read.drep.unwrap();
        assert_eq!(
            (held.deposit, held.expiry.map(|e| e.current)),
            (drep.deposit, Some(drep.expiry))
        );
        assert!(held.registered_at.is_some() && held.unregistered_at.is_none());
        let proposal = &full.gov_pre.proposals[0];
        let held = read.proposal.unwrap();
        assert_eq!(
            (held.deposit, held.max_epoch),
            (
                Some(proposal.proposal_procedure.deposit),
                Some(proposal.expires_after)
            )
        );
        let creds = &full.certs_pre.vstate.committee_state.cs_committee_creds;
        assert_eq!(read.gov.committee_auths.len(), creds.len());
    }
}
