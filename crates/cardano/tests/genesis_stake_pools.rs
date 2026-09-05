//! A stake pool declared in the Shelley genesis, carried across the first
//! epoch boundary.
//!
//! Genesis pools are registered without a transaction, so nothing debits a pot
//! for them. If the boundary then counts one as a registration that paid a
//! deposit, the deposit obligation appears with no source and the epoch
//! transition creates ada out of nothing. These tests say it must not.
//!
//! The harness is `ToyDomain` on the devnet genesis, which declares no stake
//! pools, given one. The boundary is crossed through the same ESTART work unit
//! the node runs.

use std::collections::HashMap;
use std::sync::Arc;

use dolos_core::{sync::execute_work_unit, Domain as _, Genesis, StateStore as _};
use dolos_testing::toy_domain::ToyDomain;

use dolos_cardano::{estart::EstartWorkUnit, pots::Pots, FixedNamespace as _};

use pallas::interop::hardano::configs::shelley::{Credential, Pool, RewardAccount, Staking};

const POOL_ID: &str = "747aca09f322d2dfc56243b839e2d573ab92287684e5e37d66ec0f87";
const POOL_VRF: &str = "d8252bd637a90ba4dbd2cf63afda20a19888b7895ede067081ce7fb7411a972b";
const POOL_REWARD_KEY: &str = "bc2b888f42c68e4e118a4fa16ada52db896bffb61ceea51a58301bf6";
const DELEGATOR: &str = "5e81366cb6f3c0d14837614afcea669d51b8be9519eaec4a237504f8";

/// The pool a genesis declares, shaped as the Musashi genesis declares one.
fn genesis_pool() -> Pool {
    Pool {
        cost: 0,
        margin: pallas::ledger::primitives::alonzo::RationalNumber {
            numerator: 0,
            denominator: 1,
        },
        metadata: None,
        owners: vec![],
        pledge: 0,
        public_key: POOL_ID.to_string(),
        relays: vec![],
        reward_account: RewardAccount {
            credential: Credential::KeyHash(POOL_REWARD_KEY.to_string()),
            network: "Testnet".to_string(),
        },
        vrf: POOL_VRF.to_string(),
        registration_deposit: None,
    }
}

/// The devnet genesis with one stake pool and one delegation to it, which is
/// the shape any network seeded with an initial pool has and which mainnet,
/// preprod and preview all lack.
fn genesis_with_a_stake_pool() -> Arc<Genesis> {
    let mut genesis = dolos_cardano::include::devnet::load();

    let mut pools = HashMap::new();
    pools.insert(POOL_ID.to_string(), genesis_pool());

    let mut stake = HashMap::new();
    stake.insert(DELEGATOR.to_string(), POOL_ID.to_string());

    genesis.shelley.staking = Some(Staking {
        pools: Some(pools),
        stake: Some(stake),
    });

    Arc::new(genesis)
}

/// The same genesis with its staking section left as devnet ships it, empty.
fn genesis_without_a_stake_pool() -> Arc<Genesis> {
    Arc::new(dolos_cardano::include::devnet::load())
}

/// Close the epoch and open the next one, both halves, through the same two
/// work units the node runs in that order. EWRAP is what counts the pool
/// registrations that paid a deposit, so a crossing that runs only ESTART
/// never reaches the arithmetic under test.
fn cross_the_boundary(domain: &ToyDomain) {
    let summary = dolos_cardano::eras::load_era_summary::<ToyDomain>(domain.state()).unwrap();
    let epoch = dolos_cardano::load_epoch::<ToyDomain>(domain.state()).unwrap();
    let slot = summary.epoch_start(epoch.number + 1);

    let mut ewrap = dolos_cardano::CardanoWorkUnit::Ewrap(Box::new(
        dolos_cardano::ewrap::EwrapWorkUnit::new(slot, domain.genesis()),
    ));

    execute_work_unit(domain, &mut ewrap).unwrap();

    let mut estart = dolos_cardano::CardanoWorkUnit::Estart(Box::new(EstartWorkUnit::new(
        slot,
        domain.genesis(),
    )));

    execute_work_unit(domain, &mut estart).unwrap();
}

fn pots(domain: &ToyDomain) -> Pots {
    dolos_cardano::load_epoch::<ToyDomain>(domain.state())
        .unwrap()
        .initial_pots
}

fn epoch_number(domain: &ToyDomain) -> u64 {
    dolos_cardano::load_epoch::<ToyDomain>(domain.state())
        .unwrap()
        .number as u64
}

/// The must-fire case. A genesis pool takes nothing out of any pot, so after
/// the first boundary the total supply must be the one the genesis set and the
/// pool must contribute no deposit obligation.
#[test]
fn a_genesis_stake_pool_does_not_create_a_deposit() {
    let domain = ToyDomain::new_with_genesis(genesis_with_a_stake_pool(), None, None);

    let before = pots(&domain);
    let supply = before.max_supply();

    assert_eq!(before.pool_count, 0, "no pot funded a genesis pool deposit");

    cross_the_boundary(&domain);

    let after = pots(&domain);

    assert_eq!(
        after.max_supply(),
        supply,
        "the boundary changed the total supply by {}",
        after.max_supply() as i128 - supply as i128,
    );

    assert_eq!(
        after.pool_count, 0,
        "the genesis pool was counted as a registration that paid a deposit",
    );

    assert!(after.is_consistent(supply));
    assert_eq!(epoch_number(&domain), 1, "the boundary was crossed");
}

/// The must-not case for the one above. Making a genesis pool free must not be
/// done by making every pool free, so the same crossing on a genesis with no
/// pools has to behave identically, and the flag that means "this registration
/// paid a deposit" has to still mean that for a real registration.
#[test]
fn a_genesis_without_pools_crosses_the_same_boundary_unchanged() {
    let domain = ToyDomain::new_with_genesis(genesis_without_a_stake_pool(), None, None);

    let before = pots(&domain);
    let supply = before.max_supply();

    cross_the_boundary(&domain);

    let after = pots(&domain);

    assert_eq!(after.max_supply(), supply);
    assert_eq!(after.pool_count, 0);
    assert_eq!(epoch_number(&domain), 1);
}

/// The other half of the must-not case, on the flag itself. A pool registered
/// by a certificate is charged a deposit and a genesis pool is not, so the two
/// writers must disagree about the flag. If both wrote `false` the test above
/// would pass for the wrong reason.
#[test]
fn the_new_pool_flag_separates_a_certificate_from_a_genesis_pool() {
    let domain = ToyDomain::new_with_genesis(genesis_with_a_stake_pool(), None, None);

    let key = dolos_core::EntityKey::from(hex::decode(POOL_ID).unwrap().as_slice());

    let pool = domain
        .state()
        .read_entity_typed::<dolos_cardano::PoolState>(dolos_cardano::PoolState::NS, &key)
        .unwrap()
        .expect("the genesis pool must be in the store");

    assert!(
        !pool.snapshot.unwrap_live().is_new,
        "a genesis pool must not be flagged as having paid a deposit",
    );

    // The same pool parameters, arriving the way a transaction delivers them.
    let cert = dolos_cardano::pallas_extras::MultiEraPoolRegistration {
        operator: POOL_ID.parse().unwrap(),
        vrf_keyhash: POOL_VRF.parse().unwrap(),
        pledge: 0,
        cost: 0,
        margin: pallas::ledger::primitives::alonzo::RationalNumber {
            numerator: 0,
            denominator: 1,
        },
        reward_account: vec![],
        pool_owners: vec![],
        relays: vec![],
        pool_metadata: None,
    };

    let mut delta = dolos_cardano::PoolRegistration::new(cert, 0, 0, 500_000_000);
    let mut entity = None;
    dolos_core::EntityDelta::apply(&mut delta, &mut entity);

    let registered = entity.expect("the registration must create a pool");

    assert!(
        registered.snapshot.unwrap_live().is_new,
        "a certificate registration must still be flagged as having paid a deposit",
    );
}
