//! The normaliser against the hand verified pairs in `dataset/normalization`
//! of the cardano-cbor-dataset submodule, each input giving its expected bytes.

use std::{collections::BTreeSet, path::PathBuf};

use ledger_oracle_judge::normalize::normalize;

/// Whether a pair's expected bytes differ from its input, as the pairs' README
/// lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Rewritten,
    Unchanged,
}

/// The pair directory of the submodule.
fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test_data/cardano-cbor-dataset/dataset/normalization")
}

fn read(name: &str, side: &str) -> Vec<u8> {
    let path = dir().join(format!("{name}.{side}.cbor"));
    std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}, run `git submodule update --init test_data/cardano-cbor-dataset`",
            path.display()
        )
    })
}

fn check(name: &str, outcome: Outcome) {
    let input = read(name, "input");
    let expected = read(name, "expected");
    match outcome {
        Outcome::Rewritten => assert_ne!(input, expected, "{name} is listed as rewritten"),
        Outcome::Unchanged => assert_eq!(input, expected, "{name} is listed as unchanged"),
    }
    let normalized = normalize(&input).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(hex::encode(normalized), hex::encode(&expected), "{name}");
    let again = normalize(&expected).unwrap_or_else(|e| panic!("{name} expected: {e}"));
    assert_eq!(
        hex::encode(again),
        hex::encode(&expected),
        "{name} expected"
    );
}

macro_rules! pairs {
    ($($test:ident: $name:literal, $outcome:ident;)*) => {
        const PAIRS: &[(&str, Outcome)] = &[$(($name, Outcome::$outcome)),*];

        mod pair_normalizes_to_its_expected_bytes {
            use super::{check, Outcome};
            $(
                #[test]
                fn $test() {
                    check($name, Outcome::$outcome);
                }
            )*
        }
    };
}

pairs! {
    indefinite_array: "0001-indefinite-array", Rewritten;
    indefinite_map: "0002-indefinite-map", Rewritten;
    chunked_bytes: "0003-chunked-bytes", Rewritten;
    chunked_text: "0004-chunked-text", Rewritten;
    nested_indefinite: "0005-nested-indefinite", Rewritten;
    empty_indef_array: "0006-empty-indef-array", Rewritten;
    empty_indef_map: "0007-empty-indef-map", Rewritten;
    non_minimal_int: "0008-non-minimal-int", Rewritten;
    unsorted_map_keys: "0009-unsorted-map-keys", Unchanged;
    duplicate_map_keys: "0010-duplicate-map-keys", Unchanged;
    uint_2_64_minus_1: "0011-uint-2^64-1", Unchanged;
    tag_30_rational: "0012-tag-30-rational", Unchanged;
    tag_259_map: "0013-tag-259-map", Unchanged;
    unknown_tag_1295: "0014-unknown-tag-1295", Unchanged;
    tag_over_indefinite: "0015-tag-over-indefinite", Rewritten;
    float16: "0016-float16", Unchanged;
    bignum_fits: "0017-bignum-fits", Rewritten;
    negative_bignum_fits: "0018-negative-bignum-fits", Rewritten;
    bignum_leading_zeros: "0019-bignum-leading-zeros", Rewritten;
    bignum_too_large: "0020-bignum-too-large", Unchanged;
}

#[test]
fn the_normalization_directory_holds_exactly_the_listed_pairs() {
    let mut on_disk = BTreeSet::new();
    let entries = std::fs::read_dir(dir()).unwrap_or_else(|e| {
        panic!(
            "{}: {e}, run `git submodule update --init test_data/cardano-cbor-dataset`",
            dir().display()
        )
    });
    for entry in entries {
        let file = entry.unwrap().file_name().into_string().unwrap();
        if file == "README.md" {
            continue;
        }
        let name = file
            .strip_suffix(".input.cbor")
            .or_else(|| file.strip_suffix(".expected.cbor"))
            .unwrap_or_else(|| panic!("{file} is neither an input nor an expected file"));
        on_disk.insert(name.to_owned());
    }
    let listed: BTreeSet<String> = PAIRS.iter().map(|(name, _)| (*name).to_owned()).collect();
    assert_eq!(on_disk, listed);
    for name in &listed {
        read(name, "input");
        read(name, "expected");
    }
    let unchanged = PAIRS
        .iter()
        .filter(|(_, outcome)| *outcome == Outcome::Unchanged)
        .count();
    assert_eq!((PAIRS.len(), unchanged), (20, 8));
    println!(
        "{} pairs, {} rewritten and {unchanged} unchanged",
        PAIRS.len(),
        PAIRS.len() - unchanged
    );
}
