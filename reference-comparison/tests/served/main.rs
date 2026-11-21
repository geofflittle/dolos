//! Dijkstra transactions submitted to Dolos compared with the ledger's reading
//! of them.

#[path = "../common/mod.rs"]
mod common;

use std::sync::Arc;
use std::time::Duration;

use common::classes::{self, ROOTS};
use common::refusal::{self, Refusal, same_reason};
use dolos_cardano::pallas::ledger::traverse::Era;
use dolos_cardano::validate::decode_submission;
use ledger_oracle_judge::comparison::{
    self, Allowance, Answer, ComparedLayer, Disagreement, Expected, Key, Layer, Outcome, Place,
    PlaceCoverage, Reference, Row, WholeInput, judge, kinds_from_env, selected,
};
use serde_json::Value;

/// The records class prefix of a whole transaction.
const TX: &str = "tx";

/// A transaction id.
type Id = Vec<u8>;

/// The bytes of one submitted transaction and of the transaction the
/// record's change was made to.
struct Submission {
    bytes: Vec<u8>,
    base: Option<Vec<u8>>,
}

/// The id of the transaction Dolos admits from `bytes`.
fn submit(bytes: &[u8]) -> Result<Id, String> {
    decode_submission(Era::Dijkstra, bytes)
        .map(|tx| tx.hash().to_vec())
        .map_err(|e| e.to_string())
}

/// Dolos's reading of one submission, a refusal with its reading of the
/// record's base.
fn reading(input: &Submission) -> Result<Id, Refusal> {
    let error = match submit(&input.bytes) {
        Ok(id) => return Ok(id),
        Err(error) => error,
    };
    let base = refusal::base(input.base.as_deref(), |bytes| submit(bytes).map(|_| ()));
    Err(Refusal { error, base })
}

/// The transaction id in the ledger's reading `dump` of `row`.
fn ledger_id(row: &Row, dump: &Value) -> Id {
    let at = format!("{} {}", row.class, row.variant.as_deref().unwrap_or(""));
    let hex = dump["id"]["cbor"]
        .as_str()
        .unwrap_or_else(|| panic!("{at}: the ledger's reading has no id"));
    let cbor = hex::decode(hex).unwrap_or_else(|e| panic!("{at}: the id is not hex: {e}"));
    match cbor.strip_prefix(&[0x58, 0x20]) {
        Some(id) if id.len() == 32 => id.to_vec(),
        _ => panic!("{at}: the id {hex} is not 32 bytes"),
    }
}

/// Dolos's decoding of a transaction submitted in Dijkstra.
struct Serving;

impl ComparedLayer for Serving {
    const LAYER: Layer = Layer::Served;
    const RECORDS: Layer = Layer::Decode;
    const TIME_LIMIT: Duration = Duration::from_secs(60);
    type Input = Submission;
    type Axis = WholeInput;
    type Value = Id;
    type Refusal = Refusal;

    fn input(&self, row: &Row) -> Submission {
        Submission {
            bytes: row.input.clone(),
            base: row.base.clone(),
        }
    }

    fn reference(&self, row: &Row) -> Expected<WholeInput, Id> {
        let reference = match &row.reference {
            Reference::Accepts(accepted) => Reference::Accepts(ledger_id(row, &accepted.dump)),
            Reference::Refuses(reason) => Reference::Refuses(reason.clone()),
        };
        Expected::one(WholeInput, reference)
    }

    fn run(&self, input: &Submission) -> Vec<(WholeInput, Result<Id, Refusal>)> {
        vec![(WholeInput, reading(input))]
    }

    fn same_reason(&self, reason: &str, refusal: &Refusal) -> bool {
        same_reason(reason, refusal)
    }

    fn allowance(&self, _: &Key<WholeInput>) -> Option<Allowance<Id, Refusal>> {
        None
    }
}

fn run() {
    let cddl = common::defs();
    let records = common::records(&cddl, Layer::Decode);
    let kinds = kinds_from_env();
    let declared: Vec<String> = records
        .classes()
        .iter()
        .filter(|c| c.split(' ').next() == Some(TX) && selected(c, &kinds))
        .cloned()
        .collect();
    assert!(
        !declared.is_empty(),
        "the records declare no {TX} class of the kinds {kinds:?}"
    );
    let judged = comparison::compare(&Arc::new(Serving), &records, &declared);
    let (_, root) = ROOTS
        .iter()
        .find(|(prefix, _)| *prefix == TX)
        .expect("a root for whole transactions");
    let derived: Vec<Place> = classes::places(&classes::parse(&cddl), &[root])
        .into_iter()
        .filter(|p| selected(&classes::class(p), &kinds))
        .collect();
    let changed = records
        .rows()
        .iter()
        .filter(|r| declared.contains(&r.class));
    let coverage = PlaceCoverage::new(Layer::Served, &derived, changed);
    println!("records: {}", records.generator());
    println!("{judged}");
    print!("{coverage}");
    print!("{}", classes::report(&coverage));
    let mut failures = judged.failures();
    failures.extend(coverage.failures());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn dolos_admits_what_the_ledger_reads() {
    run();
}

/// The outcome of a submission of `input`, a change to `base`, that the
/// reference refuses.
fn refused_change(input: &[u8], base: Option<&[u8]>) -> Outcome {
    let input = Submission {
        bytes: input.to_vec(),
        base: base.map(<[u8]>::to_vec),
    };
    let answer = match reading(&input) {
        Ok(id) => Answer::Accepts(id),
        Err(refusal) => Answer::Refuses(refusal),
    };
    judge(
        &Reference::Refuses("a reason".into()),
        &answer,
        same_reason,
        None,
    )
}

#[test]
fn trailing_bytes_after_a_transaction_are_refused() {
    let (tx, _) = ledger_tx();
    let mut trailing = tx.clone();
    trailing.push(0x00);
    assert_eq!(refused_change(&trailing, Some(&tx)), Outcome::Agree);
}

#[test]
fn a_refused_change_to_a_transaction_dolos_refuses_disagrees() {
    assert_eq!(
        refused_change(&[0xff], Some(&[0xff])),
        Outcome::Disagree(Disagreement::OtherReason)
    );
    assert_eq!(
        refused_change(&[0xff], None),
        Outcome::Disagree(Disagreement::OtherReason)
    );
}

#[test]
fn a_transaction_the_ledger_reads_has_the_ledger_id() {
    let (tx, id) = ledger_tx();
    assert_eq!(submit(&tx), Ok(id));
}

/// The bytes and the ledger's id of the first transaction the ledger reads in
/// the records.
fn ledger_tx() -> (Vec<u8>, Id) {
    let records = common::records(&common::defs(), Layer::Decode);
    records
        .rows()
        .iter()
        .find_map(|r| match &r.reference {
            Reference::Accepts(accepted) if r.class == TX => {
                Some((r.input.clone(), ledger_id(r, &accepted.dump)))
            }
            _ => None,
        })
        .expect("an accepted transaction")
}
