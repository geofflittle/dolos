use std::collections::BTreeMap;

use dolos_core::{ChainError, Domain, StateStore};
use pallas::{
    crypto::hash::Hash,
    ledger::primitives::{dijkstra::BlsKey, Epoch},
};

use crate::{AccountState, EraProtocol, FixedNamespace as _, PoolHash, PoolState};

/// The stake delegated to one pool in the snapshot taken at the end of an epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolStake {
    pub stake: u64,
    pub vrf_keyhash: Hash<32>,
    pub bls_key: Option<BlsKey>,
}

/// The stake of every pool active in the snapshot taken at the end of an epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolStakes {
    pub pools: BTreeMap<PoolHash, PoolStake>,
    pub total: u64,
}

impl PoolStakes {
    /// Lists every pool not retired at the end of `epoch`, each with no stake yet.
    pub fn new<'a>(pools: impl IntoIterator<Item = &'a PoolState>, epoch: Epoch) -> Self {
        let pools = pools
            .into_iter()
            .filter_map(|pool| {
                let snapshot = pool.snapshot.snapshot_at(epoch)?;
                let stake = PoolStake {
                    stake: 0,
                    vrf_keyhash: snapshot.params.vrf_keyhash,
                    bls_key: snapshot.params.bls_key.clone(),
                };
                (!snapshot.is_retired).then_some((pool.operator, stake))
            })
            .collect();

        Self { pools, total: 0 }
    }

    /// Adds the stake an account delegated at the end of `epoch` to a listed pool.
    pub fn add_account(&mut self, account: &AccountState, epoch: Epoch, protocol: EraProtocol) {
        let Some(pool) = account
            .delegated_pool_at(epoch)
            .and_then(|hash| self.pools.get_mut(hash))
        else {
            return;
        };

        let stake = account
            .stake
            .snapshot_at(epoch)
            .map(|x| x.total_for_era(protocol))
            .unwrap_or_default();

        pool.stake = pool.stake.saturating_add(stake);
        self.total = self.total.saturating_add(stake);
    }
}

/// Loads the stake of every pool active in the snapshot taken at the end of `epoch`.
pub fn load_pool_stakes<D: Domain>(
    state: &D::State,
    epoch: Epoch,
    protocol: EraProtocol,
) -> Result<PoolStakes, ChainError> {
    let pools = state
        .iter_entities_typed::<PoolState>(PoolState::NS, None)?
        .map(|record| record.map(|(_, pool)| pool))
        .collect::<Result<Vec<_>, _>>()?;

    let mut stakes = PoolStakes::new(&pools, epoch);

    for record in state.iter_entities_typed::<AccountState>(AccountState::NS, None)? {
        let (_, account) = record?;
        stakes.add_account(&account, epoch, protocol);
    }

    Ok(stakes)
}

#[cfg(test)]
mod tests {
    use pallas::ledger::primitives::{conway::RationalNumber, StakeCredential};

    use super::*;
    use crate::{EpochValue, PoolDelegation, PoolParams, PoolSnapshot, Stake};

    const EPOCH: Epoch = 10;

    fn at_epoch<T: Clone + std::fmt::Debug>(epoch: Epoch, value: T) -> EpochValue<T> {
        let mut out = EpochValue::with_live(epoch, value);
        for next in epoch + 1..=EPOCH + 2 {
            out.transition(next);
        }
        out
    }

    fn pool(byte: u8, is_retired: bool) -> PoolState {
        let params = PoolParams {
            vrf_keyhash: Hash::new([byte; 32]),
            pledge: 0,
            cost: 0,
            margin: RationalNumber {
                numerator: 0,
                denominator: 1,
            },
            reward_account: vec![],
            pool_owners: vec![],
            relays: vec![],
            pool_metadata: None,
            bls_key: None,
        };
        let snapshot = PoolSnapshot {
            is_retired,
            blocks_minted: 0,
            params,
            is_new: false,
        };

        PoolState {
            operator: Hash::new([byte; 28]),
            snapshot: at_epoch(EPOCH, snapshot),
            blocks_minted_total: 0,
            register_slot: 0,
            retiring_epoch: None,
            deposit: 0,
        }
    }

    fn account(pool: u8, delegated_at: Epoch, utxo_sum: u64) -> AccountState {
        let mut out = AccountState::new(EPOCH, StakeCredential::AddrKeyhash(Hash::new([pool; 28])));
        let stake = Stake {
            utxo_sum,
            rewards_sum: 0,
            withdrawals_sum: 0,
            utxo_sum_at_pointer_addresses: 0,
        };
        out.stake = at_epoch(EPOCH, stake);
        out.pool = at_epoch(delegated_at, PoolDelegation::Pool(Hash::new([pool; 28])));
        out
    }

    fn gather(pools: &[PoolState], accounts: &[AccountState]) -> PoolStakes {
        let mut out = PoolStakes::new(pools, EPOCH);
        for account in accounts {
            out.add_account(account, EPOCH, EraProtocol::from(9));
        }
        out
    }

    fn stakes(stakes: &PoolStakes) -> Vec<(u8, u64)> {
        stakes
            .pools
            .iter()
            .map(|(hash, x)| (hash[0], x.stake))
            .collect()
    }

    #[test]
    fn a_retired_pool_and_its_delegators_are_left_out() {
        let out = gather(
            &[pool(1, true), pool(2, false)],
            &[account(1, EPOCH, 5), account(2, EPOCH, 7)],
        );

        assert_eq!(stakes(&out), [(2, 7)]);
        assert_eq!(out.total, 7);
    }

    #[test]
    fn a_delegation_made_after_the_epoch_is_left_out() {
        let out = gather(
            &[pool(1, false)],
            &[account(1, EPOCH + 1, 5), account(1, EPOCH, 3)],
        );

        assert_eq!(stakes(&out), [(1, 3)]);
        assert_eq!(out.total, 3);
    }

    #[test]
    fn a_pool_without_delegators_is_listed_with_no_stake() {
        let out = gather(&[pool(1, false), pool(2, false)], &[account(2, EPOCH, 4)]);

        assert_eq!(stakes(&out), [(1, 0), (2, 4)]);
        assert_eq!(out.total, 4);
    }

    #[test]
    fn delegators_add_up_per_pool_and_in_total() {
        let out = gather(
            &[pool(1, false), pool(2, false)],
            &[
                account(1, EPOCH, 5),
                account(2, EPOCH, 4),
                account(1, EPOCH, 6),
            ],
        );

        assert_eq!(stakes(&out), [(1, 11), (2, 4)]);
        assert_eq!(out.total, 15);
        assert_eq!(
            out.pools[&Hash::new([2; 28])].vrf_keyhash,
            Hash::new([2; 32])
        );
    }
}
