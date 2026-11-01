use dolos_core::SyncExt;
use gasket::{framework::*, messaging::Message};
use tracing::debug;

use crate::{adapters::DomainAdapter, prelude::*};

pub type UpstreamPort = gasket::messaging::InputPort<PullEvent>;

pub enum WorkUnit {
    PullEvent(PullEvent),
    Housekeeping,
}

impl From<Message<PullEvent>> for WorkUnit {
    fn from(value: Message<PullEvent>) -> Self {
        WorkUnit::PullEvent(value.payload)
    }
}

impl From<WorkUnit> for WorkSchedule<WorkUnit> {
    fn from(value: WorkUnit) -> Self {
        WorkSchedule::Unit(value)
    }
}

#[derive(Stage)]
#[stage(name = "apply", unit = "WorkUnit", worker = "Worker")]
pub struct Stage {
    domain: DomainAdapter,

    housekeeping_interval: std::time::Duration,

    pub upstream: UpstreamPort,

    #[metric]
    block_count: gasket::metrics::Counter,

    #[metric]
    wal_count: gasket::metrics::Counter,
}

impl Stage {
    pub fn new(domain: DomainAdapter, housekeeping_interval: std::time::Duration) -> Self {
        Self {
            domain,
            housekeeping_interval,
            upstream: Default::default(),
            block_count: Default::default(),
            wal_count: Default::default(),
        }
    }

    fn on_roll_forward(&self, block: RawBlock) -> Result<(), WorkerError> {
        debug!("handling roll forward");

        self.domain.roll_forward(block).or_panic()?;

        Ok(())
    }

    fn on_rollback(&self, point: &ChainPoint) -> Result<(), WorkerError> {
        debug!(slot = &point.slot(), "handling rollback");

        self.domain.rollback(point).or_panic()?;

        Ok(())
    }
}

pub struct Worker {
    interval: tokio::time::Interval,
}

#[async_trait::async_trait(?Send)]
impl gasket::framework::Worker<Stage> for Worker {
    async fn bootstrap(stage: &Stage) -> Result<Self, WorkerError> {
        Ok(Self {
            interval: tokio::time::interval(stage.housekeeping_interval),
        })
    }

    async fn schedule(&mut self, stage: &mut Stage) -> Result<WorkSchedule<WorkUnit>, WorkerError> {
        tokio::select! {
            msg = stage.upstream.recv() => {
                let msg = msg.or_panic()?;
                let unit = WorkUnit::from(msg);
                Ok(unit.into())
            }
            _ = self.interval.tick() => {
                Ok(WorkSchedule::Unit(WorkUnit::Housekeeping))
            }
        }
    }

    async fn execute(&mut self, unit: &WorkUnit, stage: &mut Stage) -> Result<(), WorkerError> {
        match unit {
            WorkUnit::PullEvent(evt) => match evt {
                PullEvent::RollForward(x) => stage.on_roll_forward(x.clone())?,
                PullEvent::Rollback(x) => stage.on_rollback(x)?,
            },
            WorkUnit::Housekeeping => {
                housekeeping_tick(&stage.domain).or_panic()?;
            }
        }

        Ok(())
    }
}

/// The most housekeeping rounds one tick runs.
const MAX_ROUNDS_PER_TICK: u64 = 10;

/// The pruning one housekeeping tick does, at most `MAX_ROUNDS_PER_TICK` rounds.
fn housekeeping_tick<D: Domain>(domain: &D) -> Result<(), DomainError> {
    domain.drain_housekeeping(Some(MAX_ROUNDS_PER_TICK))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use dolos_cardano::CardanoDelta;
    use dolos_core::{config::SyncConfig, LogEntry, LogValue};
    use dolos_testing::toy_domain::ToyDomain;

    use super::*;

    const MAX_ROLLBACK: u64 = 1_000;

    /// A domain whose wal holds one entry for each slot in `slots`, with `max_rollback` set.
    fn domain_with_wal(slots: std::ops::Range<u64>) -> ToyDomain {
        let mut config = SyncConfig::default();
        config.max_rollback = Some(MAX_ROLLBACK);
        let domain = ToyDomain::new(None, None).with_sync_config(config);
        append_slots(&domain, slots);
        domain
    }

    /// Appends one wal entry for each slot in `slots`.
    fn append_slots(domain: &ToyDomain, slots: std::ops::Range<u64>) {
        let entries: Vec<LogEntry<CardanoDelta>> = slots
            .map(|slot| {
                let mut hash = [1u8; 32];
                hash[0..8].copy_from_slice(&slot.to_be_bytes());
                (ChainPoint::Specific(slot, hash.into()), LogValue::origin())
            })
            .collect();
        domain.wal().append_entries(&entries).unwrap();
    }

    const ROUND: u64 = <ToyDomain as Domain>::MAX_PRUNE_SLOTS_PER_HOUSEKEEPING;

    fn wal_span(domain: &ToyDomain) -> (u64, u64) {
        let start = domain.wal().find_start().unwrap().unwrap().0.slot();
        let tip = domain.wal().find_tip().unwrap().unwrap().0.slot();
        (start, tip)
    }

    #[test]
    fn a_tick_leaves_a_wal_inside_max_rollback_untouched() {
        let domain = domain_with_wal(5_000..5_500);

        housekeeping_tick(&domain).unwrap();

        assert_eq!(wal_span(&domain), (5_000, 5_499));
    }

    #[test]
    fn a_tick_far_behind_leaves_the_wal_inside_max_rollback() {
        let tip = 3 * <ToyDomain as Domain>::MAX_PRUNE_SLOTS_PER_HOUSEKEEPING;
        let domain = domain_with_wal(1..tip + 1);

        housekeeping_tick(&domain).unwrap();

        assert_eq!(wal_span(&domain), (tip - MAX_ROLLBACK, tip));
    }

    #[test]
    fn a_tick_prunes_at_most_its_rounds() {
        let tip = 2 * MAX_ROUNDS_PER_TICK * ROUND;
        let domain = domain_with_wal(1..tip + 1);
        let (start, _) = wal_span(&domain);

        housekeeping_tick(&domain).unwrap();

        assert_eq!(
            wal_span(&domain),
            (start + MAX_ROUNDS_PER_TICK * ROUND, tip)
        );
    }

    #[test]
    fn ticks_bring_the_wal_inside_max_rollback_while_blocks_arrive() {
        let mut tip = 25 * ROUND;
        let domain = domain_with_wal(1..tip + 1);

        for _ in 0..4 {
            housekeeping_tick(&domain).unwrap();
            append_slots(&domain, tip + 1..tip + 3 * ROUND + 1);
            tip += 3 * ROUND;
        }
        housekeeping_tick(&domain).unwrap();

        assert_eq!(wal_span(&domain), (tip - MAX_ROLLBACK, tip));
    }
}
