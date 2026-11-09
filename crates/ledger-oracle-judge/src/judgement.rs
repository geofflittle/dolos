//! One judgement of a node's answer: the judge, the entry point it drove, the
//! record, the cells it touched and its outcome, and the tally every run passes
//! or fails by.

use std::{collections::BTreeMap, fmt};

use crate::{eval::Layer, record::RecordId};

/// A name that no member of the named kind prints.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no {kind} is named {name}")]
pub struct UnknownName {
    pub kind: &'static str,
    pub name: String,
}

macro_rules! named {
    ($(#[$doc:meta])* $kind:literal $enum:ident { $($member:ident = $name:literal),* $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $enum {
            $($member),*
        }

        impl $enum {
            /// Every member, in the order a run prints them.
            pub const ALL: [$enum; [$(stringify!($member)),*].len()] = [$($enum::$member),*];

            /// The name verdict lines and the arms manifest give the member.
            pub fn name(self) -> &'static str {
                match self {
                    $($enum::$member => $name),*
                }
            }
        }

        impl ::std::fmt::Display for $enum {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.name())
            }
        }

        impl ::std::str::FromStr for $enum {
            type Err = $crate::judgement::UnknownName;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                $enum::ALL
                    .into_iter()
                    .find(|member| member.name() == s)
                    .ok_or_else(|| $crate::judgement::UnknownName {
                        kind: $kind,
                        name: s.to_string(),
                    })
            }
        }
    };
}

pub(crate) use named;

named! {
    /// A judge of the oracle, by the name its verdict lines print.
    "judge" Judge {
        Validation = "validation",
        Rules = "rules",
        EvalOracle = "eval-oracle",
        EvalTx = "eval-tx",
        EvalDraft = "eval-draft",
        EvalRewardOutput = "eval-reward-output",
        Context = "context",
        Utxo = "utxo",
        AsOutput = "as-output",
        Accounts = "accounts",
        Undo = "undo",
        Revalidate = "revalidate",
        Params = "params",
        ReadTx = "read-tx",
        Script = "script",
        Datum = "datum",
        PlutusData = "plutus-data",
        ServedTx = "served-tx",
        ApplyBlock = "apply-block",
        Cost = "cost",
        Coverage = "coverage",
        Endorser = "endorser",
        Mutation = "mutation",
    }
}

named! {
    /// An axis of the cells a judgement touches, by the name its cells print.
    "axis" Axis {
        Answer = "answer",
        Body = "body",
        Builtin = "builtin",
        Bytes = "bytes",
        Certificate = "certificate",
        Datum = "datum",
        Demand = "demand",
        Dolos = "dolos",
        Draft = "draft",
        Entry = "entry",
        Failure = "failure",
        Field = "field",
        GovAction = "gov_action",
        Guard = "guard",
        Index = "index",
        Input = "input",
        Label = "label",
        Language = "language",
        Layer = "layer",
        Leaf = "leaf",
        Ledger = "ledger",
        Level = "level",
        List = "list",
        Listed = "listed",
        Mutation = "mutation",
        Param = "param",
        ParamKey = "param_key",
        Purpose = "purpose",
        Redeemer = "redeemer",
        Refusal = "refusal",
        RequestShape = "request_shape",
        Rollback = "rollback",
        Row = "row",
        Rule = "rule",
        Service = "service",
        Tx = "tx",
        Units = "units",
        Unsupported = "unsupported",
        Witness = "witness",
    }
}

named! {
    /// A shape of the transaction a submission or evaluation request carries.
    "request shape" RequestShape {
        Signed = "signed",
        Draft = "draft",
        RewardOutputDraft = "reward_output_draft",
        Unsigned = "unsigned",
        ZeroBudget = "zero_budget",
        GuardWithoutRedeemer = "guard_without_redeemer",
        ReferenceScriptOnly = "reference_script_only",
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

impl EntryPoint {
    /// Every entry point, each that takes a layer at every layer.
    pub fn every() -> Vec<EntryPoint> {
        use EntryPoint as E;
        let mut every = vec![
            E::SubmitTx,
            E::EstimateTxCalls,
            E::RedeemerArguments,
            E::MinFee,
            E::ApplyBlock,
            E::Revalidate,
            E::ReadUtxos,
            E::ReadAccounts,
            E::ReadParams,
            E::ReadData,
            E::FetchBlock,
        ];
        every.extend(Layer::ALL.map(E::EstimateTx));
        every.extend(Layer::ALL.map(E::ReadTx));
        every
    }
}

impl std::str::FromStr for EntryPoint {
    type Err = UnknownName;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        EntryPoint::every()
            .into_iter()
            .find(|entry| entry.to_string() == s)
            .ok_or_else(|| UnknownName {
                kind: "entry point",
                name: s.to_string(),
            })
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
    pub axis: Axis,
    pub member: String,
}

impl CellKey {
    /// The cell of `member` on `axis`.
    pub fn new(axis: Axis, member: impl fmt::Display) -> Self {
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
    /// The entry point's message cannot hold the answer the judge compares.
    Unrepresentable(String),
    Disagree(String),
}

impl Outcome {
    /// The name a verdict line prints for the outcome.
    pub fn name(&self) -> &'static str {
        match self {
            Outcome::Agree => "agree",
            Outcome::Known(_) => "known",
            Outcome::Excluded(_) => "excluded",
            Outcome::Unrepresentable(_) => "unrepresentable",
            Outcome::Disagree(_) => "disagree",
        }
    }

    /// The kind of pass the outcome is, none for a disagreement.
    pub fn pass(&self) -> Option<Pass> {
        match self {
            Outcome::Agree => Some(Pass::Agree),
            Outcome::Known(_) => Some(Pass::Known),
            Outcome::Excluded(_) => Some(Pass::Excluded),
            Outcome::Unrepresentable(_) => Some(Pass::Unrepresentable),
            Outcome::Disagree(_) => None,
        }
    }
}

/// The kind of an outcome that passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Pass {
    Agree,
    Known,
    Excluded,
    Unrepresentable,
}

/// A passing verdict without its record and text, the key a tally counts
/// passing verdicts under.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Passed {
    pub judge: Judge,
    pub entry: EntryPoint,
    pub cells: Vec<CellKey>,
    pub pass: Pass,
}

/// The member of the first of `cells` on `axis`.
pub fn member(cells: &[CellKey], axis: Axis) -> Option<&str> {
    let cell = cells.iter().find(|cell| cell.axis == axis);
    cell.map(|cell| cell.member.as_str())
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
        self.outcome.pass().is_none()
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
            Outcome::Known(text)
            | Outcome::Excluded(text)
            | Outcome::Unrepresentable(text)
            | Outcome::Disagree(text) => write!(f, " {text}"),
        }
    }
}

/// The verdicts of a run: the count of passing ones by their key and the
/// failing ones whole.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tally {
    pub passing: BTreeMap<Passed, usize>,
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
        let Some(pass) = verdict.outcome.pass() else {
            self.failing.push(verdict);
            return;
        };
        let Verdict {
            judge,
            entry,
            cells,
            ..
        } = verdict;
        let passed = Passed {
            judge,
            entry,
            cells,
            pass,
        };
        self.pass(passed, 1);
    }

    /// Counts `n` passing verdicts of one key, those of a judge that compares
    /// items rather than whole records.
    pub fn pass(&mut self, passed: Passed, n: usize) {
        if n > 0 {
            *self.passing.entry(passed).or_default() += n;
        }
    }

    /// Adds the verdicts of another tally after this one's.
    pub fn merge(&mut self, other: Tally) {
        let Tally { passing, failing } = other;
        for (passed, n) in passing {
            self.pass(passed, n);
        }
        self.failing.extend(failing);
    }

    /// The passing verdicts of `judge` with the number of each.
    pub fn passed(&self, judge: Judge) -> impl Iterator<Item = (&Passed, usize)> {
        let passing = self.passing.iter().filter(move |(p, _)| p.judge == judge);
        passing.map(|(passed, n)| (passed, *n))
    }

    /// The failing verdicts of `judge`.
    pub fn failed(&self, judge: Judge) -> impl Iterator<Item = &Verdict> {
        self.failing.iter().filter(move |v| v.judge == judge)
    }

    /// The number of passing verdicts of each judge.
    pub fn by_judge(&self) -> BTreeMap<Judge, usize> {
        let mut by_judge = BTreeMap::new();
        for (passed, n) in &self.passing {
            *by_judge.entry(passed.judge).or_default() += n;
        }
        by_judge
    }

    /// Adds a failing verdict for each judge of `required` that passed no
    /// verdict, so a judge that judged nothing fails the run.
    pub fn require(&mut self, required: &[(Judge, EntryPoint)]) {
        let by_judge = self.by_judge();
        for (judge, entry) in required {
            if by_judge.get(judge).copied().unwrap_or(0) == 0
                && !self.failing.iter().any(|v| v.judge == *judge)
            {
                self.add(Verdict::selection_fails(*judge, *entry, "judged=0"));
            }
        }
    }

    /// The run's verdict: a pass when no verdict fails and at least one passes.
    pub fn verdict(&self) -> RunVerdict {
        let passing = self.by_judge();
        let passed = passing.values().any(|n| *n > 0);
        if self.failing.is_empty() && passed {
            RunVerdict::Pass { passing }
        } else {
            RunVerdict::Fail {
                failing: self.failing.len(),
            }
        }
    }
}

impl fmt::Display for Tally {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let failing = &self.failing;
        for verdict in failing {
            writeln!(f, "{verdict}")?;
        }
        let by_judge = self.by_judge();
        let passing: Vec<String> = by_judge.iter().map(|(j, n)| format!("{j}:{n}")).collect();
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
    fn a_tally_counts_each_passing_verdict_under_its_judge_entry_cells_and_kind() {
        let spend = |judge, outcome| Verdict {
            cells: vec![CellKey::new(Axis::Purpose, "Spend")],
            outcome,
            ..agree(judge)
        };
        let mut tally = Tally::default();
        tally.add(spend(Judge::EvalOracle, Outcome::Agree));
        tally.add(spend(Judge::EvalOracle, Outcome::Agree));
        tally.add(spend(
            Judge::EvalOracle,
            Outcome::Unrepresentable("no carrier".into()),
        ));
        tally.add(spend(Judge::EvalDraft, Outcome::Agree));
        let compared = Passed {
            judge: Judge::Cost,
            entry: EntryPoint::MinFee,
            cells: Vec::new(),
            pass: Pass::Agree,
        };
        tally.pass(compared.clone(), 3);
        tally.pass(compared.clone(), 0);
        let passed = |pass| Passed {
            judge: Judge::EvalOracle,
            entry: EntryPoint::MinFee,
            cells: vec![CellKey::new(Axis::Purpose, "Spend")],
            pass,
        };
        let oracle: Vec<(&Passed, usize)> = tally.passed(Judge::EvalOracle).collect();
        assert_eq!(
            oracle,
            [
                (&passed(Pass::Agree), 2),
                (&passed(Pass::Unrepresentable), 1)
            ]
        );
        assert_eq!(
            tally.by_judge(),
            BTreeMap::from([
                (Judge::EvalOracle, 3),
                (Judge::EvalDraft, 1),
                (Judge::Cost, 3)
            ])
        );
        let cost: Vec<(&Passed, usize)> = tally.passed(Judge::Cost).collect();
        assert_eq!(cost, [(&compared, 3)]);
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
                vec![CellKey::new(Axis::Units, "mem")],
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
        assert_eq!(a.by_judge(), BTreeMap::from([(Judge::Cost, 2)]));
        assert_eq!(a.failing.len(), 1);
    }

    #[test]
    fn a_verdict_line_prints_its_cells_with_spaces_replaced() {
        let line = Verdict::disagree(
            Judge::EvalOracle,
            EntryPoint::EstimateTx(Layer::V1beta),
            id(4),
            vec![
                CellKey::new(Axis::Purpose, "Spend"),
                CellKey::new(Axis::Label, "new script language"),
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
    fn every_judge_and_axis_reads_back_from_its_name() {
        for judge in Judge::ALL {
            assert_eq!(judge.name().parse(), Ok(judge));
        }
        for axis in Axis::ALL {
            assert_eq!(axis.name().parse(), Ok(axis));
        }
        let unknown = |kind| UnknownName {
            kind,
            name: "nothing".into(),
        };
        assert_eq!("nothing".parse::<Judge>(), Err(unknown("judge")));
        assert_eq!("nothing".parse::<Axis>(), Err(unknown("axis")));
    }

    #[test]
    fn every_entry_point_is_listed_once_and_reads_back_from_its_name() {
        let every = EntryPoint::every();
        let names: std::collections::BTreeSet<String> =
            every.iter().map(EntryPoint::to_string).collect();
        assert_eq!(names.len(), every.len());
        for entry in every {
            assert_eq!(entry.to_string().parse(), Ok(entry));
        }
        let unknown = UnknownName {
            kind: "entry point",
            name: "submit".into(),
        };
        assert_eq!("submit".parse::<EntryPoint>(), Err(unknown));
    }
}
