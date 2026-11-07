//! The phase one rule table: the ledger failures each rule Dolos refuses by may answer, and which of its sides the corpora judge.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use crate::{
    cbor::{tx_fields, CborError, Field},
    manifest::ManifestError,
    record::{LedgerOutcome, RecordId},
    refusal::LedgerLeaf,
    verdict::DolosVerdict,
};

const TABLE: &str = include_str!("../../../test_data/ledger-oracle/phase1-rules.txt");

/// The sides of a rule the judged records hold, a ledger refusal Dolos refused by it and a ledger acceptance Dolos accepted that holds a field it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Side {
    Both,
    RefusedOnly,
    AcceptedOnly,
    Neither,
}

impl Side {
    fn of(refused: usize, accepted: usize) -> Self {
        match (refused > 0, accepted > 0) {
            (true, true) => Side::Both,
            (true, false) => Side::RefusedOnly,
            (false, true) => Side::AcceptedOnly,
            (false, false) => Side::Neither,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Side::Both => "both",
            Side::RefusedOnly => "refused_only",
            Side::AcceptedOnly => "accepted_only",
            Side::Neither => "neither",
        }
    }
}

/// A side a rule lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gap {
    RefusedOnly,
    AcceptedOnly,
    Neither,
}

/// The sides a table row declares its rule has, with the reason for a side it lacks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Declared {
    Both,
    Lacks { gap: Gap, why: String },
}

impl Declared {
    fn side(&self) -> Side {
        match self {
            Declared::Both => Side::Both,
            Declared::Lacks { gap, .. } => match gap {
                Gap::RefusedOnly => Side::RefusedOnly,
                Gap::AcceptedOnly => Side::AcceptedOnly,
                Gap::Neither => Side::Neither,
            },
        }
    }
}

/// The ledger accepted transactions whose acceptance a rule's check passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reads {
    Every,
    Holding(BTreeSet<Field>),
}

/// What the table says of one rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Ruled {
        leaves: BTreeSet<String>,
        reads: Reads,
        declared: Declared,
    },
    Unrepresentable {
        why: String,
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

/// Reads the reason after `because` that a row lacking a side or a ledger counterpart gives.
fn reason(words: &str) -> Result<(&str, String), String> {
    let (before, why) = words
        .split_once(" because ")
        .ok_or(format!("{words} gives no reason"))?;
    match why.trim() {
        "" => Err(format!("{words} gives no reason")),
        why => Ok((before.trim(), why.to_string())),
    }
}

/// Reads the sides a row declares.
fn declared(words: &str) -> Result<Declared, String> {
    if words == "both" {
        return Ok(Declared::Both);
    }
    let (side, why) = reason(words)?;
    let gap = match side {
        "refused_only" => Gap::RefusedOnly,
        "accepted_only" => Gap::AcceptedOnly,
        "neither" => Gap::Neither,
        other => return Err(format!("unknown side {other}")),
    };
    Ok(Declared::Lacks { gap, why })
}

/// Reads one rule row after its answer.
fn entry(cells: &[&str]) -> Result<Entry, String> {
    match cells {
        [unrepresentable] => match reason(unrepresentable)? {
            ("unrepresentable", why) => Ok(Entry::Unrepresentable { why }),
            (other, _) => Err(format!("expected unrepresentable, found {other}")),
        },
        [leaves, reads, side] => {
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
                declared: declared(side)?,
            })
        }
        _ => Err("expected leaves, reads and a side, or unrepresentable".to_string()),
    }
}

/// Reads the rule table, one `<answer> | leaves <L,...> | reads <every or fields> | <side>` row, `<answer> | unrepresentable because <why>` row or `known <record> | <answer> | <ledger answer>` row per line.
pub fn parse(file: &'static str, text: &str) -> Result<Rules, ManifestError> {
    let mut rules = Rules::default();
    let lines = text.lines().enumerate().map(|(i, l)| (i + 1, l.trim()));
    for (line, l) in lines.filter(|(_, l)| !l.is_empty()) {
        let error = |reason: String| ManifestError { file, line, reason };
        let cells: Vec<&str> = l.split(" | ").map(str::trim).collect();
        match cells.as_slice() {
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
                let entry = entry(rest).map_err(error)?;
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
    parse("phase1-rules.txt", TABLE)
}

/// The number of judged records on each side of each rule, and the records that fail the table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleTally {
    refused: BTreeMap<String, usize>,
    accepted: BTreeMap<String, usize>,
    unrepresentable: BTreeMap<String, usize>,
    judged: BTreeSet<RecordId>,
    failures: Vec<(RuleFail, String)>,
    known: Vec<String>,
}

/// How a record fails the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleFail {
    Mismatch,
    Unmapped,
    KnownStale,
}

impl RuleTally {
    /// Adds the tally of another part of the corpora after this one's.
    pub fn merge(&mut self, other: RuleTally) {
        let RuleTally {
            refused,
            accepted,
            unrepresentable,
            judged,
            failures,
            known,
        } = other;
        let counts = [
            (&mut self.refused, refused),
            (&mut self.accepted, accepted),
            (&mut self.unrepresentable, unrepresentable),
        ];
        for (mine, theirs) in counts {
            for (answer, n) in theirs {
                *mine.entry(answer).or_default() += n;
            }
        }
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
    /// The known records whose record is one of `records`.
    pub fn within(&self, records: &BTreeSet<RecordId>) -> Self {
        let known = self.known.iter().filter(|(id, _)| records.contains(id));
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
        tally.judged.insert(id);
        let answer = dolos.to_string();
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
                        *tally.accepted.entry(answer.clone()).or_default() += 1;
                    }
                }
                Shown::Agrees
            }
            (_, DolosVerdict::Refused { .. } | DolosVerdict::Unsupported(_)) => {
                match self.entries.get(&answer) {
                    None => Shown::Unmapped,
                    Some(Entry::Unrepresentable { .. }) => {
                        *tally.unrepresentable.entry(answer.clone()).or_default() += 1;
                        Shown::Unrepresentable
                    }
                    Some(Entry::Ruled { leaves, .. }) => match ledger {
                        LedgerOutcome::Refused { failure_names, .. }
                            if failure_names.iter().any(|p| leaves.contains(p.leaf.name())) =>
                        {
                            *tally.refused.entry(answer.clone()).or_default() += 1;
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
        let ledger = ledger_answer(ledger);
        let both = format!("dolos={answer} ledger={ledger}");
        let failure = match (shown, self.known.get(&id)) {
            (Shown::Mismatch, Some(k)) if k.dolos == answer && k.ledger == ledger => {
                tally.known.push(format!("RULE_KNOWN {id} {both}"));
                return Ok(());
            }
            (_, Some(k)) => (
                RuleFail::KnownStale,
                format!(
                    "RULE_KNOWN_STALE {id} {both} known_dolos={} known_ledger={}",
                    k.dolos, k.ledger
                ),
            ),
            (Shown::Mismatch, None) => (RuleFail::Mismatch, format!("RULE_MISMATCH {id} {both}")),
            (Shown::Unmapped, None) => (RuleFail::Unmapped, format!("RULE_UNMAPPED {id} {both}")),
            (Shown::Agrees | Shown::Unrepresentable | Shown::Unjudged, None) => return Ok(()),
        };
        tally.failures.push(failure);
        Ok(())
    }

    /// The line of each rule and each failing record, the count line, and whether the table holds, comparing each rule's sides with the table only when `whole` says the tally covers every corpus.
    pub fn bar(&self, tally: &RuleTally, whole: bool) -> (Vec<String>, bool) {
        let mut lines = Vec::new();
        let mut sides: BTreeMap<Side, usize> = BTreeMap::new();
        let (mut rules, mut unrepresentable, mut wrong_side) = (0, 0, 0);
        let count = |counts: &BTreeMap<String, usize>, answer: &String| {
            counts.get(answer).copied().unwrap_or_default()
        };
        for (answer, entry) in &self.entries {
            match entry {
                Entry::Ruled { declared, .. } => {
                    rules += 1;
                    let refused = count(&tally.refused, answer);
                    let accepted = count(&tally.accepted, answer);
                    let found = Side::of(refused, accepted);
                    *sides.entry(found).or_default() += 1;
                    lines.push(format!(
                        "RULE {answer} refused={refused} accepted={accepted} side={found}"
                    ));
                    if whole && found != declared.side() {
                        wrong_side += 1;
                        lines.push(format!(
                            "RULE_SIDE {answer} declared={} found={found}",
                            declared.side()
                        ));
                    }
                }
                Entry::Unrepresentable { .. } => {
                    unrepresentable += 1;
                    let records = count(&tally.unrepresentable, answer);
                    lines.push(format!(
                        "RULE {answer} records={records} side=unrepresentable"
                    ));
                }
            }
        }
        lines.extend(tally.known.iter().cloned());
        lines.extend(tally.failures.iter().map(|(_, line)| line.clone()));
        let failed = |kind: RuleFail| tally.failures.iter().filter(|(k, _)| *k == kind).count();
        let (mismatch, unmapped) = (failed(RuleFail::Mismatch), failed(RuleFail::Unmapped));
        let unjudged: Vec<&RecordId> = match whole {
            true => self
                .known
                .keys()
                .filter(|id| !tally.judged.contains(id))
                .collect(),
            false => Vec::new(),
        };
        lines.extend(
            unjudged
                .iter()
                .map(|id| format!("RULE_KNOWN_UNJUDGED {id}")),
        );
        let side = |s: Side| sides.get(&s).copied().unwrap_or_default();
        lines.push(format!(
            "PHASE1-RULES rules={rules} both={} refused_only={} accepted_only={} neither={} \
             mismatch={mismatch} unmapped={unmapped} unrepresentable={unrepresentable}",
            side(Side::Both),
            side(Side::RefusedOnly),
            side(Side::AcceptedOnly),
            side(Side::Neither),
        ));
        let pass = tally.failures.is_empty() && wrong_side == 0 && unjudged.is_empty();
        (lines, pass)
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        accounts::LedgerAccounts,
        record::{CorpusKind, FailurePath, NonEmpty},
        refusal::{DolosClass, RefusalClass},
    };

    const TABLE: &str = "\
refused[PostAlonzo(FeeBelowMin)] | leaves FeeTooSmallUTxO | reads every | both
refused[DijkstraInvalidCertificate(\"refund\")] | leaves RefundIncorrectDELEG | reads sub.k4 | refused_only because no accepted record deregisters
refused[DijkstraInvalidCertificate(\"registration deposit\")] | leaves DepositIncorrectDELEG | reads top.k4 | both
refused[DijkstraInvalidMetadata] | leaves DecoderErrorDeserialiseFailure | reads top.aux,sub.aux | neither because no record holds metadata
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

    #[test]
    fn rules_on_their_declared_sides_pass_and_count_each_side() {
        let rules = rules(TABLE);
        let (lines, pass) = rules.bar(&every_side(&rules), true);
        assert!(pass, "{lines:?}");
        assert_eq!(
            lines.last().map(String::as_str),
            Some(
                "PHASE1-RULES rules=4 both=2 refused_only=1 accepted_only=0 neither=1 \
                 mismatch=0 unmapped=0 unrepresentable=1"
            )
        );
        assert!(lines.contains(&format!(
            "RULE refused[{DEPOSIT}] refused=1 accepted=1 side=both"
        )));
        assert!(lines.contains(
            &"RULE unsupported[witness fields] records=1 side=unrepresentable".to_string()
        ));
    }

    #[test]
    fn a_rule_naming_no_ledger_failure_of_its_record_is_a_mismatch() {
        let rules = rules(TABLE);
        let mut tally = every_side(&rules);
        let swapped = dolos(REFUND);
        let ledger = refused(&["DepositIncorrectDELEG"]);
        judge(&rules, 6, &ledger, &swapped, &mut tally);
        let (lines, pass) = rules.bar(&tally, false);
        assert!(!pass);
        assert!(lines.contains(&format!(
            "RULE_MISMATCH supp#6 dolos=refused[{REFUND}] ledger=refused[DepositIncorrectDELEG]"
        )));
        assert!(lines.last().unwrap().contains(" mismatch=1 unmapped=0 "));
    }

    #[test]
    fn a_refusal_by_a_rule_the_table_lacks_is_unmapped() {
        let rules = rules(TABLE);
        let mut tally = every_side(&rules);
        let ledger = refused(&["FeeTooSmallUTxO"]);
        let unlisted = dolos("PostAlonzo(MaxTxSizeExceeded)");
        judge(&rules, 7, &ledger, &unlisted, &mut tally);
        let (lines, pass) = rules.bar(&tally, false);
        assert!(!pass);
        assert!(lines.contains(
            &"RULE_UNMAPPED supp#7 dolos=refused[PostAlonzo(MaxTxSizeExceeded)] \
              ledger=refused[FeeTooSmallUTxO]"
                .to_string()
        ));
        assert!(lines.last().unwrap().contains(" mismatch=0 unmapped=1 "));
    }

    const INTEGER: &str = "integer operand outside audited registration range";

    #[test]
    fn an_unsupported_phase_two_evaluation_is_no_phase_one_answer() {
        let rules = rules(TABLE);
        let mut tally = every_side(&rules);
        let evaluation = DolosVerdict::Phase2Unsupported(INTEGER.to_string());
        judge(&rules, 8, &accepted(), &evaluation, &mut tally);
        let (lines, pass) = rules.bar(&tally, false);
        assert!(pass, "{lines:?}");
        assert!(lines.last().unwrap().contains(" mismatch=0 unmapped=0 "));
    }

    #[test]
    fn an_unsupported_phase_one_answer_the_table_lacks_is_unmapped() {
        let rules = rules(TABLE);
        let mut tally = every_side(&rules);
        let feature = DolosVerdict::Unsupported(INTEGER.to_string());
        judge(&rules, 8, &accepted(), &feature, &mut tally);
        let (lines, pass) = rules.bar(&tally, false);
        assert!(!pass);
        assert!(lines.contains(&format!(
            "RULE_UNMAPPED supp#8 dolos=unsupported[{INTEGER}] ledger=accepted"
        )));
    }

    #[test]
    fn a_rule_found_on_other_sides_than_declared_fails_only_a_whole_run() {
        let rules = rules(TABLE);
        let mut tally = RuleTally::default();
        let fee = dolos("PostAlonzo(FeeBelowMin)");
        judge(&rules, 1, &refused(&["FeeTooSmallUTxO"]), &fee, &mut tally);
        let (_, pass) = rules.bar(&tally, false);
        assert!(pass);
        let (lines, pass) = rules.bar(&tally, true);
        assert!(!pass);
        assert!(lines.contains(
            &"RULE_SIDE refused[PostAlonzo(FeeBelowMin)] declared=both found=refused_only"
                .to_string()
        ));
    }

    #[test]
    fn a_known_record_passes_only_with_its_rule_and_ledger_answer() {
        let known =
            format!("{TABLE}known supp#6 | refused[{REFUND}] | refused[DepositIncorrectDELEG]\n");
        let rules = rules(&known);
        let ledger = refused(&["DepositIncorrectDELEG"]);
        let mut tally = every_side(&rules);
        judge(&rules, 6, &ledger, &dolos(REFUND), &mut tally);
        let (lines, pass) = rules.bar(&tally, true);
        assert!(pass, "{lines:?}");
        assert!(lines.contains(&format!(
            "RULE_KNOWN supp#6 dolos=refused[{REFUND}] ledger=refused[DepositIncorrectDELEG]"
        )));
        let mut tally = every_side(&rules);
        let fee = dolos("PostAlonzo(FeeBelowMin)");
        judge(&rules, 6, &ledger, &fee, &mut tally);
        let (lines, pass) = rules.bar(&tally, true);
        assert!(!pass);
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("RULE_KNOWN_STALE supp#6 ")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_known_record_the_run_does_not_judge_fails_only_a_whole_run() {
        let known =
            format!("{TABLE}known supp#9 | refused[{REFUND}] | refused[DepositIncorrectDELEG]\n");
        let rules = rules(&known);
        let tally = every_side(&rules);
        let (_, pass) = rules.bar(&tally, false);
        assert!(pass);
        let (lines, pass) = rules.bar(&tally, true);
        assert!(!pass);
        assert!(lines.contains(&"RULE_KNOWN_UNJUDGED supp#9".to_string()));
    }

    #[test]
    fn a_row_lacking_a_side_without_a_reason_does_not_parse() {
        let row =
            "refused[PostAlonzo(FeeBelowMin)] | leaves FeeTooSmallUTxO | reads every | neither";
        let error = parse("phase1-rules.txt", row).unwrap_err();
        assert_eq!(error.line, 1);
        let twice =
            format!("{TABLE}refused[PostAlonzo(FeeBelowMin)] | leaves A | reads every | both");
        let error = parse("phase1-rules.txt", &twice).unwrap_err();
        assert_eq!(error.line, 6);
        let empty = "refused[PostAlonzo(FeeBelowMin)] | leaves | reads every | both";
        assert!(parse("phase1-rules.txt", empty).is_err());
    }

    #[test]
    fn a_row_naming_a_leaf_without_a_refusal_class_does_not_parse() {
        let row = "refused[PostAlonzo(FeeBelowMin)] | leaves FeeTooSmallUTxO,FeeTooSmall | reads every | both";
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

    #[test]
    fn the_committed_table_parses() {
        committed().unwrap_or_else(|e| panic!("{e}"));
    }
}
