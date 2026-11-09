//! Mutants of the records the ledger accepted, and the judge that counts a
//! mutant as agreeing only when every verdict on it agrees.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::{
    coverage::Member,
    judgement::{Axis, CellKey, EntryPoint, Judge, Outcome, Pass, Subject, Tally, Verdict},
    record::{Record, RecordId},
};

/// One line of the mutant corpus.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MutantLine {
    pub mutation: Mutation,
    pub record: Record,
}

/// The change one operator made at one site of a record the ledger accepted.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mutation {
    pub operator: String,
    pub parent: RecordId,
    pub site: String,
}

/// The number of mutants of one operator the corpus holds, judged and agreed
/// on.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub made: usize,
    pub compared: usize,
    pub agree: usize,
}

/// The counts of each operator of the mutant corpus.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MutationTally {
    counts: BTreeMap<String, Counts>,
}

impl MutationTally {
    /// The tally of a corpus of `mutations`, none judged yet.
    pub fn new<'a>(mutations: impl IntoIterator<Item = &'a Mutation>) -> Self {
        let mut counts: BTreeMap<String, Counts> = BTreeMap::new();
        for mutation in mutations {
            counts.entry(mutation.operator.clone()).or_default().made += 1;
        }
        MutationTally { counts }
    }

    /// Judges mutant `id` made by `mutation` from `verdicts`, every verdict the
    /// judges gave on its run, adding to `out` an agreeing verdict per entry
    /// point when each is an agreement and the validation judge gave one,
    /// or else each failing verdict on the mutant and a disagreement naming
    /// the first verdict that is not an agreement.
    pub fn judge(&mut self, id: RecordId, mutation: &Mutation, verdicts: Tally, out: &mut Tally) {
        let op = &mutation.operator;
        let counts = self.counts.entry(op.clone()).or_default();
        counts.compared += 1;
        let Tally { passing, failing } = verdicts;
        let on_record = failing.into_iter();
        let failing: Vec<Verdict> = on_record.collect();
        let other = passing.keys().find(|p| p.pass != Pass::Agree);
        let validated = passing.keys().any(|p| p.judge == Judge::Validation);
        let entries: BTreeSet<EntryPoint> = passing.keys().map(|p| p.entry).collect();
        let first_failing = failing.first().map(|v| (v.judge, v.entry, "disagree"));
        let first_other = other.map(|p| (p.judge, p.entry, pass_name(p.pass)));
        let detail = match (first_failing, first_other, validated) {
            (Some((judge, entry, how)), _, _) | (None, Some((judge, entry, how)), _) => {
                Some((entry, format!("op={op} first={judge}@{entry} {how}")))
            }
            (None, None, false) => Some((EntryPoint::SubmitTx, format!("op={op} validated=0"))),
            (None, None, true) => None,
        };
        let Some((entry, detail)) = detail else {
            counts.agree += 1;
            for entry in entries {
                out.add(Verdict {
                    judge: Judge::Mutation,
                    entry,
                    subject: Subject::Record(id),
                    cells: vec![cell(op, entry)],
                    outcome: Outcome::Agree,
                });
            }
            return;
        };
        for verdict in failing {
            out.add(verdict);
        }
        let cells = vec![cell(op, entry)];
        out.add(Verdict::disagree(Judge::Mutation, entry, id, cells, detail));
    }

    /// The `MUTATION` line of each operator, adding a failing verdict to `out`
    /// for an operator with a mutant the run did not judge.
    pub fn report(&self, out: &mut Tally) -> Vec<String> {
        let mut lines = Vec::new();
        for (
            op,
            Counts {
                made,
                compared,
                agree,
            },
        ) in &self.counts
        {
            lines.push(format!(
                "MUTATION op={op} compared={compared} agree={agree}"
            ));
            if compared != made {
                let detail = format!("op={op} made={made} compared={compared}");
                out.add(Verdict::selection_fails(
                    Judge::Mutation,
                    EntryPoint::SubmitTx,
                    detail,
                ));
            }
        }
        lines
    }

    /// Adds to `out` the failing verdicts of `corpus`, every mutant judged
    /// together, that are on the selection rather than on one mutant.
    pub fn select(corpus: Tally, out: &mut Tally) {
        let failing = corpus.failing.into_iter();
        for verdict in failing.filter(|v| v.subject == Subject::Selection) {
            out.add(verdict);
        }
    }

    /// The coverage members of the mutation axis, each operator of the corpus
    /// at each entry point.
    pub fn members(&self) -> Vec<Member> {
        let ops = self.counts.keys();
        let cells = ops.flat_map(|op| EntryPoint::every().into_iter().map(move |e| cell(op, e)));
        cells
            .map(|cell| Member::counted(cell.axis, cell.member))
            .collect()
    }
}

/// The cell of operator `op` at entry point `entry`.
fn cell(op: &str, entry: EntryPoint) -> CellKey {
    CellKey::new(Axis::Mutation, format!("{op}@{entry}"))
}

/// The name a mutation disagreement prints for a pass that is not an agreement.
fn pass_name(pass: Pass) -> &'static str {
    match pass {
        Pass::Agree => "agree",
        Pass::Known => "known",
        Pass::Excluded => "excluded",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{judgement::Passed, record::CorpusKind};

    fn id(n: u32) -> RecordId {
        RecordId {
            corpus: CorpusKind::Mut,
            n,
        }
    }

    fn mutation(op: &str) -> Mutation {
        Mutation {
            operator: op.to_string(),
            parent: RecordId {
                corpus: CorpusKind::Supp,
                n: 1,
            },
            site: "top witness 0 0".to_string(),
        }
    }

    fn passed(judge: Judge, entry: EntryPoint, pass: Pass) -> Passed {
        Passed {
            judge,
            entry,
            cells: Vec::new(),
            pass,
        }
    }

    /// A tally in which the validation and fee judges agree.
    fn agreeing() -> Tally {
        let mut tally = Tally::default();
        tally.pass(
            passed(Judge::Validation, EntryPoint::SubmitTx, Pass::Agree),
            1,
        );
        tally.pass(passed(Judge::Cost, EntryPoint::MinFee, Pass::Agree), 1);
        tally
    }

    fn judged(verdicts: Tally) -> (MutationTally, Tally) {
        let made = mutation("drop_witness");
        let mut mutations = MutationTally::new([&made]);
        let mut out = Tally::default();
        mutations.judge(id(7), &made, verdicts, &mut out);
        (mutations, out)
    }

    #[test]
    fn a_mutant_every_judge_agrees_on_agrees_at_each_entry_point() {
        let (mutations, mut out) = judged(agreeing());
        assert!(out.failing.is_empty(), "{out}");
        let cells: Vec<String> = out
            .passed(Judge::Mutation)
            .map(|(p, n)| format!("{} {} {:?} {n}", p.entry, p.cells[0], p.pass))
            .collect();
        assert_eq!(
            cells,
            [
                "submit_tx mutation:drop_witness@submit_tx Agree 1",
                "min_fee mutation:drop_witness@min_fee Agree 1"
            ]
        );
        let lines = mutations.report(&mut out);
        assert_eq!(lines, ["MUTATION op=drop_witness compared=1 agree=1"]);
        assert!(out.failing.is_empty(), "{out}");
    }

    #[test]
    fn a_known_or_excluded_pass_does_not_agree() {
        for pass in [Pass::Known, Pass::Excluded] {
            let mut verdicts = agreeing();
            verdicts.pass(passed(Judge::Accounts, EntryPoint::ReadAccounts, pass), 1);
            let (mutations, mut out) = judged(verdicts);
            assert_eq!(out.passed(Judge::Mutation).count(), 0);
            let shown: Vec<String> = out.failing.iter().map(Verdict::to_string).collect();
            let how = pass_name(pass);
            assert_eq!(
                shown,
                [format!(
                    "VERDICT judge=mutation entry=read_accounts record=mut#7 \
                     cells=mutation:drop_witness@read_accounts outcome=disagree \
                     op=drop_witness first=accounts@read_accounts {how}"
                )]
            );
            let lines = mutations.report(&mut out);
            assert_eq!(lines, ["MUTATION op=drop_witness compared=1 agree=0"]);
        }
    }

    #[test]
    fn a_failing_verdict_does_not_agree_and_fails_the_run() {
        let mut verdicts = agreeing();
        let unsupported = vec![CellKey::new(Axis::Unsupported, "witness fields")];
        let failing = Verdict::disagree(
            Judge::Validation,
            EntryPoint::SubmitTx,
            id(7),
            unsupported,
            "unsupported",
        );
        verdicts.add(failing.clone());
        let (_, out) = judged(verdicts);
        assert_eq!(out.passed(Judge::Mutation).count(), 0);
        assert_eq!(out.failing.len(), 2);
        assert_eq!(out.failing[0], failing);
        assert!(out.failing[1]
            .to_string()
            .ends_with("op=drop_witness first=validation@submit_tx disagree"));
    }

    #[test]
    fn a_selection_verdict_on_a_mutant_run_is_not_a_verdict_on_the_mutant() {
        let mut verdicts = agreeing();
        let unjudged = Verdict::selection_fails(Judge::AsOutput, EntryPoint::ReadUtxos, "judged=0");
        verdicts.add(unjudged);
        let (mutations, mut out) = judged(verdicts);
        assert!(out.failing.is_empty(), "{out}");
        assert_eq!(out.passed(Judge::Mutation).count(), 2);
        let lines = mutations.report(&mut out);
        assert_eq!(lines, ["MUTATION op=drop_witness compared=1 agree=1"]);
    }

    #[test]
    fn the_corpus_adds_its_selection_failures_and_not_its_record_failures() {
        let mut corpus = agreeing();
        let unjudged = Verdict::selection_fails(Judge::AsOutput, EntryPoint::ReadUtxos, "judged=0");
        let on_record = Verdict::disagree(
            Judge::Validation,
            EntryPoint::SubmitTx,
            id(7),
            Vec::new(),
            "refused",
        );
        corpus.add(on_record);
        corpus.add(unjudged.clone());
        let mut out = Tally::default();
        MutationTally::select(corpus, &mut out);
        assert_eq!(out.failing, [unjudged]);
        assert!(out.passing.is_empty(), "{out}");
    }

    #[test]
    fn a_mutant_without_a_validation_verdict_does_not_agree() {
        let mut verdicts = Tally::default();
        verdicts.pass(passed(Judge::Cost, EntryPoint::MinFee, Pass::Agree), 1);
        let (_, out) = judged(verdicts);
        assert_eq!(out.passed(Judge::Mutation).count(), 0);
        let shown: Vec<String> = out.failing.iter().map(Verdict::to_string).collect();
        assert_eq!(
            shown,
            ["VERDICT judge=mutation entry=submit_tx record=mut#7 \
              cells=mutation:drop_witness@submit_tx outcome=disagree op=drop_witness validated=0"]
        );
    }

    #[test]
    fn an_unjudged_mutant_fails_its_operator() {
        let made = mutation("reorder_set");
        let mutations = MutationTally::new([&made, &made]);
        let mut out = Tally::default();
        let lines = mutations.report(&mut out);
        assert_eq!(lines, ["MUTATION op=reorder_set compared=0 agree=0"]);
        let shown: Vec<String> = out.failing.iter().map(Verdict::to_string).collect();
        assert_eq!(
            shown,
            [
                "VERDICT judge=mutation entry=submit_tx record=selection cells= \
              outcome=disagree op=reorder_set made=2 compared=0"
            ]
        );
    }

    #[test]
    fn each_operator_has_a_member_at_each_entry_point() {
        let mutations = MutationTally::new([&mutation("flip_guard"), &mutation("drop_witness")]);
        let members: Vec<String> = mutations
            .members()
            .iter()
            .map(|m| m.cell.to_string())
            .collect();
        let every = EntryPoint::every().len();
        assert_eq!(members.len(), 2 * every);
        assert_eq!(members[0], "mutation:drop_witness@submit_tx");
        assert_eq!(members[every], "mutation:flip_guard@submit_tx");
    }

    #[test]
    fn a_mutant_line_names_its_parent_and_refuses_an_unknown_field() {
        let line = r#"{"operator":"drop_witness","parent":"ic#851","site":"top witness 0 0"}"#;
        let parsed: Mutation = serde_json::from_str(line).unwrap();
        assert_eq!(parsed.parent.to_string(), "ic#851");
        let extra = r#"{"operator":"o","parent":"ic#1","site":"s","seed":1}"#;
        assert!(serde_json::from_str::<Mutation>(extra).is_err());
        let unknown = r#"{"operator":"o","parent":"zz#1","site":"s"}"#;
        assert!(serde_json::from_str::<Mutation>(unknown).is_err());
    }
}
