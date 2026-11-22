//! Writes the pools, DReps, committee and proposals the ledger ran a record
//! from into Dolos state.

use std::collections::BTreeMap;

use dolos_cardano::{
    model::{
        drep_to_entity_key, AccountState, Committee, CommitteeAuthorization, Constitution,
        DRepExpiry, DRepState, EpochValue, GovRoots, GovState, PParamValue, PParamsSet, PoolParams,
        PoolSnapshot, PoolState, ProposalAction, ProposalState,
    },
    FixedNamespace, SingletonEntity as _,
};
use dolos_core::{Domain as _, Entity, EntityKey, StateStore as _, StateWriter as _};
use ledger_oracle_judge::{
    accounts::Credential,
    ledger_state::{self, DolosGov, DolosState, LedgerStateChecks, ParamUpdate, Unrepresentable},
    record::{Bytes, PParams, RecordId},
    state::{
        self, ActionId, Certs, CommitteeAuthorization as LedgerAuthorization, CredentialObject,
        Fraction, Gov, GovAction, Hex, NotHeld, Proposal, ProposalProcedure, RewardAccount,
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

use super::{
    params,
    seed::{self, stake_credential},
};
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
        bls_key: future.bls_key.as_ref().map(|k| BlsKey {
            bls_pubkey: k.bls_pub_key.0.clone().into(),
            bls_possession_proof: k.bls_possession_proof.0.clone().into(),
        }),
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

/// The Dolos parameter set of a ledger parameter update, where an empty map
/// of cost models proposes no model.
fn param_set(update: &PParams) -> PParamsSet {
    let mut set = PParamsSet::default();
    for value in params::seed_values(update).unwrap_or_else(|e| panic!("{e}")) {
        match value {
            PParamValue::CostModelsUnknown(models) if models.is_empty() => {}
            value => set.set(value),
        }
    }
    set
}

fn param_update(set: &PParamsSet) -> ParamUpdate {
    ParamUpdate(set.iter().map(|value| format!("{value:?}")).collect())
}

/// A ledger parameter update as the values Dolos sets for it.
pub(super) fn ledger_update(update: &PParams) -> ParamUpdate {
    param_update(&param_set(update))
}

fn proposal_action(action: &GovAction) -> (ProposalAction, Option<GovActionId>) {
    let parent = |p: &Option<ActionId>| p.map(action_id);
    match action {
        GovAction::ParameterChange(prev, update, _) => {
            (ProposalAction::ParamChange(param_set(update)), parent(prev))
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
            let history = |v: &state::Vote| vec![(slot, Some(vote(*v)))];
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

/// The governance singleton with the ledger's committee, constitution,
/// authorizations, roots and dormant epochs.
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

/// Writes the ledger's pools, DReps, committee, constitution and proposals at
/// the slot of the epoch.
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

/// A field Dolos holds that the ledger's types cannot, with what Dolos holds.
type Unheld = (&'static str, String);

fn credential_object(c: &StakeCredential) -> CredentialObject {
    match c {
        StakeCredential::AddrKeyhash(h) => CredentialObject::KeyHash(Hex(**h)),
        StakeCredential::ScriptHash(h) => CredentialObject::ScriptHash(Hex(**h)),
    }
}

fn ledger_anchor(a: &Anchor) -> state::Anchor {
    state::Anchor {
        data_hash: Hex(*a.content_hash),
        url: a.url.clone(),
    }
}

fn ledger_action_id(id: &GovActionId) -> ActionId {
    ActionId {
        gov_action_ix: id.action_index,
        tx_id: Hex(*id.transaction_id),
    }
}

fn fraction(r: &RationalNumber) -> Fraction {
    Fraction {
        numerator: r.numerator,
        denominator: r.denominator,
    }
}

fn network_name(network: Network) -> String {
    match network {
        Network::Mainnet => "Mainnet".to_string(),
        Network::Testnet => "Testnet".to_string(),
        Network::Other(n) => format!("Other{n}"),
    }
}

fn ledger_reward_account(credential: &StakeCredential, network: Network) -> RewardAccount {
    RewardAccount {
        credential: credential_object(credential),
        network: network_name(network),
    }
}

/// The reward account of reward address bytes.
fn reward_account(bytes: &[u8]) -> Result<RewardAccount, Unheld> {
    let unheld = || ("account", hex::encode(bytes));
    let (&header, hash) = bytes.split_first().ok_or_else(unheld)?;
    let hash = Hex(hash.try_into().map_err(|_| unheld())?);
    let credential = match header >> 4 {
        0xe => CredentialObject::KeyHash(hash),
        0xf => CredentialObject::ScriptHash(hash),
        _ => return Err(unheld()),
    };
    let network = Network::from(header & 0x0f);
    Ok(RewardAccount {
        credential,
        network: network_name(network),
    })
}

fn bls_pair(key: &BlsKey) -> state::BlsKeyPair {
    state::BlsKeyPair {
        bls_possession_proof: Bytes(key.bls_possession_proof.to_vec()),
        bls_pub_key: Bytes(key.bls_pubkey.to_vec()),
    }
}

/// The ledger's pool parameters of Dolos pool parameters.
fn ledger_params(operator: &[u8; 28], p: &PoolParams) -> Result<state::PoolParams, Unheld> {
    if !p.relays.is_empty() {
        return Err(("relays", format!("{:?}", p.relays)));
    }
    if let Some(metadata) = &p.pool_metadata {
        return Err(("metadata", format!("{metadata:?}")));
    }
    Ok(state::PoolParams {
        account_address: reward_account(&p.reward_account)?,
        bls_key: p.bls_key.as_ref().map(bls_pair),
        cost: p.cost,
        margin: fraction(&p.margin),
        metadata: None,
        owners: p.pool_owners.iter().map(|h| Hex(**h)).collect(),
        pledge: p.pledge,
        pool_id: Hex(*operator),
        relays: Vec::new(),
        vrf: Hex(*p.vrf_keyhash),
    })
}

/// The ledger's registered pool of a Dolos pool's live parameters.
fn ledger_pool(
    pool: &PoolState,
    live: &PoolParams,
    delegators: Vec<CredentialObject>,
) -> Result<state::StakePool<NotHeld>, Unheld> {
    let state::PoolParams {
        account_address,
        bls_key,
        cost,
        margin,
        metadata: _,
        owners,
        pledge,
        pool_id: _,
        relays: _,
        vrf,
    } = ledger_params(&pool.operator, live)?;
    Ok(state::StakePool {
        sps_account_id: account_address.credential,
        sps_bls_key: bls_key.map(|bks_key| state::BlsKey {
            bks_key,
            bks_registered_in: NotHeld,
        }),
        sps_cost: cost,
        sps_delegators: delegators,
        sps_deposit: pool.deposit,
        sps_margin: margin,
        sps_metadata: None,
        sps_owners: owners,
        sps_pledge: pledge,
        sps_relays: Vec::new(),
        sps_vrf: vrf,
    })
}

/// The registered credentials delegated to each pool.
fn delegators(domain: &DomainAdapter) -> BTreeMap<[u8; 28], Vec<CredentialObject>> {
    let accounts = domain
        .state()
        .iter_entities_typed::<AccountState>(AccountState::NS, None);
    let mut by_pool: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for entity in accounts.unwrap() {
        let account = entity.unwrap().1;
        if let (true, Some(pool)) = (account.is_registered(), account.delegated_pool_live()) {
            let held = by_pool.entry(**pool).or_default();
            held.push(credential_object(&account.credential));
        }
    }
    by_pool
}

pub(super) fn read_all<T: Entity + FixedNamespace>(domain: &DomainAdapter) -> Vec<T> {
    let entities = domain
        .state()
        .iter_entities_typed::<T>(T::NS, None)
        .unwrap();
    entities.map(|entity| entity.unwrap().1).collect()
}

/// The pools Dolos holds as the ledger's pool state.
fn pools(domain: &DomainAdapter, unheld: &mut Vec<Unrepresentable>) -> state::Pools<NotHeld> {
    let mut delegators = delegators(domain);
    let mut held = state::Pools {
        future_stake_pool_params: BTreeMap::new(),
        retiring: BTreeMap::new(),
        stake_pools: BTreeMap::new(),
        vrf_key_hashes: BTreeMap::new(),
    };
    for pool in read_all::<PoolState>(domain) {
        let Some(live) = pool.snapshot.live().filter(|s| !s.is_retired) else {
            continue;
        };
        let id = Hex(*pool.operator);
        let mut unrepresentable = |(field, dolos): Unheld| {
            unheld.push(Unrepresentable {
                table: "stake_pools",
                key: id.to_string(),
                field,
                dolos,
            })
        };
        let delegated = delegators.remove(&id.0).unwrap_or_default();
        match ledger_pool(&pool, &live.params, delegated) {
            Ok(registered) => {
                *held.vrf_key_hashes.entry(registered.sps_vrf).or_default() += 1;
                held.stake_pools.insert(id, registered);
            }
            Err(field) => unrepresentable(field),
        }
        if let Some(future) = pool.snapshot.next() {
            match ledger_params(&pool.operator, &future.params) {
                Ok(future) => {
                    if future.vrf != Hex(*live.params.vrf_keyhash) {
                        *held.vrf_key_hashes.entry(future.vrf).or_default() += 1;
                    }
                    held.future_stake_pool_params.insert(id, future);
                }
                Err(field) => unrepresentable(field),
            }
        }
        if let Some(epoch) = pool.retiring_epoch {
            held.retiring.insert(id, epoch);
        }
    }
    held
}

/// The registered DReps Dolos holds as the ledger's DRep states.
pub(super) fn dreps(
    domain: &DomainAdapter,
    unheld: &mut Vec<Unrepresentable>,
) -> BTreeMap<Credential, state::DRepState> {
    let mut held = BTreeMap::new();
    for drep in read_all::<DRepState>(domain) {
        if drep.registered_at.is_none() || drep.is_unregistered() {
            continue;
        }
        let credential = match &drep.identifier {
            conway::DRep::Key(h) => Ok(Credential::Key(**h)),
            conway::DRep::Script(h) => Ok(Credential::Script(**h)),
            other => Err(format!("{other:?}")),
        };
        let expiry = drep.expiry.map(|e| e.current);
        match (credential, expiry) {
            (Ok(credential), Some(expiry)) => {
                let state = state::DRepState {
                    anchor: drep.anchor.as_ref().map(ledger_anchor),
                    deposit: drep.deposit,
                    expiry,
                };
                held.insert(credential, state);
            }
            (credential, _) => unheld.push(Unrepresentable {
                table: "dreps",
                key: credential
                    .as_ref()
                    .map_or_else(|k| k.clone(), |c| c.to_string()),
                field: if credential.is_ok() { "expiry" } else { "key" },
                dolos: format!("{:?}", drep.identifier),
            }),
        }
    }
    held
}

fn ledger_vote(v: &conway::Vote) -> state::Vote {
    match v {
        conway::Vote::Yes => state::Vote::VoteYes,
        conway::Vote::No => state::Vote::VoteNo,
        conway::Vote::Abstain => state::Vote::Abstain,
    }
}

/// The last vote of each voter whose last entry is a vote, or the voter with
/// an empty history.
fn last_votes<K: Ord + Clone, L: Ord>(
    votes: &BTreeMap<K, Vec<(u64, Option<conway::Vote>)>>,
    key: impl Fn(&K) -> L,
) -> Result<BTreeMap<L, state::Vote>, String> {
    let mut last = BTreeMap::new();
    for (voter, history) in votes {
        match history.last() {
            Some((_, Some(vote))) => {
                last.insert(key(voter), ledger_vote(vote));
            }
            Some((_, None)) => {}
            None => {
                let voters = votes.len();
                return Err(format!(
                    "no vote in the history of a voter of {voters} voters"
                ));
            }
        }
    }
    Ok(last)
}

/// The ledger's governance action of a Dolos proposal's action and parent.
fn ledger_action(
    action: &ProposalAction,
    parent: Option<&GovActionId>,
    network: Network,
) -> Result<GovAction<NotHeld, ParamUpdate>, Unheld> {
    let prev = parent.map(ledger_action_id);
    let no_parent = |action: GovAction<NotHeld, ParamUpdate>| match prev {
        None => Ok(action),
        Some(id) => Err(("gov_action", format!("parent {id:?} of {action:?}"))),
    };
    match action {
        ProposalAction::ParamChange(set) => {
            Ok(GovAction::ParameterChange(prev, param_update(set), NotHeld))
        }
        ProposalAction::HardFork((major, minor)) => Ok(GovAction::HardForkInitiation(
            prev,
            state::ProtocolVersion {
                major: *major,
                minor: *minor,
            },
        )),
        ProposalAction::TreasuryWithdrawal(paid) => {
            let paid = paid
                .iter()
                .map(|(c, coin)| (ledger_reward_account(c, network), *coin));
            no_parent(GovAction::TreasuryWithdrawals(paid.collect(), NotHeld))
        }
        ProposalAction::Other => Err(("gov_action", "Other".to_string())),
        ProposalAction::NoConfidence => Ok(GovAction::NoConfidence(prev)),
        ProposalAction::UpdateCommittee {
            to_remove,
            to_add,
            threshold,
        } => Ok(GovAction::UpdateCommittee(
            prev,
            to_remove.iter().map(credential_object).collect(),
            to_add
                .iter()
                .map(|(c, epoch)| (seed::credential(c), *epoch))
                .collect(),
            fraction(threshold),
        )),
        ProposalAction::NewConstitution {
            anchor,
            guardrail_script,
        } => Ok(GovAction::NewConstitution(
            prev,
            state::Constitution {
                anchor: ledger_anchor(anchor),
                script: guardrail_script.map(|h| Hex(*h)),
            },
        )),
        ProposalAction::Info => no_parent(GovAction::InfoAction),
    }
}

/// The ledger's proposal of a live Dolos proposal.
fn ledger_proposal(
    p: &ProposalState,
    network: Network,
) -> Result<Proposal<NotHeld, ParamUpdate>, Unheld> {
    let some = |field: &'static str, held: bool| match held {
        true => Ok(()),
        false => Err((field, "none".to_string())),
    };
    let (Some(deposit), Some(expires_after), Some(proposed_in), Some(anchor), Some(account)) = (
        p.deposit,
        p.max_epoch,
        p.proposed_in,
        p.anchor.as_ref(),
        p.reward_account.as_ref(),
    ) else {
        some("deposit", p.deposit.is_some())?;
        some("expires_after", p.max_epoch.is_some())?;
        some("proposed_in", p.proposed_in.is_some())?;
        some("anchor", p.anchor.is_some())?;
        return Err(("return_addr", "none".to_string()));
    };
    let votes = |e| ("votes", e);
    Ok(Proposal {
        action_id: ActionId {
            gov_action_ix: p.idx,
            tx_id: Hex(*p.tx),
        },
        committee_votes: last_votes(&p.cc_votes, seed::credential).map_err(votes)?,
        drep_votes: last_votes(&p.drep_votes, seed::credential).map_err(votes)?,
        expires_after,
        proposal_procedure: ProposalProcedure {
            anchor: ledger_anchor(anchor),
            deposit,
            gov_action: ledger_action(&p.action, p.parent.as_ref(), network)?,
            return_addr: ledger_reward_account(account, network),
        },
        proposed_in,
        stake_pool_votes: last_votes(&p.spo_votes, |pool| Hex(**pool)).map_err(votes)?,
    })
}

/// The ledger's committee authorizations of the last entry of each Dolos
/// authorization history.
fn committee_creds(
    gov: &GovState,
    unheld: &mut Vec<Unrepresentable>,
) -> BTreeMap<Credential, LedgerAuthorization> {
    let mut held = BTreeMap::new();
    for (cold, history) in &gov.committee_auths {
        let cold = seed::credential(cold);
        let auth = match history.last() {
            Some((_, CommitteeAuthorization::HotCredential(hot))) => {
                LedgerAuthorization::CommitteeHotCredential(credential_object(hot))
            }
            Some((_, CommitteeAuthorization::Resigned(a))) => {
                LedgerAuthorization::CommitteeMemberResigned(a.as_ref().map(ledger_anchor))
            }
            None => {
                unheld.push(Unrepresentable {
                    table: "committee_creds",
                    key: cold.to_string(),
                    field: "authorization",
                    dolos: "none".to_string(),
                });
                continue;
            }
        };
        held.insert(cold, auth);
    }
    held
}

/// The pools, DReps, committee, constitution and proposals Dolos holds, in the
/// ledger's types.
pub(super) fn read(domain: &DomainAdapter) -> DolosState {
    let network = dolos_cardano::network_from_genesis(&domain.genesis());
    let mut unrepresentable = Vec::new();
    let gov = domain
        .state()
        .read_entity_typed::<GovState>(GovState::NS, &GovState::singleton_key())
        .unwrap()
        .expect("the store holds the governance singleton");
    let pstate = pools(domain, &mut unrepresentable);
    let dreps = dreps(domain, &mut unrepresentable);
    let cs_committee_creds = committee_creds(&gov, &mut unrepresentable);
    let mut proposals = Vec::new();
    for p in read_all::<ProposalState>(domain) {
        if p.ratified_epoch.is_some() || p.canceled_epoch.is_some() {
            continue;
        }
        match ledger_proposal(&p, network) {
            Ok(proposal) => proposals.push(proposal),
            Err((field, dolos)) => unrepresentable.push(Unrepresentable {
                table: "proposals",
                key: format!("{}#{}", hex::encode(*p.tx), p.idx),
                field,
                dolos,
            }),
        }
    }
    let GovState {
        constitution,
        committee,
        prev_gov_action_ids: roots,
        num_dormant_epochs,
        ..
    } = gov;
    let committee = committee.map(|c| state::Committee {
        members: c
            .members
            .iter()
            .map(|(cold, epoch)| (seed::credential(cold), *epoch))
            .collect(),
        threshold: fraction(&c.threshold),
    });
    let constitution = constitution.map(|c| state::Constitution {
        anchor: ledger_anchor(&c.anchor),
        script: c.guardrail_script.map(|h| Hex(*h)),
    });
    let id = |id: Option<GovActionId>| id.as_ref().map(ledger_action_id);
    DolosState {
        certs: Certs {
            pstate,
            vstate: state::Votes {
                committee_state: state::CommitteeState { cs_committee_creds },
                dreps,
                num_dormant_epochs,
            },
        },
        gov: DolosGov {
            committee,
            constitution,
            proposals,
            roots: state::Roots {
                committee: id(roots.committee),
                constitution: id(roots.constitution),
                hard_fork: id(roots.hard_fork),
                pparam_update: id(roots.pparam_update),
            },
        },
        unrepresentable,
    }
}

/// Counts each row of the ledger state `certs` and `gov` hold after record
/// `id` against the row Dolos holds.
pub(super) fn compare(
    domain: &DomainAdapter,
    id: RecordId,
    certs: &Certs,
    gov: &Gov,
    checks: &mut LedgerStateChecks,
) {
    let held = read(domain);
    ledger_state::compare(id, (certs, gov), &held, &ledger_update, checks);
}

#[cfg(test)]
mod tests {
    use ledger_oracle_judge::record::{CorpusKind, Record};

    use super::{
        super::{load, seed},
        *,
    };

    /// An update proposing a model for one named language proposes no model
    /// for any other.
    #[test]
    fn an_update_of_one_cost_model_proposes_that_model_alone() {
        let mut map = serde_json::Map::new();
        let models = serde_json::json!({"PlutusV3": [4], "Unknown": {}});
        map.insert("costModels".into(), models);
        let proposed = ledger_update(&PParams(map));
        let expected = ["CostModelsPlutusV3([4])".to_string()];
        assert_eq!(proposed, ParamUpdate(expected.into()));
    }

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
        let genesis = seed::genesis(run.era());
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
