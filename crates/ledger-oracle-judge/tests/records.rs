//! The records fixtures under `tests/records`, each passing or failing
//! `Records::check` as the directory it is in says, a failure for the reason
//! its `reason` file states.

use std::path::{Path, PathBuf};

use ledger_oracle_judge::comparison::{Layer, Records};

/// Each case directory under `verdict`, with its records directory, its layer
/// and the kinds and classes it requires.
fn cases(verdict: &str) -> Vec<(PathBuf, PathBuf, Layer, Vec<String>)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/records")
        .join(verdict);
    let mut out = Vec::new();
    for case in std::fs::read_dir(&root).unwrap() {
        let case = case.unwrap().path();
        let pin = std::fs::read_dir(&case)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.is_dir())
            .unwrap_or_else(|| panic!("{} has no records directory", case.display()));
        let args = std::fs::read_to_string(case.join("args")).unwrap();
        let mut lines = args.lines();
        let layer = Layer::from_name(lines.next().unwrap()).unwrap();
        let required = lines.map(str::to_owned).collect();
        out.push((case, pin, layer, required));
    }
    out.sort();
    out
}

/// Every failure `Records::read_dir` and `Records::check` give for one case.
fn failures(pin: &Path, layer: Layer, required: &[String]) -> Vec<String> {
    match Records::read_dir(pin, layer) {
        Ok(records) => records.check(layer, required),
        Err(e) => vec![e.to_string()],
    }
}

#[test]
fn well_formed_records_pass() {
    let cases = cases("pass");
    assert!(!cases.is_empty(), "no passing case");
    for (case, pin, layer, required) in &cases {
        assert_eq!(
            failures(pin, *layer, required),
            Vec::<String>::new(),
            "{}",
            case.display()
        );
    }
}

#[test]
fn malformed_records_fail_for_their_reason() {
    let cases = cases("fail");
    assert!(!cases.is_empty(), "no failing case");
    for (case, pin, layer, required) in &cases {
        let reason = std::fs::read_to_string(case.join("reason")).unwrap();
        let failures = failures(pin, *layer, required);
        assert!(
            failures.iter().any(|f| f.contains(reason.trim())),
            "{}: wanted {:?}, got {failures:?}",
            case.display(),
            reason.trim()
        );
    }
}
