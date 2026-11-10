//! Mutants of the records the ledger accepted, and the judge that counts a
//! mutant as agreeing only when every verdict on it agrees.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::{
    coverage::Member,
    judgement::{
        Axis, CellKey, EntryPoint, Instrument, Judge, Outcome, Pass, Subject, Tally, Verdict,
    },
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

/// The number of mutants of one operator the corpus holds, judged, agreed on
/// and disagreed on only as a known list declares.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub made: usize,
    pub compared: usize,
    pub agree: usize,
    pub known: usize,
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

    /// Judges mutant `id` made by `mutation` from the `verdicts` on its run,
    /// adding to `out` an agreement per entry point when every applicable
    /// verdict agrees and validation gave one, a known verdict when the rest
    /// agree, or else its failing verdicts and a disagreement.
    pub fn judge(&mut self, id: RecordId, mutation: &Mutation, verdicts: Tally, out: &mut Tally) {
        let op = &mutation.operator;
        let counts = self.counts.entry(op.clone()).or_default();
        counts.compared += 1;
        let Tally { passing, failing } = verdicts;
        let on_record = failing
            .into_iter()
            .filter(|v| v.subject != Subject::Selection);
        let failing: Vec<Verdict> = on_record.collect();
        let applies = passing.keys().filter(|p| p.pass != Pass::Unrepresentable);
        let other = applies
            .clone()
            .find(|p| p.pass != Pass::Agree && p.pass != Pass::Known);
        let known = applies.clone().find(|p| p.pass == Pass::Known);
        let validated = passing.keys().any(|p| p.judge == Judge::Validation);
        let agreed = applies.filter(|p| p.pass == Pass::Agree);
        let entries: BTreeSet<EntryPoint> = agreed.filter_map(|p| p.entry).collect();
        let first_failing = failing.first().map(|v| (v.judge, v.entry, "disagree"));
        let first_other = other.map(|p| (p.judge, p.entry, pass_name(p.pass)));
        let detail = match (first_failing, first_other, validated) {
            (Some((judge, entry, how)), _, _) | (None, Some((judge, entry, how)), _) => {
                let shown = shown(entry);
                Some((entry, format!("op={op} first={judge}@{shown} {how}")))
            }
            (None, None, false) => Some((Some(VALIDATION), format!("op={op} validated=0"))),
            (None, None, true) => None,
        };
        if let (None, Some(known)) = (&detail, known) {
            counts.known += 1;
            let (judge, entry) = (known.judge, known.entry);
            out.add(Verdict {
                judge: Judge::Mutation,
                entry,
                subject: Subject::Record(id),
                cells: cells(op, entry),
                outcome: Outcome::Known(format!("op={op} first={judge}@{} known", shown(entry))),
            });
            return;
        }
        let Some((entry, detail)) = detail else {
            counts.agree += 1;
            for entry in entries {
                out.add(Verdict {
                    judge: Judge::Mutation,
                    entry: Some(entry),
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
        out.add(Verdict {
            judge: Judge::Mutation,
            entry,
            subject: Subject::Record(id),
            cells: cells(op, entry),
            outcome: Outcome::Disagree(detail),
        });
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
                known,
            },
        ) in &self.counts
        {
            lines.push(format!(
                "MUTATION op={op} compared={compared} agree={agree} known={known}"
            ));
            if compared != made {
                let detail = format!("op={op} made={made} compared={compared}");
                out.add(Verdict::selection_fails(
                    Judge::Mutation,
                    VALIDATION,
                    detail,
                ));
            }
        }
        lines
    }

    /// Adds to `out` the failing verdicts of `corpus`, every mutant judged
    /// together, that are on a record no run in `held` judged or on the
    /// selection, save the as-output judge's selection verdict, which compares
    /// only inputs another record of a run made and so judges nothing when each
    /// mutant runs alone.
    pub fn select(corpus: Tally, held: &BTreeSet<RecordId>, out: &mut Tally) {
        let on_no_mutant = |v: &Verdict| match v.subject {
            Subject::Record(id) => !held.contains(&id),
            Subject::Selection => v.judge != Judge::AsOutput,
        };
        for verdict in corpus.failing.into_iter().filter(on_no_mutant) {
            out.add(verdict);
        }
    }

    /// Adds to `out` the verdicts of `pool`, the named records of every corpus
    /// judged together, save the failing verdicts on a mutant in `held`, which
    /// the judgement of its own run adds.
    pub fn select_pooled(pool: Tally, held: &BTreeSet<RecordId>, out: &mut Tally) {
        let Tally { passing, failing } = pool;
        let on_held = |v: &Verdict| matches!(v.subject, Subject::Record(id) if held.contains(&id));
        let failing = failing.into_iter().filter(|v| !on_held(v)).collect();
        out.merge(Tally { passing, failing });
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

/// The entry point of the validation judge, which a mutant needs a verdict from
/// to agree.
const VALIDATION: EntryPoint = EntryPoint::Instrument(Instrument::ReceiveTx);

/// The cell of operator `op` at entry point `entry`.
fn cell(op: &str, entry: EntryPoint) -> CellKey {
    CellKey::new(Axis::Mutation, format!("{op}@{entry}"))
}

/// The cell of operator `op` at `entry`, none for a verdict that names no entry
/// point.
fn cells(op: &str, entry: Option<EntryPoint>) -> Vec<CellKey> {
    entry.map(|entry| cell(op, entry)).into_iter().collect()
}

/// The name a mutation line prints for `entry`, `none` when there is none.
fn shown(entry: Option<EntryPoint>) -> String {
    entry.map_or("none".to_string(), |entry| entry.to_string())
}

/// The name a mutation disagreement prints for a pass that is not an agreement.
fn pass_name(pass: Pass) -> &'static str {
    match pass {
        Pass::Agree => "agree",
        Pass::Known => "known",
        Pass::Excluded => "excluded",
        Pass::Unrepresentable => "unrepresentable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        judgement::{Passed, Route, Version},
        record::CorpusKind,
    };

    const MIN_FEE: EntryPoint = EntryPoint::Instrument(Instrument::MinFee);
    const READ_ACCOUNTS: EntryPoint = EntryPoint::Instrument(Instrument::ReadAccounts);
    const FETCH_BLOCK: EntryPoint = EntryPoint::Route(Route::FetchBlock, Version::V1);

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
            entry: Some(entry),
            cells: Vec::new(),
            pass,
        }
    }

    /// A tally in which the validation and fee judges agree.
    fn agreeing() -> Tally {
        let mut tally = Tally::default();
        tally.pass(passed(Judge::Validation, VALIDATION, Pass::Agree), 1);
        tally.pass(passed(Judge::Cost, MIN_FEE, Pass::Agree), 1);
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
            .map(|(p, n)| format!("{} {} {:?} {n}", shown(p.entry), p.cells[0], p.pass))
            .collect();
        assert_eq!(
            cells,
            [
                "receive_tx mutation:drop_witness@receive_tx Agree 1",
                "min_fee mutation:drop_witness@min_fee Agree 1"
            ]
        );
        let lines = mutations.report(&mut out);
        assert_eq!(
            lines,
            ["MUTATION op=drop_witness compared=1 agree=1 known=0"]
        );
        assert!(out.failing.is_empty(), "{out}");
    }

    #[test]
    fn a_known_pass_does_not_agree_and_passes_the_run_as_known() {
        let mut verdicts = agreeing();
        let known = passed(Judge::EvalOracle, MIN_FEE, Pass::Known);
        verdicts.pass(known, 1);
        let (mutations, mut out) = judged(verdicts);
        assert!(out.failing.is_empty(), "{out}");
        let lines: Vec<String> = out
            .passed(Judge::Mutation)
            .map(|(p, n)| format!("{} {} {:?} {n}", shown(p.entry), p.cells[0], p.pass))
            .collect();
        assert_eq!(lines, ["min_fee mutation:drop_witness@min_fee Known 1"]);
        let lines = mutations.report(&mut out);
        assert_eq!(
            lines,
            ["MUTATION op=drop_witness compared=1 agree=0 known=1"]
        );
    }

    #[test]
    fn an_excluded_pass_does_not_agree_and_fails_the_run_before_a_known_one() {
        let mut verdicts = agreeing();
        let excluded = passed(Judge::Accounts, READ_ACCOUNTS, Pass::Excluded);
        verdicts.pass(excluded, 1);
        let known = passed(Judge::EvalOracle, MIN_FEE, Pass::Known);
        verdicts.pass(known, 1);
        let (mutations, mut out) = judged(verdicts);
        assert_eq!(out.passed(Judge::Mutation).count(), 0);
        let shown: Vec<String> = out.failing.iter().map(Verdict::to_string).collect();
        assert_eq!(
            shown,
            ["VERDICT judge=mutation entry=read_accounts record=mut#7 \
              cells=mutation:drop_witness@read_accounts outcome=disagree \
              op=drop_witness first=accounts@read_accounts excluded"]
        );
        let lines = mutations.report(&mut out);
        assert_eq!(
            lines,
            ["MUTATION op=drop_witness compared=1 agree=0 known=0"]
        );
    }

    #[test]
    fn an_unrepresentable_pass_does_not_apply_and_gives_its_entry_no_cell() {
        let mut verdicts = agreeing();
        let v1beta = EntryPoint::Route(Route::EvalTx, Version::V1beta);
        let unrepresentable = passed(Judge::EvalOracle, v1beta, Pass::Unrepresentable);
        verdicts.pass(unrepresentable, 1);
        let (mutations, mut out) = judged(verdicts);
        assert!(out.failing.is_empty(), "{out}");
        let entries: Vec<String> = out
            .passed(Judge::Mutation)
            .map(|(p, _)| shown(p.entry))
            .collect();
        assert_eq!(entries, ["receive_tx", "min_fee"]);
        let lines = mutations.report(&mut out);
        assert_eq!(
            lines,
            ["MUTATION op=drop_witness compared=1 agree=1 known=0"]
        );
    }

    #[test]
    fn an_unsupported_validation_answer_does_not_agree() {
        let mut verdicts = Tally::default();
        let unsupported = Passed {
            cells: vec![CellKey::new(Axis::Unsupported, "witness fields")],
            ..passed(Judge::Validation, VALIDATION, Pass::Excluded)
        };
        verdicts.pass(unsupported, 1);
        verdicts.pass(passed(Judge::Cost, MIN_FEE, Pass::Agree), 1);
        let (mutations, mut out) = judged(verdicts);
        assert_eq!(out.passed(Judge::Mutation).count(), 0);
        let shown: Vec<String> = out.failing.iter().map(Verdict::to_string).collect();
        assert_eq!(
            shown,
            ["VERDICT judge=mutation entry=receive_tx record=mut#7 \
              cells=mutation:drop_witness@receive_tx outcome=disagree \
              op=drop_witness first=validation@receive_tx excluded"]
        );
        let lines = mutations.report(&mut out);
        assert_eq!(
            lines,
            ["MUTATION op=drop_witness compared=1 agree=0 known=0"]
        );
    }

    #[test]
    fn a_failing_verdict_does_not_agree_and_fails_the_run() {
        let mut verdicts = agreeing();
        let unsupported = vec![CellKey::new(Axis::Unsupported, "witness fields")];
        let failing = Verdict::disagree(
            Judge::Validation,
            VALIDATION,
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
            .ends_with("op=drop_witness first=validation@receive_tx disagree"));
    }

    #[test]
    fn a_selection_verdict_on_a_mutant_run_is_not_a_verdict_on_the_mutant() {
        let mut verdicts = agreeing();
        let read_utxos = EntryPoint::Route(Route::ReadUtxos, Version::V1);
        let unjudged = Verdict::selection_fails(Judge::AsOutput, read_utxos, "judged=0");
        verdicts.add(unjudged);
        let (mutations, mut out) = judged(verdicts);
        assert!(out.failing.is_empty(), "{out}");
        assert_eq!(out.passed(Judge::Mutation).count(), 2);
        let lines = mutations.report(&mut out);
        assert_eq!(
            lines,
            ["MUTATION op=drop_witness compared=1 agree=1 known=0"]
        );
    }

    #[test]
    fn the_corpus_adds_no_as_output_selection_failure() {
        let mut corpus = agreeing();
        let unjudged = Verdict::selection_fails(Judge::AsOutput, FETCH_BLOCK, "judged=0");
        corpus.add(unjudged);
        let mut out = Tally::default();
        MutationTally::select(corpus, &BTreeSet::new(), &mut out);
        assert!(out.failing.is_empty(), "{out}");
    }

    #[test]
    fn the_corpus_adds_a_failure_on_a_mutant_no_run_held() {
        let mut corpus = agreeing();
        let fail = |n| {
            Verdict::disagree(
                Judge::EvalOracle,
                Instrument::EstimateTxCalls.into(),
                id(n),
                Vec::new(),
                "not judged",
            )
        };
        corpus.add(fail(7));
        corpus.add(fail(9));
        let mut out = Tally::default();
        MutationTally::select(corpus, &BTreeSet::from([id(7)]), &mut out);
        assert_eq!(out.failing, [fail(9)]);
    }

    #[test]
    fn the_corpus_adds_its_selection_failures_and_not_its_record_failures() {
        let mut corpus = agreeing();
        let unjudged = Verdict::selection_fails(Judge::Accounts, READ_ACCOUNTS, "judged=0");
        let on_record =
            Verdict::disagree(Judge::Validation, VALIDATION, id(7), Vec::new(), "refused");
        corpus.add(on_record);
        corpus.add(unjudged.clone());
        let mut out = Tally::default();
        MutationTally::select(corpus, &BTreeSet::from([id(7)]), &mut out);
        assert_eq!(out.failing, [unjudged]);
        assert!(out.passing.is_empty(), "{out}");
    }

    #[test]
    fn a_pool_adds_its_verdicts_save_the_failures_on_a_held_mutant() {
        let mut pool = agreeing();
        let fail =
            |n| Verdict::disagree(Judge::Validation, VALIDATION, id(n), Vec::new(), "refused");
        let unjudged = Verdict::selection_fails(Judge::AsOutput, FETCH_BLOCK, "judged=0");
        pool.add(fail(7));
        pool.add(fail(9));
        pool.add(unjudged.clone());
        let mut out = Tally::default();
        MutationTally::select_pooled(pool, &BTreeSet::from([id(7)]), &mut out);
        assert_eq!(out.failing, [fail(9), unjudged]);
        assert_eq!(out.passing, agreeing().passing);
    }

    #[test]
    fn a_mutant_without_a_validation_verdict_does_not_agree() {
        let mut verdicts = Tally::default();
        verdicts.pass(passed(Judge::Cost, MIN_FEE, Pass::Agree), 1);
        let (_, out) = judged(verdicts);
        assert_eq!(out.passed(Judge::Mutation).count(), 0);
        let shown: Vec<String> = out.failing.iter().map(Verdict::to_string).collect();
        assert_eq!(
            shown,
            ["VERDICT judge=mutation entry=receive_tx record=mut#7 \
              cells=mutation:drop_witness@receive_tx outcome=disagree op=drop_witness validated=0"]
        );
    }

    #[test]
    fn an_unjudged_mutant_fails_its_operator() {
        let made = mutation("reorder_set");
        let mutations = MutationTally::new([&made, &made]);
        let mut out = Tally::default();
        let lines = mutations.report(&mut out);
        assert_eq!(
            lines,
            ["MUTATION op=reorder_set compared=0 agree=0 known=0"]
        );
        let shown: Vec<String> = out.failing.iter().map(Verdict::to_string).collect();
        assert_eq!(
            shown,
            [
                "VERDICT judge=mutation entry=receive_tx record=selection cells= \
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
        assert_eq!(members[0], "mutation:drop_witness@receive_tx");
        assert_eq!(members[every], "mutation:flip_guard@receive_tx");
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
