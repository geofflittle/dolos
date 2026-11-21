//! UTxO replies of the local state query decoded by pallas compared with the
//! ledger's reading of them.

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ledger_oracle_judge::cbor::{self, Data, Int, TxIn};
use ledger_oracle_judge::comparison::{
    self, Allowance, Answer, ComparedLayer, Expected, Key, Layer, Outcome, Records, Reference, Row,
    WholeInput, catch, cddl_pin, kinds_from_env,
};
use pallas::network::miniprotocols::localstate::queries_v16 as q;
use pallas_codec::minicbor;
use serde_json::Value as Json;

/// An era whose UTxO replies the records hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Era {
    Conway,
    Dijkstra,
}

impl Era {
    const ALL: [Era; 2] = [Era::Conway, Era::Dijkstra];

    /// The era's name in a class.
    fn name(self) -> &'static str {
        match self {
            Era::Conway => "conway",
            Era::Dijkstra => "dijkstra",
        }
    }

    /// The era a class names before its `-utxo`.
    fn of_class(class: &str) -> Option<Era> {
        let (name, _) = class.split_once("-utxo ")?;
        Era::ALL.into_iter().find(|e| e.name() == name)
    }
}

/// The forms of output the ledger's output decoder tells apart.
const FORMS: &[&str] = &[
    "legacy-output",
    "map-output",
    "script-native",
    "script-plutus-v1",
    "script-plutus-v2",
    "script-plutus-v3",
    "script-plutus-v4",
    "script-unknown-arm",
    "datum-hash",
    "datum-inline",
    "datum-unknown-arm",
    "unknown-key",
    "native-pubkey",
    "native-all",
    "native-any",
    "native-n-of-k",
    "native-invalid-before",
    "native-invalid-hereafter",
    "native-require-guard",
    "native-unknown-arm",
    "map-indefinite",
    "no-address",
    "no-value",
];

/// The classes the records must declare, an era's reply crossed with a form
/// of output.
fn classes() -> Vec<String> {
    Era::ALL
        .into_iter()
        .flat_map(|era| {
            FORMS
                .iter()
                .map(move |form| format!("{}-utxo {form}", era.name()))
        })
        .collect()
}

/// One output read as its address, value, datum and script.
#[derive(Debug, PartialEq)]
struct Output {
    address: Vec<u8>,
    coin: Data,
    assets: BTreeMap<Vec<u8>, BTreeMap<Vec<u8>, Data>>,
    datum: Datum,
    script: Script,
}

/// The datum an output holds.
#[derive(Debug, PartialEq)]
enum Datum {
    None,
    Hash(Vec<u8>),
    Inline(Data),
}

/// The script an output holds, a native script in its CDDL form.
#[derive(Debug, PartialEq)]
enum Script {
    None,
    Native(Data),
    Plutus(u64, Vec<u8>),
}

/// A reply read as its outputs ordered by input, or why it could not be read
/// into that form.
#[derive(Debug)]
enum Reply {
    Read(Vec<(TxIn, Output)>),
    Unreadable(String),
}

impl PartialEq for Reply {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Reply::Read(a), Reply::Read(b)) => a == b,
            _ => false,
        }
    }
}

impl Reply {
    /// The reply of `utxos`, ordered by input.
    fn of(read: Result<Vec<(TxIn, Output)>, String>) -> Reply {
        match read {
            Ok(mut utxos) => {
                utxos.sort_by_key(|(input, _)| *input);
                Reply::Read(utxos)
            }
            Err(e) => Reply::Unreadable(e),
        }
    }
}

/// An unsigned integer as a data value.
fn uint(n: u64) -> Data {
    Data::Int(Int::new(false, &n.to_be_bytes()))
}

/// The unsigned integer a data value holds.
fn small(d: &Data) -> Result<u64, String> {
    match d {
        Data::Int(Int {
            negative: false,
            magnitude,
        }) if magnitude.len() <= 8 => Ok(magnitude
            .iter()
            .fold(0u64, |acc, b| (acc << 8) | u64::from(*b))),
        other => Err(format!("{other} is not a 64 bit unsigned integer")),
    }
}

/// The bytes a data value holds.
fn bytes(d: Data) -> Result<Vec<u8>, String> {
    match d {
        Data::Bytes(b) => Ok(b),
        other => Err(format!("{other} is not a byte string")),
    }
}

/// The data value of pallas's own encoding of `x`.
fn encoded<T: minicbor::Encode<()>>(x: &T) -> Result<Data, String> {
    let raw = minicbor::to_vec(x).map_err(|e| e.to_string())?;
    cbor::data(&raw).map_err(|e| format!("pallas's encoding {}: {e}", hex::encode(&raw)))
}

/// The script of a script reference's CDDL form, its arm and its body.
fn script(reference: Data) -> Result<Script, String> {
    let Data::List(mut parts) = reference else {
        return Err(format!("{reference} is not a script reference"));
    };
    if parts.len() != 2 {
        return Err("a script reference is an arm and a body".to_owned());
    }
    let body = parts.pop().expect("two parts");
    match small(&parts[0])? {
        0 => Ok(Script::Native(body)),
        arm => Ok(Script::Plutus(arm, bytes(body)?)),
    }
}

/// Pallas's reading of one output.
fn pallas_output(x: &q::TransactionOutput) -> Result<Output, String> {
    let (address, amount, datum, script_ref) = match x {
        q::TransactionOutput::Legacy(o) => (
            &o.address,
            &o.amount,
            match &o.datum_hash {
                None => Datum::None,
                Some(h) => Datum::Hash(h.to_vec()),
            },
            None,
        ),
        q::TransactionOutput::Current(o) => (
            &o.address,
            &o.amount,
            match &o.inline_datum {
                None => Datum::None,
                Some(q::DatumOption::Hash(h)) => Datum::Hash(h.to_vec()),
                Some(q::DatumOption::Data(d)) => Datum::Inline(encoded(&d.0)?),
            },
            o.script_ref.as_ref(),
        ),
    };
    let (coin, assets) = match amount {
        q::Value::Coin(c) => (uint(u64::from(c)), BTreeMap::new()),
        q::Value::Multiasset(c, policies) => (
            uint(u64::from(c)),
            policies
                .iter()
                .map(|(policy, names)| {
                    let names = names
                        .iter()
                        .map(|(name, n)| (name.to_vec(), uint(u64::from(n))))
                        .collect();
                    (policy.to_vec(), names)
                })
                .collect(),
        ),
    };
    Ok(Output {
        address: address.to_vec(),
        coin,
        assets,
        datum,
        script: match script_ref {
            None => Script::None,
            Some(s) => script(encoded(&s.0)?)?,
        },
    })
}

/// Pallas's reading of one Dijkstra output.
fn dijkstra_output(x: &q::dijkstra::TransactionOutput) -> Result<Output, String> {
    match x {
        q::dijkstra::TransactionOutput::Legacy(o) => {
            pallas_output(&q::TransactionOutput::Legacy(o.clone()))
        }
        q::dijkstra::TransactionOutput::Current(o) => {
            let mut output = pallas_output(&q::TransactionOutput::Current(
                q::PostAlonsoTransactionOutput {
                    address: o.address.clone(),
                    amount: o.amount.clone(),
                    inline_datum: o.inline_datum.clone(),
                    script_ref: None,
                },
            ))?;
            if let Some(s) = &o.script_ref {
                output.script = script(encoded(&s.0)?)?;
            }
            Ok(output)
        }
    }
}

/// Pallas's reading of a UTxO reply of an era.
fn pallas_reply(raw: &[u8], era: Era) -> Result<Reply, String> {
    let input = |i: &q::UTxO| TxIn {
        tx: *i.transaction_id,
        ix: u64::from(&i.index),
    };
    let read = match era {
        Era::Conway => {
            let utxos: q::UTxOByAddress = minicbor::decode(raw).map_err(|e| e.to_string())?;
            utxos
                .iter()
                .map(|(i, output)| Ok((input(i), pallas_output(output)?)))
                .collect()
        }
        Era::Dijkstra => {
            let utxos: q::dijkstra::UTxOByAddress =
                minicbor::decode(raw).map_err(|e| e.to_string())?;
            utxos
                .iter()
                .map(|(i, output)| Ok((input(i), dijkstra_output(output)?)))
                .collect()
        }
    };
    Ok(Reply::of(read))
}

/// The bytes of a `{"cbor": hex}` leaf of a record.
fn leaf(v: Option<&Json>) -> Result<Vec<u8>, String> {
    v.and_then(|v| v.get("cbor"))
        .and_then(Json::as_str)
        .and_then(|h| hex::decode(h).ok())
        .ok_or_else(|| format!("{v:?} is not a cbor leaf"))
}

/// The data value of a `{"cbor": hex}` leaf of a record.
fn leaf_data(v: Option<&Json>) -> Result<Data, String> {
    let raw = leaf(v)?;
    cbor::data(&raw).map_err(|e| format!("leaf {}: {e}", hex::encode(&raw)))
}

/// The text of field `name` of a record object.
fn text<'a>(v: &'a Json, name: &str) -> Result<&'a str, String> {
    v.get(name)
        .and_then(Json::as_str)
        .ok_or_else(|| format!("{v} has no text {name}"))
}

/// The list in field `name` of a record object.
fn list<'a>(v: &'a Json, name: &str) -> Result<&'a [Json], String> {
    v.get(name)
        .and_then(Json::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("{v} has no list {name}"))
}

/// The two items of a record's `[key, value]` pair.
fn pair(v: &Json) -> Result<(&Json, &Json), String> {
    match v.as_array().map(Vec::as_slice) {
        Some([k, v]) => Ok((k, v)),
        _ => Err(format!("{v} is not a pair")),
    }
}

/// The integer of a decimal text, a negative one being minus one minus its
/// magnitude.
fn decimal(text: &str) -> Result<Int, String> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, text),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("{text:?} is not a decimal integer"));
    }
    let mut magnitude: Vec<u8> = Vec::new();
    for digit in digits.bytes().map(|b| u32::from(b - b'0')) {
        let mut carry = digit;
        for byte in magnitude.iter_mut().rev() {
            let n = u32::from(*byte) * 10 + carry;
            *byte = (n & 0xff) as u8;
            carry = n >> 8;
        }
        while carry > 0 {
            magnitude.insert(0, (carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    if negative {
        let Some(last) = magnitude.iter().rposition(|b| *b != 0) else {
            return Err(format!("{text:?} is minus zero"));
        };
        magnitude[last] -= 1;
        for byte in &mut magnitude[last + 1..] {
            *byte = 0xff;
        }
    }
    Ok(Int::new(negative, &magnitude))
}

/// The data value of a record's tagged Plutus data.
fn tagged_data(v: &Json) -> Result<Data, String> {
    let items = |name: &str| -> Result<Vec<Data>, String> {
        list(v, name)?.iter().map(tagged_data).collect()
    };
    match text(v, "tag")? {
        "constr" => {
            let ix = text(v, "ix")?;
            let ix = ix
                .parse()
                .map_err(|_| format!("{ix:?} is no constructor"))?;
            Ok(Data::Constr(ix, items("fields")?))
        }
        "map" => Ok(Data::Map(
            list(v, "entries")?
                .iter()
                .map(|e| {
                    let (k, v) = pair(e)?;
                    Ok((tagged_data(k)?, tagged_data(v)?))
                })
                .collect::<Result<_, String>>()?,
        )),
        "list" => Ok(Data::List(items("items")?)),
        "int" => Ok(Data::Int(decimal(text(v, "v")?)?)),
        "bytes" => hex::decode(text(v, "v")?)
            .map(Data::Bytes)
            .map_err(|e| e.to_string()),
        other => Err(format!("{other:?} is no Plutus data")),
    }
}

/// The CDDL form of a record's tagged native script, its arm and fields.
fn tagged_native(v: &Json) -> Result<Data, String> {
    let scripts = || -> Result<Data, String> {
        Ok(Data::List(
            list(v, "scripts")?
                .iter()
                .map(tagged_native)
                .collect::<Result<_, _>>()?,
        ))
    };
    let parts = match text(v, "tag")? {
        "sig" => vec![uint(0), leaf_data(v.get("hash"))?],
        "all" => vec![uint(1), scripts()?],
        "any" => vec![uint(2), scripts()?],
        "m_of" => vec![uint(3), Data::Int(decimal(text(v, "m")?)?), scripts()?],
        "start" => vec![uint(4), leaf_data(v.get("slot"))?],
        "expire" => vec![uint(5), leaf_data(v.get("slot"))?],
        "guard" => vec![uint(6), leaf_data(v.get("cred"))?],
        other => return Err(format!("{other:?} is no native script")),
    };
    Ok(Data::List(parts))
}

/// The ledger's reading of one output, as its records write it.
fn ledger_output(o: &Json) -> Result<Output, String> {
    let assets = list(o.get("assets").ok_or("no assets")?, "map")?
        .iter()
        .map(|p| {
            let (policy, names) = pair(p)?;
            let names = list(names, "map")?
                .iter()
                .map(|n| {
                    let (name, n) = pair(n)?;
                    Ok((bytes(leaf_data(Some(name))?)?, leaf_data(Some(n))?))
                })
                .collect::<Result<_, String>>()?;
            Ok((bytes(leaf_data(Some(policy))?)?, names))
        })
        .collect::<Result<_, String>>()?;
    let datum = match o.get("datum").ok_or("no datum")? {
        Json::Null => Datum::None,
        d => match text(d, "tag")? {
            "hash" => Datum::Hash(bytes(leaf_data(d.get("hash"))?)?),
            "inline" => Datum::Inline(tagged_data(d.get("data").ok_or("no data")?)?),
            other => return Err(format!("{other:?} is no datum")),
        },
    };
    let script = match o.get("script").ok_or("no script")? {
        Json::Null => Script::None,
        s => match text(s, "tag")? {
            "native" => Script::Native(tagged_native(s.get("script").ok_or("no script")?)?),
            tag => {
                let version = tag
                    .strip_prefix("plutus_v")
                    .and_then(|n| n.parse().ok())
                    .ok_or_else(|| format!("{tag:?} is no script"))?;
                let body = hex::decode(text(s, "bytes")?).map_err(|e| e.to_string())?;
                Script::Plutus(version, body)
            }
        },
    };
    Ok(Output {
        address: bytes(leaf_data(o.get("address"))?)?,
        coin: leaf_data(o.get("coin"))?,
        assets,
        datum,
        script,
    })
}

/// The ledger's reading of a UTxO reply, as its records write it.
fn ledger_reply(dump: &Json) -> Reply {
    let read = list(dump, "utxos").and_then(|utxos| {
        utxos
            .iter()
            .map(|u| {
                let input = cbor::txin(&leaf(u.get("input"))?).map_err(|e| e.to_string())?;
                Ok((input, ledger_output(u.get("output").ok_or("no output")?)?))
            })
            .collect()
    });
    Reply::of(read.map_err(|e| format!("the record: {e}")))
}

/// Pallas's reading of the reply a recorded change was made to.
#[derive(Debug, PartialEq)]
enum Base {
    Reads,
    Refuses(String),
    Unrecorded,
}

/// A refusal by pallas, with its reading of the reply the change was made to.
#[derive(Debug, PartialEq)]
struct Refusal {
    error: String,
    base: Base,
}

/// The bytes of one recorded reply, of the reply its change was made to, and
/// the era its class names.
struct Encoded {
    bytes: Vec<u8>,
    base: Option<Vec<u8>>,
    era: Option<Era>,
}

/// Pallas's reading of one record, or a refusal with its reading of the
/// record's base.
fn reading(input: &Encoded) -> Result<Reply, Refusal> {
    let Some(era) = input.era else {
        return Err(Refusal {
            error: "the class names no era".into(),
            base: Base::Unrecorded,
        });
    };
    let error = match pallas_reply(&input.bytes, era) {
        Ok(reply) => return Ok(reply),
        Err(error) => error,
    };
    let base = match &input.base {
        None => Base::Unrecorded,
        Some(raw) => match catch(AssertUnwindSafe(|| pallas_reply(raw, era))) {
            Ok(Ok(_)) => Base::Reads,
            Ok(Err(e)) | Err(e) => Base::Refuses(e),
        },
    };
    Err(Refusal { error, base })
}

/// Pallas's decoding of local state query UTxO replies.
struct Networking;

impl ComparedLayer for Networking {
    const LAYER: Layer = Layer::Networking;
    const TIME_LIMIT: Duration = Duration::from_secs(60);
    type Input = Encoded;
    type Axis = WholeInput;
    type Value = Reply;
    type Refusal = Refusal;

    fn input(&self, row: &Row) -> Encoded {
        Encoded {
            bytes: row.input.clone(),
            base: row.base.clone(),
            era: Era::of_class(&row.class),
        }
    }

    fn reference(&self, row: &Row) -> Expected<WholeInput, Reply> {
        let reference = match &row.reference {
            Reference::Accepts(accepted) => Reference::Accepts(ledger_reply(&accepted.dump)),
            Reference::Refuses(reason) => Reference::Refuses(reason.clone()),
        };
        Expected::one(WholeInput, reference)
    }

    fn run(&self, input: &Encoded) -> Vec<(WholeInput, Result<Reply, Refusal>)> {
        vec![(WholeInput, reading(input))]
    }

    /// A refusal has the ledger's reason when pallas reads the reply the
    /// change was made to.
    fn same_reason(&self, _: &str, refusal: &Refusal) -> bool {
        refusal.base == Base::Reads
    }

    fn allowance(&self, _: &Key<WholeInput>) -> Option<Allowance<Reply, Refusal>> {
        None
    }

    fn detail(
        &self,
        row: &Row,
        reference: &Reference<Reply>,
        answer: &Answer<Reply, Refusal>,
    ) -> String {
        let at = format!("{} {}", row.class, row.variant.as_deref().unwrap_or(""));
        match (reference, answer) {
            (Reference::Accepts(Reply::Read(want)), Answer::Accepts(Reply::Read(got))) => {
                match want.iter().zip(got).find(|(w, g)| w != g) {
                    Some((w, g)) => format!("{at}: the ledger reads {w:?}, pallas {g:?}"),
                    None => format!(
                        "{at}: the ledger reads {} outputs, pallas {}",
                        want.len(),
                        got.len()
                    ),
                }
            }
            (Reference::Accepts(Reply::Unreadable(e)), _) => format!("{at}: {e}"),
            (_, Answer::Accepts(Reply::Unreadable(e))) => format!("{at}: pallas reads {e}"),
            (_, Answer::Refuses(Refusal { error, base })) => {
                format!("{at}: pallas refuses: {error}, its base: {base:?}")
            }
            (_, Answer::Panics(m)) => format!("{at}: pallas panics: {m}"),
            (Reference::Refuses(r), Answer::Accepts(_)) => {
                let r: String = r.chars().take(160).collect();
                format!("{at}: the ledger refuses ({r}) and pallas accepts")
            }
        }
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
                Layer::Networking
            )
        })
        .collect()
}

/// Each reason the records in `dir` fail the comparison over `kinds`.
fn failures(dir: &Path, kinds: &[String]) -> Vec<String> {
    let records = match Records::read_dir(dir, Layer::Networking) {
        Ok(records) => records,
        Err(e) => return vec![format!("{}: {e}", Layer::Networking)],
    };
    let classes = classes();
    let mut out = records.check(Layer::Networking, &classes);
    out.extend(unknown_classes(records.classes(), &classes));
    let judged = comparison::compare(&Arc::new(Networking), &records, kinds);
    let mut examples = BTreeMap::new();
    for j in judged
        .judgements
        .iter()
        .filter(|j| j.outcome != Outcome::Agree)
    {
        examples.entry(&j.row.class).or_insert(&j.detail);
    }
    println!("records: {}", records.generator());
    println!("{judged}");
    examples.values().for_each(|e| println!("{e}"));
    out.extend(judged.failures());
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
fn pallas_reads_the_utxo_replies_the_ledger_writes() {
    let outcome = std::thread::Builder::new()
        .stack_size(1 << 28)
        .spawn(run)
        .expect("spawn")
        .join();
    if let Err(e) = outcome {
        std::panic::resume_unwind(e);
    }
}

/// A directory named by a pin, holding `text` as the networking records
/// unless it is `None`.
fn records_at(name: &str, text: Option<&str>) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("networking-{name}"))
        .join("0".repeat(40));
    std::fs::create_dir_all(&dir).expect("create the records directory");
    let file = dir.join("networking.jsonl");
    match text {
        Some(text) => std::fs::write(&file, text).expect("write the records"),
        None => {
            let _ = std::fs::remove_file(&file);
        }
    }
    dir
}

#[test]
fn a_missing_records_file_fails() {
    let failures = failures(&records_at("missing", None), &[]);
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].starts_with("networking: "), "{failures:?}");
    assert!(failures[0].contains("networking.jsonl"), "{failures:?}");
}

#[test]
fn an_empty_records_file_fails() {
    let failures = failures(&records_at("empty", Some("")), &[]);
    assert_eq!(failures, ["networking: line 1: no header"]);
}

#[test]
fn a_class_the_check_does_not_know_fails() {
    let text = format!(
        "{}\n{}\n",
        r#"{"reference":"0000000000000000000000000000000000000000","generator":"a probe","layer":"networking","classes":["dijkstra-utxo trailing-bytes"]}"#,
        r#"{"class":"dijkstra-utxo trailing-bytes","input":"a0","reference":{"refuses":"a probe row, not a reference answer"}}"#,
    );
    let failures = failures(&records_at("unknown-class", Some(&text)), &[]);
    let unknown = "networking: class \"dijkstra-utxo trailing-bytes\" is not one this check knows";
    assert!(failures.iter().any(|f| f == unknown), "{failures:?}");
    let unnamed = failures
        .iter()
        .filter(|f| f.contains("the records name no kind or class"))
        .count();
    assert_eq!(unnamed, classes().len(), "{failures:?}");
}

#[test]
fn the_known_classes_pass_the_class_check() {
    let classes = classes();
    assert_eq!(classes.len(), Era::ALL.len() * FORMS.len());
    assert_eq!(unknown_classes(&classes, &classes), Vec::<String>::new());
}

#[test]
fn a_class_is_read_with_the_era_it_names() {
    assert_eq!(Era::of_class("babbage-utxo map-output"), None);
    assert_eq!(Era::of_class("dijkstra map-output"), None);
    assert_eq!(Era::of_class("conway-utxo map-output"), Some(Era::Conway));
    assert_eq!(
        Era::of_class("dijkstra-utxo native-require-guard"),
        Some(Era::Dijkstra)
    );
    let unnamed = Encoded {
        bytes: vec![0xa0],
        base: None,
        era: None,
    };
    assert_eq!(
        reading(&unnamed).unwrap_err().error,
        "the class names no era"
    );
}

#[test]
fn an_unreadable_reply_equals_nothing() {
    let a = Reply::Unreadable("x".to_owned());
    let b = Reply::Unreadable("x".to_owned());
    assert_ne!(a, b);
    assert_ne!(Reply::Read(Vec::new()), a);
    assert_eq!(Reply::Read(Vec::new()), Reply::Read(Vec::new()));
}

/// The data value pallas reads and encodes from the Plutus data `raw`.
fn pallas_data(raw: &str) -> Data {
    let raw = hex::decode(raw).expect("hex");
    let datum: q::PlutusData = minicbor::decode(&raw).expect("pallas reads the datum");
    encoded(&datum).expect("pallas's encoding reads back")
}

/// The script pallas reads from the script reference `raw`.
fn pallas_script(raw: &str) -> Script {
    use pallas::network::miniprotocols::localtxsubmission::primitives::ScriptRef;
    let raw = hex::decode(raw).expect("hex");
    let reference: ScriptRef = minicbor::decode(&raw).expect("pallas reads the script");
    script(encoded(&reference).expect("pallas's encoding reads back")).expect("a script")
}

#[test]
fn equal_integers_read_equal_from_both_sides() {
    let cases = [
        ("-1", "20"),
        ("0", "00"),
        ("23", "17"),
        ("-256", "38ff"),
        ("18446744073709551615", "1bffffffffffffffff"),
        ("18446744073709551616", "c249010000000000000000"),
        ("-18446744073709551617", "c349010000000000000000"),
    ];
    for (decimal, raw) in cases {
        let tagged = tagged_data(&serde_json::json!({"tag": "int", "v": decimal})).expect(decimal);
        assert_eq!(
            tagged,
            cbor::data(&hex::decode(raw).expect("hex")).expect(raw),
            "{decimal}"
        );
        assert_eq!(tagged, pallas_data(raw), "{decimal}");
    }
}

#[test]
fn unequal_integers_read_unequal() {
    let one = tagged_data(&serde_json::json!({"tag": "int", "v": "1"})).expect("one");
    assert_ne!(one, pallas_data("20"));
    assert_ne!(one, pallas_data("00"));
}

#[test]
fn constructors_read_equal_whatever_their_tag() {
    let cases = [
        ("0", "d87980"),
        ("6", "d87f80"),
        ("7", "d9050080"),
        ("127", "d9057880"),
        ("200", "d8668218c880"),
    ];
    for (ix, raw) in cases {
        let tagged =
            tagged_data(&serde_json::json!({"tag": "constr", "ix": ix, "fields": []})).expect(ix);
        assert_eq!(
            tagged,
            cbor::data(&hex::decode(raw).expect("hex")).expect(raw),
            "{ix}"
        );
        assert_eq!(tagged, pallas_data(raw), "{ix}");
    }
    let other = tagged_data(&serde_json::json!({"tag": "constr", "ix": "1", "fields": []}));
    assert_ne!(other.expect("one"), pallas_data("d87980"));
}

#[test]
fn nested_data_reads_equal_from_both_sides() {
    let tagged = serde_json::json!({"tag": "map", "entries": [[
        {"tag": "bytes", "v": "ab"},
        {"tag": "list", "items": [{"tag": "int", "v": "5"}]},
    ]]});
    let raw = "a141ab9f05ff";
    assert_eq!(tagged_data(&tagged).expect("map"), pallas_data(raw));
}

#[test]
fn native_scripts_read_equal_from_both_sides() {
    let key = "11".repeat(28);
    let sig = serde_json::json!({"tag": "sig", "hash": {"cbor": format!("581c{key}")}});
    let cases = [
        (sig.clone(), format!("82008200581c{key}")),
        (
            serde_json::json!({"tag": "m_of", "m": "1", "scripts": [sig.clone()]}),
            format!("8200830301818200581c{key}"),
        ),
        (
            serde_json::json!({"tag": "any", "scripts": [
                {"tag": "start", "slot": {"cbor": "1864"}},
                {"tag": "expire", "slot": {"cbor": "1a00010000"}},
            ]}),
            "82008202828204186482051a00010000".to_owned(),
        ),
    ];
    for (tagged, raw) in cases {
        let want = Script::Native(tagged_native(&tagged).expect("a native script"));
        assert_eq!(want, pallas_script(&raw), "{raw}");
    }
    let all = serde_json::json!({"tag": "all", "scripts": [sig]});
    let all = Script::Native(tagged_native(&all).expect("a native script"));
    assert_ne!(all, pallas_script(&format!("82008202818200581c{key}")));
}

#[test]
fn plutus_scripts_read_their_version() {
    assert_eq!(
        pallas_script("820243010203"),
        Script::Plutus(2, vec![1, 2, 3])
    );
    let tagged = serde_json::json!({"tag": "plutus_v3", "bytes": "010203"});
    let output = serde_json::json!({
        "address": {"cbor": "4100"},
        "coin": {"cbor": "01"},
        "assets": {"map": []},
        "datum": null,
        "script": tagged,
    });
    let read = ledger_output(&output).expect("an output");
    assert_eq!(read.script, Script::Plutus(3, vec![1, 2, 3]));
    assert_ne!(read.script, pallas_script("820243010203"));
}

#[test]
fn a_malformed_record_reads_as_unreadable() {
    let cases = [
        serde_json::json!({}),
        serde_json::json!({"utxos": [{"input": {"cbor": "zz"}, "output": {}}]}),
        serde_json::json!({"utxos": [{"input": {"cbor": "825820"}, "output": {}}]}),
    ];
    for dump in cases {
        assert!(
            matches!(ledger_reply(&dump), Reply::Unreadable(_)),
            "{dump}"
        );
    }
}
