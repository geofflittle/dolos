//! Runs the arm sets under scripts/musashi so a harness that stopped refusing
//! what it was written to refuse fails the suite.
//!
//! Each harness prints one verdict line carrying the number of arms it took and
//! the number that behaved, and each test here asserts that whole line rather
//! than the exit, because a harness that ran no arm at all also exits zero.

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The verdict the harness printed, with its own output attached to any
/// refusal, so a reader of a failure sees which arm disagreed.
fn run(script: &str) -> String {
    let path = root().join(script);

    assert!(
        path.is_file(),
        "{} is not in the tree, so nothing was run",
        path.display(),
    );

    let out = Command::new("sh")
        .arg(&path)
        .arg(root())
        .current_dir(root())
        .output()
        .unwrap_or_else(|err| panic!("{} could not be started: {err}", path.display()));

    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    assert!(
        out.status.success(),
        "{} exited {:?} instead of 0:\n{text}",
        path.display(),
        out.status.code(),
    );

    text
}

/// A harness whose arms need python3 has to fail here when python3 is absent,
/// because a test that steps aside on a missing tool cannot fail and says
/// nothing about the arms it was written to take.
fn require_python() {
    let out = Command::new("python3")
        .arg("--version")
        .output()
        .expect("python3 is needed to take these arms and could not be started");

    assert!(out.status.success(), "python3 --version did not succeed");
}

fn assert_verdict(text: &str, verdict: &str, script: &Path) {
    assert!(
        text.lines().any(|line| line.trim() == verdict),
        "{} printed no line reading `{verdict}`:\n{text}",
        script.display(),
    );
}

/// Every arm of the comparison harness behaves as the harness says it should,
/// over a node and a follower that answer from fixtures.
#[test]
fn the_comparison_harness_meets_every_arm() {
    require_python();

    let script = "scripts/musashi/probe/run-probe.sh";
    let text = run(script);

    assert_verdict(&text, "PROBE 6 arms, 6 as expected", &root().join(script));
}

/// The inventory check accepts a log whose every binary asserted something and
/// refuses an unrecorded silent binary, a recorded entry that now passes a
/// test, a log naming no binary, a log holding a failure, and a log naming one
/// target twice.
#[test]
fn the_inventory_harness_meets_every_arm() {
    require_python();

    let script = "scripts/musashi/probe/run-inventory-probe.sh";
    let text = run(script);

    assert_verdict(
        &text,
        "INVENTORY PROBE 6 arms, 6 as expected",
        &root().join(script),
    );
}
