//! One judgement of a node's answer: the judge, the entry point it drove, the
//! record, the cells it touched and its outcome, and the tally every run passes
//! or fails by.

use std::{collections::BTreeMap, fmt};

use crate::record::RecordId;

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
        Submit = "submit",
        ApplyBlock = "apply-block",
        Cost = "cost",
        Coverage = "coverage",
        Endorser = "endorser",
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
        Param = "param",
        ParamKey = "param_key",
        Purpose = "purpose",
        Redeemer = "redeemer",
        Refusal = "refusal",
        RequestShape = "request_shape",
        Rollback = "rollback",
        Row = "row",
        Rule = "rule",
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

named! {
    /// A version of the utxorpc spec Dolos serves.
    "version" Version {
        V1 = "v1",
        V1beta = "v1beta",
        V1alpha = "v1alpha",
    }
}

named! {
    /// A method of the utxorpc spec, by its name in snake case.
    "route" Route {
        SubmitTx = "submit_tx",
        EvalTx = "eval_tx",
        WaitForTx = "wait_for_tx",
        ReadMempool = "read_mempool",
        WatchMempool = "watch_mempool",
        FetchBlock = "fetch_block",
        FollowTip = "follow_tip",
        DumpHistory = "dump_history",
        ReadTip = "read_tip",
        WatchTx = "watch_tx",
        ReadParams = "read_params",
        ReadData = "read_data",
        ReadUtxos = "read_utxos",
        SearchUtxos = "search_utxos",
        ReadTx = "read_tx",
        ReadGenesis = "read_genesis",
        ReadEraSummary = "read_era_summary",
        ReadState = "read_state",
    }
}

impl Route {
    /// Whether the spec defines the method at `version`.
    pub fn at(self, version: Version) -> bool {
        !matches!((self, version), (Route::ReadState, Version::V1alpha))
    }
}

named! {
    /// A function of Dolos a judge calls in place of a served route.
    "instrument" Instrument {
        ReceiveTx = "receive_tx",
        EstimateTxCalls = "estimate_tx_calls",
        RedeemerArguments = "redeemer_arguments",
        MinFee = "min_fee",
        ApplyBlock = "apply_block",
        Revalidate = "revalidate",
        ReadAccounts = "read_accounts",
        ScriptByHash = "script_by_hash",
        PlutusData = "plutus_data",
    }
}

/// The node entry point a judgement drove.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntryPoint {
    Instrument(Instrument),
    Route(Route, Version),
}

impl From<Instrument> for EntryPoint {
    fn from(instrument: Instrument) -> Self {
        EntryPoint::Instrument(instrument)
    }
}

impl fmt::Display for EntryPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EntryPoint::Instrument(instrument) => write!(f, "{instrument}"),
            EntryPoint::Route(route, version) => write!(f, "{route}:{version}"),
        }
    }
}

impl EntryPoint {
    /// Every instrument and every route at each version the spec defines it.
    pub fn every() -> Vec<EntryPoint> {
        let instruments = Instrument::ALL.map(EntryPoint::Instrument);
        let routes = Route::ALL.into_iter().flat_map(|route| {
            let versions = Version::ALL.into_iter().filter(move |v| route.at(*v));
            versions.map(move |version| EntryPoint::Route(route, version))
        });
        instruments.into_iter().chain(routes).collect()
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

    /// The kind of pass the outcome is, none for a disagreement.
    pub fn pass(&self) -> Option<Pass> {
        match self {
            Outcome::Agree => Some(Pass::Agree),
            Outcome::Known(_) => Some(Pass::Known),
            Outcome::Excluded(_) => Some(Pass::Excluded),
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
}

/// A passing verdict without its record and text, the key a tally counts passing verdicts under.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Passed {
    pub judge: Judge,
    pub entry: Option<EntryPoint>,
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
    /// The entry point the judge drove, none for a judge of the oracle's own tables.
    pub entry: Option<EntryPoint>,
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
            entry: Some(entry),
            subject: Subject::Record(id),
            cells,
            outcome: Outcome::Disagree(detail.into()),
        }
    }

    /// The failure of `judge` over the whole selection, shown by `detail`.
    pub fn selection_fails(judge: Judge, entry: EntryPoint, detail: impl Into<String>) -> Self {
        Verdict {
            judge,
            entry: Some(entry),
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
        let entry = entry.map_or("none".to_string(), |entry| entry.to_string());
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

    /// Adds a failing verdict for each judge and entry point of `required`
    /// with no verdict, so an entry point a judge judged nothing at fails the
    /// run.
    pub fn require(&mut self, required: &[(Judge, EntryPoint)]) {
        for (judge, entry) in required {
            let at = |j: Judge, e: Option<EntryPoint>| j == *judge && e == Some(*entry);
            let passed = self.passing.keys().any(|p| at(p.judge, p.entry));
            if !passed && !self.failing.iter().any(|v| at(v.judge, v.entry)) {
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

    const MIN_FEE: EntryPoint = EntryPoint::Instrument(Instrument::MinFee);

    fn id(n: u32) -> RecordId {
        RecordId {
            corpus: CorpusKind::Supp,
            n,
        }
    }

    fn agree(judge: Judge) -> Verdict {
        Verdict {
            judge,
            entry: Some(MIN_FEE),
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
        tally.require(&[(Judge::Cost, MIN_FEE)]);
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
            Outcome::Excluded("no carrier".into()),
        ));
        tally.add(spend(Judge::EvalDraft, Outcome::Agree));
        let compared = Passed {
            judge: Judge::Cost,
            entry: Some(MIN_FEE),
            cells: Vec::new(),
            pass: Pass::Agree,
        };
        tally.pass(compared.clone(), 3);
        tally.pass(compared.clone(), 0);
        let passed = |pass| Passed {
            judge: Judge::EvalOracle,
            entry: Some(MIN_FEE),
            cells: vec![CellKey::new(Axis::Purpose, "Spend")],
            pass,
        };
        let oracle: Vec<(&Passed, usize)> = tally.passed(Judge::EvalOracle).collect();
        assert_eq!(
            oracle,
            [(&passed(Pass::Agree), 2), (&passed(Pass::Excluded), 1)]
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
                MIN_FEE,
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
        tally.require(&[(Judge::Cost, MIN_FEE)]);
        assert_eq!(tally.verdict(), RunVerdict::Fail { failing: 1 });
        assert_eq!(
            tally.failing[0].to_string(),
            "VERDICT judge=cost entry=min_fee record=selection cells= outcome=disagree judged=0"
        );
    }

    #[test]
    fn a_required_entry_point_a_judge_judged_nothing_at_fails_the_tally() {
        let eval_tx = |version| EntryPoint::Route(Route::EvalTx, version);
        let mut tally = Tally::default();
        tally.add(Verdict {
            entry: Some(eval_tx(Version::V1)),
            ..agree(Judge::Cost)
        });
        tally.require(&[(Judge::Cost, eval_tx(Version::V1))]);
        assert!(tally.failing.is_empty(), "{tally}");
        tally.require(&[(Judge::Cost, eval_tx(Version::V1beta))]);
        assert_eq!(tally.verdict(), RunVerdict::Fail { failing: 1 });
        assert_eq!(
            tally.failing[0].to_string(),
            "VERDICT judge=cost entry=eval_tx:v1beta record=selection cells= outcome=disagree judged=0"
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
            EntryPoint::Route(Route::ReadUtxos, Version::V1),
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
            EntryPoint::Route(Route::EvalTx, Version::V1beta),
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
            "VERDICT judge=eval-oracle entry=eval_tx:v1beta record=supp#4 \
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
        for entry in &every {
            assert_eq!(entry.to_string().parse(), Ok(*entry));
        }
        let unknown = UnknownName {
            kind: "entry point",
            name: "submit".into(),
        };
        assert_eq!("submit".parse::<EntryPoint>(), Err(unknown));
        let read_state = EntryPoint::Route(Route::ReadState, Version::V1alpha);
        assert!(!every.contains(&read_state));
        assert_eq!(every.len(), Instrument::ALL.len() + 53);
    }
}
