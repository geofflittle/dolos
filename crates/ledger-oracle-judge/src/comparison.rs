//! One implementation layer compared with its reference on recorded inputs: the
//! records, the outcome of each input, the owner of each allowed difference and
//! the per class tally a run passes or fails by.

use std::{collections::BTreeMap, fmt};

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

/// The judgement of one input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Agree,
    Disagree(Disagreement),
    Panic(String),
    Allowed(Allowed),
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

/// The counts of one input class.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Counts {
    pub inputs: usize,
    pub agree: usize,
    pub disagree: BTreeMap<Disagreement, usize>,
    pub panic: usize,
    pub allowed: BTreeMap<Allowed, usize>,
    pub not_judged: usize,
}

/// The per class counts of one layer's run, over the classes its records
/// declare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tally {
    layer: Layer,
    classes: BTreeMap<String, Counts>,
    undeclared: BTreeMap<String, usize>,
}

impl Tally {
    /// An empty tally over `classes`, every one of which needs an input.
    pub fn new<S: AsRef<str>>(layer: Layer, classes: &[S]) -> Self {
        Tally {
            layer,
            classes: classes
                .iter()
                .map(|c| (c.as_ref().to_owned(), Counts::default()))
                .collect(),
            undeclared: BTreeMap::new(),
        }
    }

    /// Counts one judged input of `class`.
    pub fn add(&mut self, class: &str, outcome: &Outcome) {
        let Some(counts) = self.classes.get_mut(class) else {
            *self.undeclared.entry(class.to_owned()).or_default() += 1;
            return;
        };
        counts.inputs += 1;
        match outcome {
            Outcome::Agree => counts.agree += 1,
            Outcome::Disagree(d) => *counts.disagree.entry(*d).or_default() += 1,
            Outcome::Panic(_) => counts.panic += 1,
            Outcome::Allowed(class) => *counts.allowed.entry(*class).or_default() += 1,
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

    /// The counts of `class`, if the tally declares it.
    pub fn counts(&self, class: &str) -> Option<&Counts> {
        self.classes.get(class)
    }

    /// Each reason the run fails: a disagreement, a panic, a declared class
    /// with no input, or an input of a class the records do not declare.
    pub fn failures(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (class, c) in &self.classes {
            if c.inputs == 0 {
                out.push(format!("{}: class {class:?} has no input", self.layer));
            }
            for (d, n) in &c.disagree {
                out.push(format!(
                    "{}: class {class:?} disagrees ({}) {n}",
                    self.layer,
                    d.name()
                ));
            }
            if c.panic > 0 {
                out.push(format!(
                    "{}: class {class:?} panics {}",
                    self.layer, c.panic
                ));
            }
        }
        for (class, n) in &self.undeclared {
            out.push(format!(
                "{}: class {class:?} is not declared, {n} inputs",
                self.layer
            ));
        }
        out
    }
}

impl fmt::Display for Tally {
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
        for (class, c) in &self.classes {
            write!(
                f,
                "{:<10} {:<32} {:>6} {:>6}",
                self.layer.name(),
                class,
                c.inputs,
                c.agree
            )?;
            for d in Disagreement::ALL {
                write!(f, " {:>12}", c.disagree.get(&d).copied().unwrap_or(0))?;
            }
            let allowed: Vec<String> = c.allowed.iter().map(|(a, n)| format!("{n} {a}")).collect();
            writeln!(
                f,
                " {:>6} {:>10}  {}",
                c.panic,
                c.not_judged,
                allowed.join(", ")
            )?;
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
        Ok(())
    }
}

/// One recorded input and the reference's answer for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub class: String,
    pub variant: Option<String>,
    pub input: Vec<u8>,
    pub reference: Reference<Value>,
}

/// A record file whose reference pin equals the pin of the head under test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Records {
    pin: String,
    generator: String,
    layer: Layer,
    classes: Vec<String>,
    rows: Vec<Row>,
}

/// Why a record file cannot be read for the head under test.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RecordsError {
    #[error("records were written at reference pin {records}, the head vendors {head}")]
    PinDiffers { records: String, head: String },
    #[error("line {line}: {reason}")]
    Malformed { line: usize, reason: String },
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
        let mut rows = Vec::new();
        for (n, line) in lines {
            let v: Value = serde_json::from_str(line).map_err(|e| malformed(n, &e.to_string()))?;
            let text = |name: &str| v.get(name).and_then(Value::as_str);
            let class = text("class").ok_or_else(|| malformed(n, "row has no class"))?;
            let input = text("input")
                .and_then(|h| hex::decode(h).ok())
                .ok_or_else(|| malformed(n, "row has no hex input"))?;
            let reference = match v.get("reference") {
                Some(r) => match (r.get("accepts"), r.get("refuses")) {
                    (Some(dump), None) => Reference::Accepts(dump.clone()),
                    (None, Some(Value::String(reason))) => Reference::Refuses(reason.clone()),
                    _ => return Err(malformed(n, "reference is neither accepts nor refuses")),
                },
                None => return Err(malformed(n, "row has no reference")),
            };
            rows.push(Row {
                class: class.to_owned(),
                variant: text("variant").map(str::to_owned),
                input,
                reference,
            });
        }
        Ok(Records {
            pin: records_pin,
            generator: field("generator")?,
            layer: Layer::from_name(&field("layer")?)
                .ok_or_else(|| malformed(n, "header names no known layer"))?,
            classes,
            rows,
        })
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
        let mut tally = Tally::new(Layer::Validation, &["zero denominator"]);
        tally.add("zero denominator", &outcome);
        let counts = tally.counts("zero denominator").unwrap();
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
        let mut tally = Tally::new(Layer::Decode, &["trailing"]);
        tally.add("trailing", &accepts);
        assert!(tally.failures().is_empty());
        assert_eq!(tally.counts("trailing").unwrap().allowed[&TRAILING], 1);
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
        let mut tally = Tally::new(Layer::Validation, &["generated extra"]);
        tally.add("generated extra", &outcome);
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
        let mut tally = Tally::new(Layer::Validation, &["base", "zero denominator"]);
        tally.add("base", &Outcome::Agree);
        tally.add("zero denominator", &Outcome::Agree);
        assert_eq!(tally.failures(), Vec::<String>::new());
    }

    #[test]
    fn a_declared_class_with_no_input_fails() {
        let mut tally = Tally::new(Layer::Validation, &["base", "zero denominator"]);
        tally.add("base", &Outcome::Agree);
        assert_eq!(
            tally.failures(),
            ["validation: class \"zero denominator\" has no input"]
        );
    }

    #[test]
    fn a_class_with_only_inputs_not_judged_fails() {
        let mut tally = Tally::new(Layer::Validation, &["base", "drop"]);
        tally.add("base", &Outcome::Agree);
        tally.not_judged("drop");
        assert_eq!(tally.counts("drop").unwrap().not_judged, 1);
        assert_eq!(
            tally.failures(),
            ["validation: class \"drop\" has no input"]
        );
    }

    #[test]
    fn an_undeclared_class_fails() {
        let mut tally = Tally::new(Layer::Validation, &["base"]);
        tally.add("base", &Outcome::Agree);
        tally.add("bese", &Outcome::Agree);
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
    fn records_of_an_unknown_layer_are_refused() {
        let text = RECORDS.replace("\"validation\"", "\"valuation\"");
        assert!(matches!(
            Records::read(&text, "9df9f98c"),
            Err(RecordsError::Malformed { line: 1, .. })
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
