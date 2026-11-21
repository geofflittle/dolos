//! The comparison driver over a stand in layer, whose implementation does what
//! each input's bytes say, and over the records fixtures of `tests/records`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ledger_oracle_judge::comparison::{
    compare, Allowance, Allowed, Answer, ComparedLayer, Counts, Disagreement, Expected, Key, Layer,
    LayerRun, Outcome, Owner, Records, Reference, Row, WholeInput,
};
use serde_json::Value;

const PIN: &str = "9df9f98cce86c5f2195142b2c02dd9b39dd9dc7c";

/// The difference the stand in lets pass for classes of the value kind
/// `allowed`.
const OWNED: Allowed = Allowed::new(
    "an owned refusal",
    Disagreement::Refuses,
    Owner::Check {
        layer: Layer::Decode,
        test: "interface stand in",
    },
);

/// A layer whose implementation reads its input as an instruction: `00 n`
/// accepts `n`, `01 c` refuses with `r` and `c`, `02` panics, `03` sleeps past
/// the time limit, `04 n` makes `n` observations, `05` makes none, `06` makes
/// one under an axis the reference has not and `07` refuses as `owned`.
struct StandIn;

impl ComparedLayer for StandIn {
    const LAYER: Layer = Layer::Decode;
    const TIME_LIMIT: Duration = Duration::from_millis(500);
    type Input = Vec<u8>;
    type Axis = u8;
    type Value = u64;
    type Refusal = String;

    fn input(&self, row: &Row) -> Vec<u8> {
        row.input.clone()
    }

    fn reference(&self, row: &Row) -> Expected<u8, u64> {
        let dump = match &row.reference {
            Reference::Refuses(reason) => {
                return Expected::one(0, Reference::Refuses(reason.clone()));
            }
            Reference::Accepts(accepted) => &accepted.dump,
        };
        match dump {
            Value::Number(n) => Expected::one(0, Reference::Accepts(n.as_u64().unwrap())),
            Value::Array(values) => {
                let mut all = values
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (i as u8, Reference::Accepts(v.as_u64().unwrap())));
                let first = all.next().unwrap();
                Expected::Judged {
                    first,
                    rest: all.collect(),
                }
            }
            _ => Expected::NotJudged,
        }
    }

    fn run(&self, input: &Vec<u8>) -> Vec<(u8, Result<u64, String>)> {
        match input[..] {
            [0, n] => vec![(0, Ok(u64::from(n)))],
            [1, c] => vec![(0, Err(format!("r{c}")))],
            [2] => panic!("the stand in panics"),
            [3] => {
                std::thread::sleep(Duration::from_secs(5));
                vec![(0, Ok(0))]
            }
            [4, n] => (0..n).map(|i| (i, Ok(u64::from(i)))).collect(),
            [5] => Vec::new(),
            [6] => vec![(0, Ok(0)), (9, Ok(0))],
            [7] => vec![(0, Err("owned".to_owned()))],
            _ => panic!("no instruction {input:02x?}"),
        }
    }

    fn same_reason(&self, reason: &str, refusal: &String) -> bool {
        reason == refusal
    }

    fn allowance(&self, _: &Row, key: &Key<u8>) -> Option<Allowance<u64, String>> {
        (key.value_kind() == "allowed").then_some(Allowance {
            allowed: OWNED,
            shown: |answer| matches!(answer, Answer::Refuses(r) if r == "owned"),
        })
    }
}

/// Records of the stand in's layer declaring `classes`, one row per class,
/// input hex and reference answer.
fn records(classes: &[&str], rows: &[(&str, &str, &str)]) -> Records {
    let header = serde_json::json!({
        "reference": PIN,
        "generator": "interface test",
        "layer": "decode",
        "classes": classes,
    });
    let mut text = format!("{header}\n");
    for (class, input, reference) in rows {
        let reference = match reference.strip_prefix("refuses ") {
            Some(reason) => format!(r#"{{"refuses":"{reason}"}}"#),
            None => format!(r#"{{"accepts":{reference},"encoding":"00"}}"#),
        };
        text.push_str(&format!(
            r#"{{"class":"{class}","input":"{input}","reference":{reference}}}"#
        ));
        text.push('\n');
    }
    Records::read(&text, PIN).unwrap()
}

/// The stand in's run over every row of `records`.
fn one(records: &Records) -> LayerRun<'_, u8> {
    compare(&Arc::new(StandIn), records, &[])
}

/// The counts of `run` under `class` and `axis`.
fn counts<'r>(run: &'r LayerRun<'_, u8>, class: &str, axis: u8) -> &'r Counts {
    let key = Key {
        class: class.to_owned(),
        axis,
    };
    run.tally.counts(&key).unwrap()
}

/// The outcome of the only judgement of `run`.
fn outcome(run: &LayerRun<'_, u8>) -> Outcome {
    assert_eq!(run.judgements.len(), 1, "{:?}", run.judgements);
    run.judgements[0].outcome.clone()
}

#[test]
fn equal_values_agree() {
    let r = records(&["equal"], &[("equal", "0005", "5")]);
    let run = one(&r);
    assert_eq!(outcome(&run), Outcome::Agree);
    assert_eq!(
        (
            counts(&run, "equal", 0).inputs,
            counts(&run, "equal", 0).agree
        ),
        (1, 1)
    );
    assert_eq!(run.failures(), Vec::<String>::new());
}

#[test]
fn unequal_values_disagree() {
    let r = records(&["unequal"], &[("unequal", "0006", "5")]);
    let run = one(&r);
    assert_eq!(outcome(&run), Outcome::Disagree(Disagreement::Values));
    assert_eq!(
        run.failures(),
        ["decode: class \"unequal 0\" disagrees (values) 1"]
    );
}

#[test]
fn a_refusal_of_an_accepted_input_disagrees() {
    let r = records(&["refused"], &[("refused", "0101", "5")]);
    let run = one(&r);
    assert_eq!(outcome(&run), Outcome::Disagree(Disagreement::Refuses));
    assert_eq!(
        run.failures(),
        ["decode: class \"refused 0\" disagrees (refuses) 1"]
    );
}

#[test]
fn a_panic_on_an_accepted_input_counts_as_a_panic() {
    let r = records(&["panicked"], &[("panicked", "02", "5")]);
    let run = one(&r);
    assert_eq!(outcome(&run), Outcome::Panic("the stand in panics".into()));
    assert_eq!(counts(&run, "panicked", 0).panic, 1);
    assert_eq!(run.failures(), ["decode: class \"panicked 0\" panics 1"]);
}

#[test]
fn an_acceptance_of_a_refused_input_disagrees() {
    let r = records(&["accepted"], &[("accepted", "0005", "refuses r1")]);
    let run = one(&r);
    assert_eq!(outcome(&run), Outcome::Disagree(Disagreement::Accepts));
    assert_eq!(
        run.failures(),
        ["decode: class \"accepted 0\" disagrees (accepts) 1"]
    );
}

#[test]
fn a_refusal_for_the_same_reason_agrees() {
    let r = records(&["same-reason"], &[("same-reason", "0101", "refuses r1")]);
    let run = one(&r);
    assert_eq!(outcome(&run), Outcome::Agree);
    assert_eq!(run.failures(), Vec::<String>::new());
}

#[test]
fn a_refusal_for_another_reason_disagrees() {
    let r = records(&["other-reason"], &[("other-reason", "0102", "refuses r1")]);
    let run = one(&r);
    assert_eq!(outcome(&run), Outcome::Disagree(Disagreement::OtherReason));
    assert_eq!(
        run.failures(),
        ["decode: class \"other-reason 0\" disagrees (other reason) 1"]
    );
}

#[test]
fn a_panic_on_a_refused_input_counts_as_a_panic() {
    let r = records(&["panic-refused"], &[("panic-refused", "02", "refuses r1")]);
    let run = one(&r);
    assert_eq!(outcome(&run), Outcome::Panic("the stand in panics".into()));
    assert_eq!(
        run.failures(),
        ["decode: class \"panic-refused 0\" panics 1"]
    );
}

#[test]
fn an_allowed_class_passes_its_own_disagreement() {
    let r = records(&["allowed"], &[("allowed", "07", "5")]);
    let run = one(&r);
    assert_eq!(outcome(&run), Outcome::Allowed(OWNED));
    assert_eq!(counts(&run, "allowed", 0).allowed[&OWNED], 1);
    assert_eq!(run.failures(), Vec::<String>::new());
}

#[test]
fn an_allowed_class_fails_another_disagreement() {
    let r = records(&["allowed other"], &[("allowed other", "07", "refuses r1")]);
    let run = one(&r);
    assert_eq!(outcome(&run), Outcome::Disagree(Disagreement::OtherReason));
    assert_eq!(
        run.failures(),
        ["decode: class \"allowed other 0\" disagrees (other reason) 1"]
    );
}

#[test]
fn an_allowed_class_fails_without_its_reason_in_the_answer() {
    let r = records(&["allowed unshown"], &[("allowed unshown", "0101", "5")]);
    let run = one(&r);
    assert_eq!(outcome(&run), Outcome::Disagree(Disagreement::Refuses));
    assert_eq!(
        run.failures(),
        ["decode: class \"allowed unshown 0\" disagrees (refuses) 1"]
    );
}

#[test]
fn an_input_over_the_time_limit_times_out() {
    let r = records(
        &["slow", "fast"],
        &[("slow", "03", "0"), ("fast", "0000", "0")],
    );
    let run = one(&r);
    let outcomes: Vec<_> = run.judgements.iter().map(|j| j.outcome.clone()).collect();
    assert_eq!(outcomes, [Outcome::Timeout, Outcome::Agree]);
    assert_eq!(counts(&run, "slow", 0).timeout, 1);
    assert_eq!(run.failures(), ["decode: class \"slow 0\" times out 1"]);
    assert!(run.to_string().contains("slow 0"));
}

#[test]
fn a_run_with_no_observation_fails() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/records/fail/no-rows")
        .join(PIN);
    let r = Records::read_dir(&dir, Layer::Decode).unwrap();
    let run = one(&r);
    assert!(run.judgements.is_empty());
    assert_eq!(run.failures(), ["decode: no observation judged"]);
    assert!(run
        .to_string()
        .contains("decode: 0 observations judged under 0 keys"));
}

#[test]
fn a_declared_class_with_no_observation_fails() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/records/fail/class-without-row")
        .join(PIN);
    let r = Records::read_dir(&dir, Layer::Decode).unwrap();
    let run = compare(&Arc::new(Echo), &r, &[]);
    assert_eq!(run.judgements.len(), 3);
    assert_eq!(
        run.failures(),
        ["decode: class \"body array-short\" has no judged observation"]
    );
}

#[test]
fn an_undeclared_record_class_fails() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/records/fail/undeclared-class")
        .join(PIN);
    let r = Records::read_dir(&dir, Layer::Decode).unwrap();
    let run = compare(&Arc::new(Echo), &r, &[]);
    assert_eq!(run.judgements.len(), 3);
    assert_eq!(
        run.failures(),
        ["decode: class \"tx\" is not declared, 1 inputs"]
    );
}

/// The filter kinds `names`.
fn kinds(names: &[&str]) -> Vec<String> {
    names.iter().map(|k| k.to_string()).collect()
}

/// Records of two classes of one kind of change and one of another.
fn filtered() -> Records {
    records(
        &["x array-extra", "y array-extra", "z map-duplicate"],
        &[
            ("x array-extra", "0001", "1"),
            ("y array-extra", "0001", "1"),
            ("z map-duplicate", "0006", "1"),
        ],
    )
}

#[test]
fn a_filter_kind_with_no_input_fails() {
    let r = filtered();
    let run = compare(
        &Arc::new(StandIn),
        &r,
        &kinds(&["array-extra", "set-duplicate"]),
    );
    assert_eq!(run.judgements.len(), 2);
    assert_eq!(
        run.failures(),
        ["decode: filter kind \"set-duplicate\" has no input"]
    );
}

#[test]
fn a_filter_kind_with_inputs_passes() {
    let r = filtered();
    let run = compare(&Arc::new(StandIn), &r, &kinds(&["array-extra"]));
    let classes: Vec<_> = run
        .judgements
        .iter()
        .map(|j| j.key.class.as_str())
        .collect();
    assert_eq!(classes, ["x array-extra", "y array-extra"]);
    assert_eq!(run.failures(), Vec::<String>::new());
    let run = compare(&Arc::new(StandIn), &r, &kinds(&["z map-duplicate"]));
    assert_eq!(run.judgements.len(), 1);
    assert_eq!(
        run.failures(),
        ["decode: class \"z map-duplicate 0\" disagrees (values) 1"]
    );
}

#[test]
fn an_input_with_several_observations_counts_once_per_key() {
    let r = records(&["several"], &[("several", "0403", "[0, 1, 5]")]);
    let run = one(&r);
    let outcomes: Vec<_> = run.judgements.iter().map(|j| j.outcome.clone()).collect();
    assert_eq!(
        outcomes,
        [
            Outcome::Agree,
            Outcome::Agree,
            Outcome::Disagree(Disagreement::Values)
        ]
    );
    for axis in 0..3 {
        assert_eq!(counts(&run, "several", axis).inputs, 1);
    }
    assert_eq!(run.tally.judged(), (3, 3));
    assert_eq!(
        run.failures(),
        ["decode: class \"several 2\" disagrees (values) 1"]
    );
}

#[test]
fn an_observation_without_its_pair_fails() {
    let r = records(
        &["missing", "extra"],
        &[("missing", "05", "1"), ("extra", "06", "0")],
    );
    let run = one(&r);
    let outcomes: Vec<_> = run
        .judgements
        .iter()
        .map(|j| (j.key.to_string(), j.outcome.clone()))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("missing 0".to_owned(), Outcome::Unpaired),
            ("extra 0".to_owned(), Outcome::Agree),
            ("extra 9".to_owned(), Outcome::Unpaired),
        ]
    );
    assert_eq!(
        run.failures(),
        [
            "decode: class \"extra 9\" has unpaired observations 1",
            "decode: class \"missing 0\" has unpaired observations 1",
        ]
    );
}

#[test]
fn an_input_not_judged_is_counted_apart() {
    let r = records(
        &["judged", "not-judged"],
        &[("judged", "0001", "1"), ("not-judged", "0001", "null")],
    );
    let run = one(&r);
    assert_eq!(run.judgements.len(), 1);
    assert_eq!(run.tally.not_judged_count("not-judged"), Some(1));
    assert_eq!(
        run.failures(),
        ["decode: class \"not-judged\" has no judged observation"]
    );
}

#[test]
fn records_of_another_layer_fail() {
    let text = format!(
        r#"{{"reference":"{PIN}","generator":"g","layer":"validation","classes":["base"]}}
{{"class":"base","input":"0001","reference":{{"accepts":1}}}}
"#
    );
    let r = Records::read(&text, PIN).unwrap();
    let run = one(&r);
    assert!(run.judgements.is_empty());
    assert_eq!(
        run.failures(),
        [
            "the records are for validation, not decode",
            "decode: no observation judged",
            "decode: class \"base\" has no judged observation",
        ]
    );
}

#[test]
fn owner_rule_prints_its_clause() {
    let owner = Owner::Rule { clause: "step 4" };
    assert_eq!(owner.to_string(), "rule: step 4");
    let allowed = Allowed::new("block array-short", Disagreement::Accepts, owner);
    assert_eq!(allowed.to_string(), "block array-short (rule: step 4)");
}

#[test]
fn owner_defect_prints_its_defect() {
    let owner = Owner::Defect {
        defect: "a rational reads any tag",
    };
    assert_eq!(owner.to_string(), "defect: a rational reads any tag");
    let allowed = Allowed::new("body rational-tag", Disagreement::Accepts, owner);
    assert_eq!(
        allowed.to_string(),
        "body rational-tag (defect: a rational reads any tag)"
    );
}

/// The stand in's difference listed under a defect owner, which the stand in
/// never allows.
const DEFECT: Allowed = Allowed::new(
    "an owned refusal",
    Disagreement::Refuses,
    Owner::Defect {
        defect: "refuses as owned",
    },
);

/// Records of a class with one allowed observation, a class with two, a class
/// whose observation agrees and a class with no judged observation.
fn listed_records() -> Records {
    records(
        &["allowed once", "allowed twice", "agreeing", "not-judged"],
        &[
            ("allowed once", "07", "5"),
            ("allowed twice", "07", "5"),
            ("allowed twice", "07", "6"),
            ("agreeing", "0001", "1"),
            ("not-judged", "0001", "null"),
        ],
    )
}

#[test]
fn unused_entry_seen_nowhere_fails() {
    let r = listed_records();
    let run = one(&r);
    let listed = [
        ("agreeing", OWNED),
        ("not-judged", OWNED),
        ("allowed once", DEFECT),
    ];
    assert_eq!(
        run.tally.unused(&listed, r.classes()),
        [
            "decode: listed an owned refusal (decode: interface stand in) in class \"agreeing\" matched no observation",
            "decode: listed an owned refusal (decode: interface stand in) in class \"not-judged\" matched no observation",
            "decode: listed an owned refusal (defect: refuses as owned) in class \"allowed once\" matched no observation",
        ]
    );
}

#[test]
fn unused_entry_seen_once_passes() {
    let r = listed_records();
    let run = one(&r);
    assert_eq!(counts(&run, "allowed once", 0).allowed[&OWNED], 1);
    assert_eq!(counts(&run, "allowed twice", 0).allowed[&OWNED], 2);
    let listed = [("allowed once", OWNED), ("allowed twice", OWNED)];
    assert_eq!(run.tally.unused(&listed, r.classes()), Vec::<String>::new());
}

#[test]
fn unused_undeclared_class_fails() {
    let r = listed_records();
    let run = one(&r);
    let listed = [("allowed once", OWNED), ("allowed thrice", OWNED)];
    assert_eq!(
        run.tally.unused(&listed, r.classes()),
        ["decode: listed class \"allowed thrice\" is not declared"]
    );
}

#[test]
fn unused_class_listed_twice_fails() {
    let r = listed_records();
    let run = one(&r);
    let listed = [("allowed once", OWNED), ("allowed once", OWNED)];
    assert_eq!(
        run.tally.unused(&listed, r.classes()),
        ["decode: class \"allowed once\" is listed twice"]
    );
}

#[test]
fn unused_unselected_class_is_skipped() {
    let r = listed_records();
    let run = compare(&Arc::new(StandIn), &r, &kinds(&["allowed twice"]));
    assert_eq!(run.judgements.len(), 2);
    let listed = [
        ("allowed twice", OWNED),
        ("allowed once", DEFECT),
        ("agreeing", OWNED),
    ];
    assert_eq!(run.tally.unused(&listed, r.classes()), Vec::<String>::new());
}

/// A layer whose implementation answers as the reference does, for the
/// records fixtures.
struct Echo;

impl ComparedLayer for Echo {
    const LAYER: Layer = Layer::Decode;
    const TIME_LIMIT: Duration = Duration::from_secs(5);
    type Input = bool;
    type Axis = WholeInput;
    type Value = ();
    type Refusal = ();

    fn input(&self, row: &Row) -> bool {
        matches!(row.reference, Reference::Refuses(_))
    }

    fn reference(&self, row: &Row) -> Expected<WholeInput, ()> {
        match &row.reference {
            Reference::Accepts(_) => Expected::one(WholeInput, Reference::Accepts(())),
            Reference::Refuses(r) => Expected::one(WholeInput, Reference::Refuses(r.clone())),
        }
    }

    fn run(&self, refuses: &bool) -> Vec<(WholeInput, Result<(), ()>)> {
        vec![(WholeInput, if *refuses { Err(()) } else { Ok(()) })]
    }

    fn same_reason(&self, _: &str, _: &()) -> bool {
        true
    }

    fn allowance(&self, _: &Row, _: &Key<WholeInput>) -> Option<Allowance<(), ()>> {
        None
    }
}
