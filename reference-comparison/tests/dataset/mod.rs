//! Values of one era of the cardano-cbor-dataset read by pallas, each judged
//! against the ledger's verdict and re-encoding of it.

/// A rule read as the type `$t`.
macro_rules! read {
    ($t:ty) => {
        crate::dataset::Reader::Type(crate::dataset::RuleType {
            normalized: read_as!($t, crate::dataset::plain::whole),
            exact: read_as!(
                pallas_primitives::KeepRaw<'_, $t>,
                crate::dataset::plain::kept
            ),
        })
    };
}

/// The reading of an input as the type `$t`, whose value is written by
/// `$plain`.
macro_rules! read_as {
    ($t:ty, $plain:expr) => {{
        fn read(input: &[u8]) -> Result<crate::dataset::Reading, String> {
            let mut decoder = pallas_codec::minicbor::Decoder::new(input);
            let value: $t = decoder.decode().map_err(|e| e.to_string())?;
            let consumed = decoder.position();
            let encoding = pallas_codec::minicbor::to_vec(&value).map_err(|e| e.to_string());
            let plain = ($plain)(&value);
            let round_trip = match &encoding {
                Ok(bytes) => match pallas_codec::minicbor::decode::<$t>(bytes) {
                    Ok(again) if again == value => Ok(()),
                    Ok(again) => Err(crate::dataset::first_text_difference(
                        &format!("{value:?}"),
                        &format!("{again:?}"),
                    )),
                    Err(e) => Err(format!("its encoding is refused: {e}")),
                },
                Err(e) => Err(format!("it does not encode: {e}")),
            };
            Ok(crate::dataset::Reading {
                consumed,
                encoding,
                plain,
                round_trip,
            })
        }
        read as crate::dataset::Read
    }};
}

mod changes;
mod corpus;
pub mod differences;
pub mod plain;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use changes::{Change, Place, Reform, reform};
use corpus::{Encoding, GeneratorRule};
use differences::{Evidence, Known};
use ledger_oracle_judge::comparison::{
    self, Allowance, Allowed, Answer, ComparedLayer, Expected, Key, Layer, LayerRun, Outcome,
    Owner, Records, Reference, Row, catch, kinds_from_env,
};
use ledger_oracle_judge::normalize::normalize;
use ledger_oracle_judge::parallel::in_parallel;

/// The dataset submodule, relative to the Dolos tree.
const SUBMODULE: &str = "test_data/cardano-cbor-dataset";

/// What pallas makes of one input: how much of it the value took, the
/// encoding pallas writes at the rule's position, the encoding of the value
/// inside any bytes kept as read there and whether the written encoding reads
/// back as the value.
pub struct Reading {
    pub consumed: usize,
    pub encoding: Result<Vec<u8>, String>,
    pub plain: Result<Vec<u8>, String>,
    pub round_trip: Result<(), String>,
}

/// The pallas reading of a rule's input.
pub type Read = fn(&[u8]) -> Result<Reading, String>;

/// How pallas reads a rule whose re-encoding is compared after the dataset's
/// normalisation, and how behind `KeepRaw` for one compared byte for byte.
#[derive(Clone, Copy)]
pub struct RuleType {
    pub normalized: Read,
    pub exact: Read,
}

impl RuleType {
    /// The reading for a rule compared as `encoding`.
    fn read(&self, encoding: Encoding) -> Read {
        match encoding {
            Encoding::Normalized => self.normalized,
            Encoding::Exact => self.exact,
        }
    }
}

/// The pallas type a rule's table entry names, or why pallas has none.
#[derive(Clone, Copy)]
pub enum Reader {
    Type(RuleType),
    NoType { why: &'static str },
    Owned { why: &'static str, owner: Owner },
}

/// An era of the corpus: its directory, the generator's rule table for it,
/// how pallas reads each rule and the differences the run lets pass.
pub struct Era {
    pub dir: &'static str,
    pub generator_table: &'static str,
    pub rules: &'static [(&'static str, Reader)],
    pub differences: &'static [Known],
}

/// How pallas reads a rule and how its re-encoding is compared.
type Rule = (Encoding, Read);

/// Text cut to a length a report line can hold.
pub fn shortened(text: String) -> String {
    const LONGEST: usize = 400;
    match text.char_indices().nth(LONGEST) {
        Some((at, _)) => format!("{}... ({} bytes)", &text[..at], text.len()),
        None => text,
    }
}

/// Where two value texts first differ, with some text of each from there.
pub fn first_text_difference(read: &str, again: &str) -> String {
    let at = read
        .char_indices()
        .zip(again.chars())
        .find(|((_, a), b)| a != b)
        .map_or(read.len().min(again.len()), |((i, _), _)| i);
    let from = |s: &str| {
        let start = s.floor_char_boundary(at.saturating_sub(80));
        let end = s.ceil_char_boundary((at + 160).min(s.len()));
        s[start..end].to_owned()
    };
    format!(
        "read as ...{}... reads back as ...{}...",
        from(read),
        from(again)
    )
}

/// What tells apart the observations of one sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Axis {
    /// Whether the sample is accepted.
    Verdict,
    /// Whether the value's encoding reads back as the value.
    RoundTrip,
    /// Whether the value's encoding is the ledger's.
    Bytes,
}

impl fmt::Display for Axis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Axis::Verdict => "verdict",
            Axis::RoundTrip => "round trip",
            Axis::Bytes => "bytes",
        })
    }
}

/// An observation of an accepted sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    /// The sample reads as a value.
    Read,
    /// The value or its encoding is the one the axis asks for.
    Same,
    /// The value or its encoding differs, as the text says.
    Differs(String),
}

/// One sample as pallas reads it.
pub struct Sample {
    rule: Rule,
    input: Vec<u8>,
    expected: Option<Vec<u8>>,
}

/// An era's rules that pallas has a type for, for each the normal form of the
/// ledger's encodings of the samples it accepts, the forms the ledger writes
/// embedded encodings in, and the era's differences the run lets pass.
struct Dataset {
    rules: BTreeMap<String, Rule>,
    accepted: BTreeMap<String, BTreeSet<Vec<u8>>>,
    ledger_forms: LedgerForms,
    differences: &'static [Known],
}

impl Dataset {
    fn rule(&self, row: &Row) -> Rule {
        let name = row.class.split(' ').next().unwrap_or_default();
        *self
            .rules
            .get(name)
            .unwrap_or_else(|| panic!("no pallas type reads {name}"))
    }

    fn evidence(&self, row: &Row, key: &Key<Axis>) -> Option<Evidence> {
        let (_, read) = self.rule(row);
        let input = row.input.clone();
        let reading = catch(AssertUnwindSafe(move || read(&input))).ok()?;
        let not_read = || Err("the sample is not read".to_owned());
        let mut evidence = Evidence {
            axis: key.axis,
            refused_by_ledger: matches!(row.reference, Reference::Refuses(_)),
            refusal: None,
            refused_byte: None,
            trailing: 0,
            stopped_inside: false,
            changes: not_read(),
            ledger_changes: not_read(),
            written_as_read: false,
            round_trip: Err("the sample is not read".to_owned()),
            same_as_accepted: false,
        };
        let reading = match reading {
            Ok(reading) => reading,
            Err(refusal) => {
                evidence.refused_byte =
                    refused_at(&refusal).and_then(|at| row.input.get(at).copied());
                evidence.refusal = Some(refusal);
                return Some(evidence);
            }
        };
        let normal = |bytes: &Result<Vec<u8>, String>| {
            let bytes = bytes
                .as_ref()
                .map_err(|e| format!("it does not encode: {e}"))?;
            normalize(bytes).map_err(|e| format!("no normal form: {e}"))
        };
        let value = normal(&reading.plain);
        let read = normal(&Ok(row.input[..reading.consumed].to_vec()));
        evidence.trailing = row.input.len() - reading.consumed;
        evidence.stopped_inside = stopped_inside(&row.input, reading.consumed);
        evidence.written_as_read = matches!(
            (&read, normal(&reading.encoding)),
            (Ok(read), Ok(written)) if *read == written
        );
        evidence.round_trip = reading.round_trip.clone();
        evidence.changes = changes_between(&read, &value).map(|c| self.ledger_forms.told(c));
        if let Reference::Accepts(accepted) = &row.reference {
            let ledger = accepted
                .encoding
                .as_deref()
                .ok_or_else(|| "no ledger encoding".to_owned())
                .and_then(ledger_normal);
            evidence.ledger_changes = changes_between(&ledger, &normal(&reading.encoding));
        }
        let accepted = self.accepted.get(key.value_kind());
        evidence.same_as_accepted = value.is_ok_and(|n| accepted.is_some_and(|a| a.contains(&n)));
        Some(evidence)
    }

    /// What the evidence for one observation shows, for a report line.
    fn evidence_detail(&self, row: &Row, axis: Axis) -> String {
        let key = Key {
            class: row.class.clone(),
            axis,
        };
        match self.evidence(row, &key) {
            None => " (no evidence: the reading panics)".to_owned(),
            Some(e) => format!(
                " (changes from the bytes read {:?}, from the ledger's {:?}, {} trailing)",
                e.changes, e.ledger_changes, e.trailing
            ),
        }
    }
}

/// Whether the bytes read end inside an item of an input that is one whole
/// item.
fn stopped_inside(input: &[u8], consumed: usize) -> bool {
    let whole = |bytes: &[u8]| {
        let mut d = pallas_codec::minicbor::Decoder::new(bytes);
        d.skip().is_ok() && d.position() == bytes.len()
    };
    consumed < input.len() && whole(input) && !whole(&input[..consumed])
}

/// The position a decoder's refusal names.
fn refused_at(refusal: &str) -> Option<usize> {
    let (_, rest) = refusal.split_once("position ")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Each way the ledger writes an embedded encoding in another form than the
/// one it reads, by the place of the embedded encoding, from the samples it
/// accepts.
struct LedgerForms(BTreeSet<(Place, Reform)>);

impl LedgerForms {
    /// The forms in the ledger's encodings of the accepted samples, each
    /// given as its input beside the ledger's encoding.
    fn learned<'a>(accepted: impl IntoIterator<Item = (&'a [u8], &'a [u8])>) -> Self {
        let mut forms = BTreeSet::new();
        for (input, expected) in accepted {
            let input = normalize(input).map_err(|e| e.to_string());
            let found = changes_between(&input, &ledger_normal(expected));
            for change in found.into_iter().flatten() {
                if let Change::EmbeddedForm { place, from, to } = change {
                    forms.extend(reform(&from, &to).map(|r| (place, r)));
                }
            }
        }
        LedgerForms(forms)
    }

    /// The changes with each embedded encoding written as the ledger writes
    /// one in that place told as the ledger's form.
    fn told(&self, changes: BTreeSet<Change>) -> BTreeSet<Change> {
        changes
            .into_iter()
            .map(|change| match change {
                Change::EmbeddedForm { place, from, to }
                    if reform(&from, &to).is_some_and(|r| self.0.contains(&(place.clone(), r))) =>
                {
                    Change::EmbeddedLedgerForm
                }
                change => change,
            })
            .collect()
    }
}

/// The normal form of the ledger's encoding of a sample.
fn ledger_normal(expected: &[u8]) -> Result<Vec<u8>, String> {
    normalize(expected).map_err(|e| format!("the ledger's encoding has no normal form: {e}"))
}

/// The changes that turn the first normal form encoding into the second.
fn changes_between(
    from: &Result<Vec<u8>, String>,
    to: &Result<Vec<u8>, String>,
) -> Result<BTreeSet<Change>, String> {
    let items =
        |bytes: &Result<Vec<u8>, String>| changes::items(bytes.as_ref().map_err(Clone::clone)?);
    changes::changes(&items(from)?, &items(to)?)
}

/// Where two encodings first differ, with a few bytes of each from there.
fn first_difference((a_name, a): (&str, &[u8]), (b_name, b): (&str, &[u8])) -> String {
    let at = a
        .iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .unwrap_or(a.len().min(b.len()));
    let window = |bytes: &[u8]| hex::encode(&bytes[at..bytes.len().min(at + 16)]);
    format!(
        "{a_name} {} bytes, {b_name} {} bytes, first difference at {at}: {a_name} {} {b_name} {}",
        a.len(),
        b.len(),
        window(a),
        window(b)
    )
}

fn bytes_seen(encoding: Encoding, pallas: &Result<Vec<u8>, String>, expected: &[u8]) -> Seen {
    let (pallas, expected) = match (encoding, pallas) {
        (_, Err(e)) => return Seen::Differs(format!("it does not encode: {e}")),
        (Encoding::Exact, Ok(bytes)) => (bytes.clone(), expected.to_vec()),
        (Encoding::Normalized, Ok(bytes)) => match (normalize(bytes), ledger_normal(expected)) {
            (Ok(pallas), Ok(expected)) => (pallas, expected),
            (Err(e), _) => {
                return Seen::Differs(format!("its encoding has no normal form: {e}"));
            }
            (_, Err(e)) => return Seen::Differs(e),
        },
    };
    match pallas == expected {
        true => Seen::Same,
        false => Seen::Differs(first_difference(("ledger", &expected), ("pallas", &pallas))),
    }
}

impl ComparedLayer for Dataset {
    const LAYER: Layer = Layer::Decode;
    const TIME_LIMIT: Duration = Duration::from_secs(60);
    type Input = Sample;
    type Axis = Axis;
    type Value = Seen;
    type Refusal = String;

    fn input(&self, row: &Row) -> Sample {
        Sample {
            rule: self.rule(row),
            input: row.input.clone(),
            expected: match &row.reference {
                Reference::Accepts(accepted) => accepted.encoding.clone(),
                Reference::Refuses(_) => None,
            },
        }
    }

    fn reference(&self, row: &Row) -> Expected<Axis, Seen> {
        match &row.reference {
            Reference::Accepts(_) => Expected::Judged {
                first: (Axis::Verdict, Reference::Accepts(Seen::Read)),
                rest: vec![
                    (Axis::RoundTrip, Reference::Accepts(Seen::Same)),
                    (Axis::Bytes, Reference::Accepts(Seen::Same)),
                ],
            },
            Reference::Refuses(kind) => {
                Expected::one(Axis::Verdict, Reference::Refuses(kind.clone()))
            }
        }
    }

    fn run(&self, sample: &Sample) -> Vec<(Axis, Result<Seen, String>)> {
        let (encoding, read) = sample.rule;
        match (read(&sample.input), &sample.expected) {
            (Err(e), None) => vec![(Axis::Verdict, Err(e))],
            (Err(e), Some(_)) => vec![
                (Axis::Verdict, Err(e.clone())),
                (Axis::RoundTrip, Err(e.clone())),
                (Axis::Bytes, Err(e)),
            ],
            (Ok(_), None) => vec![(Axis::Verdict, Ok(Seen::Read))],
            (Ok(reading), Some(expected)) => vec![
                (Axis::Verdict, Ok(Seen::Read)),
                (
                    Axis::RoundTrip,
                    Ok(match reading.round_trip {
                        Ok(()) => Seen::Same,
                        Err(how) => Seen::Differs(how),
                    }),
                ),
                (
                    Axis::Bytes,
                    Ok(bytes_seen(encoding, &reading.encoding, expected)),
                ),
            ],
        }
    }

    fn same_reason(&self, _: &str, _: &String) -> bool {
        true
    }

    fn allowance(&self, row: &Row, key: &Key<Axis>) -> Option<Allowance<Seen, String>> {
        let evidence = self.evidence(row, key)?;
        self.differences
            .iter()
            .find(|known| (known.shows)(&evidence))
            .map(|known| Allowance {
                allowed: known.allowance.allowed,
                shown: known.allowance.shown,
            })
    }

    fn detail(
        &self,
        row: &Row,
        reference: &Reference<Seen>,
        answer: &Answer<Seen, String>,
    ) -> String {
        let variant = row.variant.as_deref().unwrap_or("");
        let input = shortened(hex::encode(&row.input));
        let evidence = match (reference, answer) {
            (Reference::Refuses(_), Answer::Accepts(_)) => self.evidence_detail(row, Axis::Verdict),
            (Reference::Accepts(_), Answer::Accepts(Seen::Differs(_))) => {
                self.evidence_detail(row, Axis::Bytes)
            }
            _ => String::new(),
        };
        format!("{variant}: ledger {reference:?}, pallas {answer:?}{evidence}, input {input}")
    }
}

/// The counts of one rule's run.
#[derive(Default)]
struct RuleCounts {
    samples: usize,
    observations: usize,
    agree: usize,
    allowed: BTreeMap<String, usize>,
    failed: usize,
}

fn tree() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The rules the generator, the corpus and the pallas table name, refused
/// unless all three name the same rules.
fn checked_rules(
    generator: &[GeneratorRule],
    dirs: &[String],
    table: &[(&'static str, Reader)],
) -> Vec<String> {
    let mut failures = Vec::new();
    let names = |it: &mut dyn Iterator<Item = String>| it.collect::<Vec<_>>();
    let generated = names(&mut generator.iter().map(|r| r.name.clone()));
    let tabled = names(&mut table.iter().map(|(n, _)| n.to_string()));
    for (what, list) in [("generator", &generated), ("pallas table", &tabled)] {
        let unique: BTreeSet<&String> = list.iter().collect();
        if unique.len() != list.len() {
            failures.push(format!("the {what} names a rule twice"));
        }
    }
    let sets = [
        ("generator", generated.iter().collect::<BTreeSet<_>>()),
        ("corpus", dirs.iter().collect()),
        ("pallas table", tabled.iter().collect()),
    ];
    for (a, sa) in &sets {
        for (b, sb) in &sets {
            for name in sa.difference(sb) {
                failures.push(format!("the {a} names rule {name}, the {b} does not"));
            }
        }
    }
    failures
}

/// A failure for each tabled rule that pallas has no type for and nothing
/// owns.
fn untyped(table: &[(&'static str, Reader)]) -> Vec<String> {
    table
        .iter()
        .filter_map(|(rule, reader)| match reader {
            Reader::Type(_) | Reader::Owned { .. } => None,
            Reader::NoType { why } => Some(format!("rule {rule} has no pallas type: {why}")),
        })
        .collect()
}

/// Why pallas has no type for a rule, with the owner that accepts that.
fn no_type(reader: &Reader) -> Option<String> {
    match reader {
        Reader::Type(_) => None,
        Reader::NoType { why } => Some((*why).to_owned()),
        Reader::Owned { why, owner } => Some(format!("{why}, {owner}")),
    }
}

/// The ledger commit the corpus report of an era names as the
/// implementation that wrote the expected answers.
fn ledger_commit(dataset: &Path, era: &str) -> Result<String, String> {
    let path = dataset.join(format!("dataset/reports/{era}/haskell-latest.json"));
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let report: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    report["implementation_commit"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{}: no implementation_commit", path.display()))
}

fn run(era: &Era) {
    let pin = corpus::checked_out_pin(&tree(), SUBMODULE).unwrap_or_else(|e| panic!("{e}"));
    let dataset = tree().join(SUBMODULE);
    let source = std::fs::read_to_string(dataset.join("app/Generation/EraSpec.hs"))
        .unwrap_or_else(|e| panic!("the generator source: {e}"));
    let generator =
        corpus::generator_rules(&source, era.generator_table).unwrap_or_else(|e| panic!("{e}"));
    let ledger = ledger_commit(&dataset, era.dir).unwrap_or_else(|e| panic!("{e}"));
    println!(
        "cardano-cbor-dataset {pin}, {} written by ledger {ledger}",
        era.dir
    );
    let era_dir = dataset.join("dataset").join(era.dir);
    let dirs = corpus::rule_dirs(&era_dir).unwrap_or_else(|e| panic!("{e}"));
    let mut failures = checked_rules(&generator, &dirs, era.rules);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    failures.extend(untyped(era.rules));

    let records: Vec<(String, Records)> = dirs
        .iter()
        .map(|rule| {
            let (classes, rows) =
                corpus::rule_rows(rule, &era_dir.join(rule)).unwrap_or_else(|e| panic!("{e}"));
            let generator = format!("cardano-cbor-dataset {pin} dataset/{}/{rule}", era.dir);
            let records = Records::new(pin.clone(), generator, Layer::Decode, classes, rows);
            (rule.clone(), records)
        })
        .collect();
    let mut accepted: BTreeMap<String, BTreeSet<Vec<u8>>> = BTreeMap::new();
    for (rule, records) in &records {
        for row in records.rows() {
            if let Reference::Accepts(a) = &row.reference {
                let encoding = a
                    .encoding
                    .as_deref()
                    .expect("an accepted sample has its encoding");
                let normal = ledger_normal(encoding).unwrap_or_else(|e| panic!("{rule}: {e}"));
                accepted.entry(rule.clone()).or_default().insert(normal);
            }
        }
    }
    let ledger_forms = LedgerForms::learned(records.iter().flat_map(|(_, records)| {
        records
            .rows()
            .iter()
            .filter_map(|row| match &row.reference {
                Reference::Accepts(a) => Some((&row.input[..], a.encoding.as_deref()?)),
                Reference::Refuses(_) => None,
            })
    }));
    let readers: BTreeMap<&str, Reader> = era.rules.iter().copied().collect();
    let layer = Arc::new(Dataset {
        rules: generator
            .iter()
            .filter_map(|r| match readers[r.name.as_str()] {
                Reader::Type(t) => Some((r.name.clone(), (r.encoding, t.read(r.encoding)))),
                Reader::NoType { .. } | Reader::Owned { .. } => None,
            })
            .collect(),
        accepted,
        ledger_forms,
        differences: era.differences,
    });
    let kinds = kinds_from_env();
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let indices: Vec<usize> = (0..records.len())
        .filter(|i| layer.rules.contains_key(&records[*i].0))
        .collect();
    let runs: Vec<LayerRun<'_, Axis>> = in_parallel(
        &indices,
        threads,
        |i| records[*i].1.rows().iter().map(|row| row.input.len()).sum(),
        |i| comparison::compare(&layer, &records[*i].1, &kinds),
    );
    let run_of: BTreeMap<usize, &LayerRun<'_, Axis>> = indices.iter().copied().zip(&runs).collect();

    let mut used: BTreeMap<Allowed, usize> = BTreeMap::new();
    println!(
        "{:<28} {:>7} {:>12} {:>7} {:>7} {:>7}  allowed with owner",
        "rule", "samples", "observations", "agree", "allowed", "failed"
    );
    for (i, (rule, records)) in records.iter().enumerate() {
        let mut counts = RuleCounts {
            samples: records.rows().len(),
            ..RuleCounts::default()
        };
        if counts.samples == 0 {
            failures.push(format!("rule {rule} has no sample"));
        }
        let Some(run) = run_of.get(&i) else {
            let Some(why) = no_type(&readers[rule.as_str()]) else {
                continue;
            };
            println!(
                "{rule:<28} {:>7} {:>12} {:>7} {:>7} {:>7}  no pallas type: {why}",
                counts.samples, "-", "-", "-", "-"
            );
            continue;
        };
        for j in &run.judgements {
            counts.observations += 1;
            match &j.outcome {
                Outcome::Agree => counts.agree += 1,
                Outcome::Allowed(a) => {
                    *used.entry(*a).or_default() += 1;
                    *counts.allowed.entry(a.to_string()).or_default() += 1;
                }
                _ => counts.failed += 1,
            }
        }
        let allowed: Vec<String> = counts
            .allowed
            .iter()
            .map(|(a, n)| format!("{n} {a}"))
            .collect();
        println!(
            "{rule:<28} {:>7} {:>12} {:>7} {:>7} {:>7}  {}",
            counts.samples,
            counts.observations,
            counts.agree,
            counts.allowed.values().sum::<usize>(),
            counts.failed,
            allowed.join(", ")
        );
        failures.extend(run.failures());
    }
    for run in &runs {
        for j in &run.judgements {
            if !matches!(j.outcome, Outcome::Agree | Outcome::Allowed(_)) {
                println!("{} {:?}: {}", j.key, j.outcome, j.detail);
            }
        }
    }
    failures.extend(unused_differences(era.differences, &used, kinds.is_empty()));
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Each listed difference that fails the run: one listed twice, or, in a run
/// of every kind, one no observation was allowed by.
fn unused_differences(
    list: &[Known],
    used: &BTreeMap<Allowed, usize>,
    every_kind: bool,
) -> Vec<String> {
    let mut failures = Vec::new();
    let mut seen = BTreeSet::new();
    for known in list {
        let allowed = known.allowance.allowed;
        if !seen.insert(allowed) {
            failures.push(format!("{allowed} is listed twice"));
        }
        if every_kind && !used.contains_key(&allowed) {
            failures.push(format!("{allowed} matched no observation"));
        }
    }
    failures
}

/// Runs an era on a thread whose stack holds pallas's deepest reading.
pub fn run_on_large_stack(era: &'static Era) {
    let outcome = std::thread::Builder::new()
        .stack_size(1 << 28)
        .spawn(|| run(era))
        .expect("spawn")
        .join();
    if let Err(e) = outcome {
        std::panic::resume_unwind(e);
    }
}

#[cfg(test)]
fn table(names: &[&'static str]) -> Vec<(&'static str, Reader)> {
    names.iter().map(|n| (*n, read!(u64))).collect()
}

#[cfg(test)]
fn generated(names: &[&str], encoding: Encoding) -> Vec<GeneratorRule> {
    names
        .iter()
        .map(|n| GeneratorRule {
            name: n.to_string(),
            encoding,
        })
        .collect()
}

#[test]
fn rules_named_alike_everywhere_pass() {
    let failures = checked_rules(
        &generated(&["coin", "slot"], Encoding::Normalized),
        &["coin".to_owned(), "slot".to_owned()],
        &table(&["coin", "slot"]),
    );
    assert_eq!(failures, Vec::<String>::new());
}

#[test]
fn a_rule_the_table_lacks_fails() {
    let failures = checked_rules(
        &generated(&["coin", "slot"], Encoding::Normalized),
        &["coin".to_owned(), "slot".to_owned()],
        &table(&["coin"]),
    );
    assert_eq!(
        failures,
        [
            "the generator names rule slot, the pallas table does not",
            "the corpus names rule slot, the pallas table does not",
        ]
    );
}

#[test]
fn a_rule_directory_the_generator_lacks_fails() {
    let failures = checked_rules(
        &generated(&["coin"], Encoding::Normalized),
        &["coin".to_owned(), "slot".to_owned()],
        &table(&["coin", "slot"]),
    );
    assert_eq!(
        failures,
        [
            "the corpus names rule slot, the generator does not",
            "the pallas table names rule slot, the generator does not",
        ]
    );
}

#[test]
fn a_rule_tabled_twice_fails() {
    let failures = checked_rules(
        &generated(&["coin"], Encoding::Normalized),
        &["coin".to_owned()],
        &table(&["coin", "coin"]),
    );
    assert_eq!(failures, ["the pallas table names a rule twice"]);
}

#[test]
fn the_generator_table_is_read_with_its_encodings() {
    let source = "conwayRules :: [GenerationRule]\nconwayRules =\n  let version = eraProtVerHigh @ConwayEra\n   in [ mkAnnotatorRule @(Block (Header StandardCrypto) ConwayEra) version \"block\",\n        mkByteExactRule @(TxBody TopTx ConwayEra) version \"transaction_body\",\n        -- a comment\n        mkPositiveMultiAssetRule version \"%multiasset<positive_coin>\",\n        mkRule @Coin version \"coin\"\n      ]\n\ndijkstraRules =\n   in [ mkRule @Coin version \"other\"\n";
    let rules = corpus::generator_rules(source, "conwayRules").expect("read");
    let read: Vec<(&str, Encoding)> = rules
        .iter()
        .map(|r| (r.name.as_str(), r.encoding))
        .collect();
    assert_eq!(
        read,
        [
            ("block", Encoding::Normalized),
            ("transaction_body", Encoding::Exact),
            ("%multiasset<positive_coin>", Encoding::Normalized),
            ("coin", Encoding::Normalized),
        ]
    );
}

#[test]
fn the_dijkstra_generator_table_is_read_with_its_encodings() {
    let source = "conwayRules =\n   in [ mkRule @Coin version \"other\"\n      ]\n\ndijkstraRules :: [GenerationRule]\ndijkstraRules =\n  let version = eraProtVerHigh @DijkstraEra\n   in [ -- A comment\n        -- over two lines.\n        mkByteExactAnnotatorRule @(Leios.Header StandardCrypto) version \"header\",\n        mkRule @(Hash HASH EraIndependentTx, Word32) version \"eb_reference\",\n        mkSubTransactionsRule version \"sub_transactions\",\n        mkRequiredTopLevelGuardsRule version \"required_top_level_guards\",\n        mkGuardsRule version \"guards\",\n        mkByteExactRule @(TxBody TopTx DijkstraEra) version \"transaction_body\"\n      ]\n\n-- | Next\neraSpecName :: EraSpec -> Text\n";
    let rules = corpus::generator_rules(source, "dijkstraRules").expect("read");
    let read: Vec<(&str, Encoding)> = rules
        .iter()
        .map(|r| (r.name.as_str(), r.encoding))
        .collect();
    assert_eq!(
        read,
        [
            ("header", Encoding::Exact),
            ("eb_reference", Encoding::Normalized),
            ("sub_transactions", Encoding::Normalized),
            ("required_top_level_guards", Encoding::Normalized),
            ("guards", Encoding::Normalized),
            ("transaction_body", Encoding::Exact),
        ]
    );
}

#[test]
fn an_unknown_generator_constructor_fails() {
    let source = "conwayRules =\n   in [ mkStrangeRule @Coin version \"coin\"\n      ]\n";
    let error = corpus::generator_rules(source, "conwayRules").expect_err("an unknown constructor");
    assert!(error.contains("does not know"), "{error}");
}

#[test]
fn a_source_with_no_table_of_the_name_fails() {
    let source = "dijkstraRules =\n   in [ mkRule @Coin version \"coin\"\n      ]\n";
    assert!(corpus::generator_rules(source, "conwayRules").is_err());
    assert!(corpus::generator_rules(source, "dijkstraRules").is_ok());
}

#[test]
fn a_typed_table_has_no_untyped_rule() {
    assert_eq!(untyped(&table(&["coin", "slot"])), Vec::<String>::new());
}

#[test]
fn a_rule_with_no_pallas_type_is_tabled_and_fails() {
    let mut rules = table(&["coin"]);
    rules.push((
        "slot",
        Reader::NoType {
            why: "no type reads it",
        },
    ));
    assert_eq!(
        checked_rules(
            &generated(&["coin", "slot"], Encoding::Normalized),
            &["coin".to_owned(), "slot".to_owned()],
            &rules,
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        untyped(&rules),
        ["rule slot has no pallas type: no type reads it"]
    );
}

#[test]
fn a_rule_with_no_pallas_type_and_an_owner_is_tabled_and_passes() {
    let mut rules = table(&["coin"]);
    rules.push((
        "slot",
        Reader::Owned {
            why: "pallas reads slots in another form",
            owner: Owner::Decision {
                date: "2026-10-02",
                words: "keep the other form",
            },
        },
    ));
    assert_eq!(
        checked_rules(
            &generated(&["coin", "slot"], Encoding::Normalized),
            &["coin".to_owned(), "slot".to_owned()],
            &rules,
        ),
        Vec::<String>::new()
    );
    assert_eq!(untyped(&rules), Vec::<String>::new());
    assert_eq!(
        no_type(&rules[1].1).as_deref(),
        Some("pallas reads slots in another form, decision of 2026-10-02: \"keep the other form\"")
    );
}

#[test]
fn a_class_directory_that_is_no_sample_kind_fails() {
    assert_eq!(corpus::SampleKind::from_dir("invalid-zap-x"), None);
    assert_eq!(corpus::SampleKind::from_dir("unknown"), None);
    assert_eq!(
        corpus::SampleKind::from_dir("invalid-zap-3"),
        Some(corpus::SampleKind::Zap(3))
    );
}

#[test]
fn bytes_read_that_end_inside_an_item_are_told() {
    let input = hex::decode("820600").expect("hex");
    assert!(stopped_inside(&input, 2));
    assert!(!stopped_inside(&input, 3));
    let two = hex::decode("0600").expect("hex");
    assert!(!stopped_inside(&two, 1));
    let indefinite = hex::decode("bf0001ff").expect("hex");
    assert!(stopped_inside(&indefinite, 2));
}

#[test]
fn the_position_a_refusal_names_is_read() {
    assert_eq!(
        refused_at("decode error at position 41: flat enum requires definite-length array"),
        Some(41)
    );
    assert_eq!(
        refused_at("unexpected type tag at position 3: expected u64"),
        Some(3)
    );
    assert_eq!(refused_at("end of input bytes"), None);
}

#[test]
fn a_missing_corpus_fails() {
    let missing = Path::new(env!("CARGO_MANIFEST_DIR")).join("no-such-corpus");
    assert!(corpus::rule_dirs(&missing).is_err());
}

#[test]
fn an_exact_encoding_is_compared_byte_for_byte() {
    let read = vec![0x82, 0x01, 0x02];
    assert_eq!(
        bytes_seen(Encoding::Exact, &Ok(read.clone()), &read),
        Seen::Same
    );
    assert!(matches!(
        bytes_seen(Encoding::Exact, &Ok(vec![0x9f, 0x01, 0x02, 0xff]), &read),
        Seen::Differs(_)
    ));
}

#[test]
fn a_normalized_encoding_is_compared_after_normalisation() {
    let definite = vec![0x82, 0x01, 0x02];
    let indefinite = vec![0x9f, 0x01, 0x02, 0xff];
    assert_eq!(
        bytes_seen(Encoding::Normalized, &Ok(indefinite), &definite),
        Seen::Same
    );
    assert!(matches!(
        bytes_seen(Encoding::Normalized, &Ok(vec![0x82, 0x01, 0x03]), &definite),
        Seen::Differs(_)
    ));
}

#[cfg(test)]
fn a_ledger_writing_key_3_definite() -> LedgerForms {
    let (input, expected) = (bytes_of("a103d818449f0102ff"), bytes_of("a103d81843820102"));
    LedgerForms::learned([(&input[..], &expected[..])])
}

#[cfg(test)]
fn embedded_form(place: Place, from: &str, to: &str) -> Change {
    Change::EmbeddedForm {
        place,
        from: bytes_of(from),
        to: bytes_of(to),
    }
}

#[test]
fn an_embedded_form_the_ledger_is_not_seen_writing_stays_told() {
    let ledger = a_ledger_writing_key_3_definite();
    let key_3 = Place::under(&changes::Item::Leaf(vec![0x03]));
    let datum = Place::under(&changes::Item::Leaf(vec![0x02])).at(1);
    for other in [
        embedded_form(key_3.clone(), "9f0102ff", "98020102"),
        embedded_form(datum, "9f0102ff", "820102"),
        embedded_form(key_3.clone(), "9f9f01ff02ff", "82810102"),
    ] {
        assert_eq!(ledger.told([other.clone()].into()), [other].into());
    }
    let nothing = LedgerForms::learned([]);
    let same = embedded_form(key_3, "9f0102ff", "820102");
    assert_eq!(nothing.told([same.clone()].into()), [same].into());
}

#[test]
fn an_embedded_form_the_ledger_writes_in_that_place_is_told_as_the_ledger_form() {
    let ledger = a_ledger_writing_key_3_definite();
    let key_3 = Place::under(&changes::Item::Leaf(vec![0x03]));
    for same in [
        embedded_form(key_3.clone(), "9f0102ff", "820102"),
        embedded_form(key_3, "9f0103ff", "820103"),
    ] {
        assert_eq!(
            ledger.told([same, Change::EntriesReordered].into()),
            [Change::EmbeddedLedgerForm, Change::EntriesReordered].into()
        );
    }
}

#[cfg(test)]
fn bytes_of(text: &str) -> Vec<u8> {
    hex::decode(text).expect("hex")
}

#[test]
fn another_rational_differs_from_the_ledger_unreduced_one() {
    let two_fourths = hex::decode("d81e820204").expect("hex");
    let three_fourths = hex::decode("d81e820304").expect("hex");
    let one_half = hex::decode("d81e820102").expect("hex");
    assert!(matches!(
        bytes_seen(Encoding::Normalized, &Ok(three_fourths), &two_fourths),
        Seen::Differs(_)
    ));
    assert!(matches!(
        bytes_seen(Encoding::Exact, &Ok(one_half), &two_fourths),
        Seen::Differs(_)
    ));
}

#[test]
fn the_ledger_unreduced_rational_is_normalised_too() {
    let two_fourths = hex::decode("d81e820204").expect("hex");
    let one_half = hex::decode("d81e820102").expect("hex");
    assert_eq!(
        bytes_seen(Encoding::Normalized, &Ok(two_fourths.clone()), &two_fourths),
        Seen::Same
    );
    assert_eq!(
        bytes_seen(Encoding::Normalized, &Ok(one_half), &two_fourths),
        Seen::Same
    );
}
