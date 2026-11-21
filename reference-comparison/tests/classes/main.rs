//! The places derived from a CDDL grammar, checked against the grammar read by
//! hand.

#[path = "../common/mod.rs"]
mod common;

use std::collections::BTreeSet;

use common::classes::{ROOTS, class, parse, places, rule_names};
use ledger_oracle_judge::comparison::{
    ChangeKind as K, Layer, Part, Place, PlaceCoverage, Reference, Row, Step,
};

const WHOLE: [K; 2] = [K::WideHeads, K::Zap];
const RECORD: [K; 2] = [K::ArrayExtra, K::ArrayShort];
const SET: [K; 3] = [K::SetDuplicate, K::SetTwoWays, K::SetReorder];
const RATIONAL: [K; 6] = [
    K::RationalTag,
    K::RationalThird,
    K::RationalUnreduced,
    K::RationalOne,
    K::RationalEmpty,
    K::RationalIndefinite,
];

fn key(rule: &str, key: u64) -> Step {
    Step::Key {
        rule: rule.into(),
        key,
    }
}

fn member(rule: &str, member: Part) -> Step {
    Step::Member {
        rule: rule.into(),
        member,
    }
}

fn arm(rule: &str, arm: Part) -> Step {
    Step::Arm {
        rule: rule.into(),
        arm,
    }
}

fn named(name: &str) -> Part {
    Part::Named(name.into())
}

fn at(root: &str, path: &[Step], kinds: &[K]) -> Vec<Place> {
    kinds
        .iter()
        .map(|kind| Place {
            root: root.into(),
            path: path.to_vec(),
            kind: *kind,
        })
        .collect()
}

/// The places of a grammar under its rule `r`.
fn derive(text: &str) -> BTreeSet<Place> {
    places(&parse(text), &["r"])
}

/// The places `expected` lists, each a path and its kinds under the rule `r`.
fn expect(expected: &[(&[Step], &[K])]) -> BTreeSet<Place> {
    expected
        .iter()
        .flat_map(|(path, kinds)| at("r", path, kinds))
        .collect()
}

/// The places of the Dijkstra grammar under every root.
fn dijkstra() -> BTreeSet<Place> {
    let roots: Vec<&str> = ROOTS.iter().map(|(_, root)| *root).collect();
    places(&parse(&common::defs()), &roots)
}

/// The kinds derived at exactly `path` under `root`.
fn kinds(places: &BTreeSet<Place>, root: &str, path: &[Step]) -> BTreeSet<K> {
    places
        .iter()
        .filter(|p| p.root == root && p.path == path)
        .map(|p| p.kind)
        .collect()
}

fn changed(place: &Place) -> Row {
    Row {
        class: class(place),
        variant: None,
        input: Vec::new(),
        reference: Reference::Refuses("a reason".into()),
        rules: Vec::new(),
        site: None,
        mutated: None,
        base: None,
        place: Some(place.clone()),
    }
}

#[test]
fn rule_names_are_read_from_definitions() {
    let cddl = "; a comment\nblock = [hash32]\nnonempty_set<a0> = #6.258([+ a0])\nhash32 = bytes .size 32\n";
    assert_eq!(
        rule_names(&parse(cddl)),
        ["block", "nonempty_set", "hash32"]
    );
}

#[test]
fn an_unreachable_rule_and_an_unbounded_number_give_no_place() {
    let derived = derive("r = [a: uint, b: word]\nword = uint .size 8\nlanguage = 0 .. 3\n");
    assert_eq!(derived, expect(&[(&[], &WHOLE), (&[], &RECORD)]));
}

#[test]
fn a_size_below_eight_bytes_bounds_a_number_above() {
    let derived = derive("r = [a: uint .size 2, b: bytes .size 32, c: bytes .size (0 .. 64)]\n");
    assert_eq!(
        derived,
        expect(&[
            (&[], &WHOLE),
            (&[], &RECORD),
            (&[member("r", named("a"))], &[K::RangeAbove]),
            (&[member("r", named("b"))], &[K::RangeAbove, K::RangeBelow]),
            (&[member("r", named("c"))], &[K::RangeAbove]),
        ])
    );
}

#[test]
fn a_choice_of_rules_with_no_leading_number_is_not_a_sum() {
    let derived = derive("r = [a: x / y]\nx = [uint]\ny = {0: uint}\n");
    assert_eq!(
        derived,
        expect(&[
            (&[], &WHOLE),
            (&[], &RECORD),
            (&[member("r", named("a")), arm("r", named("x"))], &RECORD),
            (
                &[member("r", named("a")), arm("r", named("y"))],
                &[K::MapUnknownKey]
            ),
        ])
    );
}

#[test]
fn arms_told_apart_by_a_leading_number_are_a_sum() {
    let derived = derive("r = [0, uint // 1, bytes .size 2]\n");
    assert_eq!(
        derived,
        expect(&[
            (&[], &WHOLE),
            (&[], &[K::UnknownArm]),
            (&[], &RECORD),
            (
                &[arm("r", Part::Position(1)), member("r", Part::Position(1))],
                &[K::RangeAbove, K::RangeBelow]
            ),
        ])
    );
    let numbers = derive("r = 0 / 1 / 2\n");
    assert_eq!(numbers, expect(&[(&[], &WHOLE), (&[], &[K::UnknownArm])]));
}

#[test]
fn a_list_under_the_set_tag_and_bare_is_a_set() {
    let derived = derive("r = s<uint>\ns<a0> = #6.258([* a0]) / [* a0]\n");
    assert_eq!(derived, expect(&[(&[], &WHOLE), (&[], &SET)]));
}

#[test]
fn a_map_of_numbered_keys_is_a_record_and_a_map_of_one_key_type_is_not() {
    let derived = derive("r = {0: uint, ? 1: t}\nt = {* uint => v}\nv = 0 / 1\n");
    assert_eq!(
        derived,
        expect(&[
            (&[], &WHOLE),
            (&[], &[K::MapUnknownKey]),
            (&[key("r", 1), Step::EntryValue], &[K::UnknownArm]),
        ])
    );
}

#[test]
fn a_rational_takes_the_rational_kinds_and_a_unit_interval_two_more() {
    let derived = derive(
        "r = [m: q, u: unit_interval]\nq = #6.30([uint, uint])\nunit_interval = #6.30([uint, uint])\n",
    );
    let unit: Vec<K> = RATIONAL
        .iter()
        .copied()
        .chain([K::UnitAbove, K::UnitZeroDenominator])
        .collect();
    assert_eq!(
        derived,
        expect(&[
            (&[], &WHOLE),
            (&[], &RECORD),
            (&[member("r", named("m"))], &RATIONAL),
            (&[member("r", named("u"))], &unit),
        ])
    );
}

#[test]
fn a_value_inside_cbor_bytes_is_walked() {
    let derived = derive("r = [d: #6.24(bytes .cbor p)]\np = [0, uint // 1, uint]\n");
    assert_eq!(
        derived,
        expect(&[
            (&[], &WHOLE),
            (&[], &RECORD),
            (
                &[member("r", named("d"))],
                &[K::UnknownArm, K::ArrayExtra, K::ArrayShort]
            ),
        ])
    );
}

#[test]
fn a_rule_inside_itself_is_walked_once() {
    let derived = derive("r = [* n]\nn = [0, uint // 1, [* n]]\n");
    assert_eq!(
        derived,
        expect(&[
            (&[], &WHOLE),
            (&[Step::Element], &[K::UnknownArm]),
            (&[Step::Element], &RECORD),
        ])
    );
}

#[test]
fn the_sums_of_a_vote_a_proposal_a_redeemer_and_a_datum_are_derived_in_a_top_and_a_sub() {
    let derived = dijkstra();
    let tb = "transaction_body";
    let oset = |n| arm("nonempty_oset", Part::Position(n));
    let sums: [(&str, Vec<Step>); 6] = [
        (
            tb,
            vec![
                key(tb, 20),
                oset(0),
                Step::Element,
                member("proposal_procedure", Part::Position(2)),
            ],
        ),
        (tb, vec![key(tb, 19), Step::EntryKey]),
        (
            "transaction_witness_set",
            vec![
                key("transaction_witness_set", 5),
                Step::EntryKey,
                member("redeemers", named("tag")),
            ],
        ),
        (
            tb,
            vec![
                key(tb, 1),
                Step::Element,
                arm("transaction_output", named("babbage_transaction_output")),
                key("babbage_transaction_output", 2),
            ],
        ),
        (
            tb,
            vec![
                key(tb, 23),
                oset(1),
                Step::Element,
                member("sub_transaction", Part::Position(0)),
                key("sub_transaction_body", 19),
                Step::EntryKey,
            ],
        ),
        (
            tb,
            vec![
                key(tb, 23),
                oset(0),
                Step::Element,
                member("sub_transaction", Part::Position(1)),
                key("transaction_witness_set", 5),
                Step::EntryKey,
                member("redeemers", named("tag")),
            ],
        ),
    ];
    for (root, path) in sums {
        assert!(
            kinds(&derived, root, &path).contains(&K::UnknownArm),
            "{root} {path:?}"
        );
    }
}

#[test]
fn a_pool_margin_takes_the_unit_interval_kinds() {
    let derived = dijkstra();
    let tb = "transaction_body";
    let margin = [
        key(tb, 4),
        arm("nonempty_oset", Part::Position(0)),
        Step::Element,
        arm("certificate", named("pool_registration_cert")),
        member("pool_params", named("margin")),
    ];
    let unit: BTreeSet<K> = RATIONAL
        .iter()
        .copied()
        .chain([K::UnitAbove, K::UnitZeroDenominator])
        .collect();
    assert_eq!(kinds(&derived, tb, &margin), unit);
}

#[test]
fn a_native_script_inside_a_native_script_is_not_walked_again() {
    let derived = dijkstra();
    let wits = "transaction_witness_set";
    let script = [
        key(wits, 1),
        arm("nonempty_set", Part::Position(0)),
        Step::Element,
    ];
    assert_eq!(
        kinds(&derived, wits, &script),
        BTreeSet::from([K::UnknownArm, K::ArrayExtra, K::ArrayShort])
    );
    let twice = derived.iter().find(|p| {
        p.path
            .iter()
            .filter(|s| matches!(s, Step::Arm { rule, .. } if rule == "native_script"))
            .count()
            > 1
    });
    assert_eq!(twice, None);
}

#[test]
fn a_fee_and_a_slot_give_no_place() {
    let derived = dijkstra();
    let tb = "transaction_body";
    for k in [2, 3] {
        assert_eq!(
            kinds(&derived, tb, &[key(tb, k)]),
            BTreeSet::new(),
            "key {k}"
        );
    }
}

#[test]
fn a_mempool_transaction_takes_the_kinds_no_rule_states() {
    let derived = dijkstra();
    let whole: BTreeSet<K> = [K::WideHeads, K::Zap, K::TrailingBytes, K::BlockForm].into();
    assert_eq!(kinds(&derived, "mempool_transaction", &[]), whole);
}

#[test]
fn every_derived_place_changed_passes() {
    let derived: Vec<Place> = dijkstra().into_iter().collect();
    let rows: Vec<Row> = derived.iter().map(changed).collect();
    let failures = PlaceCoverage::new(Layer::Decode, &derived, &rows).failures();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_places_no_input_changes_are_named() {
    let derived: Vec<Place> = dijkstra().into_iter().collect();
    let gov_action = |p: &Place| {
        p.kind == K::UnknownArm
            && p.path.last() == Some(&member("proposal_procedure", Part::Position(2)))
    };
    let rows: Vec<Row> = derived
        .iter()
        .filter(|p| !gov_action(p))
        .map(changed)
        .collect();
    let dropped: Vec<String> = derived
        .iter()
        .filter(|p| gov_action(p))
        .map(|p| format!("decode: no input changes {p}"))
        .collect();
    assert!(!dropped.is_empty());
    let coverage = PlaceCoverage::new(Layer::Decode, &derived, &rows);
    assert_eq!(coverage.failures(), dropped);
}
