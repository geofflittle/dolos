//! The Conway values of the cardano-cbor-dataset read by a build of pallas
//! without `unstable`, each compared with a base build's reading of it. A run
//! with `STABLE_READINGS_WRITE` writes this build's readings to that file. A
//! run with `STABLE_READINGS_BASE` compares this build's readings with that
//! file's.

#[cfg(feature = "unstable")]
compile_error!(
    "the stable comparison reads a build without unstable: --no-default-features --features stable"
);

#[allow(dead_code)]
#[macro_use]
#[path = "../dataset/mod.rs"]
mod dataset;
#[path = "../dataset-conway/rules.rs"]
mod rules;

use std::collections::{BTreeMap, BTreeSet};
use std::panic::AssertUnwindSafe;
use std::process::Command;

use dataset::{Era, Reader, corpus};
use ledger_oracle_judge::comparison::catch;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const CONWAY: Era = Era {
    dir: "conway",
    generator_table: "conwayRules",
    rules: rules::RULES,
    differences: &[],
};

/// The encoding a build writes for a value, by its length and SHA-256 digest.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Written {
    length: usize,
    sha256: String,
}

impl Written {
    fn of(bytes: &[u8]) -> Self {
        Written {
            length: bytes.len(),
            sha256: hex::encode(Sha256::digest(bytes)),
        }
    }
}

/// What a build of pallas makes of one sample.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    /// The sample's first `read` bytes read as a value whose encoding is
    /// `written`.
    Accepts {
        read: usize,
        written: Result<Written, String>,
    },
    /// The sample is refused, for the reason given.
    Refuses(String),
    /// The reading panics, with the message given.
    Panics(String),
}

impl Outcome {
    fn verdict(&self) -> &'static str {
        match self {
            Outcome::Accepts { .. } => "accepts",
            Outcome::Refuses(_) => "refuses",
            Outcome::Panics(_) => "panics",
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Outcome::Accepts {
                read,
                written: Ok(w),
            } => json!({"accepts": {"read": read, "length": w.length, "sha256": w.sha256}}),
            Outcome::Accepts {
                read,
                written: Err(e),
            } => json!({"accepts": {"read": read, "error": e}}),
            Outcome::Refuses(why) => json!({ "refuses": why }),
            Outcome::Panics(why) => json!({ "panics": why }),
        }
    }

    fn from_json(value: &Value) -> Result<Self, String> {
        let text = |v: &Value| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("not a text: {v}"))
        };
        let count = |v: &Value| {
            v.as_u64()
                .map(|n| n as usize)
                .ok_or_else(|| format!("not a count: {v}"))
        };
        let object = value
            .as_object()
            .filter(|o| o.len() == 1)
            .ok_or_else(|| format!("not an outcome: {value}"))?;
        let (verdict, body) = object.iter().next().expect("one entry");
        match verdict.as_str() {
            "accepts" => {
                let read = count(&body["read"])?;
                let written = match (&body["error"], &body["length"], &body["sha256"]) {
                    (Value::Null, length, sha256) => Ok(Written {
                        length: count(length)?,
                        sha256: text(sha256)?,
                    }),
                    (error, Value::Null, Value::Null) => Err(text(error)?),
                    _ => return Err(format!("not an accepted outcome: {body}")),
                };
                Ok(Outcome::Accepts { read, written })
            }
            "refuses" => Ok(Outcome::Refuses(text(body)?)),
            "panics" => Ok(Outcome::Panics(text(body)?)),
            _ => Err(format!("not an outcome: {value}")),
        }
    }
}

/// One build's outcome for each sample of an era, by the sample's path, with
/// the corpus commit and the pallas commit it ran at.
#[derive(Debug, PartialEq)]
struct Readings {
    corpus: String,
    era: String,
    pallas: String,
    samples: BTreeMap<String, Outcome>,
}

impl Readings {
    fn to_json(&self) -> Value {
        let samples: serde_json::Map<String, Value> = self
            .samples
            .iter()
            .map(|(path, outcome)| (path.clone(), outcome.to_json()))
            .collect();
        json!({
            "corpus": self.corpus,
            "era": self.era,
            "pallas": self.pallas,
            "samples": samples,
        })
    }

    fn from_json(value: &Value) -> Result<Self, String> {
        let text = |key: &str| {
            value[key]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("the readings have no {key}"))
        };
        let samples = value["samples"]
            .as_object()
            .ok_or_else(|| "the readings have no samples".to_owned())?
            .iter()
            .map(|(path, outcome)| {
                Outcome::from_json(outcome)
                    .map(|o| (path.clone(), o))
                    .map_err(|e| format!("{path}: {e}"))
            })
            .collect::<Result<_, _>>()?;
        Ok(Readings {
            corpus: text("corpus")?,
            era: text("era")?,
            pallas: text("pallas")?,
            samples,
        })
    }
}

/// The rule a sample's path names first.
fn rule_of(path: &str) -> &str {
    path.split('/').next().unwrap_or_default()
}

/// The counts of one rule's samples compared across the two builds.
#[derive(Debug, Default, PartialEq)]
struct RuleCounts {
    compared: usize,
    verdicts_equal: usize,
    both_accept: usize,
    bytes_equal: usize,
}

/// Two builds' readings compared: the counts per rule, the samples whose
/// outcome differs or that one build lacks, and each failure of the run.
#[derive(Debug, Default)]
struct Comparison {
    rules: BTreeMap<String, RuleCounts>,
    samples: usize,
    differing: Vec<String>,
    failures: Vec<String>,
}

fn compare(base: &Readings, head: &Readings) -> Comparison {
    let mut comparison = Comparison::default();
    for (what, b, h) in [
        ("corpus", &base.corpus, &head.corpus),
        ("era", &base.era, &head.era),
    ] {
        if b != h {
            comparison
                .failures
                .push(format!("the base read {what} {b}, this build {what} {h}"));
        }
    }
    let paths: BTreeSet<&String> = base.samples.keys().chain(head.samples.keys()).collect();
    comparison.samples = paths.len();
    for path in paths {
        let (b, h) = match (base.samples.get(path), head.samples.get(path)) {
            (Some(b), Some(h)) => (b, h),
            (b, _) => {
                let side = match b {
                    Some(_) => "the base",
                    None => "this build",
                };
                comparison
                    .differing
                    .push(format!("{path}: read by {side} only"));
                continue;
            }
        };
        let counts = comparison
            .rules
            .entry(rule_of(path).to_owned())
            .or_default();
        counts.compared += 1;
        let verdict_equal = b.verdict() == h.verdict();
        counts.verdicts_equal += usize::from(verdict_equal);
        let bytes_equal = match (b, h) {
            (Outcome::Accepts { .. }, Outcome::Accepts { .. }) => {
                counts.both_accept += 1;
                let equal = b == h;
                counts.bytes_equal += usize::from(equal);
                equal
            }
            _ => true,
        };
        if !verdict_equal || !bytes_equal {
            comparison.differing.push(format!(
                "{path}: base {}, this build {}",
                dataset::shortened(format!("{b:?}")),
                dataset::shortened(format!("{h:?}"))
            ));
        }
    }
    if comparison.rules.values().all(|c| c.compared == 0) {
        comparison
            .failures
            .push("no sample is read by both builds".to_owned());
    }
    comparison
}

/// What this build of pallas makes of a sample read by `read`.
fn outcome(read: dataset::Read, input: &[u8]) -> Outcome {
    match catch(AssertUnwindSafe(|| read(input))) {
        Err(panic) => Outcome::Panics(panic),
        Ok(Err(refusal)) => Outcome::Refuses(refusal),
        Ok(Ok(reading)) => Outcome::Accepts {
            read: reading.consumed,
            written: reading.encoding.map(|bytes| Written::of(&bytes)),
        },
    }
}

/// The commit of the pallas tree this build compiles.
fn pallas_commit() -> String {
    let dir = dataset::tree().join("../pallas-head");
    let out = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap_or_else(|e| panic!("git rev-parse in {} could not run: {e}", dir.display()));
    let commit = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert!(
        out.status.success() && commit.len() == 40,
        "git rev-parse in {} printed {commit:?}",
        dir.display()
    );
    commit
}

/// This build's readings of every sample of an era's typed rules and why
/// pallas has no type for each other rule.
fn read_era(era: &Era) -> (Readings, BTreeMap<String, String>) {
    let tree = dataset::tree();
    let pin = corpus::checked_out_pin(&tree, dataset::SUBMODULE).unwrap_or_else(|e| panic!("{e}"));
    let corpus_dir = tree.join(dataset::SUBMODULE);
    let source = std::fs::read_to_string(corpus_dir.join("app/Generation/EraSpec.hs"))
        .unwrap_or_else(|e| panic!("the generator source: {e}"));
    let generator =
        corpus::generator_rules(&source, era.generator_table).unwrap_or_else(|e| panic!("{e}"));
    let era_dir = corpus_dir.join("dataset").join(era.dir);
    let dirs = corpus::rule_dirs(&era_dir).unwrap_or_else(|e| panic!("{e}"));
    let failures = dataset::checked_rules(&generator, &dirs, era.rules);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    let readers: BTreeMap<&str, Reader> = era.rules.iter().copied().collect();
    let mut samples = BTreeMap::new();
    let mut untyped = BTreeMap::new();
    for rule in &dirs {
        let read = match readers[rule.as_str()] {
            Reader::Type(t) => t.normalized,
            Reader::NoType { why } | Reader::Owned { why, .. } => {
                untyped.insert(rule.clone(), why.to_owned());
                continue;
            }
        };
        let (_, rows) =
            corpus::rule_rows(rule, &era_dir.join(rule)).unwrap_or_else(|e| panic!("{e}"));
        for row in rows {
            let path = row.variant.expect("a corpus row names its sample");
            samples.insert(path, outcome(read, &row.input));
        }
    }
    let readings = Readings {
        corpus: pin,
        era: era.dir.to_owned(),
        pallas: pallas_commit(),
        samples,
    };
    (readings, untyped)
}

fn run(era: &'static Era) {
    let write = std::env::var_os("STABLE_READINGS_WRITE");
    let base = std::env::var_os("STABLE_READINGS_BASE");
    let (head, untyped) = read_era(era);
    match (write, base) {
        (Some(path), None) => {
            assert!(!head.samples.is_empty(), "this build read no sample");
            let text = serde_json::to_string(&head.to_json()).expect("readings as json");
            std::fs::write(&path, text)
                .unwrap_or_else(|e| panic!("{}: {e}", path.to_string_lossy()));
            let rules: BTreeSet<&str> = head.samples.keys().map(|p| rule_of(p)).collect();
            println!(
                "wrote {} samples of {} rules read by pallas {} to {}",
                head.samples.len(),
                rules.len(),
                head.pallas,
                path.to_string_lossy()
            );
        }
        (None, Some(path)) => {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.to_string_lossy()));
            let value: Value = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{}: {e}", path.to_string_lossy()));
            let base = Readings::from_json(&value)
                .unwrap_or_else(|e| panic!("{}: {e}", path.to_string_lossy()));
            report(era, &base, &head, &untyped);
        }
        _ => panic!("set exactly one of STABLE_READINGS_WRITE and STABLE_READINGS_BASE"),
    }
}

fn report(era: &Era, base: &Readings, head: &Readings, untyped: &BTreeMap<String, String>) {
    let comparison = compare(base, head);
    println!(
        "cardano-cbor-dataset {} {}, base pallas {}, this build pallas {}, without unstable",
        head.corpus, era.dir, base.pallas, head.pallas
    );
    println!(
        "{:<28} {:>7} {:>14} {:>11} {:>11}",
        "rule", "samples", "verdicts equal", "both accept", "bytes equal"
    );
    for (rule, c) in &comparison.rules {
        println!(
            "{rule:<28} {:>7} {:>14} {:>11} {:>11}",
            c.compared, c.verdicts_equal, c.both_accept, c.bytes_equal
        );
    }
    for (rule, why) in untyped {
        println!("{rule:<28} not compared, no pallas type: {why}");
    }
    const SHOWN: usize = 200;
    for line in comparison.differing.iter().take(SHOWN) {
        println!("{line}");
    }
    if comparison.differing.len() > SHOWN {
        println!("and {} more", comparison.differing.len() - SHOWN);
    }
    let summary = format!(
        "stable {}: {} of {} samples differ",
        era.dir,
        comparison.differing.len(),
        comparison.samples
    );
    println!("{summary}");
    let mut failures = comparison.failures;
    if !comparison.differing.is_empty() {
        failures.push(summary);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn pallas_reads_the_conway_dataset_as_the_base_does() {
    dataset::on_large_stack(|| run(&CONWAY));
}

#[cfg(test)]
fn readings(samples: &[(&str, Outcome)]) -> Readings {
    Readings {
        corpus: "5f2fe7ed".to_owned(),
        era: "conway".to_owned(),
        pallas: "f6eb4c57".to_owned(),
        samples: samples
            .iter()
            .map(|(p, o)| (p.to_string(), o.clone()))
            .collect(),
    }
}

#[cfg(test)]
fn accepts(read: usize, written: &[u8]) -> Outcome {
    Outcome::Accepts {
        read,
        written: Ok(Written::of(written)),
    }
}

#[test]
fn equal_readings_differ_nowhere() {
    let samples = [
        ("coin/valid/1", accepts(1, &[0x01])),
        ("coin/invalid-zap-1/1", Outcome::Refuses("no".to_owned())),
        ("relay/valid/1", accepts(4, &[0x82, 0x02, 0x60])),
    ];
    let comparison = compare(&readings(&samples), &readings(&samples));
    assert_eq!(comparison.differing, Vec::<String>::new());
    assert_eq!(comparison.failures, Vec::<String>::new());
    assert_eq!(comparison.samples, 3);
    assert_eq!(
        comparison.rules["coin"],
        RuleCounts {
            compared: 2,
            verdicts_equal: 2,
            both_accept: 1,
            bytes_equal: 1
        }
    );
}

#[test]
fn a_changed_verdict_differs() {
    let base = readings(&[("credential/valid/1", Outcome::Refuses("no".to_owned()))]);
    let head = readings(&[("credential/valid/1", accepts(3, &[0x80]))]);
    let comparison = compare(&base, &head);
    assert_eq!(comparison.differing.len(), 1);
    assert_eq!(comparison.rules["credential"].verdicts_equal, 0);
}

#[test]
fn another_refusal_reason_is_the_same_verdict() {
    let base = readings(&[("relay/invalid-zap-1/1", Outcome::Refuses("a".to_owned()))]);
    let head = readings(&[("relay/invalid-zap-1/1", Outcome::Refuses("b".to_owned()))]);
    assert_eq!(compare(&base, &head).differing, Vec::<String>::new());
}

#[test]
fn changed_bytes_read_or_written_differ() {
    let base = readings(&[
        ("value/valid/1", accepts(3, &[0x01])),
        ("value/valid/2", accepts(3, &[0x01])),
    ]);
    let head = readings(&[
        ("value/valid/1", accepts(3, &[0x02])),
        ("value/valid/2", accepts(2, &[0x01])),
    ]);
    let comparison = compare(&base, &head);
    assert_eq!(comparison.differing.len(), 2);
    assert_eq!(
        comparison.rules["value"],
        RuleCounts {
            compared: 2,
            verdicts_equal: 2,
            both_accept: 2,
            bytes_equal: 0
        }
    );
}

#[test]
fn a_sample_one_build_lacks_differs() {
    let base = readings(&[("coin/valid/1", accepts(1, &[0x01]))]);
    let head = readings(&[
        ("coin/valid/1", accepts(1, &[0x01])),
        ("coin/valid/2", accepts(1, &[0x02])),
    ]);
    assert_eq!(
        compare(&base, &head).differing,
        ["coin/valid/2: read by this build only"]
    );
}

#[test]
fn readings_with_no_sample_fail() {
    let comparison = compare(&readings(&[]), &readings(&[]));
    assert_eq!(comparison.failures, ["no sample is read by both builds"]);
}

#[test]
fn readings_of_another_corpus_fail() {
    let base = readings(&[("coin/valid/1", accepts(1, &[0x01]))]);
    let mut head = readings(&[("coin/valid/1", accepts(1, &[0x01]))]);
    head.corpus = "00000000".to_owned();
    assert_eq!(
        compare(&base, &head).failures,
        ["the base read corpus 5f2fe7ed, this build corpus 00000000"]
    );
}

#[test]
fn readings_read_back_as_written() {
    let written = readings(&[
        ("coin/valid/1", accepts(1, &[0x01])),
        (
            "coin/valid/2",
            Outcome::Accepts {
                read: 1,
                written: Err("no encoding".to_owned()),
            },
        ),
        ("coin/invalid-zap-1/1", Outcome::Refuses("no".to_owned())),
        (
            "coin/invalid-zap-1/2",
            Outcome::Panics("short hash".to_owned()),
        ),
    ]);
    assert_eq!(Readings::from_json(&written.to_json()), Ok(written));
}

#[test]
fn malformed_readings_are_refused() {
    let mut value = readings(&[("coin/valid/1", accepts(1, &[0x01]))]).to_json();
    value["samples"]["coin/valid/1"] = json!({"accepts": {"read": 1}});
    assert!(Readings::from_json(&value).is_err());
    value["samples"]["coin/valid/1"] = json!({"agrees": "x"});
    assert!(Readings::from_json(&value).is_err());
    assert!(Readings::from_json(&json!({"corpus": "x"})).is_err());
}
