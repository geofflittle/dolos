use std::{
    borrow::Borrow,
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use crate::{
    judgement::{self, Axis, CellKey, EntryPoint, Instrument, Judge, Verdict},
    manifest::Feature,
    record::{FailurePath, LedgerOutcome, NonEmpty, RecordId},
    refusal::{DolosClass, LedgerLeaf, RefusalClass},
    row::Row,
};

/// What Dolos answered when one transaction was submitted to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DolosVerdict {
    Accepted,
    Phase2Rejected,
    Phase2Unsupported(String),
    Unsupported(String),
    Refused { kind: String, class: DolosClass },
}

impl fmt::Display for DolosVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DolosVerdict::Accepted => write!(f, "accepted"),
            DolosVerdict::Phase2Rejected => write!(f, "phase2_rejected"),
            DolosVerdict::Phase2Unsupported(label) | DolosVerdict::Unsupported(label) => {
                write!(f, "unsupported[{label}]")
            }
            DolosVerdict::Refused { kind, .. } => write!(f, "refused[{kind}]"),
        }
    }
}

/// The ledger's answer for one record, printed for a disagreement.
pub fn show_ledger(outcome: &LedgerOutcome) -> String {
    match outcome {
        LedgerOutcome::Accepted { .. } => "accepted".to_string(),
        LedgerOutcome::Refused { failure_names, .. } => {
            let leaves: Vec<&str> = failure_names.iter().map(|p| p.leaf.name()).collect();
            format!("refused[{}]", leaves.join(","))
        }
    }
}

/// The outcome of a refusal by both sides, which agrees when the node's class is one of the ledger's.
fn refusal_outcome(
    failure_names: &NonEmpty<FailurePath>,
    ledger: String,
    kind: &str,
    class: Option<RefusalClass>,
) -> Outcome {
    let ledger_classes: BTreeSet<RefusalClass> =
        failure_names.iter().map(FailurePath::class).collect();
    match class {
        Some(class) if ledger_classes.contains(&class) => Outcome::Pass(Pass::Agree),
        Some(_) | None => {
            let ledger_classes: Vec<&str> = ledger_classes.iter().map(|c| c.name()).collect();
            Outcome::Fail(Fail::ReasonMismatch {
                ledger,
                ledger_classes: ledger_classes.join(","),
                dolos: format!("refused[{kind}]"),
                dolos_class: class
                    .map_or(DolosClass::NoRule, DolosClass::Rule)
                    .name()
                    .to_string(),
            })
        }
    }
}

/// A judgement of one record that leaves the arm passing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pass {
    Agree,
    UnsupportedExpected { label: String },
    RefuseUnsupported { label: String },
    Known { row: Row, shown: String },
}

/// A judgement of one record that fails the arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fail {
    Disagree {
        ledger: String,
        dolos: String,
    },
    ReasonMismatch {
        ledger: String,
        ledger_classes: String,
        dolos: String,
        dolos_class: String,
    },
    UnsupportedUnexpected {
        label: String,
    },
    StaleManifest {
        declared: String,
        dolos: String,
    },
    KnownStale {
        row: Row,
    },
    UnknownRefusal {
        dolos: String,
    },
    RelayDependent {
        held: String,
        inflight: String,
    },
}

/// The one answer Dolos gave for a record submitted with the earlier records pending and again with them inflight.
pub fn one_answer<'v>(
    held: &'v DolosVerdict,
    inflight: &'v DolosVerdict,
) -> Result<&'v DolosVerdict, Fail> {
    if held == inflight {
        Ok(held)
    } else {
        Err(Fail::RelayDependent {
            held: held.to_string(),
            inflight: inflight.to_string(),
        })
    }
}

impl Fail {
    /// The name of the failure's kind, the member of its `failure` cell.
    pub fn kind(&self) -> &'static str {
        match self {
            Fail::Disagree { .. } => "disagree",
            Fail::ReasonMismatch { .. } => "reason_mismatch",
            Fail::UnsupportedUnexpected { .. } => "unsupported_unexpected",
            Fail::StaleManifest { .. } => "stale_manifest",
            Fail::KnownStale { .. } => "known_stale",
            Fail::UnknownRefusal { .. } => "unknown_refusal",
            Fail::RelayDependent { .. } => "relay_dependent",
        }
    }

    /// The cells the failure touches: its kind, and the answers or label it
    /// names.
    pub fn cells(&self) -> Vec<CellKey> {
        let mut cells = vec![CellKey::new(Axis::Failure, self.kind())];
        match self {
            Fail::Disagree { ledger, dolos } | Fail::ReasonMismatch { ledger, dolos, .. } => {
                cells.push(CellKey::new(Axis::Ledger, answer_member(ledger)));
                cells.push(CellKey::new(Axis::Dolos, answer_member(dolos)));
            }
            Fail::UnsupportedUnexpected { label } => {
                cells.push(CellKey::new(Axis::Unsupported, label));
            }
            Fail::StaleManifest { declared, dolos } => {
                cells.push(CellKey::new(Axis::Unsupported, declared));
                cells.push(CellKey::new(Axis::Dolos, answer_member(dolos)));
            }
            Fail::UnknownRefusal { dolos } => {
                cells.push(CellKey::new(Axis::Dolos, answer_member(dolos)));
            }
            Fail::KnownStale { row } => cells.push(CellKey::new(Axis::Row, row)),
            Fail::RelayDependent { .. } => {}
        }
        cells
    }
}

/// The member an answer names: `accepted`, or the refusal's variant for
/// `refused[Variant(..)]`.
fn answer_member(answer: &str) -> &str {
    match answer.strip_prefix("refused[") {
        Some(inner) => inner.split(['(', ']', ' ', ',']).next().unwrap_or(inner),
        None => answer,
    }
}

impl fmt::Display for Fail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fail::Disagree { ledger, dolos } => write!(f, "ledger={ledger} dolos={dolos}"),
            Fail::ReasonMismatch {
                ledger,
                ledger_classes,
                dolos,
                dolos_class,
            } => write!(
                f,
                "ledger={ledger} ledger_class={ledger_classes} dolos={dolos} \
                 dolos_class={dolos_class}"
            ),
            Fail::UnsupportedUnexpected { label } => write!(f, "label={label}"),
            Fail::StaleManifest { declared, dolos } => {
                write!(f, "declared={declared} dolos={dolos}")
            }
            Fail::KnownStale { row } => write!(f, "row={row}"),
            Fail::UnknownRefusal { dolos } => write!(f, "dolos={dolos}"),
            Fail::RelayDependent { held, inflight } => {
                write!(f, "held={held} inflight={inflight}")
            }
        }
    }
}

/// A record Dolos refused for its inputs that spends outputs failing makers in its group did not leave, made only by `with_failing_makers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cascade {
    makers: NonEmpty<FailingMaker>,
    fail: Fail,
}

impl fmt::Display for Cascade {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let makers: Vec<String> = self.makers.iter().map(|m| m.0.to_string()).collect();
        write!(f, "maker={} {}", makers.join(","), self.fail)
    }
}

/// The judgement of one record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pass(Pass),
    Fail(Fail),
    Cascade(Cascade),
}

/// The outcome of one record from the ledger's answer under a true flag, Dolos's answer and the feature it is declared unsupported for.
pub fn outcome(
    ledger: &LedgerOutcome,
    dolos: &DolosVerdict,
    declared: Option<&Feature>,
) -> Outcome {
    use DolosVerdict as D;
    use LedgerOutcome as L;
    let disagree = || {
        Outcome::Fail(Fail::Disagree {
            ledger: show_ledger(ledger),
            dolos: dolos.to_string(),
        })
    };
    match (ledger, dolos, declared) {
        (
            _,
            D::Refused {
                class: DolosClass::Unknown,
                ..
            },
            _,
        ) => Outcome::Fail(Fail::UnknownRefusal {
            dolos: dolos.to_string(),
        }),
        (L::Accepted { .. }, D::Phase2Unsupported(l) | D::Unsupported(l), Some(f)) if *l == f.0 => {
            Outcome::Pass(Pass::UnsupportedExpected { label: l.clone() })
        }
        (L::Refused { .. }, D::Phase2Unsupported(l) | D::Unsupported(l), Some(f)) if *l == f.0 => {
            Outcome::Pass(Pass::RefuseUnsupported { label: l.clone() })
        }
        (_, D::Phase2Unsupported(l) | D::Unsupported(l), _) => {
            Outcome::Fail(Fail::UnsupportedUnexpected { label: l.clone() })
        }
        (_, D::Accepted | D::Phase2Rejected | D::Refused { .. }, Some(f)) => {
            Outcome::Fail(Fail::StaleManifest {
                declared: f.to_string(),
                dolos: dolos.to_string(),
            })
        }
        (L::Accepted { .. }, D::Accepted, None) => Outcome::Pass(Pass::Agree),
        // Under a true flag the ledger's tag mismatch means the scripts
        // failed, which Dolos answers by rejecting them.
        (L::Refused { failure_names, .. }, D::Phase2Rejected, None)
            if failure_names
                .iter()
                .all(|p| p.leaf == LedgerLeaf::ValidationTagMismatch) =>
        {
            Outcome::Pass(Pass::Agree)
        }
        (
            L::Refused { failure_names, .. },
            D::Refused {
                kind,
                class: DolosClass::Rule(c),
            },
            None,
        ) => refusal_outcome(failure_names, show_ledger(ledger), kind, Some(*c)),
        (
            L::Refused { failure_names, .. },
            D::Refused {
                kind,
                class: DolosClass::NoRule,
            },
            None,
        ) => refusal_outcome(failure_names, show_ledger(ledger), kind, None),
        (L::Accepted { .. }, D::Phase2Rejected | D::Refused { .. }, None)
        | (L::Refused { .. }, D::Accepted | D::Phase2Rejected, None) => disagree(),
    }
}

/// The feature a record would be declared unsupported for, from a Dolos that named it unsupported.
pub fn declares(dolos: &DolosVerdict) -> Option<Feature> {
    match dolos {
        DolosVerdict::Phase2Unsupported(label) | DolosVerdict::Unsupported(label) => {
            Some(Feature(label.clone()))
        }
        DolosVerdict::Accepted | DolosVerdict::Phase2Rejected | DolosVerdict::Refused { .. } => {
            None
        }
    }
}

/// The outcome of a record on the known list, which passes only while the record still fails and keeps an unknown refusal failing.
pub fn with_known(outcome: Outcome, row: Option<&Row>) -> Outcome {
    match (row, outcome) {
        (None, outcome) | (Some(_), outcome @ Outcome::Fail(Fail::UnknownRefusal { .. })) => {
            outcome
        }
        (Some(&row), Outcome::Fail(fail)) => Outcome::Pass(Pass::Known {
            row,
            shown: fail.to_string(),
        }),
        (Some(&row), Outcome::Cascade(cascade)) => Outcome::Pass(Pass::Known {
            row,
            shown: cascade.to_string(),
        }),
        (Some(&row), Outcome::Pass(_)) => Outcome::Fail(Fail::KnownStale { row }),
    }
}

/// A record whose outputs are seeded for the records that spend them, made only by `maker`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SeedingMaker(RecordId);

impl Borrow<RecordId> for SeedingMaker {
    fn borrow(&self) -> &RecordId {
        &self.0
    }
}

/// A record Dolos did not take whose outcome does not pass, made only by `maker`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FailingMaker(RecordId);

impl Borrow<RecordId> for FailingMaker {
    fn borrow(&self) -> &RecordId {
        &self.0
    }
}

/// A record as the maker of outputs that later records in its group spend, by whether Dolos took it and whether it passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Maker {
    Taken,
    Seeds(SeedingMaker),
    Fails(FailingMaker),
}

/// The record as a maker, from what Dolos answered and its outcome.
pub fn maker(id: RecordId, dolos: &DolosVerdict, outcome: &Outcome) -> Maker {
    use DolosVerdict as D;
    match (dolos, outcome) {
        (D::Accepted, Outcome::Pass(_) | Outcome::Fail(_) | Outcome::Cascade(_)) => Maker::Taken,
        // An unsupported answer says nothing of the outputs, so later records
        // spend the ledger's outputs.
        (
            D::Phase2Rejected | D::Phase2Unsupported(_) | D::Unsupported(_) | D::Refused { .. },
            Outcome::Pass(_),
        )
        | (
            D::Phase2Unsupported(_) | D::Unsupported(_),
            Outcome::Fail(Fail::UnsupportedUnexpected { .. }),
        ) => Maker::Seeds(SeedingMaker(id)),
        (
            D::Phase2Rejected | D::Phase2Unsupported(_) | D::Unsupported(_) | D::Refused { .. },
            Outcome::Fail(_) | Outcome::Cascade(_),
        ) => Maker::Fails(FailingMaker(id)),
    }
}

/// The outcome of a record that spends outputs of the failing makers, a cascade of them when it fails and Dolos refused it for its inputs.
pub fn with_failing_makers(
    outcome: Outcome,
    dolos: &DolosVerdict,
    makers: BTreeSet<FailingMaker>,
) -> Outcome {
    use DolosVerdict as D;
    let inputs = match dolos {
        D::Refused {
            class: DolosClass::Rule(RefusalClass::Inputs),
            ..
        } => true,
        D::Refused { .. }
        | D::Accepted
        | D::Phase2Rejected
        | D::Phase2Unsupported(_)
        | D::Unsupported(_) => false,
    };
    let Some(makers) = NonEmpty::of(makers) else {
        return outcome;
    };
    match outcome {
        Outcome::Fail(fail @ (Fail::Disagree { .. } | Fail::ReasonMismatch { .. })) if inputs => {
            Outcome::Cascade(Cascade { makers, fail })
        }
        Outcome::Fail(_) | Outcome::Pass(_) | Outcome::Cascade(_) => outcome,
    }
}

/// The validation outcomes of one arm, with the records behind each one that is printed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ValidationTally {
    pub dependent: Vec<(RecordId, BTreeSet<RecordId>)>,
    pub agree: BTreeMap<RecordId, Vec<CellKey>>,
    pub unsupported_expected: BTreeMap<String, usize>,
    pub refuse_unsupported: Vec<(RecordId, String)>,
    pub known: Vec<(RecordId, Row, String)>,
    pub cascade: Vec<(RecordId, String)>,
    pub failing: Vec<Verdict>,
}

impl ValidationTally {
    /// The number of failing records of the failure kind `kind`.
    fn failed(&self, kind: &str) -> usize {
        let cell = CellKey::new(Axis::Failure, kind);
        self.failing
            .iter()
            .filter(|v| v.cells.contains(&cell))
            .count()
    }

    /// The number of records the arm judged with a pass.
    pub fn passing(&self) -> usize {
        self.agree.len()
            + self.unsupported_expected.values().sum::<usize>()
            + self.refuse_unsupported.len()
            + self.known.len()
    }

    /// Adds the arm's verdicts to `out`.
    pub fn report(&self, out: &mut judgement::Tally) {
        let passed = |cells, pass| judgement::Passed {
            judge: Judge::Validation,
            entry: Some(EntryPoint::Instrument(Instrument::ReceiveTx)),
            cells,
            pass,
        };
        for cells in self.agree.values() {
            out.pass(passed(cells.clone(), judgement::Pass::Agree), 1);
        }
        for (label, n) in &self.unsupported_expected {
            let cells = vec![CellKey::new(Axis::Unsupported, label)];
            out.pass(passed(cells, judgement::Pass::Excluded), *n);
        }
        let excluded = self.refuse_unsupported.len();
        out.pass(passed(Vec::new(), judgement::Pass::Excluded), excluded);
        out.pass(passed(Vec::new(), judgement::Pass::Known), self.known.len());
        self.failing.iter().for_each(|v| out.add(v.clone()));
    }

    /// Counts the outcome of one record, which touches `cells` when it agrees.
    pub fn add(&mut self, id: RecordId, outcome: Outcome, cells: Vec<CellKey>) {
        match outcome {
            Outcome::Pass(Pass::Agree) => {
                self.agree.insert(id, cells);
            }
            Outcome::Pass(Pass::UnsupportedExpected { label }) => {
                *self.unsupported_expected.entry(label).or_default() += 1
            }
            Outcome::Pass(Pass::RefuseUnsupported { label }) => {
                self.refuse_unsupported.push((id, format!("label={label}")))
            }
            Outcome::Pass(Pass::Known { row, shown }) => self.known.push((id, row, shown)),
            Outcome::Fail(fail) => self.failing.push(Verdict::disagree(
                Judge::Validation,
                EntryPoint::Instrument(Instrument::ReceiveTx),
                id,
                fail.cells(),
                fail.to_string(),
            )),
            Outcome::Cascade(cascade) => self.cascade.push((id, cascade.to_string())),
        }
    }

    /// Adds the outcomes of another arm part after this one's.
    pub fn merge(&mut self, other: ValidationTally) {
        let ValidationTally {
            dependent,
            agree,
            unsupported_expected,
            refuse_unsupported,
            known,
            cascade,
            failing,
        } = other;
        self.dependent.extend(dependent);
        self.agree.extend(agree);
        for (label, n) in unsupported_expected {
            *self.unsupported_expected.entry(label).or_default() += n;
        }
        self.refuse_unsupported.extend(refuse_unsupported);
        self.known.extend(known);
        self.cascade.extend(cascade);
        self.failing.extend(failing);
    }
}

impl fmt::Display for ValidationTally {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ValidationTally {
            dependent,
            agree,
            unsupported_expected,
            refuse_unsupported,
            known,
            cascade,
            failing: _,
        } = self;
        write!(
            f,
            "VALIDATION agree={} disagree={} reason_mismatch={} cascade={} \
             unsupported_expected={} unsupported_unexpected={} refuse_unsupported={} \
             stale_manifest={} dependent={} known={} known_stale={} unknown_refusal={}",
            agree.len(),
            self.failed("disagree") + self.failed("relay_dependent"),
            self.failed("reason_mismatch"),
            cascade.len(),
            unsupported_expected.values().sum::<usize>(),
            self.failed("unsupported_unexpected"),
            refuse_unsupported.len(),
            self.failed("stale_manifest"),
            dependent.len(),
            known.len(),
            self.failed("known_stale"),
            self.failed("unknown_refusal")
        )?;
        let dependent: Vec<String> = dependent
            .iter()
            .map(|(id, makers)| {
                let makers: Vec<String> = makers.iter().map(RecordId::to_string).collect();
                format!("{id}({})", makers.join(","))
            })
            .collect();
        write!(f, "\nDEPENDENT ids={}", dependent.join(","))?;
        for (id, row, shown) in known {
            write!(f, "\nKNOWN row={row} ids={id} {shown}")?;
        }
        for (name, lines) in [
            ("CASCADE", cascade),
            ("REFUSE_UNSUPPORTED", refuse_unsupported),
        ] {
            for (id, text) in lines {
                write!(f, "\n{name} {id} {text}")?;
            }
        }
        for (label, count) in unsupported_expected {
            write!(f, "\nUNSUPPORTED_EXPECTED label={label} count={count}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::CorpusKind;

    fn part(n: u32, label: &str) -> ValidationTally {
        let line = || vec![(supp(n), format!("line{n}"))];
        ValidationTally {
            dependent: vec![(supp(n), BTreeSet::from([supp(n + 10)]))],
            agree: BTreeMap::from([(supp(n + 20), vec![leaf_cell(n)])]),
            unsupported_expected: BTreeMap::from([(label.to_string(), 1), ("both".to_string(), 2)]),
            refuse_unsupported: line(),
            known: vec![(supp(n), Row::S1, format!("line{n}"))],
            cascade: line(),
            failing: vec![disagreeing(n)],
        }
    }

    fn leaf_cell(n: u32) -> CellKey {
        CellKey::new(Axis::Leaf, format!("Leaf{n}"))
    }

    fn disagreeing(n: u32) -> Verdict {
        let fail = Fail::Disagree {
            ledger: "accepted".into(),
            dolos: "refused[CborDecodingError]".into(),
        };
        Verdict::disagree(
            Judge::Validation,
            EntryPoint::Instrument(Instrument::ReceiveTx),
            supp(n),
            fail.cells(),
            fail.to_string(),
        )
    }

    #[test]
    fn a_failing_outcome_names_its_kind_and_both_answers_as_cells() {
        let mut t = ValidationTally::default();
        let fail = Fail::Disagree {
            ledger: "refused[PtrPresentInCollateralReturn]".into(),
            dolos: "refused[DijkstraInvalidAccount(Credential)]".into(),
        };
        t.add(supp(3), Outcome::Fail(fail), vec![leaf_cell(3)]);
        assert_eq!(
            t.failing[0].to_string(),
            "VERDICT judge=validation entry=receive_tx record=supp#3 \
             cells=failure:disagree,ledger:PtrPresentInCollateralReturn,dolos:DijkstraInvalidAccount \
             outcome=disagree ledger=refused[PtrPresentInCollateralReturn] \
             dolos=refused[DijkstraInvalidAccount(Credential)]"
        );
        t.add(supp(4), Outcome::Pass(Pass::Agree), vec![leaf_cell(4)]);
        assert_eq!((t.passing(), t.failing.len()), (1, 1));
    }

    #[test]
    fn an_agreeing_outcome_reports_its_cells_and_a_known_one_does_not() {
        let mut t = ValidationTally::default();
        let shown = String::new();
        t.add(
            supp(2),
            Outcome::Pass(Pass::Known {
                row: Row::S1,
                shown,
            }),
            vec![leaf_cell(2)],
        );
        t.add(supp(1), Outcome::Pass(Pass::Agree), vec![leaf_cell(1)]);
        assert_eq!(t.agree, BTreeMap::from([(supp(1), vec![leaf_cell(1)])]));
        let mut out = judgement::Tally::default();
        t.report(&mut out);
        let passed: Vec<(Vec<CellKey>, judgement::Pass, usize)> = out
            .passed(Judge::Validation)
            .map(|(p, n)| (p.cells.clone(), p.pass, n))
            .collect();
        assert_eq!(
            passed,
            [
                (vec![], judgement::Pass::Known, 1),
                (vec![leaf_cell(1)], judgement::Pass::Agree, 1),
            ]
        );
    }

    #[test]
    fn a_merged_part_adds_every_count_and_keeps_every_line_after_the_first_part() {
        let mut merged = part(0, "first");
        merged.merge(part(1, "second"));
        let lines = || {
            vec![
                (supp(0), "line0".to_string()),
                (supp(1), "line1".to_string()),
            ]
        };
        let both = ValidationTally {
            dependent: vec![
                (supp(0), BTreeSet::from([supp(10)])),
                (supp(1), BTreeSet::from([supp(11)])),
            ],
            agree: BTreeMap::from([
                (supp(20), vec![leaf_cell(0)]),
                (supp(21), vec![leaf_cell(1)]),
            ]),
            unsupported_expected: BTreeMap::from([
                ("both".to_string(), 4),
                ("first".to_string(), 1),
                ("second".to_string(), 1),
            ]),
            refuse_unsupported: lines(),
            known: vec![
                (supp(0), Row::S1, "line0".to_string()),
                (supp(1), Row::S1, "line1".to_string()),
            ],
            cascade: lines(),
            failing: vec![disagreeing(0), disagreeing(1)],
        };
        assert_eq!(merged, both);
    }

    #[test]
    fn merging_an_empty_part_changes_nothing() {
        let mut merged = part(0, "first");
        merged.merge(ValidationTally::default());
        assert_eq!(merged, part(0, "first"));
    }

    fn accepted() -> LedgerOutcome {
        LedgerOutcome::Accepted {
            consumed: vec![],
            produced: vec![],
            accounts_post: Default::default(),
        }
    }

    fn path(leaf: &str) -> FailurePath {
        FailurePath {
            leaf: LedgerLeaf::of_name(leaf).unwrap(),
        }
    }

    fn refused_by(leaves: &[&str]) -> LedgerOutcome {
        LedgerOutcome::Refused {
            failures: String::new(),
            failure_names: NonEmpty {
                head: path(leaves[0]),
                tail: leaves[1..].iter().map(|l| path(l)).collect(),
            },
        }
    }

    fn refused(leaf: &str) -> LedgerOutcome {
        refused_by(&[leaf])
    }

    fn dolos_refused(kind: &str, class: DolosClass) -> DolosVerdict {
        DolosVerdict::Refused {
            kind: kind.into(),
            class,
        }
    }

    #[test]
    fn the_same_answer_in_both_relay_states_is_that_answer() {
        let refused = dolos_refused("InputNotInUTxO", DolosClass::Rule(RefusalClass::Inputs));
        for verdict in [DolosVerdict::Accepted, refused] {
            assert_eq!(one_answer(&verdict, &verdict.clone()), Ok(&verdict));
        }
    }

    #[test]
    fn different_answers_in_the_two_relay_states_disagree() {
        let refused = dolos_refused("WithdrawalNotRegistered", DolosClass::NoRule);
        let fail = one_answer(&DolosVerdict::Accepted, &refused).unwrap_err();
        let mut tally = ValidationTally::default();
        tally.add(supp(3), Outcome::Fail(fail), Vec::new());
        assert_eq!(
            tally.failing.iter().map(Verdict::to_string).collect::<Vec<_>>(),
            vec![
                "VERDICT judge=validation entry=receive_tx record=supp#3 cells=failure:relay_dependent \
                 outcome=disagree held=accepted inflight=refused[WithdrawalNotRegistered]"
            ]
        );
    }

    #[test]
    fn a_refusal_in_any_of_the_ledger_classes_agrees() {
        let ledger = refused_by(&["SubMissingTxMetadata", "BadInputsUTxO"]);
        for (kind, class) in [
            ("MetadataHash", RefusalClass::Metadata),
            ("InputNotInUTxO", RefusalClass::Inputs),
        ] {
            assert_eq!(
                outcome(&ledger, &dolos_refused(kind, DolosClass::Rule(class)), None),
                Outcome::Pass(Pass::Agree),
                "{kind}"
            );
        }
    }

    #[test]
    fn a_refusal_outside_the_ledger_classes_is_a_reason_mismatch() {
        let ledger = refused_by(&["BadInputsUTxO", "BadInputsUTxO"]);
        let mut tally = ValidationTally::default();
        for (n, kind, class) in [
            (
                5,
                "PreservationOfValue",
                DolosClass::Rule(RefusalClass::Value),
            ),
            (6, "DijkstraMissingParameters", DolosClass::NoRule),
        ] {
            let dolos = dolos_refused(kind, class);
            tally.add(supp(n), outcome(&ledger, &dolos, None), Vec::new());
        }
        assert_eq!(
            tally.failing.iter().map(Verdict::to_string).collect::<Vec<_>>(),
            [
                "VERDICT judge=validation entry=receive_tx record=supp#5 \
                 cells=failure:reason_mismatch,ledger:BadInputsUTxO,dolos:PreservationOfValue \
                 outcome=disagree ledger=refused[BadInputsUTxO,BadInputsUTxO] \
                 ledger_class=inputs dolos=refused[PreservationOfValue] dolos_class=value",
                "VERDICT judge=validation entry=receive_tx record=supp#6 \
                 cells=failure:reason_mismatch,ledger:BadInputsUTxO,dolos:DijkstraMissingParameters \
                 outcome=disagree ledger=refused[BadInputsUTxO,BadInputsUTxO] \
                 ledger_class=inputs dolos=refused[DijkstraMissingParameters] dolos_class=none"
            ]
        );
        assert!(tally.to_string().contains(" reason_mismatch=2 "));
    }

    #[test]
    fn a_refusal_of_a_variant_the_table_names_is_never_unknown() {
        for ledger in [accepted(), refused("BadInputsUTxO")] {
            for class in [
                DolosClass::NoRule,
                DolosClass::Rule(RefusalClass::Value),
                DolosClass::Rule(RefusalClass::Inputs),
            ] {
                let judged = outcome(&ledger, &dolos_refused("Named", class), None);
                assert!(
                    !matches!(judged, Outcome::Fail(Fail::UnknownRefusal { .. })),
                    "{class:?} {judged:?}"
                );
            }
        }
    }

    #[test]
    fn an_unknown_refusal_fails_whatever_the_ledger_said_and_on_the_known_list() {
        let unknown = dolos_refused("FutureVariant", DolosClass::Unknown);
        let expected = Outcome::Fail(Fail::UnknownRefusal {
            dolos: "refused[FutureVariant]".into(),
        });
        for ledger in [accepted(), refused("BadInputsUTxO")] {
            let judged = outcome(&ledger, &unknown, None);
            assert_eq!(judged, expected);
            assert_eq!(with_known(judged, Some(&Row::V2)), expected);
        }
        let mut tally = ValidationTally::default();
        tally.add(supp(4), expected, Vec::new());
        assert_eq!(
            tally
                .failing
                .iter()
                .map(Verdict::to_string)
                .collect::<Vec<_>>(),
            ["VERDICT judge=validation entry=receive_tx record=supp#4 \
                 cells=failure:unknown_refusal,dolos:FutureVariant outcome=disagree \
                 dolos=refused[FutureVariant]"]
        );
    }

    fn supp(n: u32) -> RecordId {
        RecordId {
            corpus: CorpusKind::Supp,
            n,
        }
    }

    fn feature(label: &str) -> Feature {
        Feature(label.to_string())
    }

    #[test]
    fn matching_answers_agree() {
        let agree = Outcome::Pass(Pass::Agree);
        assert_eq!(outcome(&accepted(), &DolosVerdict::Accepted, None), agree);
        assert_eq!(
            outcome(&refused("BadInputsUTxO"), &input_refusal(), None),
            agree
        );
    }

    #[test]
    fn a_tag_mismatch_disagrees_with_accepting() {
        let judged = outcome(
            &refused("ValidationTagMismatch"),
            &DolosVerdict::Accepted,
            None,
        );
        assert!(
            matches!(judged, Outcome::Fail(Fail::Disagree { .. })),
            "{judged:?}"
        );
    }

    #[test]
    fn failing_scripts_agree_only_with_a_tag_mismatch_alone() {
        let agree = Outcome::Pass(Pass::Agree);
        for ledger in [
            accepted(),
            refused_by(&["ValidationTagMismatch", "BadInputsUTxO"]),
        ] {
            let judged = outcome(&ledger, &DolosVerdict::Phase2Rejected, None);
            assert!(
                matches!(judged, Outcome::Fail(Fail::Disagree { .. })),
                "{judged:?}"
            );
        }
        let mismatch = refused_by(&["ValidationTagMismatch", "ValidationTagMismatch"]);
        assert_eq!(
            outcome(&mismatch, &DolosVerdict::Phase2Rejected, None),
            agree
        );
    }

    #[test]
    fn opposite_answers_disagree_and_name_both_sides() {
        let o = outcome(&refused("BadInputsUTxO"), &DolosVerdict::Accepted, None);
        let mut tally = ValidationTally::default();
        tally.add(supp(5), o, Vec::new());
        assert_eq!(
            tally.to_string(),
            "VALIDATION agree=0 disagree=1 reason_mismatch=0 cascade=0 unsupported_expected=0 \
             unsupported_unexpected=0 \
             refuse_unsupported=0 stale_manifest=0 dependent=0 known=0 known_stale=0 \
             unknown_refusal=0\n\
             DEPENDENT ids="
        );
        assert_eq!(
            tally
                .failing
                .iter()
                .map(Verdict::to_string)
                .collect::<Vec<_>>(),
            ["VERDICT judge=validation entry=receive_tx record=supp#5 \
                 cells=failure:disagree,ledger:BadInputsUTxO,dolos:accepted outcome=disagree \
                 ledger=refused[BadInputsUTxO] dolos=accepted"]
        );
    }

    #[test]
    fn a_declared_label_passes_only_when_dolos_names_it() {
        let unsupported = DolosVerdict::Unsupported("certificate kind".into());
        assert_eq!(
            outcome(
                &accepted(),
                &unsupported,
                Some(&feature("certificate kind"))
            ),
            Outcome::Pass(Pass::UnsupportedExpected {
                label: "certificate kind".into()
            })
        );
        assert_eq!(
            outcome(&accepted(), &unsupported, Some(&feature("pointer address"))),
            Outcome::Fail(Fail::UnsupportedUnexpected {
                label: "certificate kind".into()
            })
        );
        assert_eq!(
            outcome(&accepted(), &unsupported, None),
            Outcome::Fail(Fail::UnsupportedUnexpected {
                label: "certificate kind".into()
            })
        );
    }

    #[test]
    fn a_declared_record_that_dolos_supports_is_a_stale_manifest() {
        assert_eq!(
            outcome(
                &accepted(),
                &DolosVerdict::Accepted,
                Some(&feature("certificate kind"))
            ),
            Outcome::Fail(Fail::StaleManifest {
                declared: "certificate kind".into(),
                dolos: "accepted".into()
            })
        );
        let stale = Fail::StaleManifest {
            declared: "certificate kind".into(),
            dolos: "refused[Phase2EvaluationError]".into(),
        };
        let cells = stale
            .cells()
            .iter()
            .map(CellKey::to_string)
            .collect::<Vec<_>>();
        assert_eq!(
            cells,
            [
                "failure:stale_manifest",
                "unsupported:certificate_kind",
                "dolos:Phase2EvaluationError"
            ]
        );
    }

    #[test]
    fn a_record_the_ledger_refuses_passes_unsupported_only_under_its_declared_label() {
        let unsupported = DolosVerdict::Unsupported("witness fields".into());
        let ledger = refused("MissingVKeyWitnessesUTXOW");
        assert_eq!(
            outcome(&ledger, &unsupported, Some(&feature("witness fields"))),
            Outcome::Pass(Pass::RefuseUnsupported {
                label: "witness fields".into()
            })
        );
        for declared in [None, Some(&feature("pointer address"))] {
            assert_eq!(
                outcome(&ledger, &unsupported, declared),
                Outcome::Fail(Fail::UnsupportedUnexpected {
                    label: "witness fields".into()
                })
            );
        }
    }

    #[test]
    fn a_known_record_passes_while_it_fails_and_fails_once_it_agrees() {
        let row = Row::V2;
        let failing = outcome(&accepted(), &input_refusal(), None);
        assert_eq!(
            with_known(failing.clone(), None),
            failing,
            "a record off the list keeps its outcome"
        );
        assert_eq!(
            with_known(failing, Some(&row)),
            Outcome::Pass(Pass::Known {
                row,
                shown: "ledger=accepted dolos=refused[InputNotInUTxO]".into()
            })
        );
        assert_eq!(
            with_known(Outcome::Pass(Pass::Agree), Some(&row)),
            Outcome::Fail(Fail::KnownStale { row })
        );
    }

    #[test]
    fn a_maker_dolos_did_not_take_seeds_when_it_passes_or_is_unsupported_and_fails_its_dependents_otherwise(
    ) {
        let unsupported = DolosVerdict::Unsupported("certificate kind".into());
        let expected = outcome(
            &accepted(),
            &unsupported,
            Some(&feature("certificate kind")),
        );
        let unexpected = outcome(&accepted(), &unsupported, None);
        let stale = outcome(
            &accepted(),
            &input_refusal(),
            Some(&feature("certificate kind")),
        );
        let held = Outcome::Pass(Pass::Agree);
        assert_eq!(
            maker(supp(3), &unsupported, &expected),
            Maker::Seeds(SeedingMaker(supp(3)))
        );
        assert!(matches!(
            unexpected,
            Outcome::Fail(Fail::UnsupportedUnexpected { .. })
        ));
        assert_eq!(
            maker(supp(3), &unsupported, &unexpected),
            Maker::Seeds(SeedingMaker(supp(3)))
        );
        assert!(matches!(stale, Outcome::Fail(Fail::StaleManifest { .. })));
        assert_eq!(
            maker(supp(3), &input_refusal(), &stale),
            Maker::Fails(FailingMaker(supp(3)))
        );
        assert_eq!(maker(supp(3), &DolosVerdict::Accepted, &held), Maker::Taken);
        assert_eq!(
            maker(supp(3), &DolosVerdict::Accepted, &unexpected),
            Maker::Taken
        );
    }

    fn input_refusal() -> DolosVerdict {
        dolos_refused("InputNotInUTxO", DolosClass::Rule(RefusalClass::Inputs))
    }

    fn failing(ns: &[u32]) -> BTreeSet<FailingMaker> {
        ns.iter().map(|&n| FailingMaker(supp(n))).collect()
    }

    #[test]
    fn a_dependent_refused_for_another_rule_or_with_no_failing_maker_keeps_its_outcome() {
        let value = dolos_refused("PreservationOfValue", DolosClass::Rule(RefusalClass::Value));
        let refused_for_value = outcome(&accepted(), &value, None);
        assert_eq!(
            with_failing_makers(refused_for_value.clone(), &value, failing(&[1])),
            refused_for_value
        );
        let refused_for_inputs = outcome(&accepted(), &input_refusal(), None);
        assert_eq!(
            with_failing_makers(refused_for_inputs.clone(), &input_refusal(), failing(&[])),
            refused_for_inputs
        );
        let agree = outcome(&refused("BadInputsUTxO"), &input_refusal(), None);
        assert_eq!(
            with_failing_makers(agree.clone(), &input_refusal(), failing(&[1])),
            agree
        );
        let accepted_by_dolos = outcome(&refused("BadInputsUTxO"), &DolosVerdict::Accepted, None);
        assert_eq!(
            with_failing_makers(
                accepted_by_dolos.clone(),
                &DolosVerdict::Accepted,
                failing(&[1])
            ),
            accepted_by_dolos
        );
        let stale = Outcome::Fail(Fail::KnownStale { row: Row::V2 });
        assert_eq!(
            with_failing_makers(stale.clone(), &input_refusal(), failing(&[1])),
            stale
        );
    }

    #[test]
    fn a_dependent_refused_for_its_inputs_is_a_cascade_naming_its_failing_makers() {
        let mut tally = ValidationTally::default();
        let maker_outcome = outcome(
            &accepted(),
            &DolosVerdict::Unsupported("plutus v4".into()),
            None,
        );
        tally.add(supp(1), maker_outcome, Vec::new());
        let disagree = outcome(&accepted(), &input_refusal(), None);
        tally.add(
            supp(2),
            with_failing_makers(disagree, &input_refusal(), failing(&[1])),
            Vec::new(),
        );
        let mismatch = outcome(
            &refused_by(&["ExtraRedeemers", "CollectErrors"]),
            &input_refusal(),
            None,
        );
        tally.add(
            supp(3),
            with_failing_makers(mismatch, &input_refusal(), failing(&[1, 2])),
            Vec::new(),
        );
        assert!(
            !tally.failing.is_empty(),
            "the failing maker still fails the arm"
        );
        let text = tally.to_string();
        assert!(
            text.starts_with("VALIDATION agree=0 disagree=0 reason_mismatch=0 cascade=2 "),
            "{text}"
        );
        assert!(
            text.contains(
                "\nCASCADE supp#2 maker=supp#1 ledger=accepted dolos=refused[InputNotInUTxO]\n"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "\nCASCADE supp#3 maker=supp#1,supp#2 ledger=refused[ExtraRedeemers,CollectErrors] \
                 ledger_class=script dolos=refused[InputNotInUTxO] dolos_class=inputs"
            ),
            "{text}"
        );
        assert_eq!(
            tally
                .failing
                .iter()
                .map(Verdict::to_string)
                .collect::<Vec<_>>(),
            ["VERDICT judge=validation entry=receive_tx record=supp#1 \
                 cells=failure:unsupported_unexpected,unsupported:plutus_v4 outcome=disagree \
                 label=plutus v4"]
        );
    }

    #[test]
    fn a_cascade_is_a_failing_maker_and_a_known_row_holds_it() {
        let disagree = outcome(&accepted(), &input_refusal(), None);
        let cascade = with_failing_makers(disagree, &input_refusal(), failing(&[1]));
        assert_eq!(
            maker(supp(2), &input_refusal(), &cascade),
            Maker::Fails(FailingMaker(supp(2)))
        );
        assert_eq!(
            with_known(cascade, Some(&Row::V2)),
            Outcome::Pass(Pass::Known {
                row: Row::V2,
                shown: "maker=supp#1 ledger=accepted dolos=refused[InputNotInUTxO]".into()
            })
        );
    }

    #[test]
    fn only_a_record_dolos_names_unsupported_is_declared() {
        let unsupported = DolosVerdict::Unsupported("non-vkey witnesses".into());
        assert_eq!(declares(&DolosVerdict::Accepted), None);
        assert_eq!(declares(&DolosVerdict::Phase2Rejected), None);
        assert_eq!(declares(&input_refusal()), None);
        assert_eq!(declares(&unsupported), Some(feature("non-vkey witnesses")));
    }

    #[test]
    fn an_empty_tally_does_not_pass() {
        let mut out = judgement::Tally::default();
        ValidationTally::default().report(&mut out);
        assert_eq!(
            out.verdict(),
            judgement::RunVerdict::Fail { failing: 0 },
            "{out}"
        );
    }
}
