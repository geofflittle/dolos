//! The phase one rule table: the ledger failures each rule Dolos refuses by may answer, and which of its sides the corpora judge.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    cbor::{tx_fields, CborError, Field},
    coverage::{Member, Reach},
    judgement::{self, Axis, CellKey, EntryPoint, Instrument, Judge, Verdict},
    manifest::ManifestError,
    record::{LedgerOutcome, RecordId},
    refusal::LedgerLeaf,
    verdict::DolosVerdict,
};

const FILE: &str = "phase1-rules.txt";
const TABLE: &str = include_str!("../../../test_data/ledger-oracle/phase1-rules.txt");

/// A side of a rule a judged record holds: a ledger refusal Dolos refused by it, a ledger acceptance Dolos accepted that holds a field it reads, or a refusal by a rule with no ledger counterpart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Refused,
    Accepted,
    Unrepresentable,
}

impl Side {
    /// The coverage cell of the side of the rule Dolos answers with as `answer`.
    fn cell(self, answer: &str) -> CellKey {
        let side = match self {
            Side::Refused => "refused",
            Side::Accepted => "accepted",
            Side::Unrepresentable => "unrepresentable",
        };
        CellKey::new(Axis::Rule, format!("{answer}.{side}"))
    }
}

/// The ledger accepted transactions whose acceptance a rule's check passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reads {
    Every,
    Holding(BTreeSet<Field>),
}

/// What the table says of one rule, with how the oracle reaches the side Dolos refuses by it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Ruled {
        leaves: BTreeSet<String>,
        reads: Reads,
        refused: Reach,
    },
    Unrepresentable {
        why: String,
        reach: Reach,
    },
}

/// A record whose rule names no ledger failure of it, by the rule and the ledger's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownRule {
    pub dolos: String,
    pub ledger: String,
}

/// The committed rule table, by Dolos's answer as it prints, and its known records.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules {
    pub entries: BTreeMap<String, Entry>,
    pub known: BTreeMap<RecordId, KnownRule>,
}

/// The ledger's answer for a record as a rule row writes it, its failure leaves in name order.
pub fn ledger_answer(outcome: &LedgerOutcome) -> String {
    match outcome {
        LedgerOutcome::Accepted { .. } => "accepted".to_string(),
        LedgerOutcome::Refused { failure_names, .. } => {
            let leaves: BTreeSet<&str> = failure_names.iter().map(|p| p.leaf.name()).collect();
            format!(
                "refused[{}]",
                leaves.into_iter().collect::<Vec<_>>().join(",")
            )
        }
    }
}

/// Reads the reason after `because` that a row lacking a ledger counterpart gives.
fn reason(words: &str) -> Result<(&str, String), String> {
    let (before, why) = words
        .split_once(" because ")
        .ok_or(format!("{words} gives no reason"))?;
    match why.trim() {
        "" => Err(format!("{words} gives no reason")),
        why => Ok((before.trim(), why.to_string())),
    }
}

/// Reads the exclusion that ends a row whose refusal no record can reach, `<reason> command: <command>`.
fn exclusion(cell: &str) -> Result<Reach, String> {
    let (reason, command) = cell
        .split_once(" command: ")
        .ok_or(format!("{cell} names no command"))?;
    match (reason.trim(), command.trim()) {
        ("", _) | (_, "") => Err(format!("{cell} gives no reason or no command")),
        (reason, command) => Ok(Reach::Excluded {
            reason: reason.to_string(),
            command: command.to_string(),
        }),
    }
}

/// Reads one rule row after its answer, whose refused side the oracle reaches by `reach`.
fn entry(cells: &[&str], reach: Reach) -> Result<Entry, String> {
    match cells {
        [unrepresentable] => match reason(unrepresentable)? {
            ("unrepresentable", why) => Ok(Entry::Unrepresentable { why, reach }),
            (other, _) => Err(format!("expected unrepresentable, found {other}")),
        },
        [leaves, reads] => {
            let leaves = leaves
                .strip_prefix("leaves ")
                .ok_or(format!("expected leaves, found {leaves}"))?;
            let leaves: BTreeSet<String> = leaves.split(',').map(str::to_string).collect();
            if let Some(leaf) = leaves.iter().find(|l| LedgerLeaf::of_name(l).is_none()) {
                return Err(format!("leaf {leaf} has no refusal class"));
            }
            let reads = match reads.strip_prefix("reads ") {
                Some("every") => Reads::Every,
                Some(fields) => Reads::Holding(
                    fields
                        .split(',')
                        .map(str::parse)
                        .collect::<Result<_, _>>()?,
                ),
                None => return Err(format!("expected reads, found {reads}")),
            };
            Ok(Entry::Ruled {
                leaves,
                reads,
                refused: reach,
            })
        }
        _ => Err("expected leaves and reads, or unrepresentable".to_string()),
    }
}

/// Reads the rule table, one `<answer> | leaves <L,...> | reads <every or fields>` row, `<answer> | unrepresentable because <why>` row or `known <record> | <answer> | <ledger answer>` row per line, a rule row ending `| excluded <reason> command: <command>` when no record can reach its refused side.
pub fn parse(file: &'static str, text: &str) -> Result<Rules, ManifestError> {
    let mut rules = Rules::default();
    let lines = text.lines().enumerate().map(|(i, l)| (i + 1, l.trim()));
    for (line, l) in lines.filter(|(_, l)| !l.is_empty()) {
        let error = |reason: String| ManifestError { file, line, reason };
        let (row, reach) = match l.split_once(" | excluded ") {
            Some((row, cell)) => (row, exclusion(cell).map_err(error)?),
            None => (l, Reach::Counted),
        };
        let cells: Vec<&str> = row.split(" | ").map(str::trim).collect();
        match cells.as_slice() {
            [record, ..] if record.starts_with("known ") && reach != Reach::Counted => {
                return Err(error(format!("{record} is excluded")));
            }
            [record, dolos, ledger] if record.starts_with("known ") => {
                let id: RecordId = record["known ".len()..].parse().map_err(error)?;
                let known = KnownRule {
                    dolos: dolos.to_string(),
                    ledger: ledger.to_string(),
                };
                if rules.known.insert(id, known).is_some() {
                    return Err(error(format!("{id} known twice")));
                }
            }
            [answer, rest @ ..] => {
                let entry = entry(rest, reach).map_err(error)?;
                if rules.entries.insert(answer.to_string(), entry).is_some() {
                    return Err(error(format!("{answer} listed twice")));
                }
            }
            [] => unreachable!("a split yields at least one cell"),
        }
    }
    Ok(rules)
}

/// The committed rule table.
pub fn committed() -> Result<Rules, ManifestError> {
    parse(FILE, TABLE)
}

/// The rule sides each judged record holds, the records that fail the table and the known records with their lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleTally {
    judged: BTreeMap<RecordId, Vec<CellKey>>,
    failures: BTreeMap<RecordId, Verdict>,
    known: BTreeMap<RecordId, String>,
}

/// How a record fails the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleFail {
    Mismatch,
    Unmapped,
    KnownStale,
}

impl RuleFail {
    /// The name of the failure, as its cell shows it.
    fn name(self) -> &'static str {
        match self {
            RuleFail::Mismatch => "mismatch",
            RuleFail::Unmapped => "unmapped",
            RuleFail::KnownStale => "known_stale",
        }
    }
}

impl RuleTally {
    /// Adds the tally of another part of the corpora after this one's.
    pub fn merge(&mut self, other: RuleTally) {
        let RuleTally {
            judged,
            failures,
            known,
        } = other;
        self.judged.extend(judged);
        self.failures.extend(failures);
        self.known.extend(known);
    }
}

/// What one record shows of the rule Dolos answered with.
enum Shown {
    /// The rule names a ledger failure of the record, or both accepted the transaction.
    Agrees,
    /// Dolos answered by a rule the table marks as having no ledger counterpart.
    Unrepresentable,
    /// Dolos accepted a transaction the ledger refused or rejected its scripts, which the validation tally judges.
    Unjudged,
    Mismatch,
    Unmapped,
}

impl Rules {
    /// The known records whose record `keep` holds.
    pub fn within(&self, keep: impl Fn(&RecordId) -> bool) -> Self {
        let known = self.known.iter().filter(|(id, _)| keep(id));
        Rules {
            entries: self.entries.clone(),
            known: known.map(|(id, k)| (*id, k.clone())).collect(),
        }
    }

    /// Judges one record by the rule Dolos answered with against the ledger's failures, or by the fields of its transaction `tx` when both accepted it.
    pub fn judge(
        &self,
        id: RecordId,
        ledger: &LedgerOutcome,
        dolos: &DolosVerdict,
        tx: &[u8],
        tally: &mut RuleTally,
    ) -> Result<(), CborError> {
        let answer = dolos.to_string();
        let mut sides = Vec::new();
        let shown = match (ledger, dolos) {
            (LedgerOutcome::Accepted { .. }, DolosVerdict::Accepted) => {
                let fields = tx_fields(tx)?;
                for (answer, entry) in &self.entries {
                    let held = match entry {
                        Entry::Ruled {
                            reads: Reads::Every,
                            ..
                        } => true,
                        Entry::Ruled {
                            reads: Reads::Holding(read),
                            ..
                        } => !read.is_disjoint(&fields),
                        Entry::Unrepresentable { .. } => false,
                    };
                    if held {
                        sides.push(Side::Accepted.cell(answer));
                    }
                }
                Shown::Agrees
            }
            (_, DolosVerdict::Refused { .. } | DolosVerdict::Unsupported(_)) => {
                match self.entries.get(&answer) {
                    None => Shown::Unmapped,
                    Some(Entry::Unrepresentable { .. }) => {
                        sides.push(Side::Unrepresentable.cell(&answer));
                        Shown::Unrepresentable
                    }
                    Some(Entry::Ruled { leaves, .. }) => match ledger {
                        LedgerOutcome::Refused { failure_names, .. }
                            if failure_names.iter().any(|p| leaves.contains(p.leaf.name())) =>
                        {
                            sides.push(Side::Refused.cell(&answer));
                            Shown::Agrees
                        }
                        LedgerOutcome::Refused { .. } | LedgerOutcome::Accepted { .. } => {
                            Shown::Mismatch
                        }
                    },
                }
            }
            (
                _,
                DolosVerdict::Accepted
                | DolosVerdict::Phase2Rejected
                | DolosVerdict::Phase2Unsupported(_),
            ) => Shown::Unjudged,
        };
        tally.judged.insert(id, sides);
        let ledger = ledger_answer(ledger);
        let both = format!("dolos={answer} ledger={ledger}");
        let (kind, detail) = match (shown, self.known.get(&id)) {
            (Shown::Mismatch, Some(k)) if k.dolos == answer && k.ledger == ledger => {
                tally.known.insert(id, format!("RULE_KNOWN {id} {both}"));
                return Ok(());
            }
            (_, Some(k)) => (
                RuleFail::KnownStale,
                format!("{both} known_dolos={} known_ledger={}", k.dolos, k.ledger),
            ),
            (Shown::Mismatch, None) => (RuleFail::Mismatch, both),
            (Shown::Unmapped, None) => (RuleFail::Unmapped, both),
            (Shown::Agrees | Shown::Unrepresentable | Shown::Unjudged, None) => return Ok(()),
        };
        let cells = vec![CellKey::new(Axis::Failure, kind.name())];
        let verdict = Verdict::disagree(
            Judge::Rules,
            EntryPoint::Instrument(Instrument::ReceiveTx),
            id,
            cells,
            detail,
        );
        tally.failures.insert(id, verdict);
        Ok(())
    }

    /// The coverage members of the table, each side of each rule.
    pub fn members(&self) -> Vec<Member> {
        let mut members = Vec::new();
        for (answer, entry) in &self.entries {
            let sides = match entry {
                Entry::Unrepresentable { reach, .. } => vec![(Side::Unrepresentable, reach)],
                Entry::Ruled { refused, .. } => {
                    vec![(Side::Refused, refused), (Side::Accepted, &Reach::Counted)]
                }
            };
            members.extend(sides.into_iter().map(|(side, reach)| Member {
                cell: side.cell(answer),
                reach: reach.clone(),
            }));
        }
        members
    }

    /// The line of each known record, adding a verdict for each judged record to `out`, and a failing one for each known record the tally did not judge when `whole` says it covers every corpus.
    pub fn report(
        &self,
        tally: &RuleTally,
        whole: bool,
        out: &mut judgement::Tally,
    ) -> Vec<String> {
        let unjudged: Vec<&RecordId> = match whole {
            true => self
                .known
                .keys()
                .filter(|id| !tally.judged.contains_key(id))
                .collect(),
            false => Vec::new(),
        };
        for (id, sides) in &tally.judged {
            let pass = match (tally.failures.get(id), tally.known.contains_key(id)) {
                (Some(failure), _) => {
                    out.add(failure.clone());
                    continue;
                }
                (None, true) => judgement::Pass::Known,
                (None, false) => judgement::Pass::Agree,
            };
            let passed = judgement::Passed {
                judge: Judge::Rules,
                entry: Some(EntryPoint::Instrument(Instrument::ReceiveTx)),
                cells: sides.clone(),
                pass,
            };
            out.pass(passed, 1);
        }
        for id in unjudged {
            let cells = vec![CellKey::new(Axis::Answer, "unjudged")];
            let entry = EntryPoint::Instrument(Instrument::ReceiveTx);
            out.add(Verdict::disagree(
                Judge::Rules,
                entry,
                *id,
                cells,
                "known not judged",
            ));
        }
        tally.known.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        accounts::LedgerAccounts,
        coverage,
        record::{CorpusKind, FailurePath, NonEmpty},
        refusal::{DolosClass, RefusalClass},
    };

    const TABLE: &str = "\
refused[PostAlonzo(FeeBelowMin)] | leaves FeeTooSmallUTxO | reads every
refused[DijkstraInvalidCertificate(\"refund\")] | leaves RefundIncorrectDELEG | reads sub.k4
refused[DijkstraInvalidCertificate(\"registration deposit\")] | leaves DepositIncorrectDELEG | reads top.k4
refused[DijkstraInvalidMetadata] | leaves DecoderErrorDeserialiseFailure | reads top.aux,sub.aux
unsupported[witness fields] | unrepresentable because the native validator does not decode them
";

    fn rules(text: &str) -> Rules {
        parse("phase1-rules.txt", text).unwrap_or_else(|e| panic!("{e}"))
    }

    fn supp(n: u32) -> RecordId {
        RecordId {
            corpus: CorpusKind::Supp,
            n,
        }
    }

    fn refused(leaves: &[&str]) -> LedgerOutcome {
        let paths = leaves.iter().map(|leaf| FailurePath {
            leaf: LedgerLeaf::of_name(leaf).unwrap(),
        });
        LedgerOutcome::Refused {
            failures: String::new(),
            failure_names: NonEmpty::of(paths).unwrap(),
        }
    }

    fn accepted() -> LedgerOutcome {
        LedgerOutcome::Accepted {
            consumed: Vec::new(),
            produced: Vec::new(),
            accounts_post: LedgerAccounts::default(),
        }
    }

    fn dolos(kind: &str) -> DolosVerdict {
        DolosVerdict::Refused {
            kind: kind.to_string(),
            class: DolosClass::Rule(RefusalClass::CertificateAndGovernance),
        }
    }

    /// A transaction holding a top level body with inputs and certificates and no auxiliary data, `[{0: [], 4: []}, {}, true, null]`.
    const TX: [u8; 9] = [0x84, 0xa2, 0x00, 0x80, 0x04, 0x80, 0xa0, 0xf5, 0xf6];

    fn judge(
        rules: &Rules,
        n: u32,
        ledger: &LedgerOutcome,
        dolos: &DolosVerdict,
        tally: &mut RuleTally,
    ) {
        rules.judge(supp(n), ledger, dolos, &TX, tally).unwrap();
    }

    const REFUND: &str = "DijkstraInvalidCertificate(\"refund\")";
    const DEPOSIT: &str = "DijkstraInvalidCertificate(\"registration deposit\")";

    /// Judges the records of the full table, each rule on the sides it declares.
    fn every_side(rules: &Rules) -> RuleTally {
        let mut tally = RuleTally::default();
        let fee = dolos("PostAlonzo(FeeBelowMin)");
        judge(rules, 1, &refused(&["FeeTooSmallUTxO"]), &fee, &mut tally);
        judge(
            rules,
            2,
            &refused(&["RefundIncorrectDELEG"]),
            &dolos(REFUND),
            &mut tally,
        );
        let leaves = ["DepositIncorrectDELEG", "ValueNotConservedUTxO"];
        judge(rules, 3, &refused(&leaves), &dolos(DEPOSIT), &mut tally);
        let unsupported = DolosVerdict::Unsupported("witness fields".to_string());
        judge(
            rules,
            4,
            &refused(&["MissingTxMetadata"]),
            &unsupported,
            &mut tally,
        );
        judge(rules, 5, &accepted(), &DolosVerdict::Accepted, &mut tally);
        tally
    }

    /// The table's lines over `tally` and the failing verdicts as their lines.
    fn bar(rules: &Rules, tally: &RuleTally, whole: bool) -> (Vec<String>, Vec<String>) {
        let mut out = judgement::Tally::default();
        let lines = rules.report(tally, whole, &mut out);
        (lines, out.failing.iter().map(Verdict::to_string).collect())
    }

    /// The coverage lines of the table's members over `tally`, each counted side committed at `count`.
    fn covered(rules: &Rules, tally: &RuleTally, count: usize) -> Vec<String> {
        let mut out = judgement::Tally::default();
        rules.report(tally, true, &mut out);
        let members = rules.members();
        let counted = members.iter().filter(|m| m.reach == Reach::Counted);
        let committed = counted.map(|m| (m.cell.to_string(), count));
        let counts = coverage::counts(&out);
        coverage::table(&members, &committed.collect(), &counts, &mut out)
    }

    #[test]
    fn each_judged_record_counts_once_on_every_side_it_holds() {
        let rules = rules(TABLE);
        let mut out = judgement::Tally::default();
        let (_, failing) = bar(&rules, &every_side(&rules), true);
        assert_eq!(failing, Vec::<String>::new());
        rules.report(&every_side(&rules), true, &mut out);
        let side = |answer: &str, side: &str| CellKey::new(Axis::Rule, format!("{answer}.{side}"));
        let fee = "refused[PostAlonzo(FeeBelowMin)]";
        let deposit = format!("refused[{DEPOSIT}]");
        let entry = CellKey::new(Axis::Entry, EntryPoint::Instrument(Instrument::ReceiveTx));
        assert_eq!(
            coverage::counts(&out),
            BTreeMap::from([
                (side(fee, "refused"), 1),
                (side(fee, "accepted"), 1),
                (side(&format!("refused[{REFUND}]"), "refused"), 1),
                (side(&deposit, "refused"), 1),
                (side(&deposit, "accepted"), 1),
                (side("unsupported[witness fields]", "unrepresentable"), 1),
                (entry, 5),
            ])
        );
        assert_eq!(out.by_judge(), BTreeMap::from([(Judge::Rules, 5)]));
    }

    #[test]
    fn a_side_no_record_holds_is_a_gap_and_passes_at_a_committed_zero() {
        let rules = rules(TABLE);
        let lines = covered(&rules, &every_side(&rules), 0);
        assert_eq!(lines.last().unwrap(), "coverage=PASS", "{lines:#?}");
        assert!(lines.contains(&format!(
            "COVERAGE rule:refused[{}].accepted records=0 committed=0 gap",
            REFUND.replace(' ', "_")
        )));
        assert!(
            !lines.iter().any(|l| l.contains(" excluded ")),
            "{lines:#?}"
        );
    }

    #[test]
    fn a_rule_naming_no_ledger_failure_of_its_record_is_a_mismatch() {
        let rules = rules(TABLE);
        let mut tally = every_side(&rules);
        let swapped = dolos(REFUND);
        let ledger = refused(&["DepositIncorrectDELEG"]);
        judge(&rules, 6, &ledger, &swapped, &mut tally);
        let (_, failing) = bar(&rules, &tally, false);
        assert_eq!(
            failing,
            [format!(
                "VERDICT judge=rules entry=receive_tx record=supp#6 cells=failure:mismatch \
                 outcome=disagree dolos=refused[{REFUND}] ledger=refused[DepositIncorrectDELEG]"
            )]
        );
    }

    #[test]
    fn a_refusal_by_a_rule_the_table_lacks_is_unmapped() {
        let rules = rules(TABLE);
        let mut tally = every_side(&rules);
        let ledger = refused(&["FeeTooSmallUTxO"]);
        let unlisted = dolos("PostAlonzo(MaxTxSizeExceeded)");
        judge(&rules, 7, &ledger, &unlisted, &mut tally);
        let (_, failing) = bar(&rules, &tally, false);
        assert_eq!(
            failing,
            [
                "VERDICT judge=rules entry=receive_tx record=supp#7 cells=failure:unmapped \
              outcome=disagree dolos=refused[PostAlonzo(MaxTxSizeExceeded)] \
              ledger=refused[FeeTooSmallUTxO]"
            ]
        );
    }

    const LABEL: &str = "reference script language (requires V3)";

    #[test]
    fn an_unsupported_phase_two_evaluation_is_no_phase_one_answer() {
        let rules = rules(TABLE);
        let mut tally = every_side(&rules);
        let evaluation = DolosVerdict::Phase2Unsupported(LABEL.to_string());
        judge(&rules, 8, &accepted(), &evaluation, &mut tally);
        let (lines, failing) = bar(&rules, &tally, false);
        assert_eq!(failing, Vec::<String>::new(), "{lines:?}");
    }

    #[test]
    fn an_unsupported_phase_one_answer_the_table_lacks_is_unmapped() {
        let rules = rules(TABLE);
        let mut tally = every_side(&rules);
        let feature = DolosVerdict::Unsupported(LABEL.to_string());
        judge(&rules, 8, &accepted(), &feature, &mut tally);
        let (_, failing) = bar(&rules, &tally, false);
        assert_eq!(
            failing,
            [format!(
                "VERDICT judge=rules entry=receive_tx record=supp#8 cells=failure:unmapped \
                 outcome=disagree dolos=unsupported[{LABEL}] ledger=accepted"
            )]
        );
    }

    #[test]
    fn a_side_below_its_committed_count_fails_the_coverage_table() {
        let rules = rules(TABLE);
        let mut tally = RuleTally::default();
        let fee = dolos("PostAlonzo(FeeBelowMin)");
        judge(&rules, 1, &refused(&["FeeTooSmallUTxO"]), &fee, &mut tally);
        let lines = covered(&rules, &tally, 1);
        let last = lines.last().unwrap();
        let failed: Vec<&str> = last
            .trim_start_matches("coverage=FAIL ")
            .split(',')
            .collect();
        assert!(failed.contains(&"rule:refused[PostAlonzo(FeeBelowMin)].accepted"));
        assert!(!failed.contains(&"rule:refused[PostAlonzo(FeeBelowMin)].refused"));
    }

    #[test]
    fn a_known_record_passes_only_with_its_rule_and_ledger_answer() {
        let known =
            format!("{TABLE}known supp#6 | refused[{REFUND}] | refused[DepositIncorrectDELEG]\n");
        let rules = rules(&known);
        let ledger = refused(&["DepositIncorrectDELEG"]);
        let mut tally = every_side(&rules);
        judge(&rules, 6, &ledger, &dolos(REFUND), &mut tally);
        let (lines, failing) = bar(&rules, &tally, true);
        assert_eq!(failing, Vec::<String>::new(), "{lines:?}");
        assert!(lines.contains(&format!(
            "RULE_KNOWN supp#6 dolos=refused[{REFUND}] ledger=refused[DepositIncorrectDELEG]"
        )));
        let mut tally = every_side(&rules);
        let fee = dolos("PostAlonzo(FeeBelowMin)");
        judge(&rules, 6, &ledger, &fee, &mut tally);
        let (_, failing) = bar(&rules, &tally, true);
        assert_eq!(
            failing,
            [format!(
                "VERDICT judge=rules entry=receive_tx record=supp#6 cells=failure:known_stale \
                 outcome=disagree dolos=refused[PostAlonzo(FeeBelowMin)] \
                 ledger=refused[DepositIncorrectDELEG] known_dolos=refused[{REFUND}] \
                 known_ledger=refused[DepositIncorrectDELEG]"
            )]
        );
    }

    #[test]
    fn a_known_record_the_run_does_not_judge_fails_only_a_whole_run() {
        let known =
            format!("{TABLE}known supp#9 | refused[{REFUND}] | refused[DepositIncorrectDELEG]\n");
        let rules = rules(&known);
        let tally = every_side(&rules);
        let (_, failing) = bar(&rules, &tally, false);
        assert_eq!(failing, Vec::<String>::new());
        let (_, failing) = bar(&rules, &tally, true);
        assert_eq!(
            failing,
            [
                "VERDICT judge=rules entry=receive_tx record=supp#9 cells=answer:unjudged \
              outcome=disagree known not judged"
            ]
        );
    }

    #[test]
    fn a_row_with_a_side_cell_listed_twice_or_without_leaves_does_not_parse() {
        let row = "refused[PostAlonzo(FeeBelowMin)] | leaves FeeTooSmallUTxO | reads every | both";
        let error = parse("phase1-rules.txt", row).unwrap_err();
        assert_eq!(error.line, 1);
        let twice = format!("{TABLE}refused[PostAlonzo(FeeBelowMin)] | leaves A | reads every");
        let error = parse("phase1-rules.txt", &twice).unwrap_err();
        assert_eq!(error.line, 6);
        let empty = "refused[PostAlonzo(FeeBelowMin)] | leaves | reads every";
        assert!(parse("phase1-rules.txt", empty).is_err());
    }

    #[test]
    fn a_row_naming_a_leaf_without_a_refusal_class_does_not_parse() {
        let row =
            "refused[PostAlonzo(FeeBelowMin)] | leaves FeeTooSmallUTxO,FeeTooSmall | reads every";
        let error = parse("phase1-rules.txt", row).unwrap_err();
        assert!(error.reason.contains("FeeTooSmall "), "{error}");
        assert!(parse("phase1-rules.txt", TABLE).is_ok());
    }

    #[test]
    fn the_ledger_answer_names_each_leaf_once_in_name_order() {
        let ledger = refused(&[
            "ValueNotConservedUTxO",
            "DepositIncorrectDELEG",
            "ValueNotConservedUTxO",
        ]);
        assert_eq!(
            ledger_answer(&ledger),
            "refused[DepositIncorrectDELEG,ValueNotConservedUTxO]"
        );
        assert_eq!(ledger_answer(&accepted()), "accepted");
    }

    const EXCLUDED: &str = "\
refused[PostAlonzo(FeeBelowMin)] | leaves FeeTooSmallUTxO | reads every | excluded no fee is low command: grep -n fee x.rs | head -1
unsupported[witness fields] | unrepresentable because the native validator does not decode them | excluded every witness decodes command: grep -n witness y.rs
refused[DijkstraInvalidMetadata] | leaves DecoderErrorDeserialiseFailure | reads top.aux,sub.aux
";

    /// The reach of each member of `rules`, by its cell.
    fn reaches(rules: &Rules) -> BTreeMap<String, Reach> {
        let members = rules.members().into_iter();
        members.map(|m| (m.cell.to_string(), m.reach)).collect()
    }

    #[test]
    fn a_row_without_an_exclusion_counts_every_side() {
        let reaches = reaches(&rules(TABLE));
        assert_eq!(reaches.len(), 9);
        assert!(
            reaches.values().all(|r| *r == Reach::Counted),
            "{reaches:?}"
        );
    }

    #[test]
    fn an_exclusion_cell_excludes_the_refused_or_unrepresentable_side_with_its_command() {
        let reaches = reaches(&rules(EXCLUDED));
        let excluded = |reason: &str, command: &str| Reach::Excluded {
            reason: reason.to_string(),
            command: command.to_string(),
        };
        assert_eq!(
            reaches,
            BTreeMap::from([
                (
                    "rule:refused[PostAlonzo(FeeBelowMin)].refused".to_string(),
                    excluded("no fee is low", "grep -n fee x.rs | head -1")
                ),
                (
                    "rule:refused[PostAlonzo(FeeBelowMin)].accepted".to_string(),
                    Reach::Counted
                ),
                (
                    "rule:unsupported[witness_fields].unrepresentable".to_string(),
                    excluded("every witness decodes", "grep -n witness y.rs")
                ),
                (
                    "rule:refused[DijkstraInvalidMetadata].refused".to_string(),
                    Reach::Counted
                ),
                (
                    "rule:refused[DijkstraInvalidMetadata].accepted".to_string(),
                    Reach::Counted
                ),
            ])
        );
    }

    #[test]
    fn a_record_on_an_excluded_side_fails_the_coverage_table_and_none_passes_it() {
        let rules = rules(EXCLUDED);
        let mut tally = RuleTally::default();
        judge(&rules, 1, &accepted(), &DolosVerdict::Accepted, &mut tally);
        let lines = covered(&rules, &tally, 0);
        assert_eq!(lines.last().unwrap(), "coverage=PASS", "{lines:#?}");
        assert!(lines.contains(
            &"COVERAGE rule:refused[PostAlonzo(FeeBelowMin)].refused records=0 committed=none \
              excluded no fee is low command: grep -n fee x.rs | head -1"
                .to_string()
        ));
        let fee = dolos("PostAlonzo(FeeBelowMin)");
        judge(&rules, 2, &refused(&["FeeTooSmallUTxO"]), &fee, &mut tally);
        let witness = DolosVerdict::Unsupported("witness fields".to_string());
        judge(
            &rules,
            3,
            &refused(&["MissingTxMetadata"]),
            &witness,
            &mut tally,
        );
        let lines = covered(&rules, &tally, 0);
        assert_eq!(
            lines.last().unwrap(),
            "coverage=FAIL rule:refused[PostAlonzo(FeeBelowMin)].refused,\
             rule:unsupported[witness_fields].unrepresentable",
            "{lines:#?}"
        );
    }

    #[test]
    fn an_exclusion_without_its_command_or_on_a_known_row_does_not_parse() {
        let bare = "refused[PostAlonzo(FeeBelowMin)] | leaves FeeTooSmallUTxO | reads every | excluded no fee is low";
        assert!(parse("phase1-rules.txt", bare).is_err());
        let empty = "refused[PostAlonzo(FeeBelowMin)] | leaves FeeTooSmallUTxO | reads every | excluded no fee is low command: ";
        assert!(parse("phase1-rules.txt", empty).is_err());
        let known =
            "known supp#1 | refused[A] | accepted | excluded no fee is low command: grep fee";
        assert!(parse("phase1-rules.txt", known).is_err());
        assert!(parse("phase1-rules.txt", EXCLUDED).is_ok());
    }

    #[test]
    fn the_committed_table_parses() {
        committed().unwrap_or_else(|e| panic!("{e}"));
    }
}
