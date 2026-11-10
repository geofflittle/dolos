//! The coverage table: every member of each axis the oracle judges, the number of agreeing
//! verdicts that touch it, and the gate that holds each number at its committed value.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    judgement::{self, Axis, CellKey, Judge, Pass, Passed, Subject, Verdict},
    manifest::{lines, ManifestError},
};

const COMMITTED: &str = include_str!("../../../test_data/ledger-oracle/coverage.txt");

/// How the oracle reaches one member of an axis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    Counted,
    /// No record of the corpora can touch the member, for `reason`, which `command` shows.
    Excluded {
        reason: String,
        command: String,
    },
}

/// One member of an axis with how the oracle reaches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub cell: CellKey,
    pub reach: Reach,
}

impl Member {
    /// The member `member` of `axis`, which the oracle counts.
    pub fn counted(axis: Axis, member: impl std::fmt::Display) -> Self {
        Member {
            cell: CellKey::new(axis, member),
            reach: Reach::Counted,
        }
    }

    /// The member `member` of `axis`, which no record can touch for `reason`, shown by `command`.
    pub fn excluded(
        axis: Axis,
        member: impl std::fmt::Display,
        reason: impl Into<String>,
        command: impl Into<String>,
    ) -> Self {
        Member {
            cell: CellKey::new(axis, member),
            reach: Reach::Excluded {
                reason: reason.into(),
                command: command.into(),
            },
        }
    }
}

/// Reads committed counts, one `<cell> <count>` per line.
pub fn parse(file: &'static str, text: &str) -> Result<BTreeMap<String, usize>, ManifestError> {
    let mut committed = BTreeMap::new();
    for (line, l) in lines(text) {
        let error = |reason: String| ManifestError { file, line, reason };
        let (cell, count) = l.split_once(' ').ok_or(error("no count".into()))?;
        let count = count
            .parse()
            .map_err(|_| error(format!("bad count {count}")))?;
        if committed.insert(cell.to_string(), count).is_some() {
            return Err(error(format!("{cell} committed twice")));
        }
    }
    Ok(committed)
}

/// The committed counts.
pub fn committed() -> Result<BTreeMap<String, usize>, ManifestError> {
    parse("coverage.txt", COMMITTED)
}

/// The number of agreeing verdicts of `tally` that touch each cell, a verdict touching the cells
/// it names and the entry cell of its entry point, the table's own verdicts aside.
pub fn counts(tally: &judgement::Tally) -> BTreeMap<CellKey, usize> {
    let mut counts = BTreeMap::new();
    let agreeing = tally
        .passing
        .iter()
        .filter(|(p, _)| p.pass == Pass::Agree && p.judge != Judge::Coverage);
    for (passed, n) in agreeing {
        let entry = passed.entry.map(|entry| CellKey::new(Axis::Entry, entry));
        for cell in passed.cells.iter().chain(&entry) {
            *counts.entry(cell.clone()).or_default() += n;
        }
    }
    counts
}

/// The count of each member in the `head` counts with the counts `part` adds
/// replaced by the counts of `rest`, or the first member whose head count is
/// below the count `part` adds or, when counted, absent from `head`.
pub fn replace_part(
    members: &[Member],
    head: &BTreeMap<String, usize>,
    part: &BTreeMap<CellKey, usize>,
    rest: &BTreeMap<CellKey, usize>,
) -> Result<BTreeMap<CellKey, usize>, String> {
    let mut counts = BTreeMap::new();
    for Member { cell, reach } in members {
        let held = match (head.get(&cell.to_string()), reach) {
            (Some(held), _) => *held,
            (None, Reach::Excluded { .. }) => 0,
            (None, Reach::Counted) => return Err(format!("{cell} head=none")),
        };
        let added = part.get(cell).copied().unwrap_or(0);
        let kept = rest.get(cell).copied().unwrap_or(0);
        let others = held.saturating_sub(added);
        counts.insert(cell.clone(), others + kept);
    }
    Ok(counts)
}

/// The coverage lines of `members` against the `counts` and the `committed`
/// counts, with the candidate line of each counted member's count, adding a
/// verdict per member to `out`, which fails for a counted member below its
/// committed count, an excluded member some verdict touches, a member listed
/// twice, a count on an axis of the members that names no member, and a
/// committed count of no counted member.
pub fn table(
    members: &[Member],
    committed: &BTreeMap<String, usize>,
    counts: &BTreeMap<CellKey, usize>,
    out: &mut judgement::Tally,
) -> Vec<String> {
    let mut lines = Vec::new();
    let mut candidates = Vec::new();
    let mut failed = Vec::new();
    let mut seen = BTreeSet::new();
    let mut axes: BTreeMap<Axis, [usize; 3]> = BTreeMap::new();
    let mut fail = |cell: &CellKey, detail: String, out: &mut judgement::Tally| {
        failed.push(cell.to_string());
        out.add(Verdict {
            judge: Judge::Coverage,
            entry: None,
            subject: Subject::Selection,
            cells: vec![cell.clone()],
            outcome: judgement::Outcome::Disagree(detail),
        });
    };
    let pass = |cell: &CellKey, pass, out: &mut judgement::Tally| {
        let passed = Passed {
            judge: Judge::Coverage,
            entry: None,
            cells: vec![cell.clone()],
            pass,
        };
        out.pass(passed, 1);
    };
    for Member { cell, reach } in members {
        if !seen.insert(cell) {
            fail(cell, "listed=2".into(), out);
            continue;
        }
        let records = counts.get(cell).copied().unwrap_or(0);
        let held = committed.get(&cell.to_string()).copied();
        let tally = axes.entry(cell.axis).or_default();
        let shown = held.map_or("none".to_string(), |c| c.to_string());
        let head = format!("COVERAGE {cell} records={records} committed={shown}");
        if *reach == Reach::Counted {
            candidates.push(format!("CANDIDATE coverage.txt {cell} {records}"));
        }
        match (reach, held) {
            (Reach::Counted, None) => {
                lines.push(head);
                fail(cell, format!("records={records} committed=none"), out);
            }
            (Reach::Counted, Some(held)) if records < held => {
                lines.push(head);
                fail(cell, format!("records={records} committed={held}"), out);
            }
            (Reach::Counted, Some(_)) => {
                tally[0] += 1;
                if records == 0 {
                    tally[1] += 1;
                    lines.push(format!("{head} gap"));
                } else {
                    lines.push(head);
                }
                pass(cell, Pass::Agree, out);
            }
            (Reach::Excluded { reason, command }, held) => {
                tally[2] += 1;
                lines.push(format!("{head} excluded {reason} command: {command}"));
                match (records, held) {
                    (0, None) => pass(cell, Pass::Excluded, out),
                    (_, Some(held)) => fail(cell, format!("excluded committed={held}"), out),
                    (records, None) => fail(cell, format!("excluded records={records}"), out),
                }
            }
        }
    }
    let owned: BTreeSet<Axis> = members.iter().map(|m| m.cell.axis).collect();
    let unlisted = counts
        .iter()
        .filter(|(c, _)| owned.contains(&c.axis) && !seen.contains(c));
    for (cell, records) in unlisted {
        lines.push(format!("COVERAGE {cell} records={records} member=none"));
        fail(cell, format!("records={records} member=none"), out);
    }
    let named: BTreeSet<String> = members.iter().map(|m| m.cell.to_string()).collect();
    for (cell, held) in committed.iter().filter(|(c, _)| !named.contains(*c)) {
        let detail = format!("committed={held} member=none");
        failed.push(cell.clone());
        lines.push(format!(
            "COVERAGE {cell} records=0 committed={held} member=none"
        ));
        out.add(Verdict {
            judge: Judge::Coverage,
            entry: None,
            subject: Subject::Selection,
            cells: Vec::new(),
            outcome: judgement::Outcome::Disagree(format!("cell={cell} {detail}")),
        });
    }
    for (axis, [counted, gaps, excluded]) in axes {
        lines.push(format!(
            "COVERAGE-AXIS {axis} counted={counted} gaps={gaps} excluded={excluded}"
        ));
    }
    lines.extend(candidates);
    lines.push(match failed.is_empty() {
        true => "coverage=PASS".to_string(),
        false => format!("coverage=FAIL {}", failed.join(",")),
    });
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::judgement::{EntryPoint, Instrument};
    use crate::{
        judgement::{Outcome, RunVerdict},
        record::{CorpusKind, RecordId},
    };

    fn leaf(name: &str) -> CellKey {
        CellKey::new(Axis::Leaf, name)
    }

    fn agreeing(cells: Vec<CellKey>) -> Verdict {
        Verdict {
            judge: Judge::Validation,
            entry: Some(EntryPoint::Instrument(Instrument::ReceiveTx)),
            subject: Subject::Record(RecordId {
                corpus: CorpusKind::Ic,
                n: 1,
            }),
            cells,
            outcome: Outcome::Agree,
        }
    }

    fn members() -> Vec<Member> {
        vec![
            Member::counted(Axis::Leaf, "A"),
            Member::counted(Axis::Leaf, "B"),
            Member::excluded(Axis::Leaf, "C", "no carrier", "grep C leaves.txt"),
        ]
    }

    fn committed(text: &str) -> BTreeMap<String, usize> {
        parse("coverage.txt", text).unwrap()
    }

    fn run(verdicts: Vec<Verdict>, text: &str) -> (Vec<String>, judgement::Tally) {
        let mut out = judgement::Tally::default();
        verdicts.into_iter().for_each(|v| out.add(v));
        let lines = table(&members(), &committed(text), &counts(&out), &mut out);
        (lines, out)
    }

    #[test]
    fn counts_at_their_committed_values_pass_and_a_zero_counted_member_is_a_gap() {
        let (lines, out) = run(vec![agreeing(vec![leaf("A")])], "leaf:A 1\nleaf:B 0\n");
        assert_eq!(
            lines,
            [
                "COVERAGE leaf:A records=1 committed=1",
                "COVERAGE leaf:B records=0 committed=0 gap",
                "COVERAGE leaf:C records=0 committed=none excluded no carrier command: grep C leaves.txt",
                "COVERAGE-AXIS leaf counted=2 gaps=1 excluded=1",
                "CANDIDATE coverage.txt leaf:A 1",
                "CANDIDATE coverage.txt leaf:B 0",
                "coverage=PASS",
            ]
        );
        assert!(matches!(out.verdict(), RunVerdict::Pass { .. }), "{out}");
        assert_eq!(out.by_judge().get(&Judge::Coverage), Some(&3));
    }

    #[test]
    fn a_count_below_its_committed_value_fails_on_its_cell() {
        let (lines, out) = run(vec![agreeing(vec![leaf("B")])], "leaf:A 1\nleaf:B 1\n");
        assert_eq!(lines.last().unwrap(), "coverage=FAIL leaf:A");
        assert_eq!(
            out.failing
                .iter()
                .map(Verdict::to_string)
                .collect::<Vec<_>>(),
            [
                "VERDICT judge=coverage entry=none record=selection cells=leaf:A \
              outcome=disagree records=0 committed=1"
            ]
        );
    }

    #[test]
    fn the_table_verdicts_name_no_entry_point_and_count_no_entry_cell() {
        let (_, out) = run(vec![agreeing(vec![leaf("A")])], "leaf:A 1\nleaf:B 0\n");
        let table: Vec<_> = out.passed(Judge::Coverage).map(|(p, _)| p.entry).collect();
        assert_eq!(table, [None, None, None]);
        let entries = counts(&out).into_keys().filter(|c| c.axis == Axis::Entry);
        let receive_tx = EntryPoint::Instrument(Instrument::ReceiveTx);
        assert_eq!(
            entries.collect::<Vec<_>>(),
            [CellKey::new(Axis::Entry, receive_tx)]
        );
    }

    #[test]
    fn an_excluded_member_a_verdict_touches_fails() {
        let (lines, _) = run(vec![agreeing(vec![leaf("C")])], "leaf:A 0\nleaf:B 0\n");
        assert_eq!(lines.last().unwrap(), "coverage=FAIL leaf:C");
    }

    #[test]
    fn a_member_with_no_committed_count_and_a_count_of_no_member_fail() {
        let (lines, _) = run(vec![], "leaf:A 0\nleaf:D 0\n");
        assert_eq!(lines.last().unwrap(), "coverage=FAIL leaf:B,leaf:D");
    }

    #[test]
    fn a_count_on_an_axis_of_the_members_that_names_no_member_fails() {
        let touched = vec![leaf("E"), CellKey::new(Axis::Purpose, "Spend")];
        let (lines, _) = run(vec![agreeing(touched)], "leaf:A 0\nleaf:B 0\n");
        assert!(lines.contains(&"COVERAGE leaf:E records=1 member=none".to_string()));
        assert_eq!(lines.last().unwrap(), "coverage=FAIL leaf:E");
    }

    #[test]
    fn a_member_listed_twice_fails() {
        let mut out = judgement::Tally::default();
        let mut twice = members();
        twice.push(Member::counted(Axis::Leaf, "A"));
        let lines = table(
            &twice,
            &committed("leaf:A 0\nleaf:B 0\n"),
            &counts(&out),
            &mut out,
        );
        assert_eq!(lines.last().unwrap(), "coverage=FAIL leaf:A");
    }

    #[test]
    fn only_agreeing_verdicts_count_and_each_counts_its_entry() {
        let known = Verdict {
            outcome: Outcome::Known("row=S1".into()),
            ..agreeing(vec![leaf("A")])
        };
        let mut out = judgement::Tally::default();
        out.add(known);
        out.add(agreeing(vec![leaf("B")]));
        assert_eq!(
            counts(&out),
            BTreeMap::from([
                (leaf("B"), 1),
                (
                    CellKey::new(Axis::Entry, EntryPoint::Instrument(Instrument::ReceiveTx)),
                    1
                )
            ])
        );
    }

    #[test]
    fn the_candidate_lines_read_back_as_the_counts_and_the_table_counts_none_of_its_own() {
        let (lines, mut out) = run(vec![agreeing(vec![leaf("A")])], "");
        let text: Vec<&str> = lines
            .iter()
            .filter_map(|l| l.strip_prefix("CANDIDATE coverage.txt "))
            .collect();
        assert_eq!(
            committed(&text.join("\n")),
            committed("leaf:A 1\nleaf:B 0\n")
        );
        let again = table(
            &members(),
            &committed(&text.join("\n")),
            &counts(&out),
            &mut out,
        );
        assert_eq!(again.last().unwrap(), "coverage=PASS");
        let above = table(
            &members(),
            &committed("leaf:A 2\nleaf:B 0\n"),
            &counts(&out),
            &mut out,
        );
        assert_eq!(above.last().unwrap(), "coverage=FAIL leaf:A");
    }

    fn counted(text: &str) -> BTreeMap<CellKey, usize> {
        let parsed = committed(text).into_iter();
        parsed
            .map(|(c, n)| (leaf(&c["leaf:".len()..]), n))
            .collect()
    }

    fn replaced(part: &str, rest: &str) -> Result<BTreeMap<CellKey, usize>, String> {
        let head = committed("leaf:A 1\nleaf:B 2\n");
        replace_part(&members(), &head, &counted(part), &counted(rest))
    }

    #[test]
    fn the_head_counts_with_a_part_that_adds_nothing_are_the_head_counts_and_pass() {
        let view = replaced("leaf:A 1\nleaf:B 1\n", "leaf:A 1\nleaf:B 1\n").unwrap();
        assert_eq!(view, counted("leaf:A 1\nleaf:B 2\nleaf:C 0\n"));
        let mut out = judgement::Tally::default();
        let lines = table(
            &members(),
            &committed("leaf:A 1\nleaf:B 2\n"),
            &view,
            &mut out,
        );
        assert_eq!(lines.last().unwrap(), "coverage=PASS");
    }

    #[test]
    fn the_head_counts_with_a_part_replaced_by_less_fail_on_the_cell_it_lowers() {
        let view = replaced("leaf:A 1\nleaf:B 1\n", "leaf:B 1\n").unwrap();
        assert_eq!(view, counted("leaf:A 0\nleaf:B 2\nleaf:C 0\n"));
        let mut out = judgement::Tally::default();
        let lines = table(
            &members(),
            &committed("leaf:A 1\nleaf:B 2\n"),
            &view,
            &mut out,
        );
        assert_eq!(lines.last().unwrap(), "coverage=FAIL leaf:A");
        let failing: Vec<String> = out.failing.iter().map(Verdict::to_string).collect();
        assert_eq!(
            failing,
            [
                "VERDICT judge=coverage entry=none record=selection cells=leaf:A \
              outcome=disagree records=0 committed=1"
            ]
        );
    }

    #[test]
    fn a_part_above_the_head_count_and_a_counted_member_the_head_omits_are_refused() {
        assert_eq!(
            replaced("leaf:A 2\n", "").unwrap_err(),
            "leaf:A head=1 part=2"
        );
        let head = committed("leaf:B 2\n");
        let none = BTreeMap::new();
        assert_eq!(
            replace_part(&members(), &head, &none, &none).unwrap_err(),
            "leaf:A head=none"
        );
    }

    #[test]
    fn a_count_committed_twice_is_refused() {
        let error = parse("coverage.txt", "leaf:A 1\nleaf:A 2\n").unwrap_err();
        assert_eq!(
            (error.line, error.reason.as_str()),
            (2, "leaf:A committed twice")
        );
    }
}
