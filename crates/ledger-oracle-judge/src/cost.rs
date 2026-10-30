use std::{collections::BTreeMap, fmt};

use crate::{
    cbor::Purpose,
    compare::Tally,
    record::{Evaluation, LedgerOutcome, LedgerRedeemer, Units},
};

/// What the node answered for the redeemers of one transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeCosts {
    Evaluated(Vec<LedgerRedeemer>),
    Unevaluated(String),
}

/// One redeemer's evaluation by the ledger and by the node, either side possibly absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedeemerCost {
    pub purpose: Purpose,
    pub ledger: Option<Evaluation>,
    pub node: Option<Evaluation>,
}

impl RedeemerCost {
    /// The units both sides ran the redeemer in, when they ran it in the same units.
    pub fn ran_alike(&self) -> Option<Units> {
        match (&self.ledger, &self.node) {
            (Some(Evaluation::Ran(l)), Some(Evaluation::Ran(n))) if l == n => Some(*l),
            _ => None,
        }
    }

    /// Whether both sides ran the redeemer in the same units or both failed it.
    pub fn agrees(&self) -> bool {
        let failed = |e: &Option<Evaluation>| matches!(e, Some(Evaluation::Failed(_)));
        self.ran_alike().is_some() || (failed(&self.ledger) && failed(&self.node))
    }
}

impl fmt::Display for RedeemerCost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let side = |e: &Option<Evaluation>| match e {
            Some(e) => e.to_string(),
            None => "none".to_string(),
        };
        write!(
            f,
            "purpose={} ledger=({}) dolos=({})",
            self.purpose,
            side(&self.ledger),
            side(&self.node)
        )
    }
}

/// Pairs the ledger's evaluations with the node's by purpose, a node that evaluated nothing failing each ledger redeemer.
pub fn pair(ledger: &[LedgerRedeemer], node: &NodeCosts) -> Vec<RedeemerCost> {
    fn entry(m: &mut BTreeMap<Purpose, RedeemerCost>, purpose: Purpose) -> &mut RedeemerCost {
        m.entry(purpose).or_insert(RedeemerCost {
            purpose,
            ledger: None,
            node: None,
        })
    }
    let mut by_purpose = BTreeMap::new();
    for r in ledger {
        entry(&mut by_purpose, r.purpose).ledger = Some(r.evaluation.clone());
    }
    if let NodeCosts::Evaluated(node) = node {
        for r in node {
            entry(&mut by_purpose, r.purpose).node = Some(r.evaluation.clone());
        }
    }
    match node {
        NodeCosts::Evaluated(_) => {}
        NodeCosts::Unevaluated(why) => {
            for cost in by_purpose.values_mut() {
                cost.node = Some(Evaluation::Failed(format!("unevaluated {why}")));
            }
        }
    }
    by_purpose.into_values().collect()
}

/// Why a record's costs were not compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostSkip {
    /// The ledger refused the record, so its rule evaluated no script.
    Refused,
    /// The record has a declared unsupported row, so Dolos evaluates no script of it.
    Declared,
    /// Dolos refused a maker of the record, so Dolos resolves its inputs to other outputs than the ledger.
    Cascade,
}

/// The redeemer cost and minimum fee comparisons of one arm.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CostTallies {
    pub units: Tally,
    pub ran: usize,
    pub fee: Tally,
    pub fee_unresolved_refused: usize,
    pub fee_undecoded: usize,
    pub skipped_refused: usize,
    pub skipped_declared: usize,
    pub skipped_cascade: usize,
}

impl CostTallies {
    /// Counts one record whose costs were not compared.
    pub fn skip(&mut self, why: CostSkip) {
        match why {
            CostSkip::Refused => self.skipped_refused += 1,
            CostSkip::Declared => self.skipped_declared += 1,
            CostSkip::Cascade => self.skipped_cascade += 1,
        }
    }

    /// Counts each redeemer of one record, keeping the ones whose evaluations differ.
    pub fn add_units(&mut self, id: impl fmt::Display, costs: &[RedeemerCost]) {
        for cost in costs {
            self.ran += usize::from(cost.ran_alike().is_some());
            self.units.add(cost.agrees(), || format!("{id} {cost}"));
        }
    }

    /// Counts the minimum fee of one record, kept when the node's differs or is missing from a record the ledger accepted, and counted apart when it is missing from a refused one or the ledger's decoder refused the bytes.
    pub fn add_fee(
        &mut self,
        id: impl fmt::Display,
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
            Ok(n) => format!("{id} ledger={ledger} dolos={n}"),
            Err(e) => format!("{id} ledger={ledger} dolos=none {e}"),
        });
    }

    /// Whether every redeemer agreed and some fee was compared with none differing.
    pub fn pass(&self) -> bool {
        self.units.disagree.is_empty() && self.fee.compared > 0 && self.fee.disagree.is_empty()
    }
}

/// Whether the tallies of every corpus pass and some redeemer of some corpus ran alike.
pub fn pass_all<'a>(all: impl IntoIterator<Item = &'a CostTallies>) -> bool {
    let (pass, ran) = all
        .into_iter()
        .fold((true, 0), |(pass, ran), t| (pass && t.pass(), ran + t.ran));
    pass && ran > 0
}

impl fmt::Display for CostTallies {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "COST redeemers={} ran_alike={} fees={} fees_unresolved_refused={} fees_undecoded={} skipped_refused={} skipped_declared={} skipped_cascade={}",
            self.units.compared,
            self.ran,
            self.fee.compared,
            self.fee_unresolved_refused,
            self.fee_undecoded,
            self.skipped_refused,
            self.skipped_declared,
            self.skipped_cascade
        )?;
        for (name, t) in [("units", &self.units), ("fee", &self.fee)] {
            if !t.disagree.is_empty() {
                write!(f, "\nCOST {name} disagree={}", t.disagree.len())?;
                for d in t.disagree.iter().take(20) {
                    write!(f, "\nDIFF {name} {d}")?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{FailurePath, NonEmpty};

    fn purpose(index: u64) -> Purpose {
        Purpose { tag: 0, index }
    }

    fn ran(index: u64, mem: u64, steps: u64) -> LedgerRedeemer {
        LedgerRedeemer {
            purpose: purpose(index),
            evaluation: Evaluation::Ran(Units { mem, steps }),
        }
    }

    fn failed(index: u64) -> LedgerRedeemer {
        LedgerRedeemer {
            purpose: purpose(index),
            evaluation: Evaluation::Failed("script failed".to_string()),
        }
    }

    const ACCEPTED: LedgerOutcome = LedgerOutcome::Accepted {
        consumed: Vec::new(),
        produced: Vec::new(),
        accounts_post: BTreeMap::new(),
    };

    fn refused() -> LedgerOutcome {
        let names = NonEmpty::of(["BadInputsUTxO".to_string()]).unwrap();
        LedgerOutcome::Refused {
            failures: String::new(),
            failure_names: NonEmpty::of([FailurePath {
                names,
                class: crate::refusal::RefusalClass::Inputs,
            }])
            .unwrap(),
        }
    }

    #[test]
    fn equal_units_and_failures_on_both_sides_agree() {
        let ledger = [ran(0, 10, 20), failed(1)];
        let node = NodeCosts::Evaluated(vec![failed(1), ran(0, 10, 20)]);
        let mut t = CostTallies::default();
        t.add_units("supp#1", &pair(&ledger, &node));
        t.add_fee("supp#1", Some(7), Ok(7), &ACCEPTED);
        assert_eq!((t.units.compared, t.ran), (2, 1));
        t.skip(CostSkip::Refused);
        t.skip(CostSkip::Declared);
        t.skip(CostSkip::Declared);
        t.skip(CostSkip::Cascade);
        assert!(t.pass(), "{t}");
        assert_eq!(
            t.to_string(),
            "COST redeemers=2 ran_alike=1 fees=1 fees_unresolved_refused=0 fees_undecoded=0 skipped_refused=1 skipped_declared=2 skipped_cascade=1"
        );
    }

    #[test]
    fn a_refused_record_compares_a_fee_the_node_computes_and_counts_one_it_cannot_apart() {
        let mut t = CostTallies::default();
        t.add_fee("ic#1", Some(7), Ok(7), &refused());
        t.add_fee(
            "ic#2",
            Some(7),
            Err("missing input".to_string()),
            &refused(),
        );
        assert!(t.pass(), "{t}");
        assert_eq!((t.fee.compared, t.fee_unresolved_refused), (1, 1));
        t.add_fee("ic#3", Some(9), Ok(7), &refused());
        assert_eq!(t.fee.disagree, vec!["ic#3 ledger=9 dolos=7"]);
        assert!(!t.pass());
    }

    #[test]
    fn bytes_the_ledger_does_not_decode_have_no_fee_to_compare() {
        let mut t = CostTallies::default();
        t.add_fee("supp#1", None, Ok(7), &refused());
        t.add_fee("supp#2", None, Err("cbor error".to_string()), &refused());
        assert_eq!((t.fee.compared, t.fee_undecoded), (0, 2));
        assert!(t.fee.disagree.is_empty());
        t.add_fee("supp#3", Some(9), Ok(7), &refused());
        assert_eq!(t.fee.disagree, vec!["supp#3 ledger=9 dolos=7"]);
    }

    #[test]
    fn corpora_that_ran_nothing_alike_fail_together() {
        let mut failing = CostTallies::default();
        failing.add_units(
            "ic#1",
            &pair(&[failed(0)], &NodeCosts::Evaluated(vec![failed(0)])),
        );
        failing.add_fee("ic#1", Some(7), Ok(7), &ACCEPTED);
        assert!(failing.pass(), "{failing}");
        assert!(!pass_all([&failing]));
        let mut alike = CostTallies::default();
        let node = NodeCosts::Evaluated(vec![ran(0, 1, 1)]);
        alike.add_units("supp#1", &pair(&[ran(0, 1, 1)], &node));
        alike.add_fee("supp#1", Some(7), Ok(7), &ACCEPTED);
        assert!(pass_all([&failing, &alike]));
        assert!(!pass_all([&failing, &alike, &CostTallies::default()]));
    }

    #[test]
    fn differing_units_a_missing_side_or_no_evaluation_disagree() {
        let mut t = CostTallies::default();
        let node = NodeCosts::Evaluated(vec![ran(0, 10, 21), ran(2, 1, 1)]);
        t.add_units("supp#1", &pair(&[ran(0, 10, 20), ran(1, 1, 1)], &node));
        let unevaluated = NodeCosts::Unevaluated("unsupported".to_string());
        t.add_units("supp#2", &pair(&[ran(0, 1, 1)], &unevaluated));
        t.add_fee("supp#1", Some(7), Ok(8), &ACCEPTED);
        t.add_fee(
            "supp#2",
            Some(7),
            Err("no parameters".to_string()),
            &ACCEPTED,
        );
        assert!(!t.pass());
        assert_eq!(
            t.units.disagree,
            vec![
                "supp#1 purpose=0#0 ledger=(ran mem=10 steps=20) dolos=(ran mem=10 steps=21)",
                "supp#1 purpose=0#1 ledger=(ran mem=1 steps=1) dolos=(none)",
                "supp#1 purpose=0#2 ledger=(none) dolos=(ran mem=1 steps=1)",
                "supp#2 purpose=0#0 ledger=(ran mem=1 steps=1) dolos=(failed unevaluated unsupported)",
            ]
        );
        assert_eq!(
            t.fee.disagree,
            vec![
                "supp#1 ledger=7 dolos=8",
                "supp#2 ledger=7 dolos=none no parameters"
            ]
        );
    }
}
