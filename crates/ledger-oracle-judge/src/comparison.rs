//! One implementation layer compared with its reference on recorded inputs: the
//! records, the outcome of each input, the owner of each allowed difference and
//! the per class tally a run passes or fails by.

use std::{
    collections::BTreeMap,
    fmt,
    panic::AssertUnwindSafe,
    path::Path,
    sync::{mpsc, Arc},
    time::Duration,
};

use serde_json::Value;
use thiserror::Error;

/// A layer of the implementation, compared with its own reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    Decode,
    Validation,
    Mapping,
    Networking,
}

impl Layer {
    /// The layer's name in a records header.
    pub fn name(self) -> &'static str {
        match self {
            Layer::Decode => "decode",
            Layer::Validation => "validation",
            Layer::Mapping => "mapping",
            Layer::Networking => "networking",
        }
    }

    /// The layer a records header names.
    pub fn from_name(name: &str) -> Option<Layer> {
        [
            Layer::Decode,
            Layer::Validation,
            Layer::Mapping,
            Layer::Networking,
        ]
        .into_iter()
        .find(|l| l.name() == name)
    }

    /// Whether every accepted row of this layer carries the reference's
    /// encoding of the value it read.
    pub fn encodes(self) -> bool {
        match self {
            Layer::Decode => true,
            Layer::Validation | Layer::Mapping | Layer::Networking => false,
        }
    }
}

impl fmt::Display for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// How an answer differs from the reference's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Disagreement {
    /// The implementation accepts what the reference refuses.
    Accepts,
    /// The implementation refuses what the reference accepts.
    Refuses,
    /// Both refuse, for different reasons.
    OtherReason,
    /// Both accept, with different values.
    Values,
}

impl Disagreement {
    /// Every disagreement, in report order.
    pub const ALL: [Disagreement; 4] = [
        Disagreement::Accepts,
        Disagreement::Refuses,
        Disagreement::OtherReason,
        Disagreement::Values,
    ];

    /// The disagreement's column name in a report.
    pub fn name(self) -> &'static str {
        match self {
            Disagreement::Accepts => "accepts",
            Disagreement::Refuses => "refuses",
            Disagreement::OtherReason => "other reason",
            Disagreement::Values => "values",
        }
    }
}

/// A commit named by its 40 lowercase hex digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Commit(&'static str);

impl Commit {
    /// The commit `hash`, a compile error in a constant unless it is 40
    /// lowercase hex digits.
    pub const fn new(hash: &'static str) -> Commit {
        let digits = hash.as_bytes();
        let mut i = 0;
        while i < digits.len() {
            assert!(
                matches!(digits[i], b'0'..=b'9' | b'a'..=b'f'),
                "a commit is 40 lowercase hex digits"
            );
            i += 1;
        }
        assert!(digits.len() == 40, "a commit is 40 lowercase hex digits");
        Commit(hash)
    }
}

impl fmt::Display for Commit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// What holds a difference a layer lets pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Owner {
    /// A test of another layer.
    Check { layer: Layer, test: &'static str },
    /// A commit on upstream main that accepts the difference on purpose.
    Upstream { commit: Commit },
}

impl fmt::Display for Owner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Owner::Check { layer, test } => write!(f, "{layer}: {test}"),
            Owner::Upstream { commit } => write!(f, "accepted on upstream main by {commit}"),
        }
    }
}

/// A disagreement a layer lets pass because its owner holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Allowed {
    name: &'static str,
    allows: Disagreement,
    owner: Owner,
}

impl Allowed {
    /// The class `name` letting `allows` pass, held by `owner`.
    pub const fn new(name: &'static str, allows: Disagreement, owner: Owner) -> Self {
        Allowed {
            name,
            allows,
            owner,
        }
    }

    /// What holds this difference.
    pub fn owner(&self) -> Owner {
        self.owner
    }
}

impl fmt::Display for Allowed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.name, self.owner)
    }
}

/// The reference's answer for one input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reference<D> {
    Accepts(D),
    Refuses(String),
}

/// The implementation's answer for one input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer<D, E> {
    Accepts(D),
    Refuses(E),
    Panics(String),
}

/// The judgement of one observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Agree,
    Disagree(Disagreement),
    Panic(String),
    Allowed(Allowed),
    /// The implementation gave no answer within the layer's time limit.
    Timeout,
    /// One side observed the input under a key the other side did not.
    Unpaired,
}

/// Judges an answer against the reference, where `same_reason` says whether a
/// refusal matches the reference's reason and `allowed` is the class the layer
/// assigns to this input, if any.
pub fn judge<D: PartialEq, E>(
    reference: &Reference<D>,
    answer: &Answer<D, E>,
    same_reason: impl Fn(&str, &E) -> bool,
    allowed: Option<Allowed>,
) -> Outcome {
    let disagreement = match (reference, answer) {
        (_, Answer::Panics(message)) => return Outcome::Panic(message.clone()),
        (Reference::Accepts(want), Answer::Accepts(got)) if want == got => return Outcome::Agree,
        (Reference::Accepts(_), Answer::Accepts(_)) => Disagreement::Values,
        (Reference::Accepts(_), Answer::Refuses(_)) => Disagreement::Refuses,
        (Reference::Refuses(_), Answer::Accepts(_)) => Disagreement::Accepts,
        (Reference::Refuses(reason), Answer::Refuses(error)) if same_reason(reason, error) => {
            return Outcome::Agree;
        }
        (Reference::Refuses(_), Answer::Refuses(_)) => Disagreement::OtherReason,
    };
    match allowed {
        Some(class) if class.allows == disagreement => Outcome::Allowed(class),
        _ => Outcome::Disagree(disagreement),
    }
}

/// A difference a layer lets pass, and what in the implementation's answer
/// shows the difference is the one its owner holds.
pub struct Allowance<D, E> {
    pub allowed: Allowed,
    pub shown: fn(&Answer<D, E>) -> bool,
}

/// Runs `f`, turning a panic into its message.
pub fn catch<T>(f: impl FnOnce() -> T + std::panic::UnwindSafe) -> Result<T, String> {
    std::panic::catch_unwind(f).map_err(|payload| {
        payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic with a non string payload".to_owned())
    })
}

/// The axis of a layer that makes one observation of each input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct WholeInput;

impl fmt::Display for WholeInput {
    fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
        Ok(())
    }
}

/// The class of one observation: the record class of its input, whose words
/// are the value kind and the kind of change, and the layer's own axis.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key<A> {
    pub class: String,
    pub axis: A,
}

impl<A> Key<A> {
    /// The kind of value the input is, the first word of its class.
    pub fn value_kind(&self) -> &str {
        self.class.split(' ').next().unwrap_or_default()
    }

    /// The kind of change made to the input, the second word of its class.
    pub fn change_kind(&self) -> Option<&str> {
        self.class.split_once(' ').map(|(_, kind)| kind)
    }
}

impl<A: fmt::Display> fmt::Display for Key<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let axis = self.axis.to_string();
        match axis.is_empty() {
            true => f.write_str(&self.class),
            false => write!(f, "{} {axis}", self.class),
        }
    }
}

/// The counts of the observations under one key.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Counts {
    pub inputs: usize,
    pub agree: usize,
    pub disagree: BTreeMap<Disagreement, usize>,
    pub panic: usize,
    pub allowed: BTreeMap<Allowed, usize>,
    pub timeout: usize,
    pub unpaired: usize,
}

/// The counts of one declared class: its observations by axis and its inputs
/// the layer does not judge.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ClassCounts<A> {
    axes: BTreeMap<A, Counts>,
    not_judged: usize,
}

/// The per key counts of one layer's run, over the classes its records
/// declare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tally<A = WholeInput> {
    layer: Layer,
    classes: BTreeMap<String, ClassCounts<A>>,
    undeclared: BTreeMap<String, usize>,
}

impl<A: Ord + Clone + fmt::Display> Tally<A> {
    /// An empty tally over `classes`, every one of which needs a judged
    /// observation.
    pub fn new<S: AsRef<str>>(layer: Layer, classes: &[S]) -> Self {
        Tally {
            layer,
            classes: classes
                .iter()
                .map(|c| {
                    let counts = ClassCounts {
                        axes: BTreeMap::new(),
                        not_judged: 0,
                    };
                    (c.as_ref().to_owned(), counts)
                })
                .collect(),
            undeclared: BTreeMap::new(),
        }
    }

    /// Counts one judged observation under `key`.
    pub fn add(&mut self, key: &Key<A>, outcome: &Outcome) {
        let Some(class) = self.classes.get_mut(&key.class) else {
            *self.undeclared.entry(key.class.clone()).or_default() += 1;
            return;
        };
        let counts = class.axes.entry(key.axis.clone()).or_default();
        counts.inputs += 1;
        match outcome {
            Outcome::Agree => counts.agree += 1,
            Outcome::Disagree(d) => *counts.disagree.entry(*d).or_default() += 1,
            Outcome::Panic(_) => counts.panic += 1,
            Outcome::Allowed(class) => *counts.allowed.entry(*class).or_default() += 1,
            Outcome::Timeout => counts.timeout += 1,
            Outcome::Unpaired => counts.unpaired += 1,
        }
    }

    /// Counts one input of `class` whose reference answer this layer does not
    /// judge.
    pub fn not_judged(&mut self, class: &str) {
        match self.classes.get_mut(class) {
            Some(counts) => counts.not_judged += 1,
            None => *self.undeclared.entry(class.to_owned()).or_default() += 1,
        }
    }

    /// Counts one input of `class`, which the records do not declare.
    pub fn undeclared(&mut self, class: &str) {
        *self.undeclared.entry(class.to_owned()).or_default() += 1;
    }

    /// The counts under `key`, if it has an observation.
    pub fn counts(&self, key: &Key<A>) -> Option<&Counts> {
        self.classes.get(&key.class)?.axes.get(&key.axis)
    }

    /// The number of inputs of `class` the layer does not judge, if the tally
    /// declares it.
    pub fn not_judged_count(&self, class: &str) -> Option<usize> {
        self.classes.get(class).map(|c| c.not_judged)
    }

    /// The number of judged observations and the number of keys they are
    /// under.
    pub fn judged(&self) -> (usize, usize) {
        let counts = self.classes.values().flat_map(|c| c.axes.values());
        counts.fold((0, 0), |(n, keys), c| (n + c.inputs, keys + 1))
    }

    /// Each reason the run fails: no judged observation, a declared class with
    /// none, a disagreement, a panic, a timeout, an unpaired observation, or an
    /// input of a class the records do not declare.
    pub fn failures(&self) -> Vec<String> {
        let layer = self.layer;
        let mut out = Vec::new();
        if self.judged().0 == 0 {
            out.push(format!("{layer}: no observation judged"));
        }
        for (class, c) in &self.classes {
            if c.axes.is_empty() {
                out.push(format!(
                    "{layer}: class {class:?} has no judged observation"
                ));
            }
            for (axis, c) in &c.axes {
                let key = Key {
                    class: class.clone(),
                    axis: axis.clone(),
                }
                .to_string();
                for (d, n) in &c.disagree {
                    out.push(format!(
                        "{layer}: class {key:?} disagrees ({}) {n}",
                        d.name()
                    ));
                }
                for (what, n) in [
                    ("panics", c.panic),
                    ("times out", c.timeout),
                    ("has unpaired observations", c.unpaired),
                ] {
                    if n > 0 {
                        out.push(format!("{layer}: class {key:?} {what} {n}"));
                    }
                }
            }
        }
        for (class, n) in &self.undeclared {
            out.push(format!(
                "{layer}: class {class:?} is not declared, {n} inputs"
            ));
        }
        out
    }
}

impl<A: Ord + Clone + fmt::Display> fmt::Display for Tally<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:<10} {:<32} {:>6} {:>6}",
            "layer", "class", "inputs", "agree"
        )?;
        for d in Disagreement::ALL {
            write!(f, " {:>12}", d.name())?;
        }
        writeln!(
            f,
            " {:>6} {:>10}  allowed with owner",
            "panic", "not judged"
        )?;
        let mut stopped = Vec::new();
        for (class, counts) in &self.classes {
            let none = Counts::default();
            let mut rows: Vec<(String, &Counts)> = counts
                .axes
                .iter()
                .map(|(axis, c)| {
                    let key = Key {
                        class: class.clone(),
                        axis: axis.clone(),
                    };
                    (key.to_string(), c)
                })
                .collect();
            if rows.is_empty() {
                rows.push((class.clone(), &none));
            }
            for (i, (key, c)) in rows.into_iter().enumerate() {
                write!(
                    f,
                    "{:<10} {:<32} {:>6} {:>6}",
                    self.layer.name(),
                    key,
                    c.inputs,
                    c.agree
                )?;
                for d in Disagreement::ALL {
                    write!(f, " {:>12}", c.disagree.get(&d).copied().unwrap_or(0))?;
                }
                let allowed: Vec<String> =
                    c.allowed.iter().map(|(a, n)| format!("{n} {a}")).collect();
                let not_judged = if i == 0 { counts.not_judged } else { 0 };
                writeln!(
                    f,
                    " {:>6} {:>10}  {}",
                    c.panic,
                    not_judged,
                    allowed.join(", ")
                )?;
                if c.timeout > 0 || c.unpaired > 0 {
                    stopped.push((key, c.timeout, c.unpaired));
                }
            }
        }
        for (class, n) in &self.undeclared {
            writeln!(
                f,
                "{:<10} {:<32} {:>6} undeclared",
                self.layer.name(),
                class,
                n
            )?;
        }
        for (key, timeout, unpaired) in stopped {
            writeln!(
                f,
                "{:<10} {:<32} {:>6} timed out, {unpaired} unpaired",
                self.layer.name(),
                key,
                timeout
            )?;
        }
        Ok(())
    }
}

/// The reference's observations of one input, or the reason the layer does
/// not judge it.
pub enum Expected<A, D> {
    /// One or more observations, each under its own axis.
    Judged {
        first: (A, Reference<D>),
        rest: Vec<(A, Reference<D>)>,
    },
    /// The layer does not judge the reference's answer for this input.
    NotJudged,
}

impl<A, D> Expected<A, D> {
    /// The one observation `reference` under `axis`.
    pub fn one(axis: A, reference: Reference<D>) -> Self {
        Expected::Judged {
            first: (axis, reference),
            rest: Vec::new(),
        }
    }
}

/// A layer of the implementation compared with its reference, one record at
/// a time.
pub trait ComparedLayer: Send + Sync + 'static {
    /// The layer the records are written for.
    const LAYER: Layer;
    /// The longest the implementation may take over one input.
    const TIME_LIMIT: Duration;
    /// What the implementation reads from one record.
    type Input: Send + 'static;
    /// What tells apart the observations of one input.
    type Axis: Ord + Clone + fmt::Display + Send + 'static;
    /// An accepted answer, comparable with the reference's.
    type Value: PartialEq + fmt::Debug + Send + 'static;
    /// A refusal by the implementation.
    type Refusal: fmt::Debug + Send + 'static;

    /// The input the implementation reads from `row`.
    fn input(&self, row: &Row) -> Self::Input;

    /// The reference's observations of `row`.
    fn reference(&self, row: &Row) -> Expected<Self::Axis, Self::Value>;

    /// Runs the implementation on `input`, giving each observation and its
    /// answer.
    #[allow(clippy::type_complexity)]
    fn run(&self, input: &Self::Input) -> Vec<(Self::Axis, Result<Self::Value, Self::Refusal>)>;

    /// Whether `refusal` has the reference's reason `reason`.
    fn same_reason(&self, reason: &str, refusal: &Self::Refusal) -> bool;

    /// The difference this layer lets pass under `key`, if any.
    fn allowance(&self, key: &Key<Self::Axis>) -> Option<Allowance<Self::Value, Self::Refusal>>;

    /// A line naming how the answer for one observation of `row` compares
    /// with the reference's.
    fn detail(
        &self,
        row: &Row,
        reference: &Reference<Self::Value>,
        answer: &Answer<Self::Value, Self::Refusal>,
    ) -> String {
        let variant = row.variant.as_deref().unwrap_or("");
        format!(
            "{} {variant}: reference {reference:?}, implementation {answer:?}",
            row.class
        )
    }
}

/// The judgement of one observation of one record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judgement<'r, A> {
    pub row: &'r Row,
    pub key: Key<A>,
    pub outcome: Outcome,
    pub detail: String,
}

/// One layer's run over its records: every judgement, their tally and the
/// failures of the run as a whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerRun<'r, A> {
    pub tally: Tally<A>,
    pub judgements: Vec<Judgement<'r, A>>,
    failures: Vec<String>,
}

impl<A: Ord + Clone + fmt::Display> LayerRun<'_, A> {
    /// Each reason the run fails: records of another layer, a filter kind
    /// with no input, or a failure of the tally.
    pub fn failures(&self) -> Vec<String> {
        let mut out = self.failures.clone();
        out.extend(self.tally.failures());
        out
    }
}

impl<A: Ord + Clone + fmt::Display> fmt::Display for LayerRun<'_, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (n, keys) = self.tally.judged();
        write!(f, "{}", self.tally)?;
        writeln!(
            f,
            "{}: {n} observations judged under {keys} keys",
            self.tally.layer
        )
    }
}

/// The name of the environment variable holding the kinds a run is filtered
/// to, separated by commas.
pub const KINDS: &str = "REFERENCE_COMPARISON_KINDS";

/// The kinds and classes the environment filters a run to, none when it
/// names none.
pub fn kinds_from_env() -> Vec<String> {
    std::env::var(KINDS)
        .unwrap_or_default()
        .split(',')
        .filter(|k| !k.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Whether the record class `class` is one of `kinds`, by its whole name or
/// its kind of change.
fn selected(class: &str, kinds: &[String]) -> bool {
    kinds.is_empty()
        || kinds
            .iter()
            .any(|k| class == k || class.split_once(' ').is_some_and(|(_, change)| change == k))
}

/// The stack each run of the implementation gets, enough for a recursive
/// decoder over a whole block.
const STACK: usize = 1 << 28;

/// What a run of the implementation over one input ended with.
enum Ended<T> {
    Answered(T),
    Panicked(String),
    TimedOut,
}

/// Runs the implementation on `input` in a thread of its own, giving up after
/// the layer's time limit.
fn run_limited<L: ComparedLayer>(layer: &Arc<L>, input: L::Input) -> Ended<Observed<L>> {
    let (send, receive) = mpsc::channel();
    let layer = Arc::clone(layer);
    let spawned = std::thread::Builder::new()
        .stack_size(STACK)
        .spawn(move || {
            let answer = catch(AssertUnwindSafe(|| layer.run(&input)));
            let _ = send.send(answer);
        });
    if let Err(e) = spawned {
        return Ended::Panicked(format!("the run could not start: {e}"));
    }
    match receive.recv_timeout(L::TIME_LIMIT) {
        Ok(Ok(observed)) => Ended::Answered(observed),
        Ok(Err(message)) => Ended::Panicked(message),
        Err(mpsc::RecvTimeoutError::Timeout) => Ended::TimedOut,
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Ended::Panicked("the run ended without an answer".to_owned())
        }
    }
}

/// The observations a run of `L` gives for one input.
type Observed<L> = Vec<(
    <L as ComparedLayer>::Axis,
    Result<<L as ComparedLayer>::Value, <L as ComparedLayer>::Refusal>,
)>;

/// Runs `layer` on every record of `records` whose class is one of `kinds`,
/// or on every record when `kinds` is empty, and judges each observation.
pub fn compare<'r, L: ComparedLayer>(
    layer: &Arc<L>,
    records: &'r Records,
    kinds: &[String],
) -> LayerRun<'r, L::Axis> {
    let declared: Vec<&String> = records
        .classes()
        .iter()
        .filter(|c| selected(c, kinds))
        .collect();
    let mut run = LayerRun {
        tally: Tally::new(L::LAYER, &declared),
        judgements: Vec::new(),
        failures: Vec::new(),
    };
    if records.layer() != L::LAYER {
        run.failures.push(format!(
            "the records are for {}, not {}",
            records.layer(),
            L::LAYER
        ));
        return run;
    }
    let mut hits: BTreeMap<&str, usize> = kinds.iter().map(|k| (k.as_str(), 0)).collect();
    for row in records.rows() {
        if !records.classes().contains(&row.class) {
            run.tally.undeclared(&row.class);
            continue;
        }
        if !selected(&row.class, kinds) {
            continue;
        }
        for (kind, n) in hits.iter_mut() {
            if selected(&row.class, &[(*kind).to_owned()]) {
                *n += 1;
            }
        }
        let expected = match layer.reference(row) {
            Expected::Judged { first, rest } => std::iter::once(first).chain(rest).collect(),
            Expected::NotJudged => {
                run.tally.not_judged(&row.class);
                continue;
            }
        };
        judge_row(layer, row, expected, &mut run);
    }
    for (kind, n) in hits {
        if n == 0 {
            run.failures
                .push(format!("{}: filter kind {kind:?} has no input", L::LAYER));
        }
    }
    run
}

/// Runs `layer` on one record and judges each of its observations against
/// `expected`, the reference's.
fn judge_row<'r, L: ComparedLayer>(
    layer: &Arc<L>,
    row: &'r Row,
    expected: Vec<(L::Axis, Reference<L::Value>)>,
    run: &mut LayerRun<'r, L::Axis>,
) {
    let key = |axis: L::Axis| Key {
        class: row.class.clone(),
        axis,
    };
    let mut observed = match run_limited(layer, layer.input(row)) {
        Ended::Answered(observed) => observed,
        Ended::Panicked(message) => {
            for (axis, _) in expected {
                let outcome = Outcome::Panic(message.clone());
                run.record(row, key(axis), outcome, format!("panics: {message}"));
            }
            return;
        }
        Ended::TimedOut => {
            for (axis, _) in expected {
                let detail = format!("no answer within {:?}", L::TIME_LIMIT);
                run.record(row, key(axis), Outcome::Timeout, detail);
            }
            return;
        }
    };
    for (axis, reference) in expected {
        let Some(at) = observed.iter().position(|(a, _)| *a == axis) else {
            let detail = "the implementation makes no observation under this key".to_owned();
            run.record(row, key(axis), Outcome::Unpaired, detail);
            continue;
        };
        let answer = match observed.remove(at).1 {
            Ok(value) => Answer::Accepts(value),
            Err(refusal) => Answer::Refuses(refusal),
        };
        let key = key(axis);
        let allowed = layer
            .allowance(&key)
            .filter(|a| (a.shown)(&answer))
            .map(|a| a.allowed);
        let outcome = judge(
            &reference,
            &answer,
            |reason, refusal| layer.same_reason(reason, refusal),
            allowed,
        );
        let detail = layer.detail(row, &reference, &answer);
        run.record(row, key, outcome, detail);
    }
    for (axis, _) in observed {
        let detail = "the reference makes no observation under this key".to_owned();
        run.record(row, key(axis), Outcome::Unpaired, detail);
    }
}

impl<'r, A: Ord + Clone + fmt::Display> LayerRun<'r, A> {
    /// Counts and keeps one judgement.
    fn record(&mut self, row: &'r Row, key: Key<A>, outcome: Outcome, detail: String) {
        self.tally.add(&key, &outcome);
        self.judgements.push(Judgement {
            row,
            key,
            outcome,
            detail,
        });
    }
}

/// The reference's reading of an input it accepts and its own encoding of the
/// value, when the records carry one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    pub dump: Value,
    pub encoding: Option<Vec<u8>>,
}

/// One recorded input and the reference's answer for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub class: String,
    pub variant: Option<String>,
    pub input: Vec<u8>,
    pub reference: Reference<Accepted>,
    pub rules: Vec<String>,
    pub site: Option<String>,
    pub mutated: Option<String>,
    pub base: Option<Vec<u8>>,
}

impl Row {
    /// The rule a changed row's input changes and the kind of change, the
    /// second word of its class.
    pub fn change(&self) -> Option<(&str, &str)> {
        let (_, kind) = self.class.split_once(' ')?;
        Some((self.mutated.as_deref()?, kind))
    }
}

/// A rule of the reference's spec and the kinds of change the records make to
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub rule: String,
    pub kinds: Vec<String>,
}

/// The number of inputs of each target change, over one layer's records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coverage {
    layer: Layer,
    counts: BTreeMap<(String, String), usize>,
}

impl Coverage {
    /// Counts the rows that make each change `targets` names.
    pub fn new(layer: Layer, targets: &[Target], rows: &[Row]) -> Self {
        let mut counts: BTreeMap<(String, String), usize> = targets
            .iter()
            .flat_map(|t| t.kinds.iter().map(|k| ((t.rule.clone(), k.clone()), 0)))
            .collect();
        for (rule, kind) in rows.iter().filter_map(Row::change) {
            if let Some(n) = counts.get_mut(&(rule.to_owned(), kind.to_owned())) {
                *n += 1;
            }
        }
        Coverage { layer, counts }
    }

    /// The number of inputs that change `rule` by `kind`, if it is a target.
    pub fn count(&self, rule: &str, kind: &str) -> Option<usize> {
        self.counts
            .get(&(rule.to_owned(), kind.to_owned()))
            .copied()
    }

    /// The number of target changes of each kind and how many of them an
    /// input makes.
    pub fn kinds(&self) -> BTreeMap<&str, (usize, usize)> {
        let mut out: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for ((_, kind), n) in &self.counts {
            let (targets, made) = out.entry(kind.as_str()).or_default();
            *targets += 1;
            *made += usize::from(*n > 0);
        }
        out
    }

    /// Each reason the coverage fails: a target change no input makes, or a
    /// change in `required` the targets lack.
    pub fn failures(&self, required: &[(&str, &str)]) -> Vec<String> {
        let mut out: Vec<String> = self
            .counts
            .iter()
            .filter(|(_, n)| **n == 0)
            .map(|((rule, kind), _)| format!("{}: no input changes {rule} by {kind}", self.layer))
            .collect();
        for (rule, kind) in required {
            if self.count(rule, kind).is_none() {
                out.push(format!(
                    "{}: the targets lack the required change {rule} by {kind}",
                    self.layer
                ));
            }
        }
        out
    }
}

impl fmt::Display for Coverage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let made = self.counts.values().filter(|n| **n > 0).count();
        writeln!(
            f,
            "{}: {} target changes, {made} made, {} not made",
            self.layer,
            self.counts.len(),
            self.counts.len() - made
        )
    }
}

/// A record file whose reference pin equals the pin of the head under test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Records {
    pin: String,
    generator: String,
    layer: Layer,
    classes: Vec<String>,
    targets: Vec<Target>,
    rows: Vec<Row>,
}

/// Why a record file cannot be read for the head under test.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RecordsError {
    #[error("records were written at reference pin {records}, the head vendors {head}")]
    PinDiffers { records: String, head: String },
    #[error("line {line}: {reason}")]
    Malformed { line: usize, reason: String },
    #[error("{path}: {reason}")]
    Unreadable { path: String, reason: String },
}

impl Records {
    /// Reads a record file, refusing it unless its header names `pin`.
    pub fn read(text: &str, pin: &str) -> Result<Records, RecordsError> {
        let mut lines = text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty());
        let malformed = |line: usize, reason: &str| RecordsError::Malformed {
            line: line + 1,
            reason: reason.to_owned(),
        };
        let (n, header) = lines.next().ok_or_else(|| malformed(0, "no header"))?;
        let header: Value =
            serde_json::from_str(header).map_err(|e| malformed(n, &e.to_string()))?;
        let field = |name: &str| {
            header
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| malformed(n, &format!("header has no {name}")))
        };
        let records_pin = field("reference")?;
        if records_pin != pin {
            return Err(RecordsError::PinDiffers {
                records: records_pin,
                head: pin.to_owned(),
            });
        }
        let classes = header
            .get("classes")
            .and_then(Value::as_array)
            .and_then(|a| a.iter().map(|c| c.as_str().map(str::to_owned)).collect())
            .ok_or_else(|| malformed(n, "header has no class list"))?;
        let names = |v: Option<&Value>| -> Option<Vec<String>> {
            v?.as_array()?
                .iter()
                .map(|x| x.as_str().map(str::to_owned))
                .collect()
        };
        let targets = match header.get("targets") {
            None => Vec::new(),
            Some(t) => t
                .as_array()
                .and_then(|a| {
                    a.iter()
                        .map(|t| {
                            Some(Target {
                                rule: t.get("rule")?.as_str()?.to_owned(),
                                kinds: names(t.get("kinds"))?,
                            })
                        })
                        .collect()
                })
                .ok_or_else(|| malformed(n, "targets is not a list of rules with kinds"))?,
        };
        let mut rows = Vec::new();
        for (n, line) in lines {
            let v: Value = serde_json::from_str(line).map_err(|e| malformed(n, &e.to_string()))?;
            let text = |name: &str| v.get(name).and_then(Value::as_str);
            let class = text("class").ok_or_else(|| malformed(n, "row has no class"))?;
            let input = text("input")
                .and_then(|h| hex::decode(h).ok())
                .ok_or_else(|| malformed(n, "row has no hex input"))?;
            let reference = match v.get("reference") {
                Some(r) => match (r.get("accepts"), r.get("refuses"), r.get("encoding")) {
                    (Some(dump), None, encoding) => Reference::Accepts(Accepted {
                        dump: dump.clone(),
                        encoding: match encoding {
                            None => None,
                            Some(e) => Some(
                                e.as_str()
                                    .and_then(|h| hex::decode(h).ok())
                                    .ok_or_else(|| malformed(n, "encoding is not hex"))?,
                            ),
                        },
                    }),
                    (None, Some(Value::String(reason)), None) => Reference::Refuses(reason.clone()),
                    (None, Some(_), Some(_)) => {
                        return Err(malformed(n, "an encoding beside a refusal"))
                    }
                    _ => return Err(malformed(n, "reference is neither accepts nor refuses")),
                },
                None => return Err(malformed(n, "row has no reference")),
            };
            let rules = match v.get("rules") {
                None => Vec::new(),
                Some(r) => {
                    names(Some(r)).ok_or_else(|| malformed(n, "rules is not a list of names"))?
                }
            };
            let base = match v.get("base") {
                None => None,
                Some(b) => Some(
                    b.as_str()
                        .and_then(|h| hex::decode(h).ok())
                        .ok_or_else(|| malformed(n, "base is not hex"))?,
                ),
            };
            rows.push(Row {
                class: class.to_owned(),
                variant: text("variant").map(str::to_owned),
                input,
                reference,
                rules,
                site: text("site").map(str::to_owned),
                mutated: text("mutated").map(str::to_owned),
                base,
            });
        }
        Ok(Records {
            pin: records_pin,
            generator: field("generator")?,
            layer: Layer::from_name(&field("layer")?)
                .ok_or_else(|| malformed(n, "header names no known layer"))?,
            classes,
            targets,
            rows,
        })
    }

    /// The changes the records name as targets, empty when the header names
    /// none.
    pub fn targets(&self) -> &[Target] {
        &self.targets
    }

    /// The reference pin the records were written at.
    pub fn pin(&self) -> &str {
        &self.pin
    }

    /// The identity of the program that wrote the records.
    pub fn generator(&self) -> &str {
        &self.generator
    }

    /// The layer the records are for.
    pub fn layer(&self) -> Layer {
        self.layer
    }

    /// The input classes the records declare.
    pub fn classes(&self) -> &[String] {
        &self.classes
    }

    /// The recorded inputs.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Reads the records of `layer` in `dir`, a directory named by the
    /// reference pin they must be written at.
    pub fn read_dir(dir: &Path, layer: Layer) -> Result<Records, RecordsError> {
        let path = dir.join(format!("{}.jsonl", layer.name()));
        let unreadable = |reason: String| RecordsError::Unreadable {
            path: path.display().to_string(),
            reason,
        };
        let pin = dir
            .file_name()
            .and_then(|p| p.to_str())
            .ok_or_else(|| unreadable("the directory names no pin".to_owned()))?;
        let text = std::fs::read_to_string(&path).map_err(|e| unreadable(e.to_string()))?;
        Records::read(&text, pin)
    }

    /// The number of rows of each declared class and of each class a row
    /// names that the header does not declare.
    pub fn rows_per_class(&self) -> BTreeMap<&str, usize> {
        let mut out: BTreeMap<&str, usize> = self.classes.iter().map(|c| (c.as_str(), 0)).collect();
        for row in &self.rows {
            *out.entry(row.class.as_str()).or_default() += 1;
        }
        out
    }

    /// Each reason the records cannot be judged as `layer` with the kinds and
    /// classes `required` names: another layer, no rows, a declared class with
    /// no row, a row of a class not declared, a kind with a target no row
    /// makes, a required name the records lack, or an accepted row with no
    /// encoding in a layer whose rows carry one.
    pub fn check<S: AsRef<str>>(&self, layer: Layer, required: &[S]) -> Vec<String> {
        let mut out = Vec::new();
        if self.layer != layer {
            out.push(format!("the records are for {}, not {layer}", self.layer));
        }
        let layer = self.layer;
        if self.rows.is_empty() {
            out.push(format!("{layer}: no rows"));
        }
        for (class, n) in self.rows_per_class() {
            if !self.classes.iter().any(|c| c == class) {
                out.push(format!(
                    "{layer}: class {class:?} is not declared, {n} rows"
                ));
            } else if n == 0 {
                out.push(format!("{layer}: class {class:?} has no row"));
            }
        }
        let coverage = Coverage::new(layer, &self.targets, &self.rows);
        let kinds = coverage.kinds();
        for (kind, (targets, made)) in &kinds {
            if made < targets {
                out.push(format!(
                    "{layer}: kind {kind} made {made} of {targets} targets"
                ));
            }
        }
        for name in required.iter().map(AsRef::as_ref) {
            let named = kinds.contains_key(name)
                || self
                    .classes
                    .iter()
                    .any(|c| c == name || c.split_once(' ').is_some_and(|(_, kind)| kind == name));
            if !named {
                out.push(format!(
                    "{layer}: the records name no kind or class {name:?}"
                ));
            }
        }
        if layer.encodes() {
            let mut bare: BTreeMap<&str, usize> = BTreeMap::new();
            for row in &self.rows {
                if let Reference::Accepts(Accepted { encoding: None, .. }) = &row.reference {
                    *bare.entry(row.class.as_str()).or_default() += 1;
                }
            }
            for (class, n) in bare {
                out.push(format!(
                    "{layer}: class {class:?} has {n} accepted rows with no encoding"
                ));
            }
        }
        out
    }
}

/// The 40 digit commit a vendored CDDL file names on its first line.
pub fn cddl_pin(cddl: &str) -> Option<&str> {
    let first = cddl.lines().next()?;
    first
        .split(|c: char| !c.is_ascii_hexdigit())
        .find(|word| word.len() == 40)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRAILING: Allowed = Allowed::new(
        "trailing bytes",
        Disagreement::Accepts,
        Owner::Check {
            layer: Layer::Validation,
            test: "phase_one_refuses_leftover",
        },
    );

    const EXTRA: Allowed = Allowed::new(
        "extra array elements",
        Disagreement::Accepts,
        Owner::Upstream {
            commit: Commit::new("a8516e16bb38621eeb86b83bef0e146e6123b648"),
        },
    );

    fn refuses() -> Reference<()> {
        Reference::Refuses("Denominator cannot be zero".into())
    }

    fn any_reason(_: &str, _: &&str) -> bool {
        true
    }

    fn no_reason(_: &str, _: &&str) -> bool {
        false
    }

    fn whole(class: &str) -> Key<WholeInput> {
        Key {
            class: class.to_owned(),
            axis: WholeInput,
        }
    }

    fn tally(layer: Layer, classes: &[&str]) -> Tally {
        Tally::new(layer, classes)
    }

    #[test]
    fn equal_acceptances_agree() {
        let outcome = judge(
            &Reference::Accepts(1),
            &Answer::<_, &str>::Accepts(1),
            any_reason,
            None,
        );
        assert_eq!(outcome, Outcome::Agree);
    }

    #[test]
    fn a_panic_is_never_a_refusal() {
        let answer = Answer::<(), &str>::Panics("index out of bounds".into());
        let outcome = judge(&refuses(), &answer, any_reason, Some(TRAILING));
        assert_eq!(outcome, Outcome::Panic("index out of bounds".into()));
        let mut tally = tally(Layer::Validation, &["zero denominator"]);
        tally.add(&whole("zero denominator"), &outcome);
        let counts = tally.counts(&whole("zero denominator")).unwrap();
        assert_eq!((counts.inputs, counts.agree, counts.panic), (1, 0, 1));
        assert_eq!(
            tally.failures(),
            ["validation: class \"zero denominator\" panics 1"]
        );
    }

    #[test]
    fn a_refusal_for_another_reason_disagrees() {
        let outcome = judge(
            &refuses(),
            &Answer::<(), _>::Refuses("fee"),
            no_reason,
            None,
        );
        assert_eq!(outcome, Outcome::Disagree(Disagreement::OtherReason));
    }

    #[test]
    fn an_allowed_class_passes_only_its_own_disagreement() {
        let accepts = judge(
            &refuses(),
            &Answer::<(), &str>::Accepts(()),
            any_reason,
            Some(TRAILING),
        );
        assert_eq!(accepts, Outcome::Allowed(TRAILING));
        let other = judge(
            &refuses(),
            &Answer::<(), _>::Refuses("fee"),
            no_reason,
            Some(TRAILING),
        );
        assert_eq!(other, Outcome::Disagree(Disagreement::OtherReason));
        let mut tally = tally(Layer::Decode, &["trailing"]);
        tally.add(&whole("trailing"), &accepts);
        assert!(tally.failures().is_empty());
        let counts = tally.counts(&whole("trailing")).unwrap();
        assert_eq!(counts.allowed[&TRAILING], 1);
        assert!(tally
            .to_string()
            .contains("1 trailing bytes (validation: phase_one_refuses_leftover)"));
    }

    #[test]
    fn a_difference_upstream_accepts_names_its_commit() {
        let outcome = judge(
            &refuses(),
            &Answer::<(), &str>::Accepts(()),
            any_reason,
            Some(EXTRA),
        );
        let mut tally = tally(Layer::Validation, &["generated extra"]);
        tally.add(&whole("generated extra"), &outcome);
        assert!(tally.failures().is_empty());
        assert!(tally.to_string().contains(
            "1 extra array elements (accepted on upstream main by a8516e16bb38621eeb86b83bef0e146e6123b648)"
        ));
    }

    #[test]
    #[should_panic(expected = "a commit is 40 lowercase hex digits")]
    fn a_short_commit_is_refused() {
        Commit::new("a8516e16");
    }

    #[test]
    fn an_agreeing_run_has_no_failure() {
        let mut tally = tally(Layer::Validation, &["base", "zero denominator"]);
        tally.add(&whole("base"), &Outcome::Agree);
        tally.add(&whole("zero denominator"), &Outcome::Agree);
        assert_eq!(tally.failures(), Vec::<String>::new());
    }

    #[test]
    fn a_declared_class_with_no_input_fails() {
        let mut tally = tally(Layer::Validation, &["base", "zero denominator"]);
        tally.add(&whole("base"), &Outcome::Agree);
        assert_eq!(
            tally.failures(),
            ["validation: class \"zero denominator\" has no judged observation"]
        );
    }

    #[test]
    fn a_class_with_only_inputs_not_judged_fails() {
        let mut tally = tally(Layer::Validation, &["base", "drop"]);
        tally.add(&whole("base"), &Outcome::Agree);
        tally.not_judged("drop");
        assert_eq!(tally.not_judged_count("drop"), Some(1));
        assert_eq!(
            tally.failures(),
            ["validation: class \"drop\" has no judged observation"]
        );
    }

    #[test]
    fn an_undeclared_class_fails() {
        let mut tally = tally(Layer::Validation, &["base"]);
        tally.add(&whole("base"), &Outcome::Agree);
        tally.add(&whole("bese"), &Outcome::Agree);
        assert_eq!(
            tally.failures(),
            ["validation: class \"bese\" is not declared, 1 inputs"]
        );
    }

    const RECORDS: &str = r#"{"reference":"9df9f98c","generator":"g1","layer":"validation","classes":["base","zero denominator"]}
{"class":"base","input":"a0","reference":{"accepts":{"fee":1}}}
{"class":"zero denominator","variant":"pool margin","input":"a1","reference":{"refuses":"Denominator cannot be zero"}}
"#;

    #[test]
    fn records_at_the_head_pin_are_read() {
        let records = Records::read(RECORDS, "9df9f98c").unwrap();
        assert_eq!(records.layer(), Layer::Validation);
        assert_eq!(records.classes(), ["base", "zero denominator"]);
        assert_eq!(records.rows().len(), 2);
        assert_eq!(records.rows()[1].variant.as_deref(), Some("pool margin"));
        assert_eq!(
            records.rows()[1].reference,
            Reference::Refuses("Denominator cannot be zero".into())
        );
    }

    #[test]
    fn records_at_another_pin_are_refused() {
        assert_eq!(
            Records::read(RECORDS, "1587f21a"),
            Err(RecordsError::PinDiffers {
                records: "9df9f98c".into(),
                head: "1587f21a".into()
            })
        );
    }

    #[test]
    fn the_rules_of_a_row_are_read() {
        let text = RECORDS.replace(
            r#""input":"a0","reference":{"accepts":{"fee":1}}"#,
            r#""input":"a0","reference":{"accepts":{"fee":1}},"rules":["coin","transaction_body"]"#,
        );
        let records = Records::read(&text, "9df9f98c").unwrap();
        assert_eq!(records.rows()[0].rules, ["coin", "transaction_body"]);
        assert!(records.rows()[1].rules.is_empty());
    }

    #[test]
    fn rules_that_are_not_names_are_refused() {
        let text = RECORDS.replace(
            r#""input":"a0","reference":{"accepts":{"fee":1}}"#,
            r#""input":"a0","reference":{"accepts":{"fee":1}},"rules":[1]"#,
        );
        assert!(matches!(
            Records::read(&text, "9df9f98c"),
            Err(RecordsError::Malformed { line: 2, .. })
        ));
    }

    #[test]
    fn a_row_with_both_answers_is_refused() {
        let text = RECORDS.replace(
            r#"{"accepts":{"fee":1}}"#,
            r#"{"accepts":{"fee":1},"refuses":"x"}"#,
        );
        assert!(matches!(
            Records::read(&text, "9df9f98c"),
            Err(RecordsError::Malformed { line: 2, .. })
        ));
    }

    #[test]
    fn the_encoding_of_an_accepted_row_is_read() {
        let text = RECORDS.replace(
            r#"{"accepts":{"fee":1}}"#,
            r#"{"accepts":{"fee":1},"encoding":"a10101"}"#,
        );
        let records = Records::read(&text, "9df9f98c").unwrap();
        assert_eq!(
            records.rows()[0].reference,
            Reference::Accepts(Accepted {
                dump: serde_json::json!({"fee": 1}),
                encoding: Some(vec![0xa1, 0x01, 0x01]),
            })
        );
        let plain = Records::read(RECORDS, "9df9f98c").unwrap();
        assert!(matches!(
            &plain.rows()[0].reference,
            Reference::Accepts(Accepted { encoding: None, .. })
        ));
    }

    #[test]
    fn an_encoding_beside_a_refusal_is_refused() {
        let text = RECORDS.replace(
            r#"{"refuses":"Denominator cannot be zero"}"#,
            r#"{"refuses":"Denominator cannot be zero","encoding":"a1"}"#,
        );
        assert_eq!(
            Records::read(&text, "9df9f98c"),
            Err(RecordsError::Malformed {
                line: 3,
                reason: "an encoding beside a refusal".into()
            })
        );
    }

    #[test]
    fn records_of_an_unknown_layer_are_refused() {
        let text = RECORDS.replace("\"validation\"", "\"valuation\"");
        assert!(matches!(
            Records::read(&text, "9df9f98c"),
            Err(RecordsError::Malformed { line: 1, .. })
        ));
    }

    const CHANGED: &str = r#"{"reference":"9df9f98c","generator":"g1","targets":[{"rule":"voter","kinds":["unknown-arm","array-short"]},{"rule":"transaction_body","kinds":["wide-heads"]}],"layer":"decode","classes":["body","body unknown-arm"]}
{"class":"body","input":"a0","reference":{"accepts":{"fee":1}}}
{"class":"body unknown-arm","variant":"80 5","input":"a1","reference":{"refuses":"unknown tag"},"site":"voting_procedures key","mutated":"voter","base":"a0"}
"#;

    #[test]
    fn the_change_of_a_row_is_read() {
        let records = Records::read(CHANGED, "9df9f98c").unwrap();
        let row = &records.rows()[1];
        assert_eq!(row.site.as_deref(), Some("voting_procedures key"));
        assert_eq!(row.mutated.as_deref(), Some("voter"));
        assert_eq!(row.base.as_deref(), Some(&[0xa0][..]));
        assert_eq!(row.change(), Some(("voter", "unknown-arm")));
        let plain = &records.rows()[0];
        assert_eq!((plain.base.as_deref(), plain.change()), (None, None));
    }

    #[test]
    fn a_base_that_is_not_hex_is_refused() {
        let text = CHANGED.replace(r#""base":"a0""#, r#""base":"ag""#);
        assert!(matches!(
            Records::read(&text, "9df9f98c"),
            Err(RecordsError::Malformed { line: 3, .. })
        ));
    }

    #[test]
    fn the_targets_of_records_are_read() {
        let records = Records::read(CHANGED, "9df9f98c").unwrap();
        assert_eq!(
            records.targets(),
            [
                Target {
                    rule: "voter".into(),
                    kinds: vec!["unknown-arm".into(), "array-short".into()]
                },
                Target {
                    rule: "transaction_body".into(),
                    kinds: vec!["wide-heads".into()]
                },
            ]
        );
        assert!(Records::read(RECORDS, "9df9f98c")
            .unwrap()
            .targets()
            .is_empty());
    }

    #[test]
    fn a_target_change_no_row_makes_fails() {
        let records = Records::read(CHANGED, "9df9f98c").unwrap();
        let coverage = Coverage::new(records.layer(), records.targets(), records.rows());
        assert_eq!(coverage.count("voter", "unknown-arm"), Some(1));
        assert_eq!(
            coverage.failures(&[("voter", "unknown-arm")]),
            [
                "decode: no input changes transaction_body by wide-heads",
                "decode: no input changes voter by array-short",
            ]
        );
    }

    #[test]
    fn targets_each_changed_pass() {
        let rows = [
            "\n{\"class\":\"body array-short\",\"input\":\"a1\",\"reference\":{\"refuses\":\"r\"},\"mutated\":\"voter\"}",
            "\n{\"class\":\"body wide-heads\",\"input\":\"a1\",\"reference\":{\"accepts\":{}},\"mutated\":\"transaction_body\"}",
        ];
        let records = Records::read(
            &(CHANGED.trim_end().to_owned() + rows[0] + rows[1]),
            "9df9f98c",
        )
        .unwrap();
        let coverage = Coverage::new(records.layer(), records.targets(), records.rows());
        assert_eq!(
            coverage.failures(&[("voter", "unknown-arm")]),
            Vec::<String>::new()
        );
        assert_eq!(
            coverage.to_string(),
            "decode: 3 target changes, 3 made, 0 not made\n"
        );
    }

    #[test]
    fn a_required_change_the_targets_lack_fails() {
        let records = Records::read(CHANGED, "9df9f98c").unwrap();
        let coverage = Coverage::new(records.layer(), records.targets(), records.rows());
        assert!(coverage
            .failures(&[("gov_action", "unknown-arm")])
            .contains(
                &"decode: the targets lack the required change gov_action by unknown-arm"
                    .to_owned()
            ));
    }

    #[test]
    fn the_cddl_pin_is_the_first_line_commit() {
        let cddl =
            "; Copied from cardano-ledger commit 9df9f98cce86c5f2195142b2c02dd9b39dd9dc7c,\n";
        assert_eq!(
            cddl_pin(cddl),
            Some("9df9f98cce86c5f2195142b2c02dd9b39dd9dc7c")
        );
        assert_eq!(cddl_pin("; no commit here\n"), None);
    }

    #[test]
    fn catch_returns_the_panic_message() {
        let caught = catch(|| -> u8 { panic!("boom") });
        assert_eq!(caught, Err("boom".to_owned()));
        assert_eq!(catch(|| 3), Ok(3));
    }
}
