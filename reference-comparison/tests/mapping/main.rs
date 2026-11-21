//! The datums pallas maps for the outputs of sub transactions compared with
//! the datums the ledger finds for them.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ledger_oracle_judge::cbor::{self, Data, Int};
use ledger_oracle_judge::comparison::{
    self, Allowance, Allowed, Answer, Commit, ComparedLayer, Disagreement, Expected, Key, Layer,
    Outcome, Owner, Records, Reference, Row, cddl_pin, kinds_from_env,
};
use pallas::interop::utxorpc::{self as u5c, LedgerContext, TxoRef, UtxoMap};
use pallas::ledger::traverse::{Era, MultiEraTx};
use serde_json::Value as Json;

/// Where the ledger finds the datum of an output of a sub transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    SubWitness,
    TopWitness,
    BothWitness,
    NoWitness,
    Inline,
    NoDatum,
}

impl Source {
    /// Every source, in the order of the classes.
    const ALL: [Source; 6] = [
        Source::SubWitness,
        Source::TopWitness,
        Source::BothWitness,
        Source::NoWitness,
        Source::Inline,
        Source::NoDatum,
    ];

    /// The source's name in the records.
    fn name(self) -> &'static str {
        match self {
            Source::SubWitness => "sub-witness",
            Source::TopWitness => "top-witness",
            Source::BothWitness => "both-witness",
            Source::NoWitness => "no-witness",
            Source::Inline => "inline",
            Source::NoDatum => "none",
        }
    }

    /// Whether the transaction witnesses the datum by hash.
    fn witnessed(self) -> bool {
        matches!(
            self,
            Source::SubWitness | Source::TopWitness | Source::BothWitness
        )
    }

    /// The source named `name` in the records.
    fn named(name: &str) -> Option<Source> {
        Source::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// The positions of a sub transaction in its batch.
const POSITIONS: &[&str] = &["first", "later"];

/// The classes of `sources`, each crossed with a position.
fn classes_of(sources: impl Iterator<Item = Source>) -> Vec<String> {
    sources
        .flat_map(|source| {
            POSITIONS
                .iter()
                .map(move |p| format!("{} {p}", source.name()))
        })
        .collect()
}

/// The classes the records must declare, a source crossed with a position.
fn classes() -> Vec<String> {
    classes_of(Source::ALL.into_iter())
}

/// A schema version of the mapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Version {
    V1,
    V1beta,
    V1alpha,
}

/// The versions each output is mapped under.
const VERSIONS: [Version; 3] = [Version::V1, Version::V1beta, Version::V1alpha];

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Version::V1 => "v1",
            Version::V1beta => "v1beta",
            Version::V1alpha => "v1alpha",
        })
    }
}

/// A field of a mapped datum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Field {
    Hash,
    Payload,
    Bytes,
}

impl fmt::Display for Field {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Field::Hash => "hash",
            Field::Payload => "payload",
            Field::Bytes => "bytes",
        })
    }
}

/// One field of the datum of one output of one sub transaction under one
/// version, or the transaction as a whole.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Axis {
    Datum {
        version: Version,
        sub: usize,
        output: usize,
        field: Field,
    },
    Transaction,
}

impl fmt::Display for Axis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Axis::Datum {
                version,
                sub,
                output,
                field,
            } => write!(f, "{version} sub {sub} output {output} {field}"),
            Axis::Transaction => f.write_str("transaction"),
        }
    }
}

/// A field of a datum as read, or why it could not be read.
#[derive(Debug)]
enum Seen {
    Absent,
    Bytes(Vec<u8>),
    Data(Data),
    Unreadable(String),
}

impl PartialEq for Seen {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Seen::Absent, Seen::Absent) => true,
            (Seen::Bytes(a), Seen::Bytes(b)) => a == b,
            (Seen::Data(a), Seen::Data(b)) => a == b,
            _ => false,
        }
    }
}

impl fmt::Display for Seen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Seen::Absent => f.write_str("nothing"),
            Seen::Bytes(b) => write!(f, "h'{}'", hex::encode(b)),
            Seen::Data(d) => write!(f, "{d}"),
            Seen::Unreadable(e) => write!(f, "an unreadable value ({e})"),
        }
    }
}

/// The fields of a datum, each absent for an output with no datum.
type Fields = [(Field, Seen); 3];

/// The fields of an output with no datum.
fn absent() -> Fields {
    [
        (Field::Hash, Seen::Absent),
        (Field::Payload, Seen::Absent),
        (Field::Bytes, Seen::Absent),
    ]
}

/// Bytes a schema writes as empty when it has none.
fn plain<B: AsRef<[u8]>>(b: B) -> Seen {
    match b.as_ref() {
        [] => Seen::Absent,
        b => Seen::Bytes(b.to_vec()),
    }
}

/// Bytes a schema writes as unset when it has none.
fn optional<B: AsRef<[u8]>>(b: Option<B>) -> Seen {
    match b {
        None => Seen::Absent,
        Some(b) => Seen::Bytes(b.as_ref().to_vec()),
    }
}

/// The reading of one schema version's datum message.
macro_rules! schema {
    ($version:ident, $original:ident) => {
        mod $version {
            use super::{Data, Field, Fields, Int, Seen, cbor};
            pub use pallas::interop::utxorpc::$version::spec::cardano as c;

            /// The data value of a Plutus data message.
            pub fn payload(p: &c::PlutusData) -> Result<Data, String> {
                use c::big_int::BigInt as B;
                use c::plutus_data::PlutusData as P;
                let all = |xs: &[c::PlutusData]| -> Result<Vec<Data>, String> {
                    xs.iter().map(payload).collect()
                };
                match p
                    .plutus_data
                    .as_ref()
                    .ok_or("a Plutus data with no value")?
                {
                    P::Constr(x) => {
                        let ix = cbor::constructor(u64::from(x.tag), x.any_constructor)
                            .ok_or_else(|| format!("constructor tag {}", x.tag))?;
                        Ok(Data::Constr(ix, all(&x.fields)?))
                    }
                    P::Map(m) => {
                        let pair = |p: &c::PlutusDataPair| -> Result<(Data, Data), String> {
                            let key = p.key.as_ref().ok_or("a pair with no key")?;
                            let value = p.value.as_ref().ok_or("a pair with no value")?;
                            Ok((payload(key)?, payload(value)?))
                        };
                        Ok(Data::Map(
                            m.pairs.iter().map(pair).collect::<Result<_, _>>()?,
                        ))
                    }
                    P::BigInt(b) => match b.big_int.as_ref().ok_or("an integer with no value")? {
                        B::Int(n) => Ok(Data::Int(Int::of_i64(*n))),
                        B::BigUInt(m) => Ok(Data::Int(Int::new(false, m))),
                        B::BigNInt(m) => Ok(Data::Int(Int::new(true, m))),
                    },
                    P::BoundedBytes(b) => Ok(Data::Bytes(b.to_vec())),
                    P::Array(a) => Ok(Data::List(all(&a.items)?)),
                }
            }

            /// The fields of a mapped datum, an empty message being no datum.
            pub fn fields(d: Option<&c::Datum>) -> Fields {
                let Some(d) = d else {
                    return super::absent();
                };
                let payload = match &d.payload {
                    None => Seen::Absent,
                    Some(p) => match payload(p) {
                        Ok(x) => Seen::Data(x),
                        Err(e) => Seen::Unreadable(e),
                    },
                };
                [
                    (Field::Hash, super::plain(&d.hash)),
                    (Field::Payload, payload),
                    (Field::Bytes, super::$original(d.original_cbor.clone())),
                ]
            }
        }
    };
}

schema!(v1, optional);
schema!(v1beta, optional);
schema!(v1alpha, plain);

/// A ledger with no outputs and no slot times.
#[derive(Clone)]
struct NoLedger;

impl LedgerContext for NoLedger {
    fn get_utxos(&self, _: &[TxoRef]) -> Option<UtxoMap> {
        None
    }

    fn get_slot_timestamp(&self, _: u64) -> Option<u64> {
        None
    }
}

/// The observations of each field of `fields` under `version`.
fn observed(
    out: &mut Vec<(Axis, Result<Seen, String>)>,
    version: Version,
    sub: usize,
    output: usize,
    fields: Fields,
) {
    for (field, seen) in fields {
        let axis = Axis::Datum {
            version,
            sub,
            output,
            field,
        };
        out.push((axis, Ok(seen)));
    }
}

/// Each version's datum for each output of each sub transaction of `raw`.
fn mapped(raw: &[u8]) -> Vec<(Axis, Result<Seen, String>)> {
    let tx = match MultiEraTx::decode_for_era(Era::Dijkstra, raw) {
        Ok(tx) => tx,
        Err(e) => return vec![(Axis::Transaction, Err(e.to_string()))],
    };
    let mut out = Vec::new();
    let whole = u5c::v1::Mapper::new(NoLedger).map_tx(&tx);
    for (sub, common) in whole.sub_transactions.iter().enumerate() {
        for (output, o) in common.outputs.iter().enumerate() {
            observed(
                &mut out,
                Version::V1,
                sub,
                output,
                v1::fields(o.datum.as_ref()),
            );
        }
    }
    for (sub, x) in tx.sub_transactions().iter().enumerate() {
        let beta = u5c::v1beta::Mapper::new(NoLedger).map_tx(x);
        for (output, o) in beta.outputs.iter().enumerate() {
            let fields = v1beta::fields(o.datum.as_ref());
            observed(&mut out, Version::V1beta, sub, output, fields);
        }
        let alpha = u5c::v1alpha::Mapper::new(NoLedger).map_tx(x);
        for (output, o) in alpha.outputs.iter().enumerate() {
            let fields = v1alpha::fields(o.datum.as_ref());
            observed(&mut out, Version::V1alpha, sub, output, fields);
        }
    }
    out
}

/// The hex bytes in field `name` of a recorded output, none when null.
fn hex_field(o: &Json, name: &str) -> Result<Option<Vec<u8>>, String> {
    match o.get(name) {
        Some(Json::Null) => Ok(None),
        Some(Json::String(h)) => hex::decode(h).map(Some).map_err(|e| format!("{name}: {e}")),
        _ => Err(format!("{o} has no {name}")),
    }
}

/// The fields of the datum the ledger finds for one recorded output.
fn ledger_fields(o: &Json) -> Result<Fields, String> {
    let hash = hex_field(o, "hash")?;
    let datum = hex_field(o, "datum")?;
    let payload = match &datum {
        None => Seen::Absent,
        Some(d) => match cbor::data(d) {
            Ok(x) => Seen::Data(x),
            Err(e) => Seen::Unreadable(format!("the recorded datum: {e}")),
        },
    };
    match hash {
        None if datum.is_some() => Err(format!("{o} has a datum with no hash")),
        None => Ok(absent()),
        Some(hash) => Ok([
            (Field::Hash, Seen::Bytes(hash)),
            (Field::Payload, payload),
            (Field::Bytes, optional(datum)),
        ]),
    }
}

/// The outputs of each sub transaction of a recorded transaction.
fn recorded_subs(dump: &Json) -> Result<Vec<&[Json]>, String> {
    let subs = dump
        .get("subs")
        .and_then(Json::as_array)
        .ok_or_else(|| format!("{dump} has no list of subs"))?;
    subs.iter()
        .map(|s| {
            s.as_array()
                .map(Vec::as_slice)
                .ok_or_else(|| format!("{s} is not a list of outputs"))
        })
        .collect()
}

/// The ledger's datums for a recorded transaction, under each version.
fn ledger_datums(dump: &Json) -> Result<Vec<(Axis, Reference<Seen>)>, String> {
    let mut out = Vec::new();
    for (sub, outputs) in recorded_subs(dump)?.into_iter().enumerate() {
        for (output, o) in outputs.iter().enumerate() {
            for version in VERSIONS {
                for (field, seen) in ledger_fields(o)? {
                    let axis = Axis::Datum {
                        version,
                        sub,
                        output,
                        field,
                    };
                    out.push((axis, Reference::Accepts(seen)));
                }
            }
        }
    }
    Ok(out)
}

/// The owner of v1alpha and v1beta leaving the bytes of a datum witnessed by
/// hash unset.
const WITNESSED_BYTES_UNSET: Owner = Owner::Upstream {
    commit: Commit::new("3da6ff9f84248b99a318a24e811eaf62861c0fc4"),
};

/// The difference v1alpha and v1beta show on a datum witnessed by hash.
const WITHOUT_BYTES: Allowed = Allowed::new(
    "witnessed datum without bytes",
    Disagreement::Values,
    WITNESSED_BYTES_UNSET,
);

/// Whether the answer leaves the bytes unset.
fn bytes_unset(answer: &Answer<Seen, String>) -> bool {
    matches!(answer, Answer::Accepts(Seen::Absent))
}

/// Each class the allowance lets pass, with what it allows.
fn listed() -> Vec<(String, Allowed)> {
    classes_of(Source::ALL.into_iter().filter(|s| s.witnessed()))
        .into_iter()
        .map(|class| (class, WITHOUT_BYTES))
        .collect()
}

/// Pallas's mapping of the datums of sub transaction outputs.
struct Mapping;

impl ComparedLayer for Mapping {
    const LAYER: Layer = Layer::Mapping;
    const TIME_LIMIT: Duration = Duration::from_secs(60);
    type Input = Vec<u8>;
    type Axis = Axis;
    type Value = Seen;
    type Refusal = String;

    fn input(&self, row: &Row) -> Vec<u8> {
        row.input.clone()
    }

    fn reference(&self, row: &Row) -> Expected<Axis, Seen> {
        let datums = match &row.reference {
            Reference::Refuses(reason) => {
                return Expected::one(Axis::Transaction, Reference::Refuses(reason.clone()));
            }
            Reference::Accepts(accepted) => ledger_datums(&accepted.dump),
        };
        let mut datums = match datums {
            Ok(datums) if !datums.is_empty() => datums.into_iter(),
            Ok(_) => vec![(
                Axis::Transaction,
                Reference::Accepts(Seen::Unreadable(
                    "the record holds no sub transaction output".to_owned(),
                )),
            )]
            .into_iter(),
            Err(e) => vec![(
                Axis::Transaction,
                Reference::Accepts(Seen::Unreadable(format!("the record: {e}"))),
            )]
            .into_iter(),
        };
        let first = datums.next().expect("one observation");
        Expected::Judged {
            first,
            rest: datums.collect(),
        }
    }

    fn run(&self, input: &Vec<u8>) -> Vec<(Axis, Result<Seen, String>)> {
        mapped(input)
    }

    fn same_reason(&self, _: &str, _: &String) -> bool {
        false
    }

    fn allowance(&self, key: &Key<Axis>) -> Option<Allowance<Seen, String>> {
        let unset = matches!(
            key.axis,
            Axis::Datum {
                version: Version::V1alpha | Version::V1beta,
                field: Field::Bytes,
                ..
            }
        );
        let witnessed = Source::named(key.value_kind()).is_some_and(Source::witnessed);
        (unset && witnessed).then_some(Allowance {
            allowed: WITHOUT_BYTES,
            shown: bytes_unset,
        })
    }

    fn detail(
        &self,
        row: &Row,
        reference: &Reference<Seen>,
        answer: &Answer<Seen, String>,
    ) -> String {
        let want = match reference {
            Reference::Accepts(seen) => format!("finds {seen}"),
            Reference::Refuses(reason) => {
                format!("refuses ({})", reason.chars().take(160).collect::<String>())
            }
        };
        let got = match answer {
            Answer::Accepts(seen) => format!("maps {seen}"),
            Answer::Refuses(error) => format!("refuses: {error}"),
            Answer::Panics(message) => format!("panics: {message}"),
        };
        format!("{}: the ledger {want}, pallas {got}", row.class)
    }
}

/// Each declared class that is not one of `known`.
fn unknown_classes(declared: &[String], known: &[String]) -> Vec<String> {
    declared
        .iter()
        .filter(|c| !known.contains(c))
        .map(|c| {
            format!(
                "{}: class {c:?} is not one this check knows",
                Layer::Mapping
            )
        })
        .collect()
}

/// Whether a recorded transaction holds an output of `source` in a sub
/// transaction at `position`.
fn holds(dump: &Json, source: &str, position: &str) -> bool {
    let Ok(subs) = recorded_subs(dump) else {
        return false;
    };
    let at = match position {
        "first" => &subs[..subs.len().min(1)],
        _ => subs.get(1..).unwrap_or_default(),
    };
    at.iter()
        .flat_map(|outputs| outputs.iter())
        .any(|o| o.get("source").and_then(Json::as_str) == Some(source))
}

/// Each row of a known class whose transaction holds no output of its source
/// at its position.
fn unexercised(records: &Records, known: &[String]) -> Vec<String> {
    records
        .rows()
        .iter()
        .filter(|row| known.contains(&row.class))
        .filter(|row| {
            let (source, position) = row.class.split_once(' ').unwrap_or_default();
            match &row.reference {
                Reference::Accepts(accepted) => !holds(&accepted.dump, source, position),
                Reference::Refuses(_) => true,
            }
        })
        .map(|row| {
            format!(
                "{}: a row of class {:?} holds no such output that the ledger accepts",
                Layer::Mapping,
                row.class
            )
        })
        .collect()
}

/// Each reason the records in `dir` fail the comparison over `kinds`.
fn failures(dir: &Path, kinds: &[String]) -> Vec<String> {
    let records = match Records::read_dir(dir, Layer::Mapping) {
        Ok(records) => records,
        Err(e) => return vec![format!("{}: {e}", Layer::Mapping)],
    };
    let classes = classes();
    let mut out = records.check(Layer::Mapping, &classes);
    out.extend(unknown_classes(records.classes(), &classes));
    out.extend(unexercised(&records, &classes));
    let judged = comparison::compare(&Arc::new(Mapping), &records, kinds);
    let mut examples = BTreeMap::new();
    for j in judged
        .judgements
        .iter()
        .filter(|j| !matches!(j.outcome, Outcome::Agree | Outcome::Allowed(_)))
    {
        examples.entry(&j.row.class).or_insert(&j.detail);
    }
    println!("records: {}", records.generator());
    println!("{judged}");
    examples.values().for_each(|e| println!("{e}"));
    out.extend(judged.failures());
    let listed = listed();
    let listed: Vec<(&str, Allowed)> = listed.iter().map(|(c, a)| (c.as_str(), *a)).collect();
    out.extend(judged.tally.unused(&listed, records.classes()));
    out
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../pallas-head")
}

fn run() {
    let path = "pallas-primitives/src/dijkstra/defs.cddl";
    let cddl = std::fs::read_to_string(root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
    let pin = cddl_pin(&cddl).expect("defs.cddl names no ledger commit");
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("records")
        .join(pin);
    let failures = failures(&dir, &kinds_from_env());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn pallas_maps_the_datums_the_ledger_finds() {
    let outcome = std::thread::Builder::new()
        .stack_size(1 << 28)
        .spawn(run)
        .expect("spawn")
        .join();
    if let Err(e) = outcome {
        std::panic::resume_unwind(e);
    }
}

/// A directory named by a pin, holding `text` as the mapping records unless
/// it is `None`.
fn records_at(name: &str, text: Option<&str>) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("mapping-{name}"))
        .join("0".repeat(40));
    std::fs::create_dir_all(&dir).expect("create the records directory");
    let file = dir.join("mapping.jsonl");
    match text {
        Some(text) => std::fs::write(&file, text).expect("write the records"),
        None => {
            let _ = std::fs::remove_file(&file);
        }
    }
    dir
}

/// A records file declaring `class` with one row of `class` holding `dump`.
fn one_row(class: &str, dump: &Json) -> String {
    let header = serde_json::json!({
        "reference": "0".repeat(40),
        "generator": "a probe",
        "layer": "mapping",
        "classes": [class],
    });
    let row = serde_json::json!({
        "class": class,
        "input": "80",
        "reference": {"accepts": dump},
    });
    format!("{header}\n{row}\n")
}

#[test]
fn a_missing_records_file_fails() {
    let failures = failures(&records_at("missing", None), &[]);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].starts_with("mapping: "), "{failures:?}");
    assert!(failures[0].contains("mapping.jsonl"), "{failures:?}");
}

#[test]
fn an_empty_records_file_fails() {
    let failures = failures(&records_at("empty", Some("")), &[]);
    assert_eq!(failures, ["mapping: line 1: no header"]);
}

#[test]
fn a_class_the_check_does_not_know_fails() {
    let text = one_row("sub-witness third", &serde_json::json!({"subs": []}));
    let failures = failures(&records_at("unknown-class", Some(&text)), &[]);
    let unknown = "mapping: class \"sub-witness third\" is not one this check knows";
    assert!(failures.iter().any(|f| f == unknown), "{failures:?}");
    let unnamed = failures
        .iter()
        .filter(|f| f.contains("the records name no kind or class"))
        .count();
    assert_eq!(unnamed, classes().len(), "{failures:?}");
}

#[test]
fn a_row_without_its_source_at_its_position_fails() {
    let dump = serde_json::json!({"subs": [
        [{"source": "none", "hash": null, "datum": null}],
        [{"source": "top-witness", "hash": "aa", "datum": "01"}],
    ]});
    let text = one_row("top-witness first", &dump);
    let failures = failures(&records_at("unexercised", Some(&text)), &[]);
    let unexercised = "mapping: a row of class \"top-witness first\" holds no such output that the ledger accepts";
    assert!(failures.iter().any(|f| f == unexercised), "{failures:?}");
}

#[test]
fn a_row_with_its_source_at_its_position_is_exercised() {
    let dump = serde_json::json!({"subs": [
        [{"source": "none", "hash": null, "datum": null}],
        [{"source": "top-witness", "hash": "aa", "datum": "01"}],
    ]});
    for (class, held) in [
        ("top-witness later", true),
        ("none first", true),
        ("none later", false),
    ] {
        let (source, position) = class.split_once(' ').expect("two words");
        assert_eq!(holds(&dump, source, position), held, "{class}");
    }
    let header = serde_json::json!({
        "reference": "0".repeat(40),
        "generator": "a probe",
        "layer": "mapping",
        "classes": ["top-witness later"],
    });
    let row = serde_json::json!({"class": "top-witness later", "input": "80", "reference": {"accepts": dump}});
    let records = Records::read(&format!("{header}\n{row}\n"), &"0".repeat(40)).expect("records");
    assert_eq!(unexercised(&records, &classes()), Vec::<String>::new());
}

#[test]
fn the_known_classes_pass_the_class_check() {
    let classes = classes();
    assert_eq!(classes.len(), 12);
    assert_eq!(unknown_classes(&classes, &classes), Vec::<String>::new());
    let listed: Vec<String> = listed().into_iter().map(|(class, _)| class).collect();
    assert_eq!(
        listed,
        [
            "sub-witness first",
            "sub-witness later",
            "top-witness first",
            "top-witness later",
            "both-witness first",
            "both-witness later",
        ]
    );
}

#[test]
fn an_unreadable_field_equals_nothing() {
    let a = Seen::Unreadable("x".to_owned());
    assert_ne!(a, Seen::Unreadable("x".to_owned()));
    assert_ne!(Seen::Absent, a);
    assert_eq!(Seen::Absent, Seen::Absent);
    assert_ne!(Seen::Absent, Seen::Bytes(Vec::new()));
}

#[test]
fn an_empty_datum_message_reads_as_no_datum() {
    assert_eq!(v1::fields(None), absent());
    assert_eq!(v1::fields(Some(&v1::c::Datum::default())), absent());
    assert_eq!(v1beta::fields(Some(&v1beta::c::Datum::default())), absent());
    assert_eq!(
        v1alpha::fields(Some(&v1alpha::c::Datum::default())),
        absent()
    );
}

#[test]
fn a_hash_only_datum_reads_as_its_hash() {
    let datum = v1::c::Datum {
        hash: vec![0xaa].into(),
        ..Default::default()
    };
    let read = v1::fields(Some(&datum));
    assert_ne!(read, absent());
    let recorded =
        ledger_fields(&serde_json::json!({"hash": "aa", "datum": null})).expect("fields");
    assert_eq!(read, recorded);
}

#[test]
fn set_bytes_differ_from_unset_bytes_by_version() {
    let empty = v1::c::Datum {
        hash: vec![0xaa].into(),
        original_cbor: Some(Vec::new().into()),
        ..Default::default()
    };
    assert_eq!(v1::fields(Some(&empty))[2].1, Seen::Bytes(Vec::new()));
    let alpha = v1alpha::c::Datum {
        hash: vec![0xaa].into(),
        original_cbor: Vec::new().into(),
        ..Default::default()
    };
    assert_eq!(v1alpha::fields(Some(&alpha))[2].1, Seen::Absent);
}

#[test]
fn equal_payloads_read_equal_from_both_sides() {
    use v1::c::{self, big_int, plutus_data::PlutusData as P};
    let int = |b: big_int::BigInt| c::PlutusData {
        plutus_data: Some(P::BigInt(c::BigInt { big_int: Some(b) })),
    };
    let constr = |tag: u32, any_constructor: u64| c::PlutusData {
        plutus_data: Some(P::Constr(c::Constr {
            tag,
            any_constructor,
            fields: vec![int(big_int::BigInt::Int(-1))],
        })),
    };
    let cases = [
        (int(big_int::BigInt::Int(-1)), "20"),
        (int(big_int::BigInt::Int(23)), "17"),
        (
            int(big_int::BigInt::BigUInt(
                vec![1, 0, 0, 0, 0, 0, 0, 0, 0].into(),
            )),
            "c249010000000000000000",
        ),
        (
            int(big_int::BigInt::BigNInt(
                vec![1, 0, 0, 0, 0, 0, 0, 0, 0].into(),
            )),
            "c349010000000000000000",
        ),
        (constr(121, 0), "d8799f20ff"),
        (constr(1280, 0), "d905009f20ff"),
        (constr(102, 200), "d8668218c89f20ff"),
    ];
    for (message, raw) in cases {
        let want = cbor::data(&hex::decode(raw).expect("hex")).expect(raw);
        assert_eq!(v1::payload(&message), Ok(want), "{raw}");
    }
    let other = cbor::data(&hex::decode("d87a9f20ff").expect("hex")).expect("constr 1");
    assert_ne!(v1::payload(&constr(121, 0)), Ok(other));
    assert!(v1::payload(&c::PlutusData::default()).is_err());
}

#[test]
fn a_malformed_record_fails_to_read() {
    for o in [
        serde_json::json!({"hash": null, "datum": "01"}),
        serde_json::json!({"hash": "zz", "datum": null}),
        serde_json::json!({"datum": null}),
    ] {
        assert!(ledger_fields(&o).is_err(), "{o}");
    }
    assert!(ledger_datums(&serde_json::json!({})).is_err());
    assert_eq!(
        ledger_datums(&serde_json::json!({"subs": [[]]})),
        Ok(Vec::new())
    );
}
