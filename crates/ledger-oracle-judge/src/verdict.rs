use std::{
    borrow::Borrow,
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use crate::{
    manifest::Feature,
    record::{FailurePath, LedgerOutcome, NonEmpty, Phase2, RecordId},
    refusal::{DolosClass, RefusalClass},
    row::Row,
};

/// What Dolos answered when one transaction was submitted to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DolosVerdict {
    Accepted,
    Phase2Rejected,
    Unsupported(String),
    Refused { kind: String, class: DolosClass },
}

impl fmt::Display for DolosVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DolosVerdict::Accepted => write!(f, "accepted"),
            DolosVerdict::Phase2Rejected => write!(f, "phase2_rejected"),
            DolosVerdict::Unsupported(label) => write!(f, "unsupported[{label}]"),
            DolosVerdict::Refused { kind, .. } => write!(f, "refused[{kind}]"),
        }
    }
}

/// The ledger's answer for one record, printed for a disagreement.
pub fn show_ledger(outcome: &LedgerOutcome, phase2: Phase2) -> String {
    match (outcome, phase2) {
        (LedgerOutcome::Accepted { .. }, Phase2::Valid) => "accepted".to_string(),
        (LedgerOutcome::Accepted { .. }, Phase2::Invalid) => "accepted_phase2_invalid".to_string(),
        (LedgerOutcome::Refused { failure_names, .. }, _) => {
            let leaves: Vec<&str> = failure_names.iter().map(FailurePath::leaf).collect();
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
    let ledger_classes: BTreeSet<RefusalClass> = failure_names.iter().map(|p| p.class).collect();
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

/// The outcome of one record from both answers and the feature it is declared unsupported for.
pub fn outcome(
    ledger: &LedgerOutcome,
    phase2: Phase2,
    dolos: &DolosVerdict,
    declared: Option<&Feature>,
) -> Outcome {
    use DolosVerdict as D;
    use LedgerOutcome as L;
    let disagree = || {
        Outcome::Fail(Fail::Disagree {
            ledger: show_ledger(ledger, phase2),
            dolos: dolos.to_string(),
        })
    };
    match (ledger, phase2, dolos, declared) {
        (
            _,
            _,
            D::Refused {
                class: DolosClass::Unknown,
                ..
            },
            _,
        ) => Outcome::Fail(Fail::UnknownRefusal {
            dolos: dolos.to_string(),
        }),
        (L::Accepted { .. }, _, D::Unsupported(l), Some(f)) if *l == f.0 => {
            Outcome::Pass(Pass::UnsupportedExpected { label: l.clone() })
        }
        (L::Accepted { .. }, _, D::Unsupported(l), _) => {
            Outcome::Fail(Fail::UnsupportedUnexpected { label: l.clone() })
        }
        (L::Refused { .. }, _, D::Unsupported(l), _) => {
            Outcome::Pass(Pass::RefuseUnsupported { label: l.clone() })
        }
        (_, _, D::Accepted | D::Phase2Rejected | D::Refused { .. }, Some(f)) => {
            Outcome::Fail(Fail::StaleManifest {
                declared: f.to_string(),
                dolos: dolos.to_string(),
            })
        }
        (L::Accepted { .. }, Phase2::Valid, D::Accepted, None)
        | (L::Accepted { .. }, Phase2::Invalid, D::Phase2Rejected, None) => {
            Outcome::Pass(Pass::Agree)
        }
        (
            L::Refused { failure_names, .. },
            _,
            D::Refused {
                kind,
                class: DolosClass::Rule(c),
            },
            None,
        ) => refusal_outcome(failure_names, show_ledger(ledger, phase2), kind, Some(*c)),
        (
            L::Refused { failure_names, .. },
            _,
            D::Refused {
                kind,
                class: DolosClass::NoRule,
            },
            None,
        ) => refusal_outcome(failure_names, show_ledger(ledger, phase2), kind, None),
        (L::Accepted { .. }, Phase2::Valid, D::Phase2Rejected, None)
        | (L::Accepted { .. }, Phase2::Invalid, D::Accepted, None)
        | (L::Accepted { .. }, _, D::Refused { .. }, None)
        | (L::Refused { .. }, _, D::Accepted | D::Phase2Rejected, None) => disagree(),
    }
}

/// The feature a record would be declared unsupported for, from a ledger that accepted it and a Dolos that named it unsupported.
pub fn declares(ledger: &LedgerOutcome, dolos: &DolosVerdict) -> Option<Feature> {
    match (ledger, dolos) {
        (LedgerOutcome::Accepted { .. }, DolosVerdict::Unsupported(label)) => {
            Some(Feature(label.clone()))
        }
        _ => None,
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
        // An unsupported answer says nothing of the outputs, so later records spend the ledger's outputs.
        (D::Phase2Rejected | D::Unsupported(_) | D::Refused { .. }, Outcome::Pass(_))
        | (D::Unsupported(_), Outcome::Fail(Fail::UnsupportedUnexpected { .. })) => {
            Maker::Seeds(SeedingMaker(id))
        }
        (
            D::Phase2Rejected | D::Unsupported(_) | D::Refused { .. },
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
        D::Refused { .. } | D::Accepted | D::Phase2Rejected | D::Unsupported(_) => false,
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
    pub agree: usize,
    pub unsupported_expected: BTreeMap<String, usize>,
    pub refuse_unsupported: Vec<(RecordId, String)>,
    pub known: Vec<(RecordId, Row, String)>,
    pub disagree: Vec<(RecordId, String)>,
    pub reason_mismatch: Vec<(RecordId, String)>,
    pub cascade: Vec<(RecordId, String)>,
    pub unsupported_unexpected: Vec<(RecordId, String)>,
    pub stale_manifest: Vec<(RecordId, String)>,
    pub known_stale: Vec<(RecordId, String)>,
    pub unknown_refusal: Vec<(RecordId, String)>,
}

impl ValidationTally {
    /// Counts the outcome of one record.
    pub fn add(&mut self, id: RecordId, outcome: Outcome) {
        match outcome {
            Outcome::Pass(Pass::Agree) => self.agree += 1,
            Outcome::Pass(Pass::UnsupportedExpected { label }) => {
                *self.unsupported_expected.entry(label).or_default() += 1
            }
            Outcome::Pass(Pass::RefuseUnsupported { label }) => {
                self.refuse_unsupported.push((id, format!("label={label}")))
            }
            Outcome::Pass(Pass::Known { row, shown }) => self.known.push((id, row, shown)),
            Outcome::Fail(fail) => {
                let text = fail.to_string();
                match fail {
                    Fail::Disagree { .. } | Fail::RelayDependent { .. } => {
                        self.disagree.push((id, text))
                    }
                    Fail::ReasonMismatch { .. } => self.reason_mismatch.push((id, text)),
                    Fail::UnsupportedUnexpected { .. } => {
                        self.unsupported_unexpected.push((id, text))
                    }
                    Fail::StaleManifest { .. } => self.stale_manifest.push((id, text)),
                    Fail::KnownStale { .. } => self.known_stale.push((id, text)),
                    Fail::UnknownRefusal { .. } => self.unknown_refusal.push((id, text)),
                }
            }
            Outcome::Cascade(cascade) => self.cascade.push((id, cascade.to_string())),
        }
    }

    /// Whether the arm judged a record and no outcome fails it.
    pub fn pass(&self) -> bool {
        let ValidationTally {
            dependent: _,
            agree,
            unsupported_expected,
            refuse_unsupported,
            known,
            disagree,
            reason_mismatch,
            cascade: _,
            unsupported_unexpected,
            stale_manifest,
            known_stale,
            unknown_refusal,
        } = self;
        let passing = agree
            + unsupported_expected.values().sum::<usize>()
            + refuse_unsupported.len()
            + known.len();
        let failing = disagree.len()
            + reason_mismatch.len()
            + unsupported_unexpected.len()
            + stale_manifest.len()
            + known_stale.len()
            + unknown_refusal.len();
        passing + failing > 0 && failing == 0
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
            disagree,
            reason_mismatch,
            cascade,
            unsupported_unexpected,
            stale_manifest,
            known_stale,
            unknown_refusal,
        } = self;
        write!(
            f,
            "VALIDATION agree={agree} disagree={} reason_mismatch={} cascade={} \
             unsupported_expected={} unsupported_unexpected={} refuse_unsupported={} \
             stale_manifest={} dependent={} known={} known_stale={} unknown_refusal={}",
            disagree.len(),
            reason_mismatch.len(),
            cascade.len(),
            unsupported_expected.values().sum::<usize>(),
            unsupported_unexpected.len(),
            refuse_unsupported.len(),
            stale_manifest.len(),
            dependent.len(),
            known.len(),
            known_stale.len(),
            unknown_refusal.len()
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
            ("DISAGREE", disagree),
            ("REASON_MISMATCH", reason_mismatch),
            ("CASCADE", cascade),
            ("UNSUPPORTED_UNEXPECTED", unsupported_unexpected),
            ("STALE_MANIFEST", stale_manifest),
            ("KNOWN_STALE", known_stale),
            ("UNKNOWN_REFUSAL", unknown_refusal),
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

    fn accepted() -> LedgerOutcome {
        LedgerOutcome::Accepted {
            consumed: vec![],
            produced: vec![],
            accounts_post: Default::default(),
        }
    }

    fn path(leaf: &str) -> FailurePath {
        FailurePath {
            names: NonEmpty {
                head: "UtxoFailure".to_string(),
                tail: vec![leaf.to_string()],
            },
            class: RefusalClass::of_ledger_leaf(leaf).unwrap(),
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
        tally.add(supp(3), Outcome::Fail(fail));
        assert!(!tally.pass());
        assert_eq!(
            tally.to_string().lines().last(),
            Some("DISAGREE supp#3 held=accepted inflight=refused[WithdrawalNotRegistered]")
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
                outcome(
                    &ledger,
                    Phase2::Valid,
                    &dolos_refused(kind, DolosClass::Rule(class)),
                    None
                ),
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
            tally.add(supp(n), outcome(&ledger, Phase2::Valid, &dolos, None));
        }
        assert!(!tally.pass());
        let text = tally.to_string();
        assert!(
            text.contains(
                "\nREASON_MISMATCH supp#5 ledger=refused[BadInputsUTxO,BadInputsUTxO] \
                 ledger_class=inputs dolos=refused[PreservationOfValue] dolos_class=value"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "\nREASON_MISMATCH supp#6 ledger=refused[BadInputsUTxO,BadInputsUTxO] \
                 ledger_class=inputs dolos=refused[DijkstraMissingParameters] dolos_class=none"
            ),
            "{text}"
        );
    }

    #[test]
    fn a_refusal_of_a_variant_the_table_names_is_never_unknown() {
        for ledger in [accepted(), refused("BadInputsUTxO")] {
            for class in [
                DolosClass::NoRule,
                DolosClass::Rule(RefusalClass::Value),
                DolosClass::Rule(RefusalClass::Inputs),
            ] {
                let judged = outcome(&ledger, Phase2::Valid, &dolos_refused("Named", class), None);
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
            let judged = outcome(&ledger, Phase2::Valid, &unknown, None);
            assert_eq!(judged, expected);
            assert_eq!(with_known(judged, Some(&Row::V2)), expected);
        }
        let mut tally = ValidationTally::default();
        tally.add(supp(4), expected);
        assert!(!tally.pass());
        let text = tally.to_string();
        assert!(
            text.contains("\nUNKNOWN_REFUSAL supp#4 dolos=refused[FutureVariant]"),
            "{text}"
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
        assert_eq!(
            outcome(&accepted(), Phase2::Valid, &DolosVerdict::Accepted, None),
            agree
        );
        assert_eq!(
            outcome(
                &accepted(),
                Phase2::Invalid,
                &DolosVerdict::Phase2Rejected,
                None
            ),
            agree
        );
        assert_eq!(
            outcome(
                &refused("BadInputsUTxO"),
                Phase2::Valid,
                &input_refusal(),
                None
            ),
            agree
        );
    }

    #[test]
    fn opposite_answers_disagree_and_name_both_sides() {
        let o = outcome(
            &refused("BadInputsUTxO"),
            Phase2::Valid,
            &DolosVerdict::Accepted,
            None,
        );
        let mut tally = ValidationTally::default();
        tally.add(supp(5), o);
        assert!(!tally.pass());
        assert_eq!(
            tally.to_string(),
            "VALIDATION agree=0 disagree=1 reason_mismatch=0 cascade=0 unsupported_expected=0 \
             unsupported_unexpected=0 \
             refuse_unsupported=0 stale_manifest=0 dependent=0 known=0 known_stale=0 \
             unknown_refusal=0\n\
             DEPENDENT ids=\n\
             DISAGREE supp#5 ledger=refused[BadInputsUTxO] dolos=accepted"
        );
    }

    #[test]
    fn a_declared_label_passes_only_when_dolos_names_it() {
        let unsupported = DolosVerdict::Unsupported("certificate kind".into());
        assert_eq!(
            outcome(
                &accepted(),
                Phase2::Valid,
                &unsupported,
                Some(&feature("certificate kind"))
            ),
            Outcome::Pass(Pass::UnsupportedExpected {
                label: "certificate kind".into()
            })
        );
        assert_eq!(
            outcome(
                &accepted(),
                Phase2::Valid,
                &unsupported,
                Some(&feature("pointer address"))
            ),
            Outcome::Fail(Fail::UnsupportedUnexpected {
                label: "certificate kind".into()
            })
        );
        assert_eq!(
            outcome(&accepted(), Phase2::Valid, &unsupported, None),
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
                Phase2::Valid,
                &DolosVerdict::Accepted,
                Some(&feature("certificate kind"))
            ),
            Outcome::Fail(Fail::StaleManifest {
                declared: "certificate kind".into(),
                dolos: "accepted".into()
            })
        );
    }

    #[test]
    fn a_known_record_passes_while_it_fails_and_fails_once_it_agrees() {
        let row = Row::V2;
        let failing = outcome(&accepted(), Phase2::Valid, &input_refusal(), None);
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
            Phase2::Valid,
            &unsupported,
            Some(&feature("certificate kind")),
        );
        let unexpected = outcome(&accepted(), Phase2::Valid, &unsupported, None);
        let stale = outcome(
            &accepted(),
            Phase2::Valid,
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
        let refused_for_value = outcome(&accepted(), Phase2::Valid, &value, None);
        assert_eq!(
            with_failing_makers(refused_for_value.clone(), &value, failing(&[1])),
            refused_for_value
        );
        let refused_for_inputs = outcome(&accepted(), Phase2::Valid, &input_refusal(), None);
        assert_eq!(
            with_failing_makers(refused_for_inputs.clone(), &input_refusal(), failing(&[])),
            refused_for_inputs
        );
        let agree = outcome(
            &refused("BadInputsUTxO"),
            Phase2::Valid,
            &input_refusal(),
            None,
        );
        assert_eq!(
            with_failing_makers(agree.clone(), &input_refusal(), failing(&[1])),
            agree
        );
        let accepted_by_dolos = outcome(
            &refused("BadInputsUTxO"),
            Phase2::Valid,
            &DolosVerdict::Accepted,
            None,
        );
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
            Phase2::Valid,
            &DolosVerdict::Unsupported("plutus v4".into()),
            None,
        );
        tally.add(supp(1), maker_outcome);
        let disagree = outcome(&accepted(), Phase2::Valid, &input_refusal(), None);
        tally.add(
            supp(2),
            with_failing_makers(disagree, &input_refusal(), failing(&[1])),
        );
        let mismatch = outcome(
            &refused_by(&["ExtraRedeemers", "CollectErrors"]),
            Phase2::Valid,
            &input_refusal(),
            None,
        );
        tally.add(
            supp(3),
            with_failing_makers(mismatch, &input_refusal(), failing(&[1, 2])),
        );
        assert!(!tally.pass(), "the failing maker still fails the arm");
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
        assert!(
            text.contains("\nUNSUPPORTED_UNEXPECTED supp#1 label=plutus v4"),
            "{text}"
        );
    }

    #[test]
    fn a_cascade_is_a_failing_maker_and_a_known_row_holds_it() {
        let disagree = outcome(&accepted(), Phase2::Valid, &input_refusal(), None);
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
    fn only_an_accepted_record_dolos_names_unsupported_is_declared() {
        let unsupported = DolosVerdict::Unsupported("non-vkey witnesses".into());
        assert_eq!(declares(&refused("BadInputsUTxO"), &unsupported), None);
        assert_eq!(declares(&accepted(), &DolosVerdict::Accepted), None);
        assert_eq!(declares(&accepted(), &input_refusal()), None);
        assert_eq!(
            declares(&accepted(), &unsupported),
            Some(feature("non-vkey witnesses"))
        );
    }

    #[test]
    fn an_empty_tally_does_not_pass() {
        assert!(!ValidationTally::default().pass());
    }
}
