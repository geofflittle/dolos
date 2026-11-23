//! Conway values of the cardano-cbor-dataset read by pallas, each judged
//! against the ledger's verdict and re-encoding of it.

/// A rule read as the type `$t` whose re-encoding is compared after the
/// dataset's normalisation, with `unrawed` when the type keeps bytes as read
/// inside it.
macro_rules! normalized {
    ($t:ty) => {
        (
            crate::corpus::Encoding::Normalized,
            read_as!($t, crate::plain::whole),
        )
    };
    ($t:ty, unrawed) => {
        (
            crate::corpus::Encoding::Normalized,
            read_as!($t, crate::plain::unrawed),
        )
    };
}

/// A rule read as the type `$t` behind `KeepRaw`, whose re-encoding is
/// compared byte for byte, with `unrawed` when the type keeps bytes as read
/// inside it.
macro_rules! exact {
    ($t:ty) => {
        (
            crate::corpus::Encoding::Exact,
            read_as!(pallas_primitives::KeepRaw<'_, $t>, crate::plain::kept),
        )
    };
    ($t:ty, unrawed) => {
        (
            crate::corpus::Encoding::Exact,
            read_as!(
                pallas_primitives::KeepRaw<'_, $t>,
                crate::plain::kept_unrawed
            ),
        )
    };
}

/// The reading of an input as the type `$t`, whose value is written by
/// `$plain`.
macro_rules! read_as {
    ($t:ty, $plain:expr) => {{
        fn read(input: &[u8]) -> Result<crate::Reading, String> {
            let mut decoder = pallas_codec::minicbor::Decoder::new(input);
            let value: $t = decoder.decode().map_err(|e| e.to_string())?;
            let consumed = decoder.position();
            let encoding = pallas_codec::minicbor::to_vec(&value).map_err(|e| e.to_string());
            let plain = ($plain)(&value);
            let round_trip = match &encoding {
                Ok(bytes) => match pallas_codec::minicbor::decode::<$t>(bytes) {
                    Ok(again) if again == value => Ok(()),
                    Ok(again) => Err(crate::first_text_difference(
                        &format!("{value:?}"),
                        &format!("{again:?}"),
                    )),
                    Err(e) => Err(format!("its encoding is refused: {e}")),
                },
                Err(e) => Err(format!("it does not encode: {e}")),
            };
            Ok(crate::Reading {
                consumed,
                encoding,
                plain,
                round_trip,
            })
        }
        read as crate::Read
    }};
}

mod changes;
mod corpus;
mod differences;
mod plain;
mod rules;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use changes::Change;
use corpus::{Encoding, GeneratorRule};
use differences::{Evidence, LIST};
use ledger_oracle_judge::comparison::{
    self, Allowance, Allowed, Answer, ComparedLayer, Expected, Key, Layer, LayerRun, Outcome,
    Records, Reference, Row, catch, kinds_from_env,
};
use ledger_oracle_judge::normalize::normalize;
use ledger_oracle_judge::parallel::in_parallel;
use plain::Plain;

/// The dataset submodule, relative to the Dolos tree.
const SUBMODULE: &str = "test_data/cardano-cbor-dataset";

/// What pallas makes of one input: how much of it the value took, the
/// encoding pallas writes at the rule's position, the encoding written from
/// the value alone and whether the written encoding reads back as the value.
pub struct Reading {
    consumed: usize,
    encoding: Result<Vec<u8>, String>,
    plain: Plain,
    round_trip: Result<(), String>,
}

/// The pallas reading of a rule's input.
pub type Read = fn(&[u8]) -> Result<Reading, String>;

/// How pallas reads a rule and how its re-encoding is compared.
pub type RuleType = (Encoding, Read);

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
    rule: RuleType,
    input: Vec<u8>,
    expected: Option<Vec<u8>>,
}

/// The Conway rules and, for each, the ledger's encodings of the samples it
/// accepts.
struct Dataset {
    rules: BTreeMap<&'static str, RuleType>,
    accepted: BTreeMap<String, BTreeSet<Vec<u8>>>,
}

impl Dataset {
    fn rule(&self, row: &Row) -> RuleType {
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
        let value = normal(&reading.plain.encoding);
        let read = normal(&Ok(row.input[..reading.consumed].to_vec()));
        evidence.trailing = row.input.len() - reading.consumed;
        evidence.stopped_inside = stopped_inside(&row.input, reading.consumed);
        evidence.written_as_read = matches!(
            (&read, normal(&reading.encoding)),
            (Ok(read), Ok(written)) if *read == written
        );
        evidence.round_trip = reading.round_trip.clone();
        evidence.changes = match reading.plain.raw_left {
            true => Err("the value holds bytes kept as read".to_owned()),
            false => changes_between(&read, &value),
        };
        if let Reference::Accepts(accepted) = &row.reference {
            let ledger = accepted
                .encoding
                .clone()
                .ok_or_else(|| "no ledger encoding".to_owned());
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
    let pallas = match (encoding, pallas) {
        (_, Err(e)) => return Seen::Differs(format!("it does not encode: {e}")),
        (Encoding::Exact, Ok(bytes)) => bytes.clone(),
        (Encoding::Normalized, Ok(bytes)) => match normalize(bytes) {
            Ok(normal) => normal,
            Err(e) => return Seen::Differs(format!("its encoding has no normal form: {e}")),
        },
    };
    match pallas == expected {
        true => Seen::Same,
        false => Seen::Differs(first_difference(("ledger", expected), ("pallas", &pallas))),
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
        LIST.iter()
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
/// unless all three name the same rules and agree on how each is compared.
fn checked_rules(
    generator: &[GeneratorRule],
    dirs: &[String],
    table: &[(&'static str, RuleType)],
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
    for rule in generator {
        if let Some((_, (encoding, _))) = table.iter().find(|(n, _)| *n == rule.name)
            && *encoding != rule.encoding
        {
            failures.push(format!(
                "the generator compares {} as {:?}, the pallas table as {encoding:?}",
                rule.name, rule.encoding
            ));
        }
    }
    failures
}

fn run() {
    let pin = corpus::checked_out_pin(&tree(), SUBMODULE).unwrap_or_else(|e| panic!("{e}"));
    let dataset = tree().join(SUBMODULE);
    let source = std::fs::read_to_string(dataset.join("app/Generation/EraSpec.hs"))
        .unwrap_or_else(|e| panic!("the generator source: {e}"));
    let generator = corpus::generator_rules(&source).unwrap_or_else(|e| panic!("{e}"));
    let era = dataset.join("dataset/conway");
    let dirs = corpus::rule_dirs(&era).unwrap_or_else(|e| panic!("{e}"));
    let mut failures = checked_rules(&generator, &dirs, rules::RULES);
    assert!(failures.is_empty(), "{}", failures.join("\n"));

    let records: Vec<(String, Records)> = dirs
        .iter()
        .map(|rule| {
            let (classes, rows) =
                corpus::rule_rows(rule, &era.join(rule)).unwrap_or_else(|e| panic!("{e}"));
            let generator = format!("cardano-cbor-dataset {pin} dataset/conway/{rule}");
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
                    .clone()
                    .expect("an accepted sample has its encoding");
                accepted.entry(rule.clone()).or_default().insert(encoding);
            }
        }
    }
    let layer = Arc::new(Dataset {
        rules: rules::RULES.iter().copied().collect(),
        accepted,
    });
    let kinds = kinds_from_env();
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let indices: Vec<usize> = (0..records.len()).collect();
    let runs: Vec<LayerRun<'_, Axis>> = in_parallel(
        &indices,
        threads,
        |i| records[*i].1.rows().iter().map(|row| row.input.len()).sum(),
        |i| comparison::compare(&layer, &records[*i].1, &kinds),
    );

    let mut used: BTreeMap<Allowed, usize> = BTreeMap::new();
    println!(
        "{:<28} {:>7} {:>12} {:>7} {:>7} {:>7}  allowed with owner",
        "rule", "samples", "observations", "agree", "allowed", "failed"
    );
    for ((rule, records), run) in records.iter().zip(&runs) {
        let mut counts = RuleCounts {
            samples: records.rows().len(),
            ..RuleCounts::default()
        };
        if counts.samples == 0 {
            failures.push(format!("rule {rule} has no sample"));
        }
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
    failures.extend(unused_differences(&used, kinds.is_empty()));
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Each listed difference that fails the run: one listed twice, or, in a run
/// of every kind, one no observation was allowed by.
fn unused_differences(used: &BTreeMap<Allowed, usize>, every_kind: bool) -> Vec<String> {
    let mut failures = Vec::new();
    let mut seen = BTreeSet::new();
    for known in LIST {
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

#[test]
fn pallas_reads_the_conway_dataset_as_the_ledger_does() {
    let outcome = std::thread::Builder::new()
        .stack_size(1 << 28)
        .spawn(run)
        .expect("spawn")
        .join();
    if let Err(e) = outcome {
        std::panic::resume_unwind(e);
    }
}

#[cfg(test)]
fn table(names: &[&'static str], encoding: Encoding) -> Vec<(&'static str, RuleType)> {
    names
        .iter()
        .map(|n| (*n, (encoding, read_as!(u64, plain::whole))))
        .collect()
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
        &table(&["coin", "slot"], Encoding::Normalized),
    );
    assert_eq!(failures, Vec::<String>::new());
}

#[test]
fn a_rule_the_table_lacks_fails() {
    let failures = checked_rules(
        &generated(&["coin", "slot"], Encoding::Normalized),
        &["coin".to_owned(), "slot".to_owned()],
        &table(&["coin"], Encoding::Normalized),
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
        &table(&["coin", "slot"], Encoding::Normalized),
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
fn a_rule_compared_another_way_fails() {
    let failures = checked_rules(
        &generated(&["coin"], Encoding::Exact),
        &["coin".to_owned()],
        &table(&["coin"], Encoding::Normalized),
    );
    assert_eq!(
        failures,
        ["the generator compares coin as Exact, the pallas table as Normalized"]
    );
}

#[test]
fn a_rule_tabled_twice_fails() {
    let failures = checked_rules(
        &generated(&["coin"], Encoding::Normalized),
        &["coin".to_owned()],
        &table(&["coin", "coin"], Encoding::Normalized),
    );
    assert_eq!(failures, ["the pallas table names a rule twice"]);
}

#[test]
fn the_generator_table_is_read_with_its_encodings() {
    let source = "conwayRules :: [GenerationRule]\nconwayRules =\n  let version = eraProtVerHigh @ConwayEra\n   in [ mkAnnotatorRule @(Block (Header StandardCrypto) ConwayEra) version \"block\",\n        mkByteExactRule @(TxBody TopTx ConwayEra) version \"transaction_body\",\n        -- a comment\n        mkPositiveMultiAssetRule version \"%multiasset<positive_coin>\",\n        mkRule @Coin version \"coin\"\n      ]\n\ndijkstraRules =\n   in [ mkRule @Coin version \"other\"\n";
    let rules = corpus::generator_rules(source).expect("read");
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
fn an_unknown_generator_constructor_fails() {
    let source = "conwayRules =\n   in [ mkStrangeRule @Coin version \"coin\"\n      ]\n";
    let error = corpus::generator_rules(source).expect_err("an unknown constructor");
    assert!(error.contains("does not know"), "{error}");
}

#[test]
fn a_source_with_no_conway_table_fails() {
    assert!(corpus::generator_rules("dijkstraRules =\n   in [ ]\n").is_err());
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
fn a_value_holding_bytes_kept_as_read_is_told_apart() {
    let pair = [0x82, 0x01, 0x02];
    let nested = read_as!((u64, pallas_primitives::KeepRaw<'_, u64>), plain::whole);
    let whole = read_as!((u64, u64), plain::whole);
    let outer = read_as!(pallas_primitives::KeepRaw<'_, (u64, u64)>, plain::kept);
    assert!(nested(&pair).expect("read").plain.raw_left);
    assert!(!whole(&pair).expect("read").plain.raw_left);
    assert!(!outer(&pair).expect("read").plain.raw_left);
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
