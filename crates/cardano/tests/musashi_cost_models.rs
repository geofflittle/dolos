use dolos_core::{config::CardanoConfig, Domain, Genesis};
use dolos_testing::toy_domain::ToyDomain;
use std::{error::Error, path::PathBuf, sync::Arc};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test_data/musashi-cost-models")
}

fn genesis() -> Result<Genesis, Box<dyn Error>> {
    let root = root();
    let genesis = Genesis::from_file_paths(
        root.join("byron-genesis.json"),
        root.join("shelley-genesis.json"),
        root.join("alonzo-genesis.json"),
        root.join("conway-genesis.json"),
        Some(12),
    )?
    .with_dijkstra(root.join("dijkstra-genesis.json"))?;
    Ok(genesis)
}

fn published() -> Result<serde_json::Value, Box<dyn Error>> {
    Ok(serde_json::from_slice(&std::fs::read(
        root().join("alonzo-genesis.json"),
    )?)?)
}

// Any setup failure returns an error, so the one panic is the assertion.
#[test]
fn musashi_era_translation_keeps_the_published_v3_cost_model() -> Result<(), Box<dyn Error>> {
    let genesis = genesis()?;
    let translated = dolos_cardano::forks::force_pparams_version(
        &dolos_cardano::forks::from_byron_genesis(&genesis.byron),
        &genesis,
        0,
        12,
    )?;
    let v3: Vec<i64> =
        serde_json::from_value(published()?["extraConfig"]["costModels"]["PlutusV3"].clone())?;
    assert_eq!(translated.cost_models_plutus_v3(), Some(v3));
    Ok(())
}

#[test]
fn musashi_bootstrap_uses_the_published_cost_models() {
    let genesis = genesis().unwrap();
    let expected = published().unwrap();
    let v3: Vec<i64> =
        serde_json::from_value(expected["extraConfig"]["costModels"]["PlutusV3"].clone()).unwrap();
    let domain = ToyDomain::new_with_genesis_and_config(
        Arc::new(genesis),
        CardanoConfig {
            magic: 164,
            is_testnet: true,
            stop_epoch: None,
            custom_utxos: vec![],
        },
        None,
        None,
    );
    let epoch = dolos_cardano::load_epoch::<ToyDomain>(domain.state()).unwrap();
    let actual = epoch.pparams.unwrap_live();
    assert_eq!(
        actual.cost_models_plutus_v3().unwrap(),
        v3,
        "forced Musashi bootstrap must retain every published V3 override coefficient"
    );
    for (language, actual) in [
        ("PlutusV1", actual.cost_models_plutus_v1().unwrap()),
        ("PlutusV2", actual.cost_models_plutus_v2().unwrap()),
    ] {
        let expected: Vec<i64> =
            serde_json::from_value(expected["extraConfig"]["costModels"][language].clone())
                .unwrap();
        assert_eq!(
            actual, expected,
            "{language} override must be preserved exactly"
        );
    }
    let dijkstra: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root().join("dijkstra-genesis.json")).unwrap())
            .unwrap();
    let v4: Vec<i64> = serde_json::from_value(dijkstra["plutusV4CostModel"].clone()).unwrap();
    assert_eq!(actual.cost_models_unknown_or_default().get(&3), Some(&v4));
}
