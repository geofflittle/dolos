//! Reading a Dijkstra genesis file.
//!
//! The file under test is a node's own, copied byte for byte, so a value
//! asserted here is a value a chain is governed by. Its provenance is beside
//! it.

use std::path::{Path, PathBuf};

use dolos_core::{dijkstra, Genesis};

fn test_data() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("test_data")
}

fn proto_devnet_dijkstra_path() -> PathBuf {
    test_data()
        .join("proto-devnet")
        .join("dijkstra-genesis.json")
}

fn preview_genesis_paths(root: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    dolos_cardano::include::preview::save(root).unwrap();

    (
        root.join("byron.json"),
        root.join("shelley.json"),
        root.join("alonzo.json"),
        root.join("conway.json"),
    )
}

/// MUST FIRE: every parameter the node's file declares reaches the value the
/// file states, one assertion per parameter.
///
/// A count of parameters would pass on a file whose values were all read into
/// the wrong fields, and a struct comparison would say only that something
/// differs, so each one is named.
#[test]
fn every_parameter_of_the_node_genesis_is_read() {
    let file = dijkstra::from_file(proto_devnet_dijkstra_path()).unwrap();

    assert_eq!(file.leios_announcement_period_length, 1000);
    assert_eq!(file.leios_committee_size, 900);
    assert_eq!(file.leios_diffusion_period_length, 7000);
    assert_eq!(file.leios_quorum_stake_threshold, 0.75);
    assert_eq!(file.leios_vote_period_length, 4000);
    assert_eq!(file.max_endorser_block_execution_units.memory, 310_000_000);
    assert_eq!(
        file.max_endorser_block_execution_units.steps,
        100_000_000_000
    );
    assert_eq!(file.max_endorser_block_references_size, 100_000);
    assert_eq!(file.max_endorser_block_txs_size, 1_000_000);
    assert_eq!(file.max_pledge_leverage, None);
    assert_eq!(file.max_ref_script_size_per_block, 1_048_576);
    assert_eq!(file.max_ref_script_size_per_endorser_block, 4_000_000);
    assert_eq!(file.max_ref_script_size_per_tx, 204_800);
    assert_eq!(file.min_pool_margin, 0.015);
    assert_eq!(file.peras_bootstrap_round, None);
    assert_eq!(file.peras_cert_boost, 0);
    assert_eq!(file.peras_healing_factor, 1.0);
    assert_eq!(file.peras_min_candidate_block_age, 0);
    assert_eq!(file.peras_quorum_threshold_safety_margin, 0.0);
    assert_eq!(file.peras_target_committee_size, 0);
    assert_eq!(file.plutus_v4_cost_model.len(), 251);
    assert_eq!(file.plutus_v4_cost_model.first(), Some(&100_788));
    assert_eq!(
        file.plutus_v4_cost_model.iter().min(),
        Some(&-900),
        "a cost model entry is negative, so the entries are signed",
    );
    assert_eq!(file.ref_inputs_cost_per_datum_byte, 0);
    assert_eq!(file.ref_inputs_cost_per_multi_asset_policy, 0);
    assert_eq!(file.ref_script_cost_multiplier, 1.2);
    assert_eq!(file.ref_script_cost_stride, 25_600);
}

/// MUST FIRE: each Peras and reference input parameter reaches its own field,
/// which the node's file cannot show because it sets most of them to zero.
#[test]
fn each_peras_and_reference_input_parameter_reaches_its_own_field() {
    let original = std::fs::read_to_string(proto_devnet_dijkstra_path()).unwrap();
    let mut json: serde_json::Value = serde_json::from_str(&original).unwrap();
    for (key, value) in [
        ("perasBootstrapRound", serde_json::json!(1)),
        ("perasCertBoost", serde_json::json!(2)),
        ("perasHealingFactor", serde_json::json!(3.5)),
        ("perasMinCandidateBlockAge", serde_json::json!(4)),
        ("perasQuorumThresholdSafetyMargin", serde_json::json!(5.5)),
        ("perasTargetCommitteeSize", serde_json::json!(6)),
        ("refInputsCostPerDatumByte", serde_json::json!(7)),
        ("refInputsCostPerMultiAssetPolicy", serde_json::json!(8)),
    ] {
        assert!(
            json.get(key).is_some(),
            "the node's file does not declare {key}"
        );
        json[key] = value;
    }

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("dijkstra-genesis.json");
    std::fs::write(&path, json.to_string()).unwrap();
    let file = dijkstra::from_file(&path).unwrap();

    assert_eq!(
        (
            file.peras_bootstrap_round,
            file.peras_cert_boost,
            file.peras_healing_factor,
            file.peras_min_candidate_block_age,
            file.peras_quorum_threshold_safety_margin,
            file.peras_target_committee_size,
            file.ref_inputs_cost_per_datum_byte,
            file.ref_inputs_cost_per_multi_asset_policy,
        ),
        (Some(1), 2, 3.5, 4, 5.5, 6, 7, 8)
    );
}

/// MUST FIRE: a parameter the file names and this type does not model is
/// refused, because a follower running without it is applying a different
/// rule set from the node and cannot say so.
///
/// MUST NOT FIRE: the same file without the extra parameter loads, so the
/// refusal is about the unknown parameter and not about the file.
#[test]
fn a_parameter_with_nowhere_to_go_stops_the_load() {
    let original = std::fs::read_to_string(proto_devnet_dijkstra_path()).unwrap();

    assert!(
        dijkstra::from_file(proto_devnet_dijkstra_path()).is_ok(),
        "the file this case edits loads as it stands",
    );

    let widened = original.replacen('{', "{\"leiosSomethingNew\": 1,", 1);

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("dijkstra-genesis.json");
    std::fs::write(&path, widened).unwrap();

    let error = dijkstra::from_file(&path).expect_err("an unknown parameter was accepted");

    assert!(
        error.to_string().contains("leiosSomethingNew"),
        "the error does not name the parameter it refused: {error}",
    );
}

/// MUST FIRE: a parameter the file does not name is refused rather than read
/// as a zero, because a zero is a value the follower would go on to apply as
/// the rule.
///
/// MUST NOT FIRE: the file with the parameter present loads, so the refusal is
/// about the absence and not about the edit.
#[test]
fn a_parameter_the_file_omits_stops_the_load() {
    let original = std::fs::read_to_string(proto_devnet_dijkstra_path()).unwrap();

    assert!(
        dijkstra::from_file(proto_devnet_dijkstra_path()).is_ok(),
        "the file this case edits loads as it stands",
    );

    let narrowed = original.replace("  \"leiosCommitteeSize\": 900,\n", "");

    assert_ne!(
        narrowed, original,
        "the parameter was not removed, so this case is not an omission",
    );

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("dijkstra-genesis.json");
    std::fs::write(&path, narrowed).unwrap();

    let error = dijkstra::from_file(&path).expect_err("an omitted parameter was accepted");

    assert!(
        error.to_string().contains("leiosCommitteeSize"),
        "the error does not name the parameter it missed: {error}",
    );
}

/// MUST FIRE: a parameter the ledger reads as a 16 bit or 32 bit word is
/// refused one past that word's maximum, as the node refuses it.
///
/// MUST NOT FIRE: the same parameter at the word's maximum loads.
#[test]
fn a_parameter_past_its_ledger_word_stops_the_load() {
    let original = std::fs::read_to_string(proto_devnet_dijkstra_path()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&original).unwrap();

    for (key, max) in [
        ("leiosCommitteeSize", u64::from(u16::MAX)),
        ("perasCertBoost", u64::from(u16::MAX)),
        ("perasTargetCommitteeSize", u64::from(u16::MAX)),
        ("perasBootstrapRound", u64::from(u32::MAX)),
    ] {
        assert!(
            json.get(key).is_some(),
            "the node's file does not declare {key}"
        );

        let load = |value: u64| {
            let mut edited = json.clone();
            edited[key] = serde_json::json!(value);
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("dijkstra-genesis.json");
            std::fs::write(&path, edited.to_string()).unwrap();
            dijkstra::from_file(&path)
        };

        assert!(load(max).is_ok(), "{key} at {max} was refused");

        let error = load(max + 1).expect_err(&format!("{key} past {max} was accepted"));

        assert!(
            error.to_string().contains(&(max + 1).to_string()),
            "the error does not name the value it refused: {error}",
        );
    }
}

/// MUST FIRE: a genesis loaded without a Dijkstra path carries no Dijkstra
/// parameters, and one loaded with the path carries them, so the field is
/// additive and a caller can tell the two apart.
#[test]
fn the_dijkstra_file_is_additive_to_the_genesis() {
    let directory = tempfile::tempdir().unwrap();
    let (byron, shelley, alonzo, conway) = preview_genesis_paths(directory.path());

    let without = Genesis::from_file_paths(&byron, &shelley, &alonzo, &conway, None).unwrap();

    assert!(
        without.dijkstra.is_none(),
        "a genesis with no dijkstra path invented one",
    );

    let with = Genesis::from_file_paths(&byron, &shelley, &alonzo, &conway, None)
        .unwrap()
        .with_dijkstra(proto_devnet_dijkstra_path())
        .unwrap();

    let dijkstra = with.dijkstra.expect("the dijkstra file was not carried");

    assert_eq!(dijkstra.leios_committee_size, 900);
    assert_eq!(with.shelley_hash, without.shelley_hash);
}

/// MUST FIRE: a path that names no file is an error rather than an empty set
/// of parameters, because a missing consensus rule and a rule set of zero
/// rules are not the same answer.
#[test]
fn a_missing_dijkstra_file_is_an_error() {
    let directory = tempfile::tempdir().unwrap();

    let error = dijkstra::from_file(directory.path().join("absent.json"))
        .expect_err("a missing file produced a genesis");

    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}
