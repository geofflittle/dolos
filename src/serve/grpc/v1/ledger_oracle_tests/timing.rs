//! The time each pass of judging a corpus takes, summed over its runs.

use std::time::{Duration, Instant};

use ledger_oracle_judge::record::CorpusKind;

/// The number of passes a corpus is timed in.
const PASSES: usize = 16;

/// The time each pass of judging a corpus took, summed over the runs that judged it.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(super) struct Timing {
    pub(super) seed: Duration,
    pub(super) read_params: Duration,
    pub(super) groups: Duration,
    pub(super) evaluate: Duration,
    pub(super) receive: Duration,
    pub(super) submit_routes: Duration,
    pub(super) roll: Duration,
    pub(super) group_state: Duration,
    pub(super) datums: Duration,
    pub(super) scripts: Duration,
    pub(super) plutus_data: Duration,
    pub(super) served_tx: Duration,
    pub(super) read_tx: Duration,
    pub(super) rolled_routes: Duration,
    pub(super) as_output: Duration,
    pub(super) sweep: Duration,
}

impl Timing {
    /// Adds the time of another part of the corpus to this one's.
    pub(super) fn merge(&mut self, other: Timing) {
        for ((_, mine), (_, theirs)) in self.passes_mut().into_iter().zip(other.passes()) {
            *mine += theirs;
        }
    }

    /// One line a pass, naming the corpus, the pass and its seconds.
    pub(super) fn lines(&self, kind: CorpusKind) -> Vec<String> {
        let corpus = kind.name();
        let line = |(pass, time): (&str, Duration)| {
            let seconds = time.as_secs_f64();
            format!("TIMING corpus={corpus} pass={pass} seconds={seconds:.1}")
        };
        self.passes().into_iter().map(line).collect()
    }

    fn passes(&self) -> [(&'static str, Duration); PASSES] {
        let mut copy = *self;
        copy.passes_mut().map(|(pass, time)| (pass, *time))
    }

    fn passes_mut(&mut self) -> [(&'static str, &mut Duration); PASSES] {
        let Timing {
            seed,
            read_params,
            groups,
            evaluate,
            receive,
            submit_routes,
            roll,
            group_state,
            datums,
            scripts,
            plutus_data,
            served_tx,
            read_tx,
            rolled_routes,
            as_output,
            sweep,
        } = self;
        [
            ("seed", seed),
            ("read_params", read_params),
            ("groups", groups),
            ("evaluate", evaluate),
            ("receive", receive),
            ("submit_routes", submit_routes),
            ("roll", roll),
            ("group_state", group_state),
            ("datums", datums),
            ("scripts", scripts),
            ("plutus_data", plutus_data),
            ("served_tx", served_tx),
            ("read_tx", read_tx),
            ("rolled_routes", rolled_routes),
            ("as_output", as_output),
            ("sweep", sweep),
        ]
    }
}

/// A clock that charges the time since its last reading to one pass.
pub(super) struct Stopwatch(Instant);

impl Stopwatch {
    /// A clock whose first reading counts from now.
    pub(super) fn start() -> Self {
        Stopwatch(Instant::now())
    }

    /// Adds the time since the last reading to the pass and counts the next reading from now.
    pub(super) fn charge(&mut self, pass: &mut Duration) {
        let now = Instant::now();
        *pass += now - self.0;
        self.0 = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pass_never_charged_prints_zero_seconds() {
        let lines = Timing::default().lines(CorpusKind::Supp);
        assert_eq!(lines.len(), PASSES);
        assert!(lines.contains(&"TIMING corpus=supp pass=sweep seconds=0.0".to_string()));
    }

    #[test]
    fn merged_parts_print_the_sum_of_each_pass_and_no_other() {
        let mut whole = Timing {
            read_tx: Duration::from_millis(1500),
            ..Timing::default()
        };
        whole.merge(Timing {
            read_tx: Duration::from_millis(2500),
            sweep: Duration::from_secs(7),
            ..Timing::default()
        });
        let lines = whole.lines(CorpusKind::Ic);
        assert!(lines.contains(&"TIMING corpus=ic pass=read_tx seconds=4.0".to_string()));
        assert!(lines.contains(&"TIMING corpus=ic pass=sweep seconds=7.0".to_string()));
        assert!(lines.contains(&"TIMING corpus=ic pass=served_tx seconds=0.0".to_string()));
    }

    #[test]
    fn a_charge_goes_to_the_named_pass_alone() {
        let mut timing = Timing::default();
        let mut watch = Stopwatch::start();
        std::thread::sleep(Duration::from_millis(20));
        watch.charge(&mut timing.read_tx);
        let charged = Timing {
            read_tx: timing.read_tx,
            ..Timing::default()
        };
        assert!(timing.read_tx >= Duration::from_millis(20));
        assert_eq!(timing, charged);
    }
}
