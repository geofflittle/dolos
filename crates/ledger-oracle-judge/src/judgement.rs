//! One judgement of a node's answer: the judge, the entry point it drove, the
//! record, the cells it touched and its outcome, and the tally every run passes
//! or fails by.

use std::{collections::BTreeMap, fmt, str::FromStr};

use crate::{eval::Layer, record::RecordId};

/// A judge of the oracle, by the name its verdict lines print.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Judge {
    Validation,
    Rules,
    EvalOracle,
    EvalTx,
    EvalDraft,
    EvalRewardOutput,
    Context,
    Utxo,
    AsOutput,
    Accounts,
    Undo,
    Revalidate,
    Params,
    ReadTx,
    Script,
    Datum,
    PlutusData,
    ServedTx,
    ApplyBlock,
    Cost,
    Construct,
    Builtin,
    Inventory,
    Endorser,
}

impl Judge {
    /// Every judge, in the order a run prints them.
    pub const ALL: [Judge; 24] = [
        Judge::Validation,
        Judge::Rules,
        Judge::EvalOracle,
        Judge::EvalTx,
        Judge::EvalDraft,
        Judge::EvalRewardOutput,
        Judge::Context,
        Judge::Utxo,
        Judge::AsOutput,
        Judge::Accounts,
        Judge::Undo,
        Judge::Revalidate,
        Judge::Params,
        Judge::ReadTx,
        Judge::Script,
        Judge::Datum,
        Judge::PlutusData,
        Judge::ServedTx,
        Judge::ApplyBlock,
        Judge::Cost,
        Judge::Construct,
        Judge::Builtin,
        Judge::Inventory,
        Judge::Endorser,
    ];

    /// The name verdict lines and the arms manifest give the judge.
    pub fn name(self) -> &'static str {
        match self {
            Judge::Validation => "validation",
            Judge::Rules => "rules",
            Judge::EvalOracle => "eval-oracle",
            Judge::EvalTx => "eval-tx",
            Judge::EvalDraft => "eval-draft",
            Judge::EvalRewardOutput => "eval-reward-output",
            Judge::Context => "context",
            Judge::Utxo => "utxo",
            Judge::AsOutput => "as-output",
            Judge::Accounts => "accounts",
            Judge::Undo => "undo",
            Judge::Revalidate => "revalidate",
            Judge::Params => "params",
            Judge::ReadTx => "read-tx",
            Judge::Script => "script",
            Judge::Datum => "datum",
            Judge::PlutusData => "plutus-data",
            Judge::ServedTx => "served-tx",
            Judge::ApplyBlock => "apply-block",
            Judge::Cost => "cost",
            Judge::Construct => "construct",
            Judge::Builtin => "builtin",
            Judge::Inventory => "inventory",
            Judge::Endorser => "endorser",
        }
    }
}

impl fmt::Display for Judge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A name that no judge prints.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no judge is named {0}")]
pub struct UnknownJudge(pub String);

impl FromStr for Judge {
    type Err = UnknownJudge;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Judge::ALL
            .into_iter()
            .find(|judge| judge.name() == s)
            .ok_or_else(|| UnknownJudge(s.to_string()))
    }
}

/// The node entry point a judgement drove.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntryPoint {
    SubmitTx,
    EstimateTx(Layer),
    EstimateTxCalls,
    RedeemerArguments,
    MinFee,
    ApplyBlock,
    Revalidate,
    ReadUtxos,
    ReadAccounts,
    ReadParams,
    ReadData,
    ReadTx(Layer),
    FetchBlock,
}

impl fmt::Display for EntryPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EntryPoint::SubmitTx => f.write_str("submit_tx"),
            EntryPoint::EstimateTx(layer) => write!(f, "estimate_tx:{layer}"),
            EntryPoint::EstimateTxCalls => f.write_str("estimate_tx_calls"),
            EntryPoint::RedeemerArguments => f.write_str("redeemer_arguments"),
            EntryPoint::MinFee => f.write_str("min_fee"),
            EntryPoint::ApplyBlock => f.write_str("apply_block"),
            EntryPoint::Revalidate => f.write_str("revalidate"),
            EntryPoint::ReadUtxos => f.write_str("read_utxos"),
            EntryPoint::ReadAccounts => f.write_str("read_accounts"),
            EntryPoint::ReadParams => f.write_str("read_params"),
            EntryPoint::ReadData => f.write_str("read_data"),
            EntryPoint::ReadTx(layer) => write!(f, "read_tx:{layer}"),
            EntryPoint::FetchBlock => f.write_str("fetch_block"),
        }
    }
}

/// What a judgement is about, one record or the whole selection a run judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Subject {
    Record(RecordId),
    Selection,
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Subject::Record(id) => write!(f, "{id}"),
            Subject::Selection => f.write_str("selection"),
        }
    }
}

/// A member of one axis a judgement touched, printed as `axis:member`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CellKey {
    pub axis: &'static str,
    pub member: String,
}

impl CellKey {
    /// The cell of `member` on `axis`.
    pub fn new(axis: &'static str, member: impl fmt::Display) -> Self {
        CellKey {
            axis,
            member: member.to_string(),
        }
    }
}

impl fmt::Display for CellKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let member: String = self
            .member
            .chars()
            .map(|c| match c {
                ' ' | ',' | '\t' | '\n' => '_',
                c => c,
            })
            .collect();
        write!(f, "{}:{member}", self.axis)
    }
}

/// The outcome of one judgement, each with the text that shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Agree,
    /// The node disagrees in the way a manifest row lists.
    Known(String),
    /// The judge cannot compare the answer and says why.
    Excluded(String),
    Disagree(String),
}

impl Outcome {
    /// The name a verdict line prints for the outcome.
    pub fn name(&self) -> &'static str {
        match self {
            Outcome::Agree => "agree",
            Outcome::Known(_) => "known",
            Outcome::Excluded(_) => "excluded",
            Outcome::Disagree(_) => "disagree",
        }
    }
}

/// A disagreement on one record that a judge's tally holds until the judge
/// names itself and its entry point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub id: RecordId,
    pub cells: Vec<CellKey>,
    pub detail: String,
}

impl Finding {
    /// The disagreement on record `id`, touching `cells`, shown by `detail`.
    pub fn new(id: RecordId, cells: Vec<CellKey>, detail: impl Into<String>) -> Self {
        Finding {
            id,
            cells,
            detail: detail.into(),
        }
    }

    /// The failing verdict of `judge` at `entry` the finding is.
    pub fn verdict(self, judge: Judge, entry: EntryPoint) -> Verdict {
        let Finding { id, cells, detail } = self;
        Verdict::disagree(judge, entry, id, cells, detail)
    }
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.id, self.detail)
    }
}

/// One judgement of a node's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub judge: Judge,
    pub entry: EntryPoint,
    pub subject: Subject,
    pub cells: Vec<CellKey>,
    pub outcome: Outcome,
}

impl Verdict {
    /// The disagreement of `judge` at `entry` on record `id`, touching `cells`,
    /// shown by `detail`.
    pub fn disagree(
        judge: Judge,
        entry: EntryPoint,
        id: RecordId,
        cells: Vec<CellKey>,
        detail: impl Into<String>,
    ) -> Self {
        Verdict {
            judge,
            entry,
            subject: Subject::Record(id),
            cells,
            outcome: Outcome::Disagree(detail.into()),
        }
    }

    /// The failure of `judge` over the whole selection, shown by `detail`.
    pub fn selection_fails(judge: Judge, entry: EntryPoint, detail: impl Into<String>) -> Self {
        Verdict {
            judge,
            entry,
            subject: Subject::Selection,
            cells: Vec::new(),
            outcome: Outcome::Disagree(detail.into()),
        }
    }

    /// Whether the verdict fails the run.
    pub fn fails(&self) -> bool {
        match self.outcome {
            Outcome::Disagree(_) => true,
            Outcome::Agree | Outcome::Known(_) | Outcome::Excluded(_) => false,
        }
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Verdict {
            judge,
            entry,
            subject,
            cells,
            outcome,
        } = self;
        let cells: Vec<String> = cells.iter().map(CellKey::to_string).collect();
        write!(
            f,
            "VERDICT judge={judge} entry={entry} record={subject} cells={} outcome={}",
            cells.join(","),
            outcome.name()
        )?;
        match outcome {
            Outcome::Agree => Ok(()),
            Outcome::Known(text) | Outcome::Excluded(text) | Outcome::Disagree(text) => {
                write!(f, " {text}")
            }
        }
    }
}

/// The verdicts of a run: the count of passing ones by judge and the failing
/// ones whole.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tally {
    pub passing: BTreeMap<Judge, usize>,
    pub failing: Vec<Verdict>,
}

/// Whether a run passes, with the passing count by judge or the failing
/// verdicts that decide it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunVerdict {
    Pass { passing: BTreeMap<Judge, usize> },
    Fail { failing: usize },
}

impl Tally {
    /// Counts a passing verdict and keeps a failing one.
    pub fn add(&mut self, verdict: Verdict) {
        if verdict.fails() {
            self.failing.push(verdict);
        } else {
            *self.passing.entry(verdict.judge).or_default() += 1;
        }
    }

    /// Counts `n` passing verdicts of `judge` that its own counter holds.
    pub fn count(&mut self, judge: Judge, n: usize) {
        *self.passing.entry(judge).or_default() += n;
    }

    /// Adds the verdicts of another tally after this one's.
    pub fn merge(&mut self, other: Tally) {
        let Tally { passing, failing } = other;
        for (judge, n) in passing {
            self.count(judge, n);
        }
        self.failing.extend(failing);
    }

    /// Adds a failing verdict for each judge of `required` that passed no
    /// verdict, so a judge that judged nothing fails the run.
    pub fn require(&mut self, required: &[(Judge, EntryPoint)]) {
        for (judge, entry) in required {
            if self.passing.get(judge).copied().unwrap_or(0) == 0
                && !self.failing.iter().any(|v| v.judge == *judge)
            {
                self.add(Verdict::selection_fails(*judge, *entry, "judged=0"));
            }
        }
    }

    /// The run's verdict: a pass when no verdict fails and at least one passes.
    pub fn verdict(&self) -> RunVerdict {
        let passed = self.passing.values().any(|n| *n > 0);
        if self.failing.is_empty() && passed {
            RunVerdict::Pass {
                passing: self.passing.clone(),
            }
        } else {
            RunVerdict::Fail {
                failing: self.failing.len(),
            }
        }
    }
}

impl fmt::Display for Tally {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Tally { passing, failing } = self;
        for verdict in failing {
            writeln!(f, "{verdict}")?;
        }
        let passing: Vec<String> = passing.iter().map(|(j, n)| format!("{j}:{n}")).collect();
        write!(
            f,
            "VERDICTS passing={} failing={}",
            passing.join(","),
            failing.len()
        )
    }
}

impl fmt::Display for RunVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunVerdict::Pass { passing } => {
                write!(f, "RESULT PASS judged={}", passing.values().sum::<usize>())
            }
            RunVerdict::Fail { failing } => write!(f, "RESULT FAIL failing={failing}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::CorpusKind;

    fn id(n: u32) -> RecordId {
        RecordId {
            corpus: CorpusKind::Supp,
            n,
        }
    }

    fn agree(judge: Judge) -> Verdict {
        Verdict {
            judge,
            entry: EntryPoint::MinFee,
            subject: Subject::Record(id(1)),
            cells: Vec::new(),
            outcome: Outcome::Agree,
        }
    }

    #[test]
    fn a_tally_of_passing_and_known_verdicts_passes() {
        let mut tally = Tally::default();
        tally.add(agree(Judge::Cost));
        tally.add(Verdict {
            outcome: Outcome::Known("row=S1".into()),
            ..agree(Judge::Utxo)
        });
        tally.require(&[(Judge::Cost, EntryPoint::MinFee)]);
        assert_eq!(
            tally.verdict(),
            RunVerdict::Pass {
                passing: BTreeMap::from([(Judge::Utxo, 1), (Judge::Cost, 1)])
            }
        );
    }

    #[test]
    fn one_disagreement_of_any_judge_fails_the_tally() {
        for judge in Judge::ALL {
            let mut tally = Tally::default();
            tally.add(agree(Judge::Validation));
            tally.add(Verdict::disagree(
                judge,
                EntryPoint::MinFee,
                id(2),
                vec![CellKey::new("units", "mem")],
                "ledger=1 dolos=2",
            ));
            assert_eq!(tally.verdict(), RunVerdict::Fail { failing: 1 }, "{judge}");
        }
    }

    #[test]
    fn an_empty_tally_fails() {
        assert_eq!(Tally::default().verdict(), RunVerdict::Fail { failing: 0 });
    }

    #[test]
    fn a_required_judge_that_judged_nothing_fails_the_tally() {
        let mut tally = Tally::default();
        tally.add(agree(Judge::Validation));
        tally.require(&[(Judge::Cost, EntryPoint::MinFee)]);
        assert_eq!(tally.verdict(), RunVerdict::Fail { failing: 1 });
        assert_eq!(
            tally.failing[0].to_string(),
            "VERDICT judge=cost entry=min_fee record=selection cells= outcome=disagree judged=0"
        );
    }

    #[test]
    fn merging_keeps_both_counts_and_failures() {
        let mut a = Tally::default();
        a.add(agree(Judge::Cost));
        let mut b = Tally::default();
        b.add(agree(Judge::Cost));
        b.add(Verdict::disagree(
            Judge::Utxo,
            EntryPoint::ReadUtxos,
            id(3),
            Vec::new(),
            "x",
        ));
        a.merge(b);
        assert_eq!(a.passing, BTreeMap::from([(Judge::Cost, 2)]));
        assert_eq!(a.failing.len(), 1);
    }

    #[test]
    fn a_verdict_line_prints_its_cells_with_spaces_replaced() {
        let line = Verdict::disagree(
            Judge::EvalOracle,
            EntryPoint::EstimateTx(Layer::V1beta),
            id(4),
            vec![
                CellKey::new("purpose", "Spend"),
                CellKey::new("label", "new script language"),
            ],
            "ledger=(none)",
        )
        .to_string();
        assert_eq!(
            line,
            "VERDICT judge=eval-oracle entry=estimate_tx:v1beta record=supp#4 \
             cells=purpose:Spend,label:new_script_language outcome=disagree ledger=(none)"
        );
    }

    #[test]
    fn every_judge_reads_back_from_its_name() {
        for judge in Judge::ALL {
            assert_eq!(judge.name().parse(), Ok(judge));
        }
        assert_eq!(
            "nothing".parse::<Judge>(),
            Err(UnknownJudge("nothing".into()))
        );
    }
}
