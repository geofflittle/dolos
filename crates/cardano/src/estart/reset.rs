use dolos_core::ChainError;

use crate::{
    estart::{AccountId, PoolId, WorkContext},
    pots::{apply_delta, PotDelta, Pots},
    AccountState, AccountTransition, EpochTransitionV2, PoolState, PoolTransition,
};

pub fn define_new_pots(ctx: &super::WorkContext) -> Pots {
    let epoch = ctx.ended_state();

    let rolling = epoch.rolling.unwrap_live();

    let end = epoch.end.as_ref().expect("no end stats available");

    let pparams = epoch.pparams.unwrap_live();

    let delta = PotDelta {
        // Use effective MIR amounts from EndStats (only MIRs applied to registered accounts)
        // Rolling stats contain total from MIR certificates, which includes unregistered accounts
        reserve_mirs: end.reserve_mirs,
        treasury_mirs: end.treasury_mirs,
        treasury_withdrawals: end.treasury_withdrawals,
        proposal_refunds: end.proposal_refunds,
        proposal_invalid_refunds: end.proposal_invalid_refunds,
        effective_rewards: end.effective_rewards,
        unspendable_to_treasury: end.unspendable_to_treasury,
        unspendable_to_reserves: end.unspendable_to_reserves,
        pool_deposit_count: end.pool_deposit_count,
        pool_refund_count: end.pool_refund_count,
        pool_invalid_refund_count: end.pool_invalid_refund_count,
        mark_protocol_version: epoch
            .pparams
            .mark()
            .map(|p| p.protocol_major_or_default())
            .unwrap_or_else(|| epoch.pparams.unwrap_live().protocol_major_or_default()),
        avvm_reclamation: ctx.avvm_reclamation.total,
        ..PotDelta::from_rolling(rolling, pparams)
    };

    tracing::debug!(
        epoch = epoch.number,
        initial_reserves = epoch.initial_pots.reserves,
        initial_treasury = epoch.initial_pots.treasury,
        incentives_total = end.epoch_incentives.total,
        incentives_treasury_tax = end.epoch_incentives.treasury_tax,
        incentives_available_rewards = end.epoch_incentives.available_rewards,
        incentives_used_fees = end.epoch_incentives.used_fees,
        effective_rewards = end.effective_rewards,
        unspendable_to_treasury = end.unspendable_to_treasury,
        unspendable_to_reserves = end.unspendable_to_reserves,
        consumed_incentives = delta.consumed_incentives(),
        returned_rewards = end
            .epoch_incentives
            .available_rewards
            .saturating_sub(delta.consumed_incentives()),
        effective_reserve_mirs = end.reserve_mirs,
        effective_treasury_mirs = end.treasury_mirs,
        invalid_reserve_mirs = end.invalid_reserve_mirs,
        invalid_treasury_mirs = end.invalid_treasury_mirs,
        treasury_withdrawals = end.treasury_withdrawals,
        invalid_treasury_withdrawals = end.invalid_treasury_withdrawals,
        pool_invalid_refund_count = end.pool_invalid_refund_count,
        proposal_invalid_refunds = end.proposal_invalid_refunds,
        treasury_donations = rolling.treasury_donations,
        direct_deposits = delta.direct_deposits,
        produced_utxos = delta.produced_utxos,
        consumed_utxos = delta.consumed_utxos,
        initial_utxos = epoch.initial_pots.utxos,
        withdrawals = delta.withdrawals,
        gathered_fees = delta.gathered_fees,
        avvm_reclamation = delta.avvm_reclamation,
        protocol_version = delta.protocol_version,
        mark_protocol_version = delta.mark_protocol_version,
        "pot delta components for ESTART"
    );

    let pots = apply_delta(epoch.initial_pots.clone(), &end.epoch_incentives, &delta);

    tracing::debug!(
        rewards = pots.rewards,
        reserves = pots.reserves,
        treasury = pots.treasury,
        fees = pots.fees,
        utxos = pots.utxos,
        "pots after reset"
    );

    report_drift(ctx.lenient_apply, epoch.number, &epoch.initial_pots, &pots, || {
        dbg!(end);
        dbg!(&epoch.initial_pots);
        dbg!(&pots);
        dbg!(delta);
    });

    pots
}

/// Reports an epoch boundary where the pots no longer sum to the supply the
/// epoch started with.
///
/// Under the strict rule this is an invariant violation and the assertion
/// stands exactly as it did, with the same dump before it. Under the lenient
/// rule it is expected on every boundary, because the node re-creates outputs
/// that were already spent and skips inputs that are not there, so it is
/// measured and logged instead: signed, in lovelace, with the epoch and the
/// pots that moved.
///
/// Split out and taking the flag rather than the context so both paths can be
/// driven from a test without an epoch boundary to hand.
pub(crate) fn report_drift(
    lenient: bool,
    epoch: u64,
    initial: &Pots,
    ended: &Pots,
    dump: impl FnOnce(),
) {
    let drift = crate::pots::measure_drift(epoch, initial, ended);

    if drift.total() == 0 {
        return;
    }

    if !lenient {
        dump();
        debug_assert!(ended.is_consistent(initial.max_supply()));
        return;
    }

    let moved = drift
        .moved()
        .iter()
        .map(|(pot, by)| format!("{pot} {by:+}"))
        .collect::<Vec<_>>()
        .join(", ");

    tracing::warn!(
        epoch = drift.epoch,
        drift_lovelace = drift.total() as i64,
        expected_max_supply = drift.expected_max_supply,
        actual_max_supply = drift.actual_max_supply,
        moved = %moved,
        "pots drifted from max supply under lenient apply"
    );
}

/// Per-entity transition visitor for the snapshot rotation that ESTART
/// performs. Emits deltas straight onto `WorkContext.deltas` (no internal
/// `Vec` accumulator) so the per-account branch can be driven from a
/// shard-scoped iteration without holding millions of deltas in RAM.
///
/// The single `EpochTransition` delta — which advances the epoch number,
/// recomputes pots, and (optionally) migrates pparams across an era
/// boundary — is emitted by `WorkContext::compute_global_deltas` (the
/// finalize-phase entry point), not by `flush`. Sharded callers therefore
/// run the per-account branch alone and let the finalize unit handle the
/// closing global delta.
#[derive(Default)]
pub struct BoundaryVisitor;

impl super::BoundaryVisitor for BoundaryVisitor {
    fn visit_account(
        &mut self,
        ctx: &mut super::WorkContext,
        id: &AccountId,
        _: &AccountState,
    ) -> Result<(), ChainError> {
        ctx.add_delta(AccountTransition::new(id.clone(), ctx.starting_epoch_no()));

        Ok(())
    }

    fn visit_pool(
        &mut self,
        ctx: &mut super::WorkContext,
        id: &PoolId,
        _: &PoolState,
    ) -> Result<(), ChainError> {
        ctx.add_delta(PoolTransition::new(id.clone(), ctx.starting_epoch_no()));

        Ok(())
    }

    // No `flush` override — the closing `EpochTransition` is emitted by
    // `WorkContext::compute_global_deltas`, which sees both the ended
    // state and the newly-defined pots.
}

/// Emit the closing `EpochTransition` delta — invoked from
/// `WorkContext::compute_global_deltas` after all per-entity transitions
/// have been emitted.
pub fn emit_epoch_transition(ctx: &mut WorkContext) {
    let new_pots = define_new_pots(ctx);
    let era_transition = ctx.ended_state().pparams.era_transition();
    let genesis = ctx.genesis.clone();
    ctx.deltas.add_for_entity(EpochTransitionV2::new(
        ctx.starting_epoch_no(),
        new_pots,
        era_transition,
        Some(genesis),
    ));
}

#[cfg(test)]
mod drift_tests {
    use crate::pots::{measure_drift, Pots};

    use super::report_drift;

    /// The pots of the epoch boundary that stopped the sync at 13:08:26 UTC on
    /// 2026-09-07, read from the panic's own dump, so the numbers under test
    /// are the ones the chain produced rather than ones invented for a test.
    fn boundary() -> (Pots, Pots) {
        let initial = Pots {
            reserves: 14302730267316072,
            treasury: 706019927311550,
            utxos: 29986097951147725,
            rewards: 4849331134086,
            fees: 243459090567,
            pool_count: 113,
            account_count: 782,
            deposit_per_pool: 500000000,
            deposit_per_account: 2000000,
            nominal_deposits: 0,
            drep_deposits: 1000000000,
            proposal_deposits: 0,
        };

        let ended = Pots {
            reserves: 14294140394309010,
            treasury: 714650257290053,
            utxos: 29990621427242621,
            rewards: 5052333253212,
            fees: 241300905104,
            pool_count: 114,
            ..initial.clone()
        };

        (initial, ended)
    }

    /// MUST FIRE: the drift is measured, signed, and attributed to the pots
    /// that moved.
    ///
    /// MUST NOT FIRE: pots that did not move at all report a zero in every
    /// field and name no pot, so a report is not something every boundary
    /// produces a number for.
    #[test]
    fn the_drift_is_measured_and_attributed() {
        let (initial, ended) = boundary();

        let drift = measure_drift(42, &initial, &ended);

        assert_eq!(drift.epoch, 42);
        assert_eq!(drift.expected_max_supply, 45_000_000_000_000_000);
        assert_eq!(drift.actual_max_supply, 45_004_765_277_000_000);
        assert_eq!(drift.total(), 4_765_277_000_000);

        let moved = drift.moved();
        assert_eq!(
            moved.first().map(|(pot, _)| *pot),
            Some("treasury"),
            "the largest movement is named first"
        );
        assert_eq!(drift.utxos, 4_523_476_094_896);
        assert_eq!(drift.reserves, -8_589_873_007_062);

        let still = measure_drift(42, &initial, &initial);
        assert_eq!(still.total(), 0);
        assert!(
            still.moved().is_empty(),
            "pots that did not move named a pot anyway"
        );
    }

    /// MUST FIRE: a transfer is reported as a transfer. The utxo pot gained
    /// exactly what the reserves lost, so the supply is untouched and two pots
    /// moved, and a report that carried only the sum would say nothing
    /// happened.
    ///
    /// MUST NOT FIRE: the pots that did not take part are zero, so this is an
    /// attribution and not a report that lights up whenever anything moves.
    #[test]
    fn a_transfer_between_pots_reports_a_zero_total_and_names_both() {
        let (initial, _) = boundary();

        let moved_amount = 1_000_000_000u64;

        let ended = Pots {
            reserves: initial.reserves - moved_amount,
            utxos: initial.utxos + moved_amount,
            ..initial.clone()
        };

        let drift = measure_drift(42, &initial, &ended);

        assert_eq!(drift.total(), 0, "a transfer created no supply");
        assert_eq!(drift.utxos, moved_amount as i128);
        assert_eq!(drift.reserves, -(moved_amount as i128));
        assert_eq!(drift.treasury, 0);
        assert_eq!(drift.rewards, 0);
        assert_eq!(drift.fees, 0);
        assert_eq!(drift.obligations, 0);

        let moved = drift.moved();
        assert_eq!(moved.len(), 2, "the two pots that moved are both named");
    }

    /// MUST FIRE: value that came from nowhere is reported with a non zero
    /// total naming the pot it appeared in, which is the case a total on its
    /// own cannot tell apart from the transfer above.
    #[test]
    fn a_pot_that_gained_from_nowhere_is_named_with_a_non_zero_total() {
        let (initial, _) = boundary();

        let created = 1_000_000_000u64;

        let ended = Pots {
            utxos: initial.utxos + created,
            ..initial.clone()
        };

        let drift = measure_drift(42, &initial, &ended);

        assert_eq!(drift.total(), created as i128);
        assert_eq!(
            drift.moved(),
            vec![("utxos", created as i128)],
            "the utxo pot is the only one named"
        );
    }

    /// MUST NOT FIRE: a transfer is an ordinary epoch, so the reporting path
    /// neither dumps nor asserts on one. Making the measurement total must not
    /// turn every boundary into a warning.
    #[test]
    fn a_transfer_is_not_reported_as_drift_under_either_setting() {
        let (initial, _) = boundary();

        let ended = Pots {
            reserves: initial.reserves - 1_000_000_000,
            utxos: initial.utxos + 1_000_000_000,
            ..initial.clone()
        };

        report_drift(true, 42, &initial, &ended, || {
            panic!("a transfer is not a drift")
        });
        report_drift(false, 42, &initial, &ended, || {
            panic!("a transfer is not a drift")
        });
    }

    /// MUST NOT FIRE: under the lenient rule a drifted boundary is reported and
    /// the sync carries on. This is the whole point, and it is asserted by the
    /// call returning at all.
    #[test]
    fn lenient_reports_the_drift_and_does_not_panic() {
        let (initial, ended) = boundary();

        let mut dumped = false;
        report_drift(true, 42, &initial, &ended, || dumped = true);

        assert!(
            !dumped,
            "the lenient path reports, it does not dump the debug state"
        );
    }

    /// MUST FIRE: under the strict rule the assertion still fires on exactly
    /// the same pots. Every network other than this one runs it, and a
    /// leniency that leaked into it would turn a real accounting bug into a log
    /// line nobody reads.
    #[test]
    #[should_panic(expected = "is_consistent")]
    fn strict_still_asserts_on_the_same_pots() {
        let (initial, ended) = boundary();

        report_drift(false, 42, &initial, &ended, || {});
    }

    /// MUST FIRE: the guard the two epoch transition deltas assert through
    /// lets a drifted boundary past when the process is lenient.
    ///
    /// MUST NOT FIRE: it refuses one when the process is strict, and it accepts
    /// a consistent boundary under both. Those deltas are handed only the
    /// entity, so this guard is the only thing standing between the setting and
    /// a panic five seconds after the boundary has already been reported, which
    /// is exactly what happened when only the reset path was fixed.
    ///
    /// The flag is process wide, so this test sets and restores it rather than
    /// leaving it set for whatever runs next in the same binary.
    #[test]
    fn the_delta_guard_follows_the_process_setting() {
        let (initial, ended) = boundary();
        let expected = initial.max_supply();

        let before = crate::pots::lenient_apply();

        crate::pots::set_lenient_apply(false);
        assert!(
            !crate::pots::supply_holds_or_lenient(&ended, expected),
            "a drifted boundary is refused under the strict setting"
        );
        assert!(
            crate::pots::supply_holds_or_lenient(&initial, expected),
            "a consistent boundary is accepted under the strict setting"
        );

        crate::pots::set_lenient_apply(true);
        assert!(
            crate::pots::supply_holds_or_lenient(&ended, expected),
            "a drifted boundary is let past under the lenient setting"
        );
        assert!(
            crate::pots::supply_holds_or_lenient(&initial, expected),
            "and a consistent one still is"
        );

        crate::pots::set_lenient_apply(before);
    }

    /// MUST NOT FIRE: a consistent boundary is silent under both settings, so
    /// neither one is reporting or asserting on every epoch.
    #[test]
    fn a_consistent_boundary_is_silent_under_both_settings() {
        let (initial, _) = boundary();

        report_drift(true, 42, &initial, &initial, || {
            panic!("nothing to dump on a consistent boundary")
        });
        report_drift(false, 42, &initial, &initial, || {
            panic!("nothing to dump on a consistent boundary")
        });
    }
}
