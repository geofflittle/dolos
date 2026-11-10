//! Offline replay through actual Dolos admission, evaluation and estimation.
//! Includes the successfully executed Musashi spend; see
//! test_data/musashi-ttl/README.md.
use dolos_cardano::{
    model::{EpochState, PParamValue as P},
    SingletonEntity,
};
use dolos_core::{config::CardanoConfig, *};
use dolos_testing::toy_domain::ToyDomain;
use pallas::ledger::{
    primitives::{ExUnitPrices, ExUnits, RationalNumber},
    traverse::{Era, MultiEraTx},
};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

fn root() -> PathBuf {
    std::env::var_os("PALLAS_TTL_FIXTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test_data/musashi-ttl")
        })
}
fn json(name: &str) -> Value {
    serde_json::from_slice(&std::fs::read(root().join("confirmed-ttl").join(name)).unwrap())
        .unwrap()
}
fn number(v: &Value) -> u64 {
    v.as_u64().unwrap_or_else(|| {
        v.as_str()
            .map(|s| s.parse().unwrap())
            .unwrap_or_else(|| number(&v["int"]))
    })
}
fn ratio(v: &Value) -> RationalNumber {
    RationalNumber {
        numerator: number(&v["numerator"]),
        denominator: number(&v["denominator"]),
    }
}
fn units(v: &Value) -> ExUnits {
    ExUnits {
        mem: number(&v["memory"]),
        steps: number(&v["steps"]),
    }
}

fn fixture_from(case: &Path) -> (ToyDomain, Vec<u8>) {
    let root = root();
    let json = |name: &str| -> Value {
        serde_json::from_slice(&std::fs::read(case.join(name)).unwrap()).unwrap()
    };
    let genesis = Genesis::from_file_paths(
        root.join("genesis/byron.json"),
        root.join("genesis/shelley.json"),
        root.join("genesis/alonzo.json"),
        root.join("genesis/conway.json"),
        Some(12),
    )
    .unwrap()
    .with_dijkstra(root.join("genesis/dijkstra.json"))
    .unwrap();
    let bytes = hex::decode(
        std::fs::read_to_string(case.join("transaction.hex"))
            .unwrap()
            .trim(),
    )
    .unwrap();
    let mut delta = UtxoSetDelta::default();
    for input in json("inputs.json").as_array().unwrap() {
        delta.produced_utxo.insert(
            TxoRef(
                input["hash"].as_str().unwrap().parse().unwrap(),
                number(&input["index"]) as u32,
            ),
            Arc::new(EraCbor(
                8,
                hex::decode(input["output_cbor_hex"].as_str().unwrap()).unwrap(),
            )),
        );
    }
    let domain = ToyDomain::new_with_genesis_and_config(
        Arc::new(genesis),
        CardanoConfig {
            magic: 164,
            is_testnet: true,
            stop_epoch: None,
            custom_utxos: vec![],
        },
        Some(delta),
        None,
    );
    let captured = json("parameters.json");
    let p = &captured["values"]["cardano"];
    let context = json("replay.json");
    let tip = json("validation-tip.json");
    let epoch_no =
        number(&context["validation_slot"]) / number(&json("genesis.json")["epoch_length"]);
    let mut epoch = dolos_cardano::load_epoch::<ToyDomain>(domain.state()).unwrap();
    epoch.number = epoch_no;
    let pp = epoch.pparams.unwrap_live_mut();
    for value in [
        P::ProtocolVersion((12, 0)),
        P::MinFeeA(number(&p["minFeeCoefficient"])),
        P::MinFeeB(number(&p["minFeeConstant"])),
        P::MaxTransactionSize(number(&p["maxTxSize"])),
        P::AdaPerUtxoByte(number(&p["coinsPerUtxoByte"])),
        P::MaxValueSize(number(&p["maxValueSize"]) as u32),
        P::CostModelsPlutusV3(
            p["costModels"]["plutusV3"]["values"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().parse().unwrap())
                .collect(),
        ),
        P::ExecutionCosts(ExUnitPrices {
            mem_price: ratio(&p["prices"]["memory"]),
            step_price: ratio(&p["prices"]["steps"]),
        }),
        P::MaxTxExUnits(units(&p["maxExecutionUnitsPerTransaction"])),
        P::CollateralPercentage(number(&p["collateralPercentage"]) as u32),
        P::MaxCollateralInputs(number(&p["maxCollateralInputs"]) as u32),
        P::MinFeeRefScriptCostPerByte(ratio(&p["minFeeScriptRefCostPerByte"])),
    ] {
        pp.set(value);
    }
    let pp = pp.clone();
    epoch.pparams = dolos_cardano::model::EpochValue::with_live(epoch_no, pp);
    let writer = domain.state().start_writer().unwrap();
    writer
        .write_entity_typed(&EpochState::singleton_key(), &epoch)
        .unwrap();
    writer
        .set_cursor(ChainPoint::Specific(
            number(&context["validation_slot"]),
            tip["hash"].as_str().unwrap().parse().unwrap(),
        ))
        .unwrap();
    writer.commit().unwrap();
    (domain, bytes)
}

#[test]
fn captured_ttl_admission_evaluation_and_estimation() {
    assert_admission_evaluation_and_estimation(&root().join("confirmed-ttl"));
}

#[test]
fn cross_epoch_signed_spend_past_the_horizon_is_refused() {
    let case = root().join("cross-epoch-ttl");
    let replay: Value =
        serde_json::from_slice(&std::fs::read(case.join("replay.json")).unwrap()).unwrap();
    let epoch_length = number(&json("genesis.json")["epoch_length"]);
    assert!(
        number(&replay["validation_slot"]) / epoch_length < number(&replay["ttl"]) / epoch_length
    );
    let (domain, bytes) = fixture_from(&case);
    let genesis = domain.genesis();
    let utxos = MempoolAwareUtxoStore::<ToyDomain>::new(domain.state(), domain.mempool());
    let evaluations = [
        format!(
            "{:?}",
            dolos_cardano::validate::evaluate_tx(&bytes, &utxos, &genesis)
        ),
        format!(
            "{:?}",
            dolos_cardano::validate::estimate_tx(&bytes, &utxos, &genesis)
        ),
        format!(
            "{:?}",
            dolos_cardano::validate::estimate_tx_calls(&bytes, &utxos, &genesis)
        ),
    ];
    for refusal in evaluations {
        assert!(
            refusal.starts_with("Err(")
                && refusal.contains("slot 900481 is at or past the forecast horizon 885600"),
            "{refusal}"
        );
    }
    let admission = format!(
        "{:?}",
        domain.receive_tx("historical TTL capture", &domain.read_chain(), &bytes)
    );
    assert!(
        admission.starts_with("Err(")
            && admission.contains("PastHorizon { slot: 900481, horizon: 885600 }"),
        "{admission}"
    );
    assert!(domain.mempool().peek_pending().is_empty());
}

fn assert_admission_evaluation_and_estimation(case: &Path) {
    let (domain, bytes) = fixture_from(case);
    let genesis = domain.genesis();
    let context: Value =
        serde_json::from_slice(&std::fs::read(case.join("replay.json")).unwrap()).unwrap();
    let expected = units(&context["reference_execution_units"]);
    let utxos = MempoolAwareUtxoStore::<ToyDomain>::new(domain.state(), domain.mempool());
    for report in [
        dolos_cardano::validate::evaluate_tx(&bytes, &utxos, &genesis).unwrap(),
        dolos_cardano::validate::estimate_tx(&bytes, &utxos, &genesis).unwrap(),
    ] {
        assert_eq!(report.len(), 1);
        assert!(report[0].success, "{report:?}");
        assert_eq!(report[0].units, expected);
    }
    let calls = dolos_cardano::validate::estimate_tx_calls(&bytes, &utxos, &genesis).unwrap();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].0.success);
    assert_eq!(calls[0].0.units, expected);
    assert!(!calls[0].1.is_empty());
    let minimum = dolos_cardano::validate::min_fee(&bytes, &utxos)
        .unwrap()
        .unwrap();
    assert!(
        MultiEraTx::decode_for_era(Era::Dijkstra, &bytes)
            .unwrap()
            .fee()
            .unwrap()
            >= minimum
    );
    let hash = domain
        .receive_tx("historical TTL capture", &domain.read_chain(), &bytes)
        .unwrap();
    assert_eq!(hash.to_string(), context["txid"].as_str().unwrap());
    let stored = domain.mempool().peek_pending().remove(0);
    assert_eq!(stored.payload().0, 8);
    assert_eq!(stored.payload().1, bytes);
    assert_eq!(
        MultiEraTx::decode_for_era(Era::Dijkstra, &stored.payload().1)
            .unwrap()
            .hash(),
        hash
    );
}

#[test]
fn captured_ttl_expires_at_the_exclusive_upper_slot() {
    for case in [root().join("confirmed-ttl"), root().join("cross-epoch-ttl")] {
        assert_expires_at_upper_slot(&case);
    }
}

fn assert_expires_at_upper_slot(case: &Path) {
    let (domain, bytes) = fixture_from(case);
    let context: Value =
        serde_json::from_slice(&std::fs::read(case.join("replay.json")).unwrap()).unwrap();
    let ttl = number(&context["ttl"]);
    let mut epoch = dolos_cardano::load_epoch::<ToyDomain>(domain.state()).unwrap();
    let number = ttl / number(&json("genesis.json")["epoch_length"]);
    let pp = epoch.pparams.unwrap_live().clone();
    epoch.number = number;
    epoch.pparams = dolos_cardano::model::EpochValue::with_live(number, pp);
    let writer = domain.state().start_writer().unwrap();
    writer
        .write_entity_typed(&EpochState::singleton_key(), &epoch)
        .unwrap();
    writer
        .set_cursor(ChainPoint::Specific(ttl, [0; 32].into()))
        .unwrap();
    writer.commit().unwrap();
    let result = domain.validate_tx(&domain.read_chain(), &bytes);
    assert!(format!("{:?}", result.unwrap_err()).contains("BlockExceedsValInt"));
}
