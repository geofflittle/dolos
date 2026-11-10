//! Judges each redeemer Dolos evaluates against the ledger's evaluation, by level, purpose and answer.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    str::FromStr,
};

use crate::{
    cbor::Purpose,
    judgement::{
        self, member, Axis, CellKey, EntryPoint, Instrument, Judge, Outcome, Pass, RequestShape,
        Route, Subject, Verdict, Version,
    },
    manifest::ManifestError,
    record::{Evaluation, LedgerOutcome, LedgerRedeemers, Record, RecordId, Units},
    refusal::LedgerLeaf,
};

const EVAL_KNOWN: &str = include_str!("../../../test_data/ledger-oracle/eval-known.txt");
const EVAL_TX_KNOWN: &str = include_str!("../../../test_data/ledger-oracle/eval-tx-known.txt");

/// The ledger's name of each redeemer tag, by tag number.
const TAG_NAMES: [&str; 7] = [
    "Spend", "Mint", "Cert", "Reward", "Vote", "Propose", "Guarding",
];

/// The level of a transaction a redeemer is in, the top level or a sub transaction by position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Top,
    Sub(u32),
}

impl Level {
    /// The name the tally counts the level under, every sub transaction alike.
    pub fn kind(self) -> &'static str {
        match self {
            Level::Top => "top",
            Level::Sub(_) => "sub",
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Level::Top => f.write_str("top"),
            Level::Sub(k) => write!(f, "sub{k}"),
        }
    }
}

impl FromStr for Level {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.strip_prefix("sub") {
            _ if s == "top" => Ok(Level::Top),
            Some(k) => k
                .parse()
                .map(Level::Sub)
                .map_err(|_| format!("bad level {s}")),
            None => Err(format!("bad level {s}")),
        }
    }
}

/// The name of a redeemer tag, its number when the ledger names no such tag.
pub fn tag_name(tag: u64) -> String {
    match TAG_NAMES.get(tag as usize) {
        Some(name) => name.to_string(),
        None => tag.to_string(),
    }
}

/// The tag number of a redeemer tag name, or of a tag written as its number.
fn tag_number(name: &str) -> Option<u64> {
    match TAG_NAMES.iter().position(|n| *n == name) {
        Some(tag) => Some(tag as u64),
        None => name.parse().ok(),
    }
}

/// The purpose a message names as `<tag>#<index>`.
pub(crate) fn parse_purpose(s: &str) -> Result<Purpose, String> {
    let (tag, index) = s.split_once('#').ok_or(format!("bad purpose {s}"))?;
    let tag = tag_number(tag).ok_or(format!("bad purpose tag {tag}"))?;
    let index = index
        .parse()
        .map_err(|_| format!("bad purpose index {index}"))?;
    Ok(Purpose { tag, index })
}

/// One redeemer of a transaction, by its level and purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowKey {
    pub level: Level,
    pub purpose: Purpose,
}

impl RowKey {
    /// The purpose, index and level cells of the row.
    pub fn cells(&self) -> Vec<CellKey> {
        vec![
            CellKey::new(Axis::Purpose, tag_name(self.purpose.tag)),
            CellKey::new(Axis::Index, self.purpose.index),
            CellKey::new(Axis::Level, self.level.kind()),
        ]
    }
}

impl fmt::Display for RowKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Purpose { tag, index } = self.purpose;
        write!(f, "{} {}#{index}", self.level, tag_name(tag))
    }
}

/// The verdict of one evaluation of a redeemer, with its units when it ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Ran(Units),
    Failed,
}

impl Answer {
    /// The name of the answer without its units.
    pub fn kind(&self) -> &'static str {
        match self {
            Answer::Ran(_) => "ran",
            Answer::Failed => "failed",
        }
    }
}

impl From<&Evaluation> for Answer {
    fn from(evaluation: &Evaluation) -> Self {
        match evaluation {
            Evaluation::Ran(units) => Answer::Ran(*units),
            Evaluation::Failed(_) => Answer::Failed,
        }
    }
}

impl fmt::Display for Answer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Answer::Ran(units) => write!(f, "ran {units}"),
            Answer::Failed => f.write_str("failed"),
        }
    }
}

/// Why a layer of Dolos evaluated no redeemer of a transaction, in the words the known list uses, on one line.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Refusal(String);

impl Refusal {
    /// The refusal of a reason, its words joined by single spaces.
    pub fn new(why: &str) -> Self {
        Refusal(why.split_whitespace().collect::<Vec<_>>().join(" "))
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What one layer of Dolos answered for the redeemers of one transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DolosAnswer {
    Evaluated(BTreeMap<RowKey, Answer>),
    Refused(Refusal),
    /// The layer's message cannot carry a redeemer the evaluator answered, and the layer says so.
    Unrepresentable(String),
}

/// What one layer of Dolos answered for one redeemer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DolosRow {
    Answered(Answer),
    Unanswered,
    Refused(Refusal),
    Unrepresentable(String),
}

impl fmt::Display for DolosRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DolosRow::Answered(answer) => write!(f, "{answer}"),
            DolosRow::Unanswered => f.write_str("none"),
            DolosRow::Refused(refusal) => write!(f, "refused {refusal}"),
            DolosRow::Unrepresentable(why) => write!(f, "unrepresentable {why}"),
        }
    }
}

/// A layer of Dolos that answers for the evaluation of a transaction, the
/// evaluator first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    Pallas,
    Eval(Version),
}

impl Layer {
    /// Every layer, in the order a run prints them.
    pub const ALL: [Layer; 1 + Version::ALL.len()] = {
        let mut all = [Layer::Pallas; 1 + Version::ALL.len()];
        let mut i = 0;
        while i < Version::ALL.len() {
            all[1 + i] = Layer::Eval(Version::ALL[i]);
            i += 1;
        }
        all
    };

    /// The entry point that answers at the layer.
    pub fn entry(self) -> EntryPoint {
        match self {
            Layer::Pallas => Instrument::EstimateTxCalls.into(),
            Layer::Eval(version) => EntryPoint::Route(Route::EvalTx, version),
        }
    }
}

impl fmt::Display for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Layer::Pallas => f.write_str("pallas"),
            Layer::Eval(version) => write!(f, "{version}"),
        }
    }
}

/// The answers of a row that make no pass: the ledger's, the layer's and the refusal the known list gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disagreement {
    pub ledger: Option<Answer>,
    pub dolos: DolosRow,
    pub listed: Option<Refusal>,
}

impl Disagreement {
    /// The answer cells of each side, with the units that differ when both ran, the layer's refusal and the listed one.
    pub fn cells(&self) -> Vec<CellKey> {
        let dolos = match &self.dolos {
            DolosRow::Answered(answer) => answer.kind(),
            DolosRow::Unanswered => "none",
            DolosRow::Refused(_) => "refused",
            DolosRow::Unrepresentable(_) => "unrepresentable",
        };
        let mut cells = vec![
            CellKey::new(
                Axis::Ledger,
                self.ledger.as_ref().map_or("none", Answer::kind),
            ),
            CellKey::new(Axis::Dolos, dolos),
        ];
        if let (Some(Answer::Ran(l)), DolosRow::Answered(Answer::Ran(d))) =
            (&self.ledger, &self.dolos)
        {
            if l.mem != d.mem {
                cells.push(CellKey::new(Axis::Units, "mem"));
            }
            if l.steps != d.steps {
                cells.push(CellKey::new(Axis::Units, "steps"));
            }
        }
        if let DolosRow::Refused(refusal) = &self.dolos {
            cells.push(CellKey::new(Axis::Refusal, refusal));
        }
        cells.extend(self.listed.iter().map(|l| CellKey::new(Axis::Listed, l)));
        cells
    }
}

impl fmt::Display for Disagreement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.ledger {
            Some(answer) => write!(f, "ledger=({answer})")?,
            None => f.write_str("ledger=(none)")?,
        }
        write!(f, " dolos=({})", self.dolos)?;
        match &self.listed {
            Some(refusal) => write!(f, " listed=({refusal})"),
            None => Ok(()),
        }
    }
}

/// The verdict of one row, a pass with its evidence or the answers that disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowVerdict {
    /// Both sides ran the redeemer in these units, or both failed it.
    Agree(Answer),
    /// Dolos refused a listed row for the reason the list gives.
    Known(Refusal),
    /// The layer cannot carry a redeemer of the transaction and says so.
    Unrepresentable(String),
    Disagree(Disagreement),
}

/// The verdict of a row from the ledger's answer, the layer's answer and the refusal the known list gives it.
pub fn row_verdict(
    ledger: Option<Answer>,
    dolos: DolosRow,
    listed: Option<&Refusal>,
) -> RowVerdict {
    match (ledger, &dolos, listed) {
        (Some(l), DolosRow::Answered(d), None) if l == *d => RowVerdict::Agree(l),
        (Some(_), DolosRow::Refused(r), Some(listed)) if r == listed => {
            RowVerdict::Known(r.clone())
        }
        (Some(_), DolosRow::Unrepresentable(why), None) => RowVerdict::Unrepresentable(why.clone()),
        _ => RowVerdict::Disagree(Disagreement {
            ledger,
            dolos,
            listed: listed.cloned(),
        }),
    }
}

/// Each row's verdict for one transaction, for every row the ledger, the layer or the known list names.
pub fn judge_tx(
    ledger: &BTreeMap<RowKey, Answer>,
    dolos: &DolosAnswer,
    listed: &BTreeMap<RowKey, Refusal>,
) -> Vec<(RowKey, RowVerdict)> {
    let answered = match dolos {
        DolosAnswer::Evaluated(rows) => rows.keys().copied().collect(),
        DolosAnswer::Refused(_) | DolosAnswer::Unrepresentable(_) => Vec::new(),
    };
    let keys: BTreeSet<RowKey> = ledger
        .keys()
        .chain(listed.keys())
        .copied()
        .chain(answered)
        .collect();
    let row = |key: &RowKey| match dolos {
        DolosAnswer::Evaluated(rows) => rows
            .get(key)
            .map_or(DolosRow::Unanswered, |a| DolosRow::Answered(*a)),
        DolosAnswer::Refused(refusal) => DolosRow::Refused(refusal.clone()),
        DolosAnswer::Unrepresentable(why) => DolosRow::Unrepresentable(why.clone()),
    };
    keys.into_iter()
        .map(|key| {
            let verdict = row_verdict(ledger.get(&key).copied(), row(&key), listed.get(&key));
            (key, verdict)
        })
        .collect()
}

/// What an evaluation of a transaction the ledger refused under one failure leaf answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeafEval {
    /// A refusal naming a missing script or redeemer.
    Refuse,
    /// The redeemers, judged as for a transaction the ledger accepted.
    Evaluate,
    /// No answer the judgement holds Dolos to.
    Undecided,
}

/// What every layer must answer for a transaction whose redeemers the ledger evaluated none of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxDemand {
    /// A refusal, for the script or redeemer leaf named.
    Refuse(LedgerLeaf),
    /// An evaluation of no redeemer.
    EvaluateNone,
    /// Nothing, the ledger's leaves naming no rule the judgement holds Dolos to.
    Undecided,
}

impl TxDemand {
    /// The name of the demand's kind, without its leaf.
    pub fn kind(self) -> &'static str {
        match self {
            TxDemand::Refuse(_) => "refuse",
            TxDemand::EvaluateNone => "evaluate_none",
            TxDemand::Undecided => "undecided",
        }
    }

    /// The demand of the ledger's outcome, a refusal when any leaf demands one and an evaluation when the ledger accepted or every leaf demands one.
    pub fn of(outcome: &LedgerOutcome) -> Self {
        let LedgerOutcome::Refused { failure_names, .. } = outcome else {
            return TxDemand::EvaluateNone;
        };
        let leaves = || failure_names.iter().map(|p| p.leaf);
        if let Some(leaf) = leaves().find(|l| l.eval() == LeafEval::Refuse) {
            return TxDemand::Refuse(leaf);
        }
        match leaves().all(|l| l.eval() == LeafEval::Evaluate) {
            true => TxDemand::EvaluateNone,
            false => TxDemand::Undecided,
        }
    }
}

impl fmt::Display for TxDemand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TxDemand::Refuse(leaf) => write!(f, "refuse[{leaf}]"),
            TxDemand::EvaluateNone => f.write_str("evaluate-none"),
            TxDemand::Undecided => f.write_str("undecided"),
        }
    }
}

/// The verdict of one layer's answer for a transaction whose redeemers the ledger evaluated none of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxVerdict {
    /// The layer refused for the leaf, naming a script or redeemer.
    Refused { leaf: LedgerLeaf, refusal: Refusal },
    /// The layer evaluated no redeemer, as the ledger did.
    EvaluatedNone,
    /// The ledger's leaves hold the layer to no answer.
    Undecided,
    /// The layer gave the answer the known list gives the record.
    Known { demand: TxDemand, dolos: String },
    /// The layer's answer, which the demand refuses, with the answer the known list gives the record.
    Disagree {
        demand: TxDemand,
        dolos: String,
        listed: Option<String>,
    },
}

/// Whether a refusal's words name a script or a redeemer.
fn names_script_or_redeemer(refusal: &Refusal) -> bool {
    let words = refusal.0.to_lowercase();
    words.contains("script") || words.contains("redeemer")
}

/// One layer's answer for a whole transaction, on one line.
fn answer_text(answer: &Result<DolosAnswer, Malformed>) -> String {
    match answer {
        Ok(DolosAnswer::Evaluated(rows)) => format!("evaluated[{} rows]", rows.len()),
        Ok(DolosAnswer::Refused(refusal)) => format!("refused[{refusal}]"),
        Ok(DolosAnswer::Unrepresentable(why)) => format!("unrepresentable[{why}]"),
        Err(malformed) => format!("malformed[{malformed}]"),
    }
}

/// The name of the kind of an answer, without its rows or reason.
fn answer_kind(answer: &Result<DolosAnswer, Malformed>) -> &'static str {
    match answer {
        Ok(DolosAnswer::Evaluated(_)) => "evaluated",
        Ok(DolosAnswer::Refused(_)) => "refused",
        Ok(DolosAnswer::Unrepresentable(_)) => "unrepresentable",
        Err(_) => "malformed",
    }
}

/// The verdict of one layer's answer against the demand of a transaction whose redeemers the ledger evaluated none of and the answer the known list gives it.
pub fn tx_verdict(
    demand: TxDemand,
    answer: &Result<DolosAnswer, Malformed>,
    listed: Option<&str>,
) -> TxVerdict {
    let dolos = answer_text(answer);
    match (demand, answer, listed) {
        (TxDemand::Undecided, _, None) => TxVerdict::Undecided,
        (TxDemand::Refuse(leaf), Ok(DolosAnswer::Refused(refusal)), None)
            if names_script_or_redeemer(refusal) =>
        {
            TxVerdict::Refused {
                leaf,
                refusal: refusal.clone(),
            }
        }
        (TxDemand::EvaluateNone, Ok(DolosAnswer::Evaluated(rows)), None) if rows.is_empty() => {
            TxVerdict::EvaluatedNone
        }
        (TxDemand::Refuse(_) | TxDemand::EvaluateNone, _, Some(listed)) if listed == dolos => {
            TxVerdict::Known { demand, dolos }
        }
        _ => TxVerdict::Disagree {
            demand,
            dolos,
            listed: listed.map(str::to_string),
        },
    }
}

/// The ledger's answer for each redeemer of a record, at its top level and in each sub transaction, none for a record whose bytes the ledger's decoder refused.
pub fn ledger_rows(record: &Record) -> BTreeMap<RowKey, Answer> {
    let LedgerRedeemers::Read(redeemers) = &record.redeemers else {
        return BTreeMap::new();
    };
    let top = redeemers.top.iter().map(|r| (Level::Top, r));
    let subs = redeemers
        .subs
        .iter()
        .enumerate()
        .flat_map(|(k, sub)| sub.iter().map(move |r| (Level::Sub(k as u32), r)));
    top.chain(subs)
        .map(|(level, r)| {
            let key = RowKey {
                level,
                purpose: r.purpose,
            };
            (key, Answer::from(&r.evaluation))
        })
        .collect()
}

/// The rows of an evaluation by key, or the first key it answers twice.
pub fn evaluated(
    rows: impl IntoIterator<Item = (RowKey, Answer)>,
) -> Result<BTreeMap<RowKey, Answer>, Malformed> {
    let mut by_key = BTreeMap::new();
    for (key, answer) in rows {
        if by_key.insert(key, answer).is_some() {
            return Err(Malformed::Twice(key));
        }
    }
    Ok(by_key)
}

/// The number of purposes a transaction's rows name at more than one level, which a message keyed by purpose alone cannot tell apart.
pub fn collisions(rows: &BTreeMap<RowKey, Answer>) -> usize {
    let mut levels: BTreeMap<Purpose, usize> = BTreeMap::new();
    for key in rows.keys() {
        *levels.entry(key.purpose).or_default() += 1;
    }
    levels.values().filter(|n| **n > 1).count()
}

/// The purpose of a u5c redeemer purpose number and index, the u5c number less one being the ledger's tag and a number below one naming the tag no ledger row has.
pub fn wire_purpose(purpose: i32, index: u32) -> Purpose {
    let tag = u64::try_from(i64::from(purpose) - 1).unwrap_or(u64::MAX);
    Purpose {
        tag,
        index: index.into(),
    }
}

/// The purpose an EvalTx error names in its `<tag>[<index>]: ` prefix.
pub fn named_in_message(msg: &str) -> Option<Purpose> {
    let (named, _) = msg.split_once("]: ")?;
    let (tag, index) = named.split_once('[')?;
    let tag = TAG_NAMES.iter().position(|n| *n == tag)? as u64;
    Some(Purpose {
        tag,
        index: index.parse().ok()?,
    })
}

/// The fee a u5c message gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireFee {
    Unset,
    Int(i64),
    Other(String),
}

/// An evaluation a layer gives that no row verdict can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Malformed {
    /// The layer answers the row twice.
    Twice(RowKey),
    /// The layer ran the row and gives it no units.
    NoUnits(RowKey),
}

impl Malformed {
    /// The cells of the row the malformed answer names and of its kind.
    pub fn cells(&self) -> Vec<CellKey> {
        let (key, kind) = match self {
            Malformed::Twice(key) => (key, "twice"),
            Malformed::NoUnits(key) => (key, "no_units"),
        };
        let mut cells = row_cells(key);
        cells.push(CellKey::new(Axis::Dolos, kind));
        cells
    }
}

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Malformed::Twice(key) => write!(f, "{key} answered twice"),
            Malformed::NoUnits(key) => write!(f, "{key} ran with no units"),
        }
    }
}

/// One EvalTx response in the terms every version of the message shares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireEval {
    pub redeemers: Vec<(Purpose, Option<Units>)>,
    /// Each error with the redeemer it names.
    pub errors: Vec<(Option<Purpose>, String)>,
    pub total: Option<Units>,
    pub fee: WireFee,
}

impl WireEval {
    /// The answer the response gives, a refusal when it carries no redeemer and no total, named as the evaluator's refusal when its message is the one that refusal renders to and as unrepresentable when it is the message that says so.
    pub fn answer(
        &self,
        refusal: Option<(&str, &Refusal)>,
        unrepresentable: Option<&str>,
    ) -> Result<DolosAnswer, Malformed> {
        if self.refuses() {
            let msg = self
                .errors
                .iter()
                .map(|(_, m)| m.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            return Ok(match (refusal, unrepresentable) {
                (Some((text, refusal)), _) if msg == text => DolosAnswer::Refused(refusal.clone()),
                (_, Some(text)) if msg == text => DolosAnswer::Unrepresentable(msg),
                _ => DolosAnswer::Refused(Refusal::new(&msg)),
            });
        }
        let failed: BTreeSet<Purpose> = self.errors.iter().filter_map(|(p, _)| *p).collect();
        let rows = self.redeemers.iter().map(|(purpose, units)| {
            let key = RowKey {
                level: Level::Top,
                purpose: *purpose,
            };
            let answer = match (failed.contains(purpose), units) {
                (true, _) => Answer::Failed,
                (false, Some(units)) => Answer::Ran(*units),
                (false, None) => return Err(Malformed::NoUnits(key)),
            };
            Ok((key, answer))
        });
        let rows: Vec<(RowKey, Answer)> = rows.collect::<Result<_, _>>()?;
        evaluated(rows).map(DolosAnswer::Evaluated)
    }

    /// Whether the response carries no redeemer and no total, the shape of a refusal.
    fn refuses(&self) -> bool {
        self.redeemers.is_empty() && self.total.is_none()
    }

    /// Whether the total equals the sum of the redeemers' units, none for a response that carries neither.
    pub fn total_agrees(&self) -> Option<bool> {
        if self.refuses() {
            return None;
        }
        let sum = self
            .redeemers
            .iter()
            .try_fold((0u64, 0u64), |(mem, steps), (_, u)| {
                let u = (*u)?;
                Some((mem.checked_add(u.mem)?, steps.checked_add(u.steps)?))
            });
        let total = self.total.map(|t| (t.mem, t.steps));
        Some(sum.is_some() && total == sum)
    }

    /// The errors of a response that evaluated the transaction that name none of its redeemers.
    pub fn unnamed(&self) -> Vec<&str> {
        if self.refuses() {
            return Vec::new();
        }
        let names =
            |p: &Option<Purpose>| p.is_some_and(|p| self.redeemers.iter().any(|(r, _)| *r == p));
        let unnamed = self.errors.iter().filter(|(p, _)| !names(p));
        unnamed.map(|(_, m)| m.as_str()).collect()
    }
}

/// The refusal each listed row of each record is known to draw.
pub type EvalKnown = BTreeMap<RecordId, BTreeMap<RowKey, Refusal>>;

/// Reads the evaluation known list, one `<record> <level> <tag>#<index> <refusal>` per line.
pub fn parse_eval_known(file: &'static str, text: &str) -> Result<EvalKnown, ManifestError> {
    let mut known = EvalKnown::new();
    let lines = text.lines().enumerate().map(|(i, l)| (i + 1, l.trim()));
    for (line, l) in lines.filter(|(_, l)| !l.is_empty()) {
        let error = |reason: String| ManifestError { file, line, reason };
        let mut words = l.splitn(4, ' ');
        let mut word = |what: &str| words.next().ok_or(error(format!("no {what}")));
        let id: RecordId = word("record")?.parse().map_err(error)?;
        let level: Level = word("level")?.parse().map_err(error)?;
        let purpose = parse_purpose(word("purpose")?).map_err(error)?;
        let refusal = Refusal::new(word("refusal")?);
        let key = RowKey { level, purpose };
        if known.entry(id).or_default().insert(key, refusal).is_some() {
            return Err(error(format!("{id} {key} known twice")));
        }
    }
    Ok(known)
}

/// Writes an evaluation known list as `parse_eval_known` reads it, in record and row order.
pub fn render_eval_known(known: &EvalKnown) -> String {
    let rows = known.iter().flat_map(|(id, rows)| {
        rows.iter()
            .map(move |(key, refusal)| format!("{id} {key} {refusal}\n"))
    });
    rows.collect()
}

/// The committed evaluation known list.
pub fn eval_known() -> Result<EvalKnown, ManifestError> {
    parse_eval_known("eval-known.txt", EVAL_KNOWN)
}

/// The answer every layer is known to give each listed record whose redeemers the ledger evaluated none of.
pub type TxKnown = BTreeMap<RecordId, String>;

/// Reads the transaction known list, one `<record> <answer>` per line.
pub fn parse_tx_known(file: &'static str, text: &str) -> Result<TxKnown, ManifestError> {
    let mut known = TxKnown::new();
    let lines = text.lines().enumerate().map(|(i, l)| (i + 1, l.trim()));
    for (line, l) in lines.filter(|(_, l)| !l.is_empty()) {
        let error = |reason: String| ManifestError { file, line, reason };
        let (id, answer) = l.split_once(' ').ok_or(error("no answer".to_string()))?;
        let id: RecordId = id.parse().map_err(error)?;
        if known.insert(id, answer.to_string()).is_some() {
            return Err(error(format!("{id} known twice")));
        }
    }
    Ok(known)
}

/// The committed transaction known list.
pub fn tx_known() -> Result<TxKnown, ManifestError> {
    parse_tx_known("eval-tx-known.txt", EVAL_TX_KNOWN)
}

/// The number of rows of one group that drew each verdict.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub agree: usize,
    pub known: usize,
    pub unrepresentable: usize,
    pub disagree: usize,
}

impl Counts {
    fn add(&mut self, pass: Pass, n: usize) {
        match pass {
            Pass::Agree => self.agree += n,
            Pass::Known => self.known += n,
            Pass::Excluded => self.unrepresentable += n,
        }
    }

    fn merge(&mut self, other: Counts) {
        self.agree += other.agree;
        self.known += other.known;
        self.unrepresentable += other.unrepresentable;
        self.disagree += other.disagree;
    }

    fn compared(&self) -> usize {
        self.agree + self.known + self.unrepresentable + self.disagree
    }
}

impl fmt::Display for Counts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Counts {
            agree,
            known,
            unrepresentable,
            disagree,
        } = self;
        write!(f, "{agree}/{known}/{unrepresentable}/{disagree}")
    }
}

/// The number of one layer's answers that drew each transaction verdict.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TxCounts {
    pub refused: usize,
    pub evaluated_none: usize,
    pub undecided: usize,
    pub known: usize,
    pub disagree: usize,
}

impl TxCounts {
    fn merge(&mut self, other: TxCounts) {
        self.refused += other.refused;
        self.evaluated_none += other.evaluated_none;
        self.undecided += other.undecided;
        self.known += other.known;
        self.disagree += other.disagree;
    }
}

/// One of the judges of a layer's evaluation of a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalJudge {
    Oracle,
    Tx,
    Draft,
    RewardOutput,
}

impl EvalJudge {
    /// Every evaluation judge in the order a run reports them.
    pub const ALL: [EvalJudge; 4] = [
        EvalJudge::Oracle,
        EvalJudge::Tx,
        EvalJudge::Draft,
        EvalJudge::RewardOutput,
    ];

    /// The judge a verdict names.
    pub fn judge(self) -> Judge {
        match self {
            EvalJudge::Oracle => Judge::EvalOracle,
            EvalJudge::Tx => Judge::EvalTx,
            EvalJudge::Draft => Judge::EvalDraft,
            EvalJudge::RewardOutput => Judge::EvalRewardOutput,
        }
    }

    /// The shape of the transaction the judge's requests carry.
    pub fn shape(self) -> RequestShape {
        match self {
            EvalJudge::Oracle | EvalJudge::Tx => RequestShape::Signed,
            EvalJudge::Draft => RequestShape::Draft,
            EvalJudge::RewardOutput => RequestShape::RewardOutputDraft,
        }
    }
}

/// The members of the answer axis that name the evaluation judges' verdicts on one row, on one part of a whole answer, or on a record or row the judge did not reach.
mod part {
    pub const ROW: &str = "row";
    pub const TOTAL: &str = "total";
    pub const FEE: &str = "fee";
    pub const UNNAMED: &str = "unnamed_error";
    pub const UNJUDGED: &str = "unjudged";
    pub const REFUSED: &str = "refused";
    pub const EVALUATED_NONE: &str = "evaluated_none";
    pub const KNOWN: &str = "known";
}

/// The member of the draft axis on the verdict of a record whose bytes derive no draft.
const UNDERIVED: &str = "underived";

/// The verdict of `judge` at layer `layer` on record `id`, which touches the shape of the judge's requests.
fn estimate(
    judge: EvalJudge,
    layer: Layer,
    id: RecordId,
    mut cells: Vec<CellKey>,
    outcome: Outcome,
) -> Verdict {
    cells.push(CellKey::new(Axis::RequestShape, judge.shape()));
    Verdict {
        judge: judge.judge(),
        entry: Some(layer.entry()),
        subject: Subject::Record(id),
        cells,
        outcome,
    }
}

/// The cells of a verdict on row `key`.
fn row_cells(key: &RowKey) -> Vec<CellKey> {
    let mut cells = key.cells();
    cells.push(CellKey::new(Axis::Answer, part::ROW));
    cells
}

/// The cells and outcome of a verdict on row `key`.
fn row_judgement(key: &RowKey, verdict: &RowVerdict) -> (Vec<CellKey>, Outcome) {
    let mut cells = row_cells(key);
    let outcome = match verdict {
        RowVerdict::Agree(answer) => {
            cells.push(CellKey::new(Axis::Ledger, answer.kind()));
            Outcome::Agree
        }
        RowVerdict::Known(refusal) => {
            cells.push(CellKey::new(Axis::Dolos, "refused"));
            Outcome::Known(refusal.to_string())
        }
        RowVerdict::Unrepresentable(why) => {
            cells.push(CellKey::new(Axis::Dolos, "unrepresentable"));
            Outcome::Excluded(why.clone())
        }
        RowVerdict::Disagree(d) => {
            cells.extend(d.cells());
            Outcome::Disagree(format!("{key} {d}"))
        }
    };
    (cells, outcome)
}

/// Whether a verdict of an evaluation judge is on one row.
fn on_a_row(cells: &[CellKey]) -> bool {
    member(cells, Axis::Answer) == Some(part::ROW)
}

/// The counts of the row verdicts of `judge` in `tally` under the key `group` gives each verdict's cells.
fn row_counts<K: Ord>(
    tally: &judgement::Tally,
    judge: Judge,
    group: impl Fn(&[CellKey]) -> Option<K>,
) -> BTreeMap<K, Counts> {
    let mut counts: BTreeMap<K, Counts> = BTreeMap::new();
    for (passed, n) in tally.passed(judge) {
        if let (true, Some(key)) = (on_a_row(&passed.cells), group(&passed.cells)) {
            counts.entry(key).or_default().add(passed.pass, n);
        }
    }
    for verdict in tally.failed(judge) {
        if let (true, Some(key)) = (on_a_row(&verdict.cells), group(&verdict.cells)) {
            counts.entry(key).or_default().disagree += 1;
        }
    }
    counts
}

/// The passing and the failing verdicts of `judge` in `tally` that name `part` on the answer axis.
fn part_counts(tally: &judgement::Tally, judge: Judge, part: &str) -> (usize, usize) {
    let named = |cells: &[CellKey]| member(cells, Axis::Answer) == Some(part);
    let passed = tally.passed(judge).filter(|(p, _)| named(&p.cells));
    let failed = tally.failed(judge).filter(|v| named(&v.cells));
    (passed.map(|(_, n)| n).sum(), failed.count())
}

/// The verdicts of the evaluation judges of one or more corpora, with the units, refusals and counts no verdict holds.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EvalTally {
    /// The verdicts of each layer.
    pub verdicts: BTreeMap<Layer, judgement::Tally>,
    /// The units each layer ran each agreeing row of a record in.
    pub ran: BTreeMap<RecordId, BTreeMap<(Layer, RowKey), Units>>,
    pub fee_unset: usize,
    pub fee_undecoded: usize,
    pub collisions: usize,
    pub skipped_cascade: usize,
    /// The records whose bytes the ledger's decoder refused, for which the ledger evaluated no redeemer.
    pub undecoded: usize,
    /// The rows the evaluator refused, with its refusal, the candidates for the known list.
    pub refused: EvalKnown,
    pub judged: BTreeSet<RecordId>,
    /// The answers of each layer for records whose ledger leaves hold the layer to no answer.
    pub undecided: BTreeMap<Layer, usize>,
    /// The records the transaction judge judged.
    pub tx_judged: BTreeSet<RecordId>,
}

impl EvalTally {
    /// Judges one layer's answer for one record and returns its row verdicts, a malformed answer disagreeing whole.
    pub fn add_answer(
        &mut self,
        layer: Layer,
        id: RecordId,
        ledger: &BTreeMap<RowKey, Answer>,
        listed: &BTreeMap<RowKey, Refusal>,
        answer: Result<DolosAnswer, Malformed>,
    ) -> Vec<(RowKey, RowVerdict)> {
        match answer {
            Ok(answer) => {
                let verdicts = judge_tx(ledger, &answer, listed);
                self.add_rows(layer, id, &verdicts);
                verdicts
            }
            Err(malformed) => {
                self.judged.insert(id);
                let cells = malformed.cells();
                self.fail(layer, id, cells, malformed.to_string());
                Vec::new()
            }
        }
    }

    /// Keeps the verdict of `judge` at `layer` on record `id`.
    fn add(
        &mut self,
        judge: EvalJudge,
        layer: Layer,
        id: RecordId,
        cells: Vec<CellKey>,
        outcome: Outcome,
    ) {
        let verdict = estimate(judge, layer, id, cells, outcome);
        self.verdicts.entry(layer).or_default().add(verdict);
    }

    /// The verdicts of every layer.
    fn every_verdict(&self) -> judgement::Tally {
        let mut all = judgement::Tally::default();
        self.verdicts.values().for_each(|t| all.merge(t.clone()));
        all
    }

    /// Keeps a failing verdict of the evaluation of record `id` at `layer`.
    fn fail(&mut self, layer: Layer, id: RecordId, cells: Vec<CellKey>, detail: String) {
        let outcome = Outcome::Disagree(detail);
        self.add(EvalJudge::Oracle, layer, id, cells, outcome);
    }

    /// Adds the verdict of each row of one layer for one record.
    pub fn add_rows(&mut self, layer: Layer, id: RecordId, verdicts: &[(RowKey, RowVerdict)]) {
        self.judged.insert(id);
        for (key, verdict) in verdicts {
            let (cells, outcome) = row_judgement(key, verdict);
            self.add(EvalJudge::Oracle, layer, id, cells, outcome);
            let refused = match verdict {
                RowVerdict::Known(refusal) => Some(refusal),
                RowVerdict::Disagree(Disagreement {
                    dolos: DolosRow::Refused(refusal),
                    ledger: Some(_),
                    ..
                }) => Some(refusal),
                _ => None,
            };
            if let (Layer::Pallas, Some(refusal)) = (layer, refused) {
                self.refused
                    .entry(id)
                    .or_default()
                    .insert(*key, refusal.clone());
            }
            if let RowVerdict::Agree(Answer::Ran(units)) = verdict {
                let rows = self.ran.entry(id).or_default();
                rows.insert((layer, *key), *units);
            }
        }
    }

    /// Judges whether one response's total equals the sum of its redeemers.
    pub fn add_total(&mut self, layer: Layer, id: RecordId, wire: &WireEval) {
        let cells = || vec![CellKey::new(Axis::Answer, part::TOTAL)];
        match wire.total_agrees() {
            Some(true) => self.add(EvalJudge::Oracle, layer, id, cells(), Outcome::Agree),
            Some(false) => {
                let total = wire.total.map_or("none".to_string(), |t| t.to_string());
                let detail = format!("total=({total}) redeemers={}", wire.redeemers.len());
                self.fail(layer, id, cells(), detail);
            }
            None => {}
        }
        for msg in wire.unnamed() {
            let msg: String = msg.chars().take(160).collect();
            let cells = vec![CellKey::new(Axis::Answer, part::UNNAMED)];
            self.fail(layer, id, cells, msg);
        }
    }

    /// Judges one response's fee against the ledger's minimum fee.
    pub fn add_fee(&mut self, layer: Layer, id: RecordId, ledger: Option<u64>, wire: &WireFee) {
        let cells = || vec![CellKey::new(Axis::Answer, part::FEE)];
        match (ledger, wire) {
            (_, WireFee::Unset) => self.fee_unset += 1,
            (None, _) => self.fee_undecoded += 1,
            (Some(l), WireFee::Int(n)) if i64::try_from(l) == Ok(*n) => {
                self.add(EvalJudge::Oracle, layer, id, cells(), Outcome::Agree)
            }
            (Some(l), other) => {
                self.fail(
                    layer,
                    id,
                    cells(),
                    format!("fee ledger={l} dolos={other:?}"),
                );
            }
        }
    }

    /// Judges one layer's transaction verdict for one record whose redeemers the ledger evaluated none of.
    pub fn add_tx(&mut self, layer: Layer, id: RecordId, verdict: &TxVerdict) {
        self.tx_judged.insert(id);
        let answer = |part| vec![CellKey::new(Axis::Answer, part)];
        let (cells, outcome) = match verdict {
            TxVerdict::Refused { .. } => (answer(part::REFUSED), Outcome::Agree),
            TxVerdict::EvaluatedNone => (answer(part::EVALUATED_NONE), Outcome::Agree),
            TxVerdict::Undecided => {
                *self.undecided.entry(layer).or_default() += 1;
                return;
            }
            TxVerdict::Known { demand, dolos } => {
                let mut cells = answer(part::KNOWN);
                cells.push(CellKey::new(Axis::Demand, demand.kind()));
                (cells, Outcome::Known(dolos.clone()))
            }
            TxVerdict::Disagree {
                demand,
                dolos,
                listed,
            } => {
                let dolos: String = dolos.chars().take(160).collect();
                let listed = listed
                    .as_ref()
                    .map_or(String::new(), |l| format!(" listed=({l})"));
                let cells = vec![CellKey::new(Axis::Demand, demand.kind())];
                let detail = format!("demand={demand} dolos={dolos}{listed}");
                (cells, Outcome::Disagree(detail))
            }
        };
        self.add(EvalJudge::Tx, layer, id, cells, outcome);
    }

    /// Fails each record of the corpus the transaction known list gives that the transaction judge did not judge.
    pub fn tx_unjudged(&mut self, known: &TxKnown, corpus: crate::record::CorpusKind) {
        let missed: Vec<_> = known
            .iter()
            .filter(|(id, _)| id.corpus == corpus && !self.tx_judged.contains(id))
            .collect();
        for (id, answer) in missed {
            let cells = vec![CellKey::new(Axis::Answer, part::UNJUDGED)];
            let outcome = Outcome::Disagree(format!("not judged listed=({answer})"));
            self.add(EvalJudge::Tx, Layer::Pallas, *id, cells, outcome);
        }
    }

    /// Judges one layer's answer for the draft of one record against the signed record's ledger rows.
    pub fn add_draft(
        &mut self,
        layer: Layer,
        signed: RecordId,
        ledger: &BTreeMap<RowKey, Answer>,
        listed: &BTreeMap<RowKey, Refusal>,
        answer: Result<DolosAnswer, Malformed>,
    ) {
        match answer {
            Ok(answer) => {
                for (key, verdict) in judge_tx(ledger, &answer, listed) {
                    let (cells, outcome) = row_judgement(&key, &verdict);
                    self.add(EvalJudge::Draft, layer, signed, cells, outcome);
                }
            }
            Err(malformed) => {
                let outcome = Outcome::Disagree(malformed.to_string());
                self.add(EvalJudge::Draft, layer, signed, malformed.cells(), outcome);
            }
        }
    }

    /// Fails record `signed`, whose bytes derive no draft for the reason `why`.
    pub fn draft_underived(&mut self, signed: RecordId, why: impl fmt::Display) {
        let cells = vec![CellKey::new(Axis::Draft, UNDERIVED)];
        let outcome = Outcome::Disagree(format!("underived {why}"));
        self.add(EvalJudge::Draft, Layer::Pallas, signed, cells, outcome);
    }

    /// Judges one layer's answer for a draft that pays a reward account against the refusal `expected`.
    pub fn add_reward_output(
        &mut self,
        layer: Layer,
        signed: RecordId,
        expected: &Refusal,
        answer: &Result<DolosAnswer, Malformed>,
    ) {
        let mut cells = vec![CellKey::new(Axis::Answer, answer_kind(answer))];
        let outcome = match answer {
            Ok(DolosAnswer::Refused(refusal)) if refusal == expected => Outcome::Agree,
            other => {
                if let Ok(DolosAnswer::Refused(refusal)) = other {
                    cells.push(CellKey::new(Axis::Refusal, refusal));
                }
                Outcome::Disagree(format!(
                    "expected=({expected}) dolos={}",
                    answer_text(other)
                ))
            }
        };
        self.add(EvalJudge::RewardOutput, layer, signed, cells, outcome);
    }

    /// Fails each listed row of a record of the corpus the judgement did not reach.
    pub fn unjudged(&mut self, known: &EvalKnown, corpus: crate::record::CorpusKind) {
        let missed: Vec<_> = known
            .iter()
            .filter(|(id, _)| id.corpus == corpus && !self.judged.contains(id))
            .collect();
        for (id, rows) in missed {
            for (key, refusal) in rows {
                let mut cells = key.cells();
                cells.push(CellKey::new(Axis::Answer, part::UNJUDGED));
                let detail = format!("{key} not judged listed=({refusal})");
                self.fail(Layer::Pallas, *id, cells, detail);
            }
        }
    }

    /// Adds the verdicts and counts of another part after this one's.
    pub fn merge(&mut self, other: EvalTally) {
        let EvalTally {
            verdicts,
            ran,
            fee_unset,
            fee_undecoded,
            collisions,
            skipped_cascade,
            undecoded,
            refused,
            judged,
            undecided,
            tx_judged,
        } = other;
        for (layer, tally) in verdicts {
            self.verdicts.entry(layer).or_default().merge(tally);
        }
        for (id, rows) in ran {
            self.ran.entry(id).or_default().extend(rows);
        }
        self.fee_unset += fee_unset;
        self.fee_undecoded += fee_undecoded;
        self.collisions += collisions;
        self.skipped_cascade += skipped_cascade;
        self.undecoded += undecoded;
        for (id, rows) in refused {
            self.refused.entry(id).or_default().extend(rows);
        }
        self.judged.extend(judged);
        for (layer, n) in undecided {
            *self.undecided.entry(layer).or_default() += n;
        }
        self.tx_judged.extend(tx_judged);
    }

    /// Adds the verdicts of every evaluation judge to `out`.
    pub fn report(&self, out: &mut judgement::Tally) {
        out.merge(self.every_verdict());
    }

    /// The row counts of `judge` by layer.
    fn rows_by_layer(&self, judge: Judge) -> BTreeMap<Layer, Counts> {
        let mut layers = BTreeMap::new();
        for (layer, tally) in &self.verdicts {
            if let Some(counts) = row_counts(tally, judge, |_| Some(())).remove(&()) {
                layers.insert(*layer, counts);
            }
        }
        layers
    }

    /// The transaction verdict counts by layer.
    fn tx_by_layer(&self) -> BTreeMap<Layer, TxCounts> {
        let mut layers: BTreeMap<Layer, TxCounts> = BTreeMap::new();
        for (layer, tally) in &self.verdicts {
            for (passed, n) in tally.passed(Judge::EvalTx) {
                let Some(part) = member(&passed.cells, Axis::Answer) else {
                    continue;
                };
                let counts = layers.entry(*layer).or_default();
                match part {
                    part::REFUSED => counts.refused += n,
                    part::EVALUATED_NONE => counts.evaluated_none += n,
                    part::KNOWN => counts.known += n,
                    _ => {}
                }
            }
            for verdict in tally.failed(Judge::EvalTx) {
                if member(&verdict.cells, Axis::Demand).is_some() {
                    layers.entry(*layer).or_default().disagree += 1;
                }
            }
        }
        for (layer, n) in &self.undecided {
            layers.entry(*layer).or_default().undecided += n;
        }
        layers
    }

    /// The number of passing verdicts of the reward output judge by layer.
    fn reward_output_by_layer(&self) -> BTreeMap<Layer, usize> {
        let mut layers: BTreeMap<Layer, usize> = BTreeMap::new();
        for (layer, tally) in &self.verdicts {
            for (_, n) in tally.passed(Judge::EvalRewardOutput) {
                *layers.entry(*layer).or_default() += n;
            }
        }
        layers
    }

    /// Whether `judge` judged at `layer` what shows the layer was reached: an agreeing row for the row judges, a refusal and an empty evaluation for the transaction judge, and the expected refusal for the reward output judge.
    pub fn judged_at(&self, judge: EvalJudge, layer: Layer) -> bool {
        match judge {
            EvalJudge::Oracle | EvalJudge::Draft => {
                let layers = self.rows_by_layer(judge.judge());
                layers.get(&layer).is_some_and(|c| c.agree > 0)
            }
            EvalJudge::Tx => {
                let layers = self.tx_by_layer();
                layers
                    .get(&layer)
                    .is_some_and(|c| c.refused > 0 && c.evaluated_none > 0)
            }
            EvalJudge::RewardOutput => {
                let layers = self.reward_output_by_layer();
                layers.get(&layer).is_some_and(|n| *n > 0)
            }
        }
    }

    /// Whether `judge` judged at every layer what shows the layer was reached.
    pub fn every_layer_judged(&self, judge: EvalJudge) -> bool {
        Layer::ALL
            .into_iter()
            .all(|layer| self.judged_at(judge, layer))
    }

    /// The failing verdicts of each layer of each evaluation judge that judged
    /// nothing of its kind.
    pub fn every_layer(&self) -> Vec<Verdict> {
        let mut failing = Vec::new();
        for layer in Layer::ALL {
            for judge in EvalJudge::ALL {
                if !self.judged_at(judge, layer) {
                    let entry = layer.entry();
                    let verdict =
                        Verdict::selection_fails(judge.judge(), entry, "layer_judged=false");
                    failing.push(verdict);
                }
            }
        }
        failing
    }

    /// The known list lines of the rows the evaluator refused, each after `CANDIDATE eval-known.txt`.
    pub fn candidates(&self) -> Vec<String> {
        let text = render_eval_known(&self.refused);
        let lines = text
            .lines()
            .map(|l| format!("CANDIDATE eval-known.txt {l}"));
        lines.collect()
    }

    /// The summary line of the evaluation judge.
    fn oracle_line(&self) -> String {
        let judge = Judge::EvalOracle;
        let layers = self.rows_by_layer(judge);
        let all = layers.values().fold(Counts::default(), |mut all, c| {
            all.merge(*c);
            all
        });
        let every = self.every_verdict();
        let purposes = row_counts(&every, judge, |cells| {
            let name = member(cells, Axis::Purpose)?;
            Some((tag_number(name), name.to_string()))
        });
        let levels = row_counts(&every, judge, |cells| {
            member(cells, Axis::Level).map(str::to_string)
        });
        let answers = row_counts(&every, judge, |cells| {
            let ledger = member(cells, Axis::Ledger)?;
            let name = member(cells, Axis::Purpose)?;
            Some((ledger.to_string(), tag_number(name), name.to_string()))
        });
        let layers: Vec<String> = layers.iter().map(|(l, c)| format!("{l}:{c}")).collect();
        let purposes: Vec<String> = purposes
            .iter()
            .map(|((_, p), c)| format!("{p}:{c}"))
            .collect();
        let levels: Vec<String> = levels.iter().map(|(l, c)| format!("{l}:{c}")).collect();
        let answers: Vec<String> = answers
            .iter()
            .map(|((l, _, p), c)| format!("{l}.{p}:{c}"))
            .collect();
        let (totals_agree, totals_disagree) = part_counts(&every, judge, part::TOTAL);
        let (_, unnamed) = part_counts(&every, judge, part::UNNAMED);
        let (fee_equal, fee_differ) = part_counts(&every, judge, part::FEE);
        format!(
            "EVAL-ORACLE compared={} agree={} known={} unrepresentable={} disagree={} by_layer={} by_purpose={} by_level={} by_ledger={} totals={} totals_disagree={totals_disagree} unnamed={unnamed} fee_unset={} fee_equal={fee_equal} fee_differ={fee_differ} fee_undecoded={} collisions={} skipped_cascade={} undecoded={} every_layer_agrees={}",
            all.compared(),
            all.agree,
            all.known,
            all.unrepresentable,
            all.disagree,
            layers.join(","),
            purposes.join(","),
            levels.join(","),
            answers.join(","),
            totals_agree + totals_disagree,
            self.fee_unset,
            self.fee_undecoded,
            self.collisions,
            self.skipped_cascade,
            self.undecoded,
            self.every_layer_judged(EvalJudge::Oracle)
        )
    }

    /// The summary line of the transaction judge.
    fn tx_line(&self) -> String {
        let layers = self.tx_by_layer();
        let mut all = TxCounts::default();
        for counts in layers.values() {
            all.merge(*counts);
        }
        let by_layer = layers.iter().map(|(l, c)| {
            let TxCounts {
                refused,
                evaluated_none,
                undecided,
                known,
                disagree,
            } = c;
            format!("{l}:{refused}/{evaluated_none}/{undecided}/{known}/{disagree}")
        });
        let by_layer: Vec<String> = by_layer.collect();
        format!(
            "EVAL-TX compared={} agree={} known={} disagree={} refused={} evaluated_none={} undecided={} by_layer={} every_layer_judged={}",
            all.refused + all.evaluated_none + all.known + all.disagree,
            all.refused + all.evaluated_none,
            all.known,
            self.every_verdict().failed(Judge::EvalTx).count(),
            all.refused,
            all.evaluated_none,
            all.undecided,
            by_layer.join(","),
            self.every_layer_judged(EvalJudge::Tx)
        )
    }

    /// The summary line of the draft judge.
    fn draft_line(&self) -> String {
        let layers = self.rows_by_layer(Judge::EvalDraft);
        let mut all = Counts::default();
        for counts in layers.values() {
            all.merge(*counts);
        }
        let every = self.every_verdict();
        let underived = every.failed(Judge::EvalDraft);
        let underived = underived.filter(|v| member(&v.cells, Axis::Draft) == Some(UNDERIVED));
        let layers: Vec<String> = layers.iter().map(|(l, c)| format!("{l}:{c}")).collect();
        format!(
            "EVAL-DRAFT compared={} agree={} known={} unrepresentable={} disagree={} underived={} by_layer={} every_layer_agrees={}",
            all.compared(),
            all.agree,
            all.known,
            all.unrepresentable,
            all.disagree,
            underived.count(),
            layers.join(","),
            self.every_layer_judged(EvalJudge::Draft)
        )
    }

    /// The summary line of the reward output judge.
    fn reward_output_line(&self) -> String {
        let layers = self.reward_output_by_layer();
        let agree: usize = layers.values().sum();
        let disagree = self.every_verdict().failed(Judge::EvalRewardOutput).count();
        let layers: Vec<String> = layers.iter().map(|(l, n)| format!("{l}:{n}")).collect();
        format!(
            "EVAL-REWARD-OUTPUT compared={} agree={agree} disagree={disagree} by_layer={} every_layer_agrees={}",
            agree + disagree,
            layers.join(","),
            self.every_layer_judged(EvalJudge::RewardOutput)
        )
    }
}

impl fmt::Display for EvalTally {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let lines = [
            self.oracle_line(),
            self.tx_line(),
            self.draft_line(),
            self.reward_output_line(),
        ];
        f.write_str(&lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::CorpusKind;

    fn key(level: Level, tag: u64, index: u64) -> RowKey {
        RowKey {
            level,
            purpose: Purpose { tag, index },
        }
    }

    fn ran(mem: u64, steps: u64) -> Answer {
        Answer::Ran(Units { mem, steps })
    }

    fn refusal(why: &str) -> Refusal {
        Refusal::new(why)
    }

    fn ic(n: u32) -> RecordId {
        RecordId {
            corpus: CorpusKind::Ic,
            n,
        }
    }

    fn passes(t: &EvalTally) -> bool {
        let mut out = judgement::Tally::default();
        t.report(&mut out);
        out.failing.is_empty()
    }

    fn lines(verdicts: &[Verdict]) -> Vec<String> {
        verdicts.iter().map(Verdict::to_string).collect()
    }

    fn failed(t: &EvalTally, judge: Judge) -> Vec<Verdict> {
        t.every_verdict().failed(judge).cloned().collect()
    }

    #[test]
    fn a_row_disagreement_names_its_index_the_layer_refusal_and_the_listed_one() {
        let shown = |cells: Vec<CellKey>| cells.iter().map(CellKey::to_string).collect::<Vec<_>>();
        assert_eq!(
            shown(key(Level::Top, 0, 3).cells()),
            ["purpose:Spend", "index:3", "level:top"]
        );
        let disagreement = |listed: Option<Refusal>| Disagreement {
            ledger: Some(ran(1, 2)),
            dolos: DolosRow::Refused(refusal("outside the subset")),
            listed,
        };
        assert_eq!(
            shown(disagreement(None).cells()),
            ["ledger:ran", "dolos:refused", "refusal:outside_the_subset"]
        );
        assert_eq!(
            shown(disagreement(Some(refusal("other reason"))).cells()),
            [
                "ledger:ran",
                "dolos:refused",
                "refusal:outside_the_subset",
                "listed:other_reason"
            ]
        );
    }

    #[test]
    fn a_refused_row_never_agrees_with_a_ledger_failure() {
        let verdict = row_verdict(
            Some(Answer::Failed),
            DolosRow::Refused(refusal("subtransaction redeemers")),
            None,
        );
        assert!(matches!(verdict, RowVerdict::Disagree(_)), "{verdict:?}");
        let unanswered = row_verdict(Some(Answer::Failed), DolosRow::Unanswered, None);
        assert!(
            matches!(unanswered, RowVerdict::Disagree(_)),
            "{unanswered:?}"
        );
    }

    #[test]
    fn equal_runs_and_shared_failures_agree_and_nothing_else_does() {
        let same = row_verdict(Some(ran(1, 2)), DolosRow::Answered(ran(1, 2)), None);
        assert_eq!(same, RowVerdict::Agree(ran(1, 2)));
        let failed = row_verdict(
            Some(Answer::Failed),
            DolosRow::Answered(Answer::Failed),
            None,
        );
        assert_eq!(failed, RowVerdict::Agree(Answer::Failed));
        for (ledger, dolos) in [
            (ran(1, 2), ran(1, 3)),
            (ran(1, 2), ran(2, 2)),
            (ran(1, 2), Answer::Failed),
            (Answer::Failed, ran(1, 2)),
        ] {
            let verdict = row_verdict(Some(ledger), DolosRow::Answered(dolos), None);
            assert!(matches!(verdict, RowVerdict::Disagree(_)), "{verdict:?}");
        }
        let unasked = row_verdict(None, DolosRow::Answered(ran(1, 2)), None);
        assert!(matches!(unasked, RowVerdict::Disagree(_)), "{unasked:?}");
    }

    #[test]
    fn a_listed_row_passes_only_while_dolos_refuses_it_for_the_listed_reason() {
        let listed = refusal("subtransaction redeemers");
        let known = row_verdict(
            Some(Answer::Failed),
            DolosRow::Refused(listed.clone()),
            Some(&listed),
        );
        assert_eq!(known, RowVerdict::Known(listed.clone()));
        let other = DolosRow::Refused(refusal("extraneous redeemer"));
        let relabelled = row_verdict(Some(Answer::Failed), other, Some(&listed));
        assert!(
            matches!(relabelled, RowVerdict::Disagree(_)),
            "{relabelled:?}"
        );
        let agreeing = row_verdict(
            Some(ran(1, 2)),
            DolosRow::Answered(ran(1, 2)),
            Some(&listed),
        );
        assert!(matches!(agreeing, RowVerdict::Disagree(_)), "{agreeing:?}");
        let rowless = row_verdict(None, DolosRow::Refused(listed.clone()), Some(&listed));
        assert!(matches!(rowless, RowVerdict::Disagree(_)), "{rowless:?}");
    }

    #[test]
    fn an_unrepresentable_answer_passes_an_unlisted_ledger_row_only() {
        let why =
            DolosRow::Unrepresentable("RPC v1beta cannot represent redeemer Guarding[0]".into());
        let verdict = row_verdict(Some(ran(1, 2)), why.clone(), None);
        assert!(
            matches!(verdict, RowVerdict::Unrepresentable(_)),
            "{verdict:?}"
        );
        let listed = refusal("x");
        let stale = row_verdict(Some(ran(1, 2)), why.clone(), Some(&listed));
        assert!(matches!(stale, RowVerdict::Disagree(_)), "{stale:?}");
        let unasked = row_verdict(None, why, None);
        assert!(matches!(unasked, RowVerdict::Disagree(_)), "{unasked:?}");
    }

    #[test]
    fn a_transaction_is_judged_on_every_row_any_side_names() {
        let ledger = BTreeMap::from([
            (key(Level::Top, 0, 0), ran(1, 2)),
            (key(Level::Sub(0), 0, 0), Answer::Failed),
        ]);
        let dolos = DolosAnswer::Evaluated(BTreeMap::from([
            (key(Level::Top, 0, 0), ran(1, 2)),
            (key(Level::Top, 1, 0), ran(3, 4)),
        ]));
        let listed = BTreeMap::from([(key(Level::Top, 2, 0), refusal("x"))]);
        let verdicts = judge_tx(&ledger, &dolos, &listed);
        let lines: Vec<String> = verdicts
            .iter()
            .map(|(k, v)| match v {
                RowVerdict::Disagree(d) => format!("{k} {d}"),
                other => format!("{k} {other:?}"),
            })
            .collect();
        assert_eq!(
            lines,
            [
                "top Spend#0 Agree(Ran(Units { mem: 1, steps: 2 }))",
                "top Mint#0 ledger=(none) dolos=(ran mem=3 steps=4)",
                "top Cert#0 ledger=(none) dolos=(none) listed=(x)",
                "sub0 Spend#0 ledger=(failed) dolos=(none)",
            ]
        );
    }

    #[test]
    fn a_refused_transaction_refuses_each_ledger_row() {
        let ledger = BTreeMap::from([
            (key(Level::Top, 0, 0), ran(1, 2)),
            (key(Level::Sub(0), 0, 0), Answer::Failed),
        ]);
        let why = refusal("subtransaction redeemers");
        let listed = BTreeMap::from([(key(Level::Sub(0), 0, 0), why.clone())]);
        let verdicts = judge_tx(&ledger, &DolosAnswer::Refused(why.clone()), &listed);
        assert_eq!(verdicts.len(), 2);
        assert!(matches!(verdicts[0].1, RowVerdict::Disagree(_)));
        assert_eq!(verdicts[1].1, RowVerdict::Known(why));
    }

    #[test]
    fn a_shared_purpose_at_two_levels_is_one_collision() {
        let rows = BTreeMap::from([
            (key(Level::Top, 0, 0), ran(1, 2)),
            (key(Level::Sub(0), 0, 0), ran(1, 2)),
            (key(Level::Sub(1), 0, 0), ran(1, 2)),
            (key(Level::Sub(0), 1, 0), ran(1, 2)),
        ]);
        assert_eq!(collisions(&rows), 1);
        let apart = BTreeMap::from([
            (key(Level::Top, 0, 0), ran(1, 2)),
            (key(Level::Sub(0), 0, 1), ran(1, 2)),
        ]);
        assert_eq!(collisions(&apart), 0);
    }

    fn wire(
        redeemers: Vec<(u64, u64, u64)>,
        errors: Vec<(Option<(u64, u64)>, &str)>,
        total: Option<(u64, u64)>,
    ) -> WireEval {
        let purpose = |(tag, index)| Purpose { tag, index };
        WireEval {
            redeemers: redeemers
                .into_iter()
                .map(|(tag, mem, steps)| (purpose((tag, 0)), Some(Units { mem, steps })))
                .collect(),
            errors: errors
                .into_iter()
                .map(|(p, m)| (p.map(purpose), m.to_string()))
                .collect(),
            total: total.map(|(mem, steps)| Units { mem, steps }),
            fee: WireFee::Unset,
        }
    }

    #[test]
    fn a_response_fails_the_redeemers_its_errors_name_and_runs_the_rest() {
        let w = wire(
            vec![(0, 1, 2), (1, 3, 4)],
            vec![(Some((1, 0)), "Mint[0]: boom")],
            Some((4, 6)),
        );
        let expected = BTreeMap::from([
            (key(Level::Top, 0, 0), ran(1, 2)),
            (key(Level::Top, 1, 0), Answer::Failed),
        ]);
        assert_eq!(w.answer(None, None), Ok(DolosAnswer::Evaluated(expected)));
        assert_eq!(w.total_agrees(), Some(true));
        assert!(w.unnamed().is_empty());
        let stray = wire(
            vec![(0, 1, 2)],
            vec![(Some((1, 0)), "Mint[0]: boom")],
            Some((1, 2)),
        );
        assert_eq!(stray.unnamed(), ["Mint[0]: boom"]);
        let twice = wire(vec![(0, 1, 2), (0, 1, 2)], vec![], Some((2, 4)));
        assert_eq!(
            twice.answer(None, None),
            Err(Malformed::Twice(key(Level::Top, 0, 0)))
        );
        let mut unitless = wire(
            vec![(0, 1, 2), (1, 3, 4)],
            vec![(Some((1, 0)), "Mint[0]: boom")],
            Some((4, 6)),
        );
        unitless.redeemers[1].1 = None;
        assert!(unitless.answer(None, None).is_ok());
        assert_eq!(unitless.total_agrees(), Some(false));
        unitless.redeemers[0].1 = None;
        assert_eq!(
            unitless.answer(None, None),
            Err(Malformed::NoUnits(key(Level::Top, 0, 0)))
        );
    }

    #[test]
    fn a_total_that_is_not_the_sum_or_absent_disagrees() {
        assert_eq!(
            wire(vec![(0, 1, 2)], vec![], Some((1, 3))).total_agrees(),
            Some(false)
        );
        let overflow = wire(
            vec![(0, 1, 2)],
            vec![(None, "total execution units overflow")],
            None,
        );
        assert_eq!(overflow.total_agrees(), Some(false));
        assert_eq!(overflow.unnamed(), ["total execution units overflow"]);
        assert_eq!(
            wire(vec![], vec![], Some((0, 0))).total_agrees(),
            Some(true)
        );
    }

    #[test]
    fn a_refusing_response_is_named_by_the_message_it_carries() {
        let w = wire(vec![], vec![(None, "ChainError(x)")], None);
        let evaluator = refusal("subtransaction redeemers");
        let answer = |r, u| w.answer(r, u).unwrap();
        assert_eq!(
            answer(Some(("ChainError(x)", &evaluator)), None),
            DolosAnswer::Refused(evaluator.clone())
        );
        assert_eq!(
            answer(Some(("ChainError(y)", &evaluator)), None),
            DolosAnswer::Refused(refusal("ChainError(x)"))
        );
        assert_eq!(
            answer(None, Some("ChainError(x)")),
            DolosAnswer::Unrepresentable("ChainError(x)".into())
        );
        assert_eq!(
            answer(None, Some("other")),
            DolosAnswer::Refused(refusal("ChainError(x)"))
        );
        assert_eq!(w.total_agrees(), None);
        assert!(w.unnamed().is_empty());
    }

    #[test]
    fn wire_purposes_and_message_prefixes_name_ledger_tags() {
        assert_eq!(wire_purpose(1, 3), Purpose { tag: 0, index: 3 });
        assert_eq!(wire_purpose(7, 0), Purpose { tag: 6, index: 0 });
        assert_eq!(
            wire_purpose(0, 0),
            Purpose {
                tag: u64::MAX,
                index: 0
            }
        );
        assert_eq!(
            wire_purpose(-4, 1),
            Purpose {
                tag: u64::MAX,
                index: 1
            }
        );
        assert_eq!(wire_purpose(8, 0), Purpose { tag: 7, index: 0 });
        assert_eq!(tag_name(7), "7");
        assert_eq!(
            named_in_message("Guarding[2]: no"),
            Some(Purpose { tag: 6, index: 2 })
        );
        assert_eq!(
            named_in_message("Spend[0]: a]: b"),
            Some(Purpose { tag: 0, index: 0 })
        );
        assert_eq!(named_in_message("total execution units overflow"), None);
        assert_eq!(named_in_message("Other[1]: x"), None);
    }

    #[test]
    fn the_known_list_reads_back_what_it_renders_and_refuses_a_row_twice() {
        let text = "ic#890 sub0 Spend#0 subtransaction redeemers\nic#900 top Guarding#0 couldn't evaluate phase-2 script: x\n";
        let known = parse_eval_known("k", text).unwrap();
        assert_eq!(
            known[&ic(890)][&key(Level::Sub(0), 0, 0)],
            refusal("subtransaction redeemers")
        );
        assert_eq!(render_eval_known(&known), text);
        let datum = refusal("missing required datum\n        Datum e8\n");
        assert_eq!(datum.to_string(), "missing required datum Datum e8");
        let multiline =
            EvalKnown::from([(ic(2), BTreeMap::from([(key(Level::Top, 0, 0), datum)]))]);
        let rendered = render_eval_known(&multiline);
        assert_eq!(
            rendered,
            "ic#2 top Spend#0 missing required datum Datum e8\n"
        );
        assert_eq!(parse_eval_known("k", &rendered).unwrap(), multiline);
        let twice = parse_eval_known("k", "ic#1 top Spend#0 a\nic#1 top Spend#0 b");
        assert_eq!(twice.unwrap_err().line, 2);
        assert!(parse_eval_known("k", "ic#1 top Spend#0").is_err());
        assert!(parse_eval_known("k", "ic#1 middle Spend#0 a").is_err());
        assert!(eval_known().is_ok());
    }

    #[test]
    fn the_tally_fails_on_any_disagreement_and_on_a_layer_with_no_agreement() {
        let mut t = EvalTally::default();
        let agree = [(key(Level::Top, 0, 0), RowVerdict::Agree(ran(1, 2)))];
        for layer in Layer::ALL {
            t.add_rows(layer, ic(1), &agree);
        }
        t.add_fee(Layer::Eval(Version::V1), ic(1), Some(7), &WireFee::Unset);
        assert!(passes(&t) && t.every_layer_judged(EvalJudge::Oracle), "{t}");
        let mut no_v1alpha = EvalTally::default();
        for layer in &Layer::ALL[..3] {
            no_v1alpha.add_rows(*layer, ic(1), &agree);
        }
        assert!(passes(&no_v1alpha) && !no_v1alpha.every_layer_judged(EvalJudge::Oracle));
        let unjudged = lines(&no_v1alpha.every_layer());
        let v1alpha = "VERDICT judge=eval-oracle entry=eval_tx:v1alpha record=selection cells= outcome=disagree layer_judged=false";
        assert!(unjudged.iter().any(|l| l == v1alpha), "{unjudged:?}");
        assert!(!unjudged
            .iter()
            .any(|l| l.contains("judge=eval-oracle entry=estimate_tx_calls")));
        let mut twice = t.clone();
        let ledger = BTreeMap::from([(key(Level::Top, 0, 0), ran(1, 2))]);
        let malformed = Err(Malformed::Twice(key(Level::Top, 0, 0)));
        let verdicts = twice.add_answer(
            Layer::Eval(Version::V1),
            ic(4),
            &ledger,
            &BTreeMap::new(),
            malformed,
        );
        assert!(verdicts.is_empty());
        assert_eq!(
            lines(&failed(&twice, Judge::EvalOracle)),
            ["VERDICT judge=eval-oracle entry=eval_tx:v1 record=ic#4 cells=purpose:Spend,index:0,level:top,answer:row,dolos:twice,request_shape:signed outcome=disagree top Spend#0 answered twice"]
        );
        assert!(!passes(&twice));
        let mut differing_fee = t.clone();
        differing_fee.add_fee(Layer::Eval(Version::V1), ic(1), Some(7), &WireFee::Int(8));
        assert!(!passes(&differing_fee));
        let mut refused = t.clone();
        let row = row_verdict(Some(Answer::Failed), DolosRow::Refused(refusal("r")), None);
        refused.add_rows(Layer::Pallas, ic(2), &[(key(Level::Sub(0), 0, 0), row)]);
        assert!(!passes(&refused));
        assert_eq!(
            lines(&failed(&refused, Judge::EvalOracle)),
            ["VERDICT judge=eval-oracle entry=estimate_tx_calls record=ic#2 cells=purpose:Spend,index:0,level:sub,answer:row,ledger:failed,dolos:refused,refusal:r,request_shape:signed outcome=disagree sub0 Spend#0 ledger=(failed) dolos=(refused r)"]
        );
        let mut slow = t.clone();
        let row = row_verdict(Some(ran(1, 2)), DolosRow::Answered(ran(3, 2)), None);
        slow.add_rows(
            Layer::Eval(Version::V1),
            ic(5),
            &[(key(Level::Top, 0, 0), row)],
        );
        let cells = &failed(&slow, Judge::EvalOracle)[0].cells;
        assert!(cells.iter().any(|c| c.to_string() == "units:mem"));
        assert!(!cells.iter().any(|c| c.to_string() == "units:steps"));
        assert_eq!(
            refused.candidates(),
            ["CANDIDATE eval-known.txt ic#2 sub0 Spend#0 r"]
        );
        let mut listed = t.clone();
        let known = EvalKnown::from([(
            ic(3),
            BTreeMap::from([(key(Level::Top, 0, 0), refusal("r"))]),
        )]);
        listed.unjudged(&known, CorpusKind::Ic);
        assert!(!passes(&listed));
        listed.unjudged(&known, CorpusKind::Supp);
        assert_eq!(failed(&listed, Judge::EvalOracle).len(), 1);
    }

    #[test]
    fn the_tally_keeps_the_units_of_each_row_a_layer_ran_and_agreed_on() {
        let mut t = EvalTally::default();
        let rows = [
            (key(Level::Top, 0, 1), RowVerdict::Agree(Answer::Failed)),
            (key(Level::Top, 0, 2), RowVerdict::Known(refusal("r"))),
            (
                key(Level::Top, 0, 3),
                row_verdict(Some(ran(1, 2)), DolosRow::Answered(ran(1, 3)), None),
            ),
            (key(Level::Top, 0, 0), RowVerdict::Agree(ran(1, 2))),
        ];
        t.add_rows(Layer::Eval(Version::V1), ic(1), &rows);
        let mut other = EvalTally::default();
        other.add_rows(Layer::Pallas, ic(1), &rows[3..]);
        t.merge(other);
        let units = Units { mem: 1, steps: 2 };
        let ran = BTreeMap::from([
            ((Layer::Pallas, key(Level::Top, 0, 0)), units),
            ((Layer::Eval(Version::V1), key(Level::Top, 0, 0)), units),
        ]);
        assert_eq!(t.ran, BTreeMap::from([(ic(1), ran)]));
    }

    #[test]
    fn the_tally_line_counts_by_layer_purpose_and_level() {
        let mut t = EvalTally::default();
        let rows = [
            (key(Level::Top, 0, 0), RowVerdict::Agree(ran(1, 2))),
            (key(Level::Sub(0), 0, 0), RowVerdict::Known(refusal("r"))),
        ];
        t.add_rows(Layer::Pallas, ic(1), &rows);
        t.add_rows(Layer::Eval(Version::V1), ic(1), &rows);
        t.undecoded = 1;
        let mut merged = EvalTally::default();
        merged.merge(t.clone());
        assert_eq!(merged, t);
        let line = t.to_string();
        assert!(
            line.starts_with("EVAL-ORACLE compared=4 agree=2 known=2 unrepresentable=0 disagree=0 by_layer=pallas:1/1/0/0,v1:1/1/0/0 by_purpose=Spend:2/2/0/0 by_level=sub:0/2/0/0,top:2/0/0/0 "),
            "{line}"
        );
        assert!(line.contains(" undecoded=1 every_layer_agrees="), "{line}");
    }

    #[test]
    fn the_tally_line_counts_rows_by_the_ledger_answer_and_purpose() {
        let mut t = EvalTally::default();
        let rows = [
            (key(Level::Top, 5, 0), RowVerdict::Known(refusal("r"))),
            (key(Level::Top, 0, 0), RowVerdict::Agree(ran(1, 2))),
            (key(Level::Top, 0, 1), RowVerdict::Agree(Answer::Failed)),
            (key(Level::Sub(0), 5, 0), RowVerdict::Agree(Answer::Failed)),
            (
                key(Level::Top, 5, 1),
                row_verdict(Some(Answer::Failed), DolosRow::Answered(ran(1, 2)), None),
            ),
        ];
        t.add_rows(Layer::Pallas, ic(1), &rows);
        let line = t.to_string();
        assert!(
            line.contains(
                " by_ledger=failed.Spend:1/0/0/0,failed.Propose:1/0/0/1,ran.Spend:1/0/0/0 "
            ),
            "{line}"
        );
    }

    fn refused_by(leaves: &[LedgerLeaf]) -> LedgerOutcome {
        let paths = leaves
            .iter()
            .map(|leaf| crate::record::FailurePath { leaf: *leaf });
        LedgerOutcome::Refused {
            failures: String::new(),
            failure_names: crate::record::NonEmpty::of(paths).unwrap(),
        }
    }

    fn accepted() -> LedgerOutcome {
        LedgerOutcome::Accepted {
            consumed: vec![],
            produced: vec![],
            accounts_post: Default::default(),
        }
    }

    fn none_evaluated() -> Result<DolosAnswer, Malformed> {
        Ok(DolosAnswer::Evaluated(BTreeMap::new()))
    }

    fn refused_for(why: &str) -> Result<DolosAnswer, Malformed> {
        Ok(DolosAnswer::Refused(refusal(why)))
    }

    #[test]
    fn an_empty_evaluation_agrees_with_an_acceptance_and_key_witness_or_fee_leaves() {
        use LedgerLeaf as L;
        let cases = [
            accepted(),
            refused_by(&[L::MissingVKeyWitnessesUTXOW]),
            refused_by(&[L::SubInvalidWitnessesUTXOW, L::SubMissingVKeyWitnessesUTXOW]),
            refused_by(&[L::FeeTooSmallUTxO, L::ExUnitsTooBigUTxO]),
        ];
        for outcome in cases {
            let demand = TxDemand::of(&outcome);
            assert_eq!(demand, TxDemand::EvaluateNone, "{outcome:?}");
            let verdict = tx_verdict(demand, &none_evaluated(), None);
            assert_eq!(verdict, TxVerdict::EvaluatedNone, "{outcome:?}");
            let refused = tx_verdict(demand, &refused_for("missing redeemer Spend[0]"), None);
            assert!(matches!(refused, TxVerdict::Disagree { .. }), "{refused:?}");
        }
    }

    #[test]
    fn leaves_of_other_rules_hold_no_answer() {
        use LedgerLeaf as L;
        for leaves in [
            &[L::BadInputsUTxO][..],
            &[L::MissingVKeyWitnessesUTXOW, L::ValueNotConservedUTxO],
            &[L::MissingRequiredGuards],
        ] {
            let demand = TxDemand::of(&refused_by(leaves));
            assert_eq!(demand, TxDemand::Undecided, "{leaves:?}");
            for answer in [none_evaluated(), refused_for("no redeemer")] {
                assert_eq!(tx_verdict(demand, &answer, None), TxVerdict::Undecided);
            }
        }
    }

    #[test]
    fn a_script_or_redeemer_leaf_demands_a_refusal_naming_a_script_or_redeemer() {
        use LedgerLeaf as L;
        let outcome = refused_by(&[L::CollectErrors, L::SubMissingRedeemers]);
        let demand = TxDemand::of(&outcome);
        assert_eq!(demand, TxDemand::Refuse(L::CollectErrors));
        let empty = tx_verdict(demand, &none_evaluated(), None);
        assert_eq!(
            empty,
            TxVerdict::Disagree {
                demand,
                dolos: "evaluated[0 rows]".to_string(),
                listed: None,
            }
        );
        let named = tx_verdict(demand, &refused_for("MissingRedeemer Spend 0"), None);
        assert_eq!(
            named,
            TxVerdict::Refused {
                leaf: L::CollectErrors,
                refusal: refusal("MissingRedeemer Spend 0")
            }
        );
        let unnamed = tx_verdict(demand, &refused_for("unknown era"), None);
        assert!(matches!(unnamed, TxVerdict::Disagree { .. }), "{unnamed:?}");
        let mixed = TxDemand::of(&refused_by(&[
            L::BadInputsUTxO,
            L::MissingScriptWitnessesUTXOW,
        ]));
        assert_eq!(mixed, TxDemand::Refuse(L::MissingScriptWitnessesUTXOW));
    }

    #[test]
    fn a_failing_native_script_demands_an_empty_evaluation() {
        use LedgerLeaf as L;
        for leaf in [
            L::ScriptWitnessNotValidatingUTXOW,
            L::SubScriptWitnessNotValidatingUTXOW,
        ] {
            let demand = TxDemand::of(&refused_by(&[leaf]));
            assert_eq!(demand, TxDemand::EvaluateNone, "{leaf}");
            let verdict = tx_verdict(demand, &none_evaluated(), None);
            assert_eq!(verdict, TxVerdict::EvaluatedNone, "{leaf}");
        }
        let beside_a_missing_script = TxDemand::of(&refused_by(&[
            L::ScriptWitnessNotValidatingUTXOW,
            L::MissingScriptWitnessesUTXOW,
        ]));
        assert_eq!(
            beside_a_missing_script,
            TxDemand::Refuse(L::MissingScriptWitnessesUTXOW)
        );
    }

    #[test]
    fn the_transaction_line_counts_by_layer_and_fails_the_tally_on_a_disagreement() {
        let mut t = EvalTally::default();
        let demand = TxDemand::Refuse(LedgerLeaf::MissingRedeemers);
        for layer in Layer::ALL {
            let refused = tx_verdict(demand, &refused_for("missing redeemer"), None);
            t.add_tx(layer, ic(1), &refused);
            t.add_tx(layer, ic(2), &TxVerdict::EvaluatedNone);
            t.add_tx(layer, ic(3), &TxVerdict::Undecided);
        }
        assert!(passes(&t));
        assert!(t.every_layer_judged(EvalJudge::Tx));
        let disagree = tx_verdict(demand, &none_evaluated(), None);
        t.add_tx(Layer::Eval(Version::V1beta), ic(4), &disagree);
        assert!(!passes(&t));
        let mut merged = EvalTally::default();
        merged.merge(t.clone());
        assert_eq!(merged, t);
        let line = t.tx_line();
        assert_eq!(
            line,
            "EVAL-TX compared=9 agree=8 known=0 disagree=1 refused=4 evaluated_none=4 undecided=4 by_layer=pallas:1/1/1/0/0,v1:1/1/1/0/0,v1beta:1/1/1/0/1,v1alpha:1/1/1/0/0 every_layer_judged=true"
        );
        assert_eq!(
            lines(&failed(&t, Judge::EvalTx)),
            ["VERDICT judge=eval-tx entry=eval_tx:v1beta record=ic#4 cells=demand:refuse,request_shape:signed outcome=disagree demand=refuse[MissingRedeemers] dolos=evaluated[0 rows]"]
        );
        let mut one_layer = EvalTally::default();
        one_layer.add_tx(Layer::Pallas, ic(1), &TxVerdict::EvaluatedNone);
        assert!(!one_layer.every_layer_judged(EvalJudge::Tx));
    }

    #[test]
    fn a_listed_record_is_known_only_while_a_layer_gives_the_listed_answer() {
        let demand = TxDemand::Refuse(LedgerLeaf::CollectErrors);
        let listed = Some("evaluated[0 rows]");
        let unlisted = tx_verdict(demand, &none_evaluated(), None);
        assert!(
            matches!(unlisted, TxVerdict::Disagree { listed: None, .. }),
            "{unlisted:?}"
        );
        let known = tx_verdict(demand, &none_evaluated(), listed);
        let dolos = "evaluated[0 rows]".to_string();
        assert_eq!(known, TxVerdict::Known { demand, dolos });
        let named = tx_verdict(demand, &refused_for("missing redeemer Spend[0]"), listed);
        assert!(
            matches!(
                named,
                TxVerdict::Disagree {
                    listed: Some(_),
                    ..
                }
            ),
            "{named:?}"
        );
        let undecided = tx_verdict(TxDemand::Undecided, &none_evaluated(), listed);
        assert!(
            matches!(undecided, TxVerdict::Disagree { .. }),
            "{undecided:?}"
        );
        let mut t = EvalTally::default();
        for layer in Layer::ALL {
            t.add_tx(layer, ic(1), &known);
        }
        assert!(failed(&t, Judge::EvalTx).is_empty(), "{t}");
        let list = TxKnown::from([
            (ic(1), "evaluated[0 rows]".to_string()),
            (ic(2), "evaluated[0 rows]".to_string()),
        ]);
        t.tx_unjudged(&list, CorpusKind::Ic);
        assert_eq!(
            lines(&failed(&t, Judge::EvalTx)),
            ["VERDICT judge=eval-tx entry=estimate_tx_calls record=ic#2 cells=answer:unjudged,request_shape:signed outcome=disagree not judged listed=(evaluated[0 rows])"]
        );
        let line = t.tx_line();
        let head = "EVAL-TX compared=4 agree=0 known=4 disagree=1 ";
        assert!(line.starts_with(head), "{line}");
    }

    #[test]
    fn the_transaction_known_list_reads_one_answer_per_record() {
        let text = "ic#3 evaluated[0 rows]\n\nsupp#9 refused[no script]\n";
        let known = parse_tx_known("t", text).unwrap();
        let answer = known.get(&ic(3)).map(String::as_str);
        assert_eq!(answer, Some("evaluated[0 rows]"));
        assert_eq!(known.len(), 2);
        assert!(parse_tx_known("t", "ic#3 a\nic#3 b\n").is_err());
        assert!(parse_tx_known("t", "ic#3\n").is_err());
        assert!(tx_known().is_ok());
    }

    #[test]
    fn a_draft_agrees_only_where_its_rows_equal_the_signed_records_ledger_rows() {
        let ledger = BTreeMap::from([(key(Level::Top, 0, 0), ran(5, 7))]);
        let unlisted = BTreeMap::new();
        let mut t = EvalTally::default();
        for layer in Layer::ALL {
            let same = Ok(DolosAnswer::Evaluated(ledger.clone()));
            t.add_draft(layer, ic(1), &ledger, &unlisted, same);
        }
        assert!(passes(&t));
        assert!(t.every_layer_judged(EvalJudge::Draft));
        let rows_failed = BTreeMap::from([(key(Level::Top, 0, 0), Answer::Failed)]);
        let budget = Ok(DolosAnswer::Evaluated(rows_failed));
        t.add_draft(Layer::Eval(Version::V1), ic(2), &ledger, &unlisted, budget);
        let witnesses = refused_for("missing key witnesses");
        t.add_draft(Layer::Pallas, ic(2), &ledger, &unlisted, witnesses);
        assert!(!passes(&t));
        let mut merged = EvalTally::default();
        merged.merge(t.clone());
        assert_eq!(merged, t);
        assert_eq!(
            t.draft_line(),
            "EVAL-DRAFT compared=6 agree=4 known=0 unrepresentable=0 disagree=2 underived=0 by_layer=pallas:1/0/0/1,v1:1/0/0/1,v1beta:1/0/0/0,v1alpha:1/0/0/0 every_layer_agrees=true"
        );
        assert_eq!(
            lines(&failed(&t, Judge::EvalDraft)),
            [
                "VERDICT judge=eval-draft entry=estimate_tx_calls record=ic#2 cells=purpose:Spend,index:0,level:top,answer:row,ledger:ran,dolos:refused,refusal:missing_key_witnesses,request_shape:draft outcome=disagree top Spend#0 ledger=(ran mem=5 steps=7) dolos=(refused missing key witnesses)",
                "VERDICT judge=eval-draft entry=eval_tx:v1 record=ic#2 cells=purpose:Spend,index:0,level:top,answer:row,ledger:ran,dolos:failed,request_shape:draft outcome=disagree top Spend#0 ledger=(ran mem=5 steps=7) dolos=(failed)"
            ]
        );
        let mut underived = EvalTally::default();
        underived.draft_underived(ic(3), "unexpected type");
        assert!(!passes(&underived));
        let line = underived.draft_line();
        assert!(line.contains(" disagree=0 underived=1 "), "{line}");
    }

    #[test]
    fn a_reward_output_draft_agrees_only_where_the_layer_gives_its_refusal() {
        let expected = refusal("reward account address");
        let mut t = EvalTally::default();
        for layer in Layer::ALL {
            let same = refused_for("reward account address");
            t.add_reward_output(layer, ic(1), &expected, &same);
        }
        assert!(passes(&t));
        assert!(t.every_layer_judged(EvalJudge::RewardOutput));
        let unsupported = refused_for("output address (requires Shelley)");
        t.add_reward_output(Layer::Pallas, ic(2), &expected, &unsupported);
        t.add_reward_output(
            Layer::Eval(Version::V1),
            ic(2),
            &expected,
            &none_evaluated(),
        );
        assert!(!passes(&t));
        let mut merged = EvalTally::default();
        merged.merge(t.clone());
        assert_eq!(merged, t);
        assert_eq!(
            t.reward_output_line(),
            "EVAL-REWARD-OUTPUT compared=6 agree=4 disagree=2 by_layer=pallas:1,v1:1,v1beta:1,v1alpha:1 every_layer_agrees=true"
        );
        assert_eq!(
            lines(&failed(&t, Judge::EvalRewardOutput)),
            [
                "VERDICT judge=eval-reward-output entry=estimate_tx_calls record=ic#2 cells=answer:refused,refusal:output_address_(requires_Shelley),request_shape:reward_output_draft outcome=disagree expected=(reward account address) dolos=refused[output address (requires Shelley)]",
                "VERDICT judge=eval-reward-output entry=eval_tx:v1 record=ic#2 cells=answer:evaluated,request_shape:reward_output_draft outcome=disagree expected=(reward account address) dolos=evaluated[0 rows]"
            ]
        );
        let mut one = EvalTally::default();
        let same = refused_for("reward account address");
        one.add_reward_output(Layer::Pallas, ic(1), &expected, &same);
        assert!(failed(&one, Judge::EvalRewardOutput).is_empty());
        assert!(!one.every_layer_judged(EvalJudge::RewardOutput));
    }

    #[test]
    fn every_leaf_reads_back_from_its_name() {
        for name in ["CollectErrors", "SubMissingRedeemers", "FeeTooSmallUTxO"] {
            let leaf = LedgerLeaf::of_name(name).unwrap();
            assert_eq!(leaf.name(), name);
        }
        assert_eq!(LedgerLeaf::of_name("NoSuchLeaf"), None);
    }

    #[test]
    fn the_layers_answer_at_the_calls_instrument_and_eval_tx_at_each_version() {
        let entries: Vec<EntryPoint> = Layer::ALL.map(Layer::entry).into();
        let mut expected = vec![Instrument::EstimateTxCalls.into()];
        expected.extend(Version::ALL.map(|v| EntryPoint::Route(Route::EvalTx, v)));
        assert_eq!(entries, expected);
    }

    #[test]
    fn a_layer_prints_the_evaluator_or_the_version_it_answers_at() {
        let names = Layer::ALL.map(|layer| layer.to_string());
        assert_eq!(names, ["pallas", "v1", "v1beta", "v1alpha"]);
    }
}
