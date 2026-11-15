mod node;

use std::path::Path;
use std::process::{Command, Output};

/// The `dolos data` commands that read the stores, each with the arguments it
/// needs and any file it writes placed under `out`.
fn read_only_commands(out: &Path) -> Vec<Vec<String>> {
    let out = |name: &str| out.join(name).display().to_string();
    let hash = "00".repeat(32);

    [
        vec!["summary".into()],
        vec!["check".into()],
        vec!["dump-wal".into()],
        vec!["dump-state".into(), "--namespace".into(), "eras".into()],
        vec!["dump-entity".into(), "--namespace".into(), "epochs".into()],
        vec!["dump-logs".into(), "--namespace".into(), "stakes".into()],
        vec!["dump-blocks".into()],
        vec!["compute-spdd".into()],
        vec!["compute-nonce".into(), "--epoch".into(), "0".into()],
        vec![
            "find-seq".into(),
            "--slot".into(),
            "100".into(),
            "--hash".into(),
            hash,
        ],
        vec!["stats".into()],
        vec!["export".into(), "--output".into(), out("export.tar.gz")],
        vec!["copy-wal".into(), "--output".into(), out("wal")],
    ]
    .into()
}

fn run(node: &node::Node, args: &[String]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dolos"))
        .arg("--config")
        .arg(node.config_path())
        .arg("data")
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn a_read_only_data_command_reads_an_existing_store() {
    let node = node::Node::new();
    node.sync();
    let out = tempfile::tempdir().unwrap();

    for args in [
        vec!["summary".to_string()],
        vec!["dump-wal".into()],
        vec!["dump-blocks".into()],
        vec!["dump-state".into(), "--namespace".into(), "eras".into()],
        vec!["stats".into()],
        vec![
            "copy-wal".into(),
            "--output".into(),
            out.path().join("wal").display().to_string(),
        ],
    ] {
        let output = run(&node, &args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn a_read_only_data_command_on_an_empty_directory_refuses_and_creates_nothing() {
    let node = node::Node::new();
    let data = node.config.storage.path.clone();
    std::fs::create_dir_all(&data).unwrap();
    let out = tempfile::tempdir().unwrap();

    let mut seen = Vec::new();
    let mut expected = Vec::new();
    for args in read_only_commands(out.path()) {
        let output = run(&node, &args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut left: Vec<_> = std::fs::read_dir(&data)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        for name in &left {
            let path = data.join(name);
            if path.is_dir() {
                std::fs::remove_dir_all(path).unwrap();
            } else {
                std::fs::remove_file(path).unwrap();
            }
        }
        let command = args[0].clone();
        expected.push((command.clone(), "refused", true, Vec::<String>::new()));
        seen.push((
            command,
            if output.status.success() {
                "exited zero"
            } else {
                "refused"
            },
            stderr.contains("store at"),
            left,
        ));
    }

    assert_eq!(seen, expected);
}
