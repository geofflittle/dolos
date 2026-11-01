use std::sync::Arc;

use dolos_cardano::{
    model::{
        AccountState, DRepDelegation, EpochState, EpochValue, EraProtocol, PParamKind,
        PParamValue, PoolDelegation,
    },
    pallas_extras, CardanoDelta, ControlledAmountInc, FixedNamespace as _, SingletonEntity as _,
};
use dolos_core::{
    apply_delta_chunk, config::CardanoConfig, save_entities, ChainLogic as _, ChainPoint,
    Domain as _, EntityKey, EntityMap, EraCbor, Genesis, StateStore as _, StateWriter as _, TxoRef,
    UtxoSetDelta, WalStore as _,
};
use dolos_testing::toy_domain::ToyDomain;
use ledger_oracle_judge::{
    accounts::{Credential, DRep, LedgerAccounts},
    cbor::TxIn,
    record::{Record, UtxoEntry},
};
use pallas::{
    codec::minicbor,
    ledger::{
        primitives::{conway, StakeCredential},
        traverse::{Era, MultiEraOutput},
    },
};

use super::ledger_state;
use crate::adapters::{
    ArchiveStoreBackend, DomainAdapter, MempoolBackend, StateStoreBackend, WalAdapter,
};

/// The era tag Dolos stores a Dijkstra output under.
pub(super) const DIJKSTRA: u16 = 8;

/// The protocol version of the corpus chain.
const PROTOCOL: u16 = 12;

/// The devnet genesis with the corpus epoch length and protocol version 12.
pub(super) fn genesis(epoch_size: u64) -> Arc<Genesis> {
    let mut genesis = dolos_cardano::include::devnet::load()
        .with_dijkstra(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/crates/core/test_data/proto-devnet/dijkstra-genesis.json"
        ))
        .expect("the devnet Dijkstra genesis loads");
    genesis.shelley.epoch_length =
        Some(u32::try_from(epoch_size).expect("the corpus epoch size fits a genesis epoch length"));
    genesis.force_protocol = Some(PROTOCOL.into());
    Arc::new(genesis)
}

/// The Dolos output reference for a ledger input.
pub(super) fn txo_ref(input: &TxIn) -> TxoRef {
    let index = u32::try_from(input.ix).expect("a ledger output index fits an output reference");
    TxoRef(input.tx.into(), index)
}

/// The account deltas Dolos makes when a block produces the outputs.
fn controlled_amounts(pre: &[UtxoEntry], epoch: u64) -> Vec<CardanoDelta> {
    pre.iter()
        .filter_map(|e| {
            let output = MultiEraOutput::decode(Era::Dijkstra, &e.output.0)
                .expect("a seeded output decodes");
            let address = output.address().expect("a seeded output has an address");
            let era = EraProtocol::from(PROTOCOL);
            let (cred, is_pointer) = pallas_extras::address_as_stake_cred(&address, era)?;
            let amount = output.value().coin();
            Some(ControlledAmountInc::new(cred, is_pointer, amount, epoch).into())
        })
        .collect()
}

/// The pallas stake credential for a ledger credential.
pub(super) fn stake_credential(credential: &Credential) -> StakeCredential {
    match credential {
        Credential::Key(h) => StakeCredential::AddrKeyhash((*h).into()),
        Credential::Script(h) => StakeCredential::ScriptHash((*h).into()),
    }
}

/// The ledger credential for a pallas stake credential.
pub(super) fn credential(stake: &StakeCredential) -> Credential {
    match stake {
        StakeCredential::AddrKeyhash(h) => Credential::Key(**h),
        StakeCredential::ScriptHash(h) => Credential::Script(**h),
    }
}

/// The pallas vote delegation for a ledger one.
fn pallas_drep(drep: &DRep) -> conway::DRep {
    match drep {
        DRep::Key(h) => conway::DRep::Key((*h).into()),
        DRep::Script(h) => conway::DRep::Script((*h).into()),
        DRep::AlwaysAbstain => conway::DRep::Abstain,
        DRep::AlwaysNoConfidence => conway::DRep::NoConfidence,
    }
}

/// The ledger vote delegation for a pallas one.
pub(super) fn drep(drep: &conway::DRep) -> DRep {
    match drep {
        conway::DRep::Key(h) => DRep::Key(**h),
        conway::DRep::Script(h) => DRep::Script(**h),
        conway::DRep::Abstain => DRep::AlwaysAbstain,
        conway::DRep::NoConfidence => DRep::AlwaysNoConfidence,
    }
}

/// The state key Dolos stores the account of a credential under.
pub(super) fn account_key(stake: &StakeCredential) -> EntityKey {
    minicbor::to_vec(stake)
        .expect("a stake credential encodes")
        .into()
}

/// Registers each ledger account at the slot, holding its balance as unwithdrawn rewards and its delegations in the epoch.
fn seed_accounts(domain: &DomainAdapter, slot: u64, epoch: u64, accounts: &LedgerAccounts) {
    let writer = domain.state().start_writer().unwrap();
    for (credential, ledger) in accounts {
        let stake = stake_credential(credential);
        let key = account_key(&stake);
        let read = domain
            .state()
            .read_entity_typed::<AccountState>(AccountState::NS, &key)
            .unwrap();
        let mut account = read.unwrap_or_else(|| AccountState::new(epoch, stake));
        account.registered_at = Some(slot);
        let mut balance = account.stake.live().cloned().unwrap_or_default();
        balance.rewards_sum = ledger.balance;
        account.stake = EpochValue::with_live(epoch, balance);
        let pool = match &ledger.spool {
            Some(pool) => PoolDelegation::Pool((*pool).into()),
            None => PoolDelegation::NotDelegated,
        };
        account.pool = EpochValue::with_live(epoch, pool);
        let vote = match &ledger.drep {
            Some(drep) => DRepDelegation::Delegated(pallas_drep(drep)),
            None => DRepDelegation::NotDelegated,
        };
        account.drep = EpochValue::with_live(epoch, vote);
        writer.write_entity_typed(&key, &account).unwrap();
    }
    writer.commit().unwrap();
}

/// A chain point the write ahead log starts at, made only by seeding a run.
pub(super) struct SeedPoint(ChainPoint);

impl SeedPoint {
    /// The chain point a rollback sweep returns to.
    pub(super) fn point(&self) -> &ChainPoint {
        &self.0
    }
}

/// A domain over memory stores that holds one run's first pre state.
pub(super) struct Seeded {
    pub(super) domain: DomainAdapter,
    pub(super) seed: SeedPoint,
    _toy: ToyDomain,
}

/// Seeds a domain with the given parameters, outputs and accounts, and the record's pools, votes, governance and treasury, at the first slot of the epoch.
pub(super) fn seed(
    genesis: &Arc<Genesis>,
    epoch: u64,
    pparams: &[PParamValue],
    pre: &[UtxoEntry],
    accounts: &LedgerAccounts,
    record: &Record,
) -> Seeded {
    let delta = UtxoSetDelta {
        produced_utxo: pre
            .iter()
            .map(|e| {
                let output = Arc::new(EraCbor(DIJKSTRA, e.output.0.clone()));
                (txo_ref(&e.input), output)
            })
            .collect(),
        ..Default::default()
    };
    let toy = ToyDomain::new_with_genesis_and_config(
        genesis.clone(),
        CardanoConfig::default(),
        Some(delta),
        None,
    );
    let slot = epoch * u64::from(genesis.shelley.epoch_length.unwrap_or_default());
    let point = ChainPoint::Specific(slot, [0; 32].into());
    let mut state = dolos_cardano::load_epoch::<ToyDomain>(toy.state()).unwrap();
    state.number = epoch;
    let live = state.pparams.unwrap_live_mut();
    // The ledger holds a cost model only for the languages it names, so the seed starts from none.
    for kind in [
        PParamKind::CostModelsPlutusV1,
        PParamKind::CostModelsPlutusV2,
        PParamKind::CostModelsPlutusV3,
    ] {
        live.clear(kind);
    }
    for value in pparams {
        live.set(value.clone());
    }
    state.pparams = EpochValue::with_live(epoch, live.clone());
    state.initial_pots.treasury = record.treasury;
    let writer = toy.state().start_writer().unwrap();
    writer
        .write_entity_typed(&EpochState::singleton_key(), &state)
        .unwrap();
    writer.set_cursor(point.clone()).unwrap();
    writer.commit().unwrap();

    let state = StateStoreBackend::Memory(toy.state().clone());
    let chain = dolos_cardano::CardanoLogic::initialize::<DomainAdapter>(
        Default::default(),
        &state,
        genesis,
    )
    .unwrap();
    let domain = DomainAdapter {
        storage_config: Arc::new(toy.storage_config().clone()),
        sync_config: Arc::new(toy.sync_config().clone()),
        genesis: genesis.clone(),
        chain: Arc::new(std::sync::RwLock::new(chain)),
        state,
        archive: ArchiveStoreBackend::Memory(toy.archive().clone()),
        wal: WalAdapter::Redb(toy.wal().clone()),
        mempool: MempoolBackend::Ephemeral(Default::default()),
        tip_broadcast: tokio::sync::broadcast::channel(8).0,
    };
    let mut deltas = controlled_amounts(pre, epoch);
    let mut entities = EntityMap::default();
    apply_delta_chunk::<DomainAdapter>(&mut entities, domain.state(), &mut deltas).unwrap();
    let writer = domain.state().start_writer().unwrap();
    save_entities::<DomainAdapter>(&writer, &entities).unwrap();
    writer.commit().unwrap();
    seed_accounts(&domain, slot, epoch, accounts);
    let network = dolos_cardano::network_from_genesis(genesis);
    let (certs, gov) = (&record.certs_pre, &record.gov_pre);
    ledger_state::seed(&domain, network, slot, epoch, certs, gov);
    domain.wal().reset_to(&point).unwrap();
    Seeded {
        domain,
        seed: SeedPoint(point),
        _toy: toy,
    }
}
