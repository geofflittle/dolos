use dolos_cardano::model::{EpochState, PParamValue as P};
use dolos_cardano::SingletonEntity;
use dolos_core::{config::CardanoConfig, *};
use dolos_testing::toy_domain::ToyDomain;
use pallas::{
    codec::minicbor,
    ledger::{
        primitives::{ExUnitPrices, ExUnits, RationalNumber},
        traverse::{Era, MultiEraBlock, MultiEraTx},
    },
};
use std::{path::PathBuf, sync::Arc};

pub fn path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test_data/musashi-native-submission")
        .join(name)
}
pub fn bytes(name: &str) -> Vec<u8> {
    hex::decode(std::fs::read_to_string(path(name)).unwrap().trim()).unwrap()
}
pub fn fixture(settings: bool) -> (ToyDomain, Vec<u8>) {
    let genesis = Genesis::from_file_paths(
        path("byron-genesis.json"),
        path("shelley-genesis.json"),
        path("alonzo-genesis.json"),
        path("conway-genesis.json"),
        Some(12),
    )
    .unwrap()
    .with_dijkstra(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("crates/core/test_data/proto-devnet/dijkstra-genesis.json"),
    )
    .unwrap();
    let cbor = bytes(if settings {
        "settings-registration.mempool.hex"
    } else {
        "control.mempool.hex"
    });
    let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &cbor).unwrap();
    let mut delta = UtxoSetDelta::default();
    if settings {
        let raw = bytes("1401345.block");
        let block = MultiEraBlock::decode(&raw).unwrap();
        let producer = block.txs().remove(0);
        for (index, output) in producer.outputs().iter().enumerate() {
            delta.produced_utxo.insert(
                TxoRef(producer.hash(), index as u32),
                Arc::new(EraCbor(8, output.encode())),
            );
        }
    } else {
        delta.produced_utxo.insert(
            TxoRef::from(&tx.inputs()[0]),
            Arc::new(EraCbor(8, bytes("control.input.hex"))),
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
    let p: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(path("registration-epoch64-parameters.json")).unwrap(),
    )
    .unwrap();
    let num = |k: &str| {
        p[k].as_u64()
            .unwrap_or_else(|| p[k].as_str().unwrap().parse().unwrap())
    };
    let rat = |numerator, denominator| RationalNumber {
        numerator,
        denominator,
    };
    let mut epoch = dolos_cardano::load_epoch::<ToyDomain>(domain.state()).unwrap();
    epoch.number = 64;
    let pp = epoch.pparams.unwrap_live_mut();
    for value in [
        P::ProtocolVersion((12, 0)),
        P::MinFeeA(num("min_fee_a")),
        P::MinFeeB(num("min_fee_b")),
        P::MaxTransactionSize(num("max_tx_size")),
        P::KeyDeposit(num("key_deposit")),
        P::AdaPerUtxoByte(num("coins_per_utxo_size")),
        P::MaxValueSize(num("max_val_size") as u32),
        P::CostModelsPlutusV3(
            serde_json::from_value(p["cost_models_raw"]["PlutusV3"].clone()).unwrap(),
        ),
        P::ExecutionCosts(ExUnitPrices {
            mem_price: rat(577, 10000),
            step_price: rat(721, 10000000),
        }),
        P::MaxTxExUnits(ExUnits {
            mem: num("max_tx_ex_mem"),
            steps: num("max_tx_ex_steps"),
        }),
        P::CollateralPercentage(num("collateral_percent") as u32),
        P::MaxCollateralInputs(num("max_collateral_inputs") as u32),
        P::MinFeeRefScriptCostPerByte(rat(15, 1)),
    ] {
        pp.set(value);
    }
    assert_eq!(pp.cost_models_plutus_v3().unwrap().len(), 350);
    assert_eq!(pp.protocol_version(), Some((12, 0)));
    assert_eq!(pp.system_start(), Some(1788739200));
    assert_eq!(pp.epoch_length(), Some(21600));
    assert_eq!(pp.slot_length(), Some(1));
    let pp = pp.clone();
    epoch.pparams = dolos_cardano::model::EpochValue::with_live(64, pp);
    let slots = dolos_cardano::eras::load_era_summary::<ToyDomain>(domain.state())
        .unwrap()
        .to_pallas_slot_config();
    assert_eq!(
        (slots.zero_slot, slots.zero_time, slots.slot_length),
        (0, 1788739200000, 1000)
    );
    // The retained history establishes the settings credential is unregistered.
    let state: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(path("registration-certificate-state.json")).unwrap(),
    )
    .unwrap();
    assert!(state["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["before_transaction"]
            == "da910a9bfbe657724d64b505099010dd47f86fb573aa1d20b4da0367163e94c6"
            && x["registered_before"] == false));
    let writer = domain.state().start_writer().unwrap();
    writer
        .write_entity_typed(&EpochState::singleton_key(), &epoch)
        .unwrap();
    writer
        .set_cursor(ChainPoint::Specific(
            if settings { 1401365 } else { 1400007 },
            [0; 32].into(),
        ))
        .unwrap();
    writer.commit().unwrap();
    (domain, cbor)
}

pub fn assert_submission(settings: bool, alternate: bool) -> MempoolTx {
    let (domain, original) = fixture(settings);
    let mut cbor = original;
    if alternate {
        // Derived, protocol-accepted validity-third/true mempool envelope.
        assert_eq!(cbor[0], 0x83);
        assert_eq!(*cbor.last().unwrap(), 0xf6);
        cbor[0] = 0x84;
        cbor.insert(cbor.len() - 1, 0xf5);
    }
    let expected = MultiEraTx::decode_for_era(Era::Dijkstra, &cbor)
        .unwrap()
        .hash();
    let result = domain.receive_tx("historical-replay", &domain.read_chain(), &cbor);
    assert_eq!(
        result.unwrap_or_else(|e| panic!("native submission failed: {e:?}")),
        expected
    );
    let tx = domain.mempool().peek_pending().remove(0);
    assert_eq!(tx.payload().0, 8);
    assert_eq!(tx.payload().1, cbor);
    assert_eq!(tx.hash(), expected);
    // Phase one verifies the original signatures; the body and witness slices
    // also remain byte-identical after the storage round trip.
    let decoded = MultiEraTx::decode_for_era(Era::Dijkstra, &tx.payload().1).unwrap();
    let before = MultiEraTx::decode_for_era(Era::Dijkstra, &cbor).unwrap();
    assert_eq!(
        minicbor::to_vec(decoded.as_dijkstra().unwrap()).unwrap(),
        minicbor::to_vec(before.as_dijkstra().unwrap()).unwrap()
    );
    tx
}
