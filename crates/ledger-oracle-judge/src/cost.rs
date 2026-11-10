use std::fmt;

use crate::{
    compare::Tally,
    judgement::{self, Axis, CellKey, Finding, Instrument, Judge},
    record::{LedgerOutcome, RecordId},
};

/// The minimum fee comparisons of one arm.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CostTallies {
    pub fee: Tally,
    pub fee_unresolved_refused: usize,
    pub fee_undecoded: usize,
    /// The records whose makers Dolos refused, so Dolos resolves their inputs
    /// to other outputs than the ledger.
    pub skipped_cascade: usize,
}

impl CostTallies {
    /// Counts the minimum fee of one record.
    pub fn add_fee(
        &mut self,
        id: RecordId,
        ledger: Option<u64>,
        node: Result<u64, String>,
        outcome: &LedgerOutcome,
    ) {
        let Some(ledger) = ledger else {
            self.fee_undecoded += 1;
            return;
        };
        if matches!(outcome, LedgerOutcome::Refused { .. }) && node.is_err() {
            self.fee_unresolved_refused += 1;
            return;
        }
        let agrees = node.as_ref() == Ok(&ledger);
        self.fee.add(agrees, || match &node {
            Ok(n) => {
                let cells = vec![CellKey::new(Axis::Answer, "differ")];
                Finding::new(id, cells, format!("ledger={ledger} dolos={n}"))
            }
            Err(e) => {
                let cells = vec![CellKey::new(Axis::Answer, "none")];
                Finding::new(id, cells, format!("ledger={ledger} dolos=none {e}"))
            }
        });
    }

    /// Adds the comparisons and skips of another arm part after this one's.
    pub fn merge(&mut self, other: CostTallies) {
        let CostTallies {
            fee,
            fee_unresolved_refused,
            fee_undecoded,
            skipped_cascade,
        } = other;
        self.fee.merge(fee);
        self.fee_unresolved_refused += fee_unresolved_refused;
        self.fee_undecoded += fee_undecoded;
        self.skipped_cascade += skipped_cascade;
    }

    /// Adds the verdict of each compared fee.
    pub fn report(&self, out: &mut judgement::Tally) {
        self.fee.report(Judge::Cost, Instrument::MinFee.into(), out);
    }
}

impl fmt::Display for CostTallies {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "COST fees={} fees_unresolved_refused={} fees_undecoded={} skipped_cascade={}",
            self.fee.compared,
            self.fee_unresolved_refused,
            self.fee_undecoded,
            self.skipped_cascade
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::{
        judgement::RunVerdict,
        record::{FailurePath, NonEmpty},
    };

    fn id(s: &str) -> RecordId {
        s.parse().unwrap()
    }

    fn passes(t: &CostTallies) -> bool {
        let mut out = judgement::Tally::default();
        t.report(&mut out);
        matches!(out.verdict(), RunVerdict::Pass { .. })
    }

    fn details(t: &CostTallies) -> Vec<String> {
        t.fee.disagree.iter().map(Finding::to_string).collect()
    }

    fn tally(lines: &[&str]) -> Tally {
        Tally {
            compared: 2 * lines.len(),
            disagree: lines
                .iter()
                .map(|l| Finding::new(id("ic#1"), Vec::new(), *l))
                .collect(),
        }
    }

    fn part(n: usize) -> CostTallies {
        let line = format!("line{n}");
        CostTallies {
            fee: tally(&[&line]),
            fee_unresolved_refused: 2,
            fee_undecoded: 3,
            skipped_cascade: 6,
        }
    }

    #[test]
    fn a_merged_part_adds_every_count_and_keeps_every_line_after_the_first_part() {
        let mut merged = part(0);
        merged.merge(part(1));
        let both = CostTallies {
            fee: tally(&["line0", "line1"]),
            fee_unresolved_refused: 4,
            fee_undecoded: 6,
            skipped_cascade: 12,
        };
        assert_eq!(merged, both);
    }

    #[test]
    fn merging_an_empty_part_changes_nothing() {
        let mut merged = part(0);
        merged.merge(CostTallies::default());
        assert_eq!(merged, part(0));
    }

    const ACCEPTED: LedgerOutcome = LedgerOutcome::Accepted {
        consumed: Vec::new(),
        produced: Vec::new(),
        accounts_post: BTreeMap::new(),
    };

    fn refused() -> LedgerOutcome {
        LedgerOutcome::Refused {
            failures: String::new(),
            failure_names: NonEmpty::of([FailurePath {
                leaf: crate::refusal::LedgerLeaf::BadInputsUTxO,
            }])
            .unwrap(),
        }
    }

    #[test]
    fn an_equal_fee_passes_and_no_fee_compared_fails() {
        let mut t = CostTallies::default();
        assert!(!passes(&t));
        t.add_fee(id("supp#1"), Some(7), Ok(7), &ACCEPTED);
        t.skipped_cascade += 1;
        assert!(passes(&t), "{t}");
        assert_eq!(
            t.to_string(),
            "COST fees=1 fees_unresolved_refused=0 fees_undecoded=0 skipped_cascade=1"
        );
    }

    #[test]
    fn a_refused_record_compares_a_fee_the_node_computes_and_counts_one_it_cannot_apart() {
        let mut t = CostTallies::default();
        t.add_fee(id("ic#1"), Some(7), Ok(7), &refused());
        t.add_fee(
            id("ic#2"),
            Some(7),
            Err("missing input".to_string()),
            &refused(),
        );
        assert!(passes(&t), "{t}");
        assert_eq!((t.fee.compared, t.fee_unresolved_refused), (1, 1));
        t.add_fee(id("ic#3"), Some(9), Ok(7), &refused());
        assert_eq!(details(&t), vec!["ic#3 ledger=9 dolos=7"]);
        assert!(!passes(&t));
    }

    #[test]
    fn bytes_the_ledger_does_not_decode_have_no_fee_to_compare() {
        let mut t = CostTallies::default();
        t.add_fee(id("supp#1"), None, Ok(7), &refused());
        t.add_fee(
            id("supp#2"),
            None,
            Err("cbor error".to_string()),
            &refused(),
        );
        assert_eq!((t.fee.compared, t.fee_undecoded), (0, 2));
        assert!(t.fee.disagree.is_empty());
        t.add_fee(id("supp#3"), Some(9), Ok(7), &refused());
        assert_eq!(details(&t), vec!["supp#3 ledger=9 dolos=7"]);
    }

    #[test]
    fn a_differing_fee_or_one_the_node_cannot_compute_disagrees() {
        let mut t = CostTallies::default();
        t.add_fee(id("supp#1"), Some(7), Ok(8), &ACCEPTED);
        t.add_fee(
            id("supp#2"),
            Some(7),
            Err("no parameters".to_string()),
            &ACCEPTED,
        );
        assert!(!passes(&t));
        assert_eq!(
            details(&t),
            vec![
                "supp#1 ledger=7 dolos=8",
                "supp#2 ledger=7 dolos=none no parameters"
            ]
        );
    }
}
