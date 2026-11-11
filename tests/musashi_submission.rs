#[path = "support/musashi_submission.rs"]
mod support;

#[test]
fn captured_control_submission() {
    support::assert_submission(false, false);
}
#[test]
fn captured_settings_submission() {
    support::assert_submission(true, false);
}
#[test]
fn captured_alternate_mempool_wrappers_submission() {
    support::assert_submission(false, true);
    support::assert_submission(true, true);
}

#[test]
fn captured_validity_third_relay_era() {
    use dolos_core::{Domain, MempoolStore, SubmitExt};
    let (domain, mut bytes) = support::fixture(false);
    assert_eq!(bytes[0], 0x83);
    bytes[0] = 0x84;
    bytes.insert(bytes.len() - 1, 0xf5);
    domain
        .receive_tx("derived-validity-third", &domain.read_chain(), &bytes)
        .unwrap();
    let stored = domain.mempool().peek_pending().remove(0);
    assert_eq!(stored.payload().1, bytes);
    assert_eq!(
        stored.payload().0,
        8,
        "active Dijkstra era must survive submission"
    );
}

use dolos_cardano::{
    model::{AccountState, EpochState, FixedNamespace, PParamValue},
    SingletonEntity,
};
use dolos_core::*;
use dolos_testing::toy_domain::ToyDomain;
use pallas::{
    codec::{minicbor, utils::KeepRaw},
    ledger::{
        primitives::dijkstra as n,
        traverse::{Era, MultiEraTx},
        validate::utils::ContextFailure,
    },
};

fn set_protocol(domain: &ToyDomain, version: (u64, u64)) {
    let mut epoch = dolos_cardano::load_epoch::<ToyDomain>(domain.state()).unwrap();
    epoch
        .pparams
        .unwrap_live_mut()
        .set(PParamValue::ProtocolVersion(version));
    let w = domain.state().start_writer().unwrap();
    w.write_entity_typed(&EpochState::singleton_key(), &epoch)
        .unwrap();
    w.commit().unwrap();
}
fn evaluate(
    domain: &ToyDomain,
    bytes: &[u8],
) -> Result<pallas::ledger::validate::phase2::MultiEraEvalReport, ChainError> {
    dolos_cardano::validate::evaluate_tx(
        bytes,
        &MempoolAwareUtxoStore::<ToyDomain>::new(domain.state(), domain.mempool()),
        &domain.genesis(),
    )
}

#[test]
fn historical_native_evaluation_keeps_full_parameters_and_budgets() {
    let (domain, bytes) = support::fixture(true);
    let report = evaluate(&domain, &bytes).unwrap();
    assert_eq!(report.len(), 1);
    assert!(report[0].success);
    assert_eq!(
        (report[0].units.mem, report[0].units.steps),
        (18485, 4805428)
    );
    let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &bytes).unwrap();
    assert_eq!(
        tx.hash().to_string(),
        "da910a9bfbe657724d64b505099010dd47f86fb573aa1d20b4da0367163e94c6"
    );
    let original = support::bytes("settings-registration.mempool.hex");
    assert_eq!(bytes, original);
}

#[test]
fn unknown_protocols_and_malformed_envelopes_reject() {
    let (domain, bytes) = support::fixture(false);
    for version in [(12, 1), (13, 0), (99, 0)] {
        set_protocol(&domain, version);
        assert!(format!(
            "{:?}",
            domain
                .validate_tx(&domain.read_chain(), &bytes)
                .unwrap_err()
        )
        .contains("unsupported submission protocol"));
        assert!(format!("{:?}", evaluate(&domain, &bytes).unwrap_err())
            .contains("unsupported submission protocol"));
    }
    set_protocol(&domain, (12, 0));
    let mut trailing = bytes.clone();
    trailing.push(0);
    for bad in [
        vec![],
        vec![0xff],
        bytes[..bytes.len() - 1].to_vec(),
        trailing,
    ] {
        assert!(domain
            .receive_tx("malformed", &domain.read_chain(), &bad)
            .is_err());
        assert!(evaluate(&domain, &bad).is_err());
    }
    assert!(!domain.mempool().has_pending());
}

#[test]
fn registered_credential_rejects_and_validation_does_not_commit_state() {
    let (domain, bytes) = support::fixture(true);
    let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &bytes).unwrap();
    let n::Certificate::Reg(credential, _) = &tx
        .as_dijkstra()
        .unwrap()
        .transaction_body
        .certificates
        .as_ref()
        .unwrap()[0]
    else {
        panic!()
    };
    let key = minicbor::to_vec(credential).unwrap().into();
    assert!(domain
        .state()
        .read_entity_typed::<AccountState>(AccountState::NS, &key)
        .unwrap()
        .is_none());
    domain.validate_tx(&domain.read_chain(), &bytes).unwrap();
    assert!(domain
        .state()
        .read_entity_typed::<AccountState>(AccountState::NS, &key)
        .unwrap()
        .is_none());
    let mut account = AccountState::new(64, credential.clone());
    account.registered_at = Some(1401000);
    account.deposit = Some(2_000_000);
    let writer = domain.state().start_writer().unwrap();
    writer.write_entity_typed(&key, &account).unwrap();
    writer.commit().unwrap();
    let error = domain
        .receive_tx("registered", &domain.read_chain(), &bytes)
        .unwrap_err();
    assert!(
        format!("{error:?}").contains("already registered"),
        "{error:?}"
    );
    assert!(!domain.mempool().has_pending());
}

#[test]
fn unregistration_of_an_unregistered_credential_rejects() {
    let (domain, bytes) = support::fixture(true);
    let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &bytes).unwrap();
    let mut native = tx.as_dijkstra().unwrap().clone();
    let mut body = (*native.transaction_body).clone();
    let n::Certificate::Reg(credential, deposit) = &body.certificates.as_ref().unwrap()[0] else {
        panic!()
    };
    body.certificates = Some(
        vec![n::Certificate::UnReg(credential.clone(), *deposit)]
            .try_into()
            .unwrap(),
    );
    native.transaction_body = KeepRaw::from(body);
    let mutated = minicbor::to_vec(native.to_mempool_transaction()).unwrap();
    evaluate(&domain, &mutated).unwrap();
    let e = domain
        .validate_tx(&domain.read_chain(), &mutated)
        .unwrap_err();
    assert!(
        format!("{e:?}").contains("stake credential not registered"),
        "{e:?}"
    );
}

#[test]
fn earlier_conway_submission_still_accepts_original_signed_transaction() {
    use pallas::ledger::primitives::conway::{LegacyTransactionOutput, TransactionOutput};
    use std::sync::Arc;
    let bytes = support::bytes("conway-control.hex");
    let tx = MultiEraTx::decode_for_era(Era::Conway, &bytes).unwrap();
    assert_eq!(
        tx.hash().to_string(),
        "90bd64b133e327daecfa0cc60c26f3b96fc6f0285a6d96cc122819908b3aaf93"
    );
    // Reconstructed UTxO from Pallas's existing successful_mainnet_tx test.
    // This is earlier-era caller coverage, not a new historical-state capture.
    let output = TransactionOutput::Legacy(KeepRaw::from(LegacyTransactionOutput {
        address: hex::decode("015c5c318d01f729e205c95eb1b02d623dd10e78ea58f72d0c13f892b2e8904edc699e2f0ce7b72be7cec991df651a222e2ae9244eb5975cba").unwrap().into(),
        amount: pallas::ledger::primitives::alonzo::Value::Coin(20_000_000), datum_hash: None,
    }));
    let mut delta = UtxoSetDelta::default();
    delta.produced_utxo.insert(
        TxoRef::from(&tx.inputs()[0]),
        Arc::new(EraCbor(7, minicbor::to_vec(output).unwrap())),
    );
    let mut genesis = dolos_cardano::include::mainnet::load();
    genesis.force_protocol = Some(9);
    let domain = ToyDomain::new_with_genesis(Arc::new(genesis), Some(delta), None);
    let writer = domain.state().start_writer().unwrap();
    writer.set_cursor(ChainPoint::Slot(137806612)).unwrap();
    writer.commit().unwrap();
    assert!(evaluate(&domain, &bytes).unwrap().is_empty());
    let result = domain.receive_tx("conway-control", &domain.read_chain(), &bytes);
    assert_eq!(result.unwrap(), tx.hash());
    let stored = domain.mempool().peek_pending().remove(0);
    assert_eq!(stored.payload().0, 7);
    assert_eq!(stored.payload().1, bytes);
}

#[test]
fn accepted_validity_third_wrapper_preserves_bytes_and_false_rejects() {
    for settings in [false, true] {
        let (domain, mut bytes) = support::fixture(settings);
        assert_eq!(bytes[0], 0x83);
        assert_eq!(*bytes.last().unwrap(), 0xf6);
        bytes[0] = 0x84;
        bytes.insert(bytes.len() - 1, 0xf5);
        let id = domain
            .receive_tx("derived-validity-third", &domain.read_chain(), &bytes)
            .unwrap();
        let stored = domain.mempool().peek_pending().remove(0);
        assert_eq!(stored.hash(), id);
        assert_eq!(stored.payload().1, bytes);
        assert_eq!(stored.payload().0, 8);
        let last = bytes.len() - 2;
        bytes[last] = 0xf4;
        assert!(evaluate(&domain, &bytes).is_err());
    }
}

#[test]
fn original_signatures_are_verified() {
    for settings in [false, true] {
        let (domain, mut bytes) = support::fixture(settings);
        let signature = {
            let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &bytes).unwrap();
            tx.as_dijkstra()
                .unwrap()
                .transaction_witness_set
                .vkeywitness
                .as_ref()
                .unwrap()[0]
                .signature
                .to_vec()
        };
        let offset = bytes
            .windows(signature.len())
            .position(|x| x == signature)
            .unwrap();
        bytes[offset] ^= 1;
        let error = domain
            .receive_tx("tampered-signature", &domain.read_chain(), &bytes)
            .unwrap_err();
        assert!(
            format!("{error:?}").contains("VKWrongSignature"),
            "{error:?}"
        );
        assert!(!domain.mempool().has_pending());
    }
}

/// Synthetic key registrations with separate funding and one shared credential.
/// Only the transaction shape and protocol context come from the control fixture.
fn independent_registrations() -> (
    ToyDomain,
    [Vec<u8>; 2],
    pallas::ledger::primitives::dijkstra::StakeCredential,
) {
    use pallas::{
        codec::utils::MaybeIndefArray,
        crypto::{hash::Hasher, key::ed25519::SecretKey},
        ledger::{
            addresses::{Network, ShelleyAddress, ShelleyDelegationPart, ShelleyPaymentPart},
            primitives::dijkstra as n,
        },
    };

    let (domain, control) = support::fixture(false);
    // Public, deterministic test key; never used for network funds.
    let key = SecretKey::from([71; 32]);
    let key_hash = Hasher::<224>::hash(key.public_key().as_ref());
    let credential = n::StakeCredential::AddrKeyhash(key_hash);
    let address = ShelleyAddress::new(
        Network::Testnet,
        ShelleyPaymentPart::Key(key_hash),
        ShelleyDelegationPart::Null,
    )
    .to_vec();
    let output = |coin| {
        n::TransactionOutput::PostAlonzo(
            n::PostAlonzoTransactionOutput {
                address: address.clone().into(),
                value: n::Value::Coin(coin),
                datum_option: None,
                script_ref: None,
            }
            .into(),
        )
    };
    let deposit = dolos_cardano::load_epoch::<ToyDomain>(domain.state())
        .unwrap()
        .pparams
        .unwrap_live()
        .key_deposit()
        .unwrap();
    let template = MultiEraTx::decode_for_era(Era::Dijkstra, &control).unwrap();
    let mut delta = UtxoSetDelta::default();
    let transactions = [81u8, 82].map(|id| {
        let input = n::TransactionInput {
            transaction_id: [id; 32].into(),
            index: 0,
        };
        delta.produced_utxo.insert(
            TxoRef(input.transaction_id, 0),
            std::sync::Arc::new(EraCbor(8, minicbor::to_vec(output(20_000_000)).unwrap())),
        );
        let mut tx = template.as_dijkstra().unwrap().clone();
        tx.transaction_body.inputs = n::Set::from(vec![input]);
        tx.transaction_body.fee = 1_000_000;
        tx.transaction_body.outputs =
            MaybeIndefArray::Def(vec![output(20_000_000 - 1_000_000 - deposit)]);
        tx.transaction_body.certificates =
            n::NonEmptySet::from_vec(vec![n::Certificate::Reg(credential.clone(), deposit)]);
        let hash = Hasher::<256>::hash(&minicbor::to_vec(&tx.transaction_body).unwrap());
        tx.transaction_witness_set.vkeywitness = n::NonEmptySet::from_vec(vec![n::VKeyWitness {
            vkey: key.public_key().as_ref().to_vec().into(),
            signature: key.sign(hash).as_ref().to_vec().into(),
        }]);
        minicbor::to_vec(tx.to_mempool_transaction()).unwrap()
    });
    let writer = domain.state().start_writer().unwrap();
    writer.apply_utxoset(&delta).unwrap();
    writer.commit().unwrap();
    (domain, transactions, credential)
}

#[test]
fn pending_registration_reserves_credential_without_changing_ledger_state() {
    use pallas::ledger::validate::utils::{CertificateFailure, ValidationError};

    for inflight in [false, true] {
        let (domain, [first, second], credential) = independent_registrations();
        let account_key = minicbor::to_vec(&credential).unwrap().into();
        let assert_unregistered = || {
            assert!(domain
                .state()
                .read_entity_typed::<AccountState>(AccountState::NS, &account_key)
                .unwrap()
                .is_none());
        };
        let first_tx = MultiEraTx::decode_for_era(Era::Dijkstra, &first).unwrap();
        let second_tx = MultiEraTx::decode_for_era(Era::Dijkstra, &second).unwrap();
        let inputs: Vec<TxoRef> = first_tx
            .inputs()
            .iter()
            .chain(second_tx.inputs().iter())
            .map(TxoRef::from)
            .collect();
        assert_ne!(inputs[0], inputs[1]);
        let before = domain.state().get_utxos(inputs.clone()).unwrap();
        assert_eq!(before.len(), 2);
        assert_unregistered();
        // Both signed transactions are valid before either reserves the credential.
        domain.validate_tx(&domain.read_chain(), &first).unwrap();
        domain.validate_tx(&domain.read_chain(), &second).unwrap();
        assert_unregistered();
        let hash = domain
            .receive_tx("synthetic-registration", &domain.read_chain(), &first)
            .unwrap();
        if inflight {
            domain.mempool().mark_inflight(&[hash]).unwrap();
        }
        assert_unregistered();
        let available = MempoolAwareUtxoStore::<ToyDomain>::new(domain.state(), domain.mempool())
            .get_utxos([inputs[1].clone()].into())
            .unwrap();
        assert_eq!(
            available.len(),
            1,
            "second registration must retain its own input"
        );
        let error = domain
            .receive_tx("conflicting-registration", &domain.read_chain(), &second)
            .unwrap_err();
        assert!(
            matches!(
                error,
                DomainError::ChainError(ChainError::Phase1ValidationRejected(
                    ValidationError::DijkstraInvalidCertificate(
                        CertificateFailure::AlreadyRegistered
                    )
                ))
            ),
            "{error:?}"
        );
        assert_unregistered();
        assert_eq!(domain.state().get_utxos(inputs).unwrap(), before);
        let mut accepted = domain.mempool().peek_pending();
        accepted.extend(domain.mempool().peek_inflight());
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].payload().1, first);
    }
}

#[test]
fn v4_reference_script_beside_a_v3_script_evaluates() {
    use pallas::codec::utils::CborWrap;
    use std::sync::Arc;
    let (domain, bytes) = support::fixture(true);
    let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &bytes).unwrap();
    let input = tx.reference_inputs()[0].clone();
    let key = TxoRef::from(&input);
    let raw = domain
        .state()
        .get_utxos(vec![key.clone()])
        .unwrap()
        .remove(&key)
        .unwrap();
    let mut output: n::TransactionOutput = minicbor::decode(&raw.1).unwrap();
    let n::TransactionOutput::PostAlonzo(inner) = &mut output else {
        panic!()
    };
    let mut changed = (**inner).clone();
    changed.script_ref = Some(CborWrap(n::ScriptRef::PlutusV4Script(n::PlutusScript(
        vec![0].into(),
    ))));
    *inner = KeepRaw::from(changed);
    let extra = n::TransactionInput {
        transaction_id: [121; 32].into(),
        index: 0,
    };
    let mut delta = UtxoSetDelta::default();
    delta.produced_utxo.insert(
        TxoRef(extra.transaction_id, 0),
        Arc::new(EraCbor(raw.0, minicbor::to_vec(output).unwrap())),
    );
    let writer = domain.state().start_writer().unwrap();
    writer.apply_utxoset(&delta).unwrap();
    writer.commit().unwrap();
    let mut native = tx.as_dijkstra().unwrap().clone();
    let mut body = (*native.transaction_body).clone();
    let mut references: Vec<_> = body.reference_inputs.iter().flatten().cloned().collect();
    references.push(extra);
    body.reference_inputs = n::NonEmptySet::from_vec(references);
    native.transaction_body = KeepRaw::from(body);
    let mutated = minicbor::to_vec(native.to_mempool_transaction()).unwrap();
    let results = evaluate(&domain, &mutated).unwrap();
    assert!(
        !results.is_empty() && results.iter().all(|x| x.success),
        "{results:?}"
    );
}

#[test]
fn a_subtransaction_running_a_plutus_v1_to_v3_script_is_refused() {
    let (domain, bytes) = support::fixture(true);
    let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &bytes).unwrap();
    let mut native = tx.as_dijkstra().unwrap().clone();
    // Synthetic child: copy the captured registration fields and witnesses into
    // a child body (the top-level fee is not a subtransaction field).
    let raw_body = native.transaction_body.raw_cbor().to_vec();
    let child_body: n::SubTransactionBody = minicbor::decode(&raw_body).unwrap();
    let child = n::SubTransaction {
        sub_transaction_body: KeepRaw::from(child_body),
        transaction_witness_set: native.transaction_witness_set.clone(),
        auxiliary_data: native.auxiliary_data.clone(),
    };
    let mut body = (*native.transaction_body).clone();
    body.sub_transactions = Some(vec![child].try_into().unwrap());
    native.transaction_body = KeepRaw::from(body);
    let error = evaluate(
        &domain,
        &minicbor::to_vec(native.to_mempool_transaction()).unwrap(),
    )
    .unwrap_err();
    let refusal = ContextFailure::LegacyScriptInSubtransaction.as_str();
    assert!(
        matches!(&error, ChainError::Phase2EvaluationError(text) if text.ends_with(refusal)),
        "{error:?}"
    );
}

#[test]
fn block_only_envelopes_are_evidence_not_relay_payloads() {
    for settings in [false, true] {
        let (domain, original) = support::fixture(settings);
        let raw = support::bytes(if settings {
            "settings-registration.block.hex"
        } else {
            "control.block.hex"
        });
        let block = MultiEraTx::decode_for_era(Era::Dijkstra, &raw).unwrap();
        let mempool = MultiEraTx::decode_for_era(Era::Dijkstra, &original).unwrap();
        assert_eq!(block.hash(), mempool.hash());
        let block = block.as_dijkstra().unwrap();
        let mempool = mempool.as_dijkstra().unwrap();
        assert_eq!(
            block.transaction_body.raw_cbor(),
            mempool.transaction_body.raw_cbor()
        );
        assert_eq!(
            block.transaction_witness_set.raw_cbor(),
            mempool.transaction_witness_set.raw_cbor()
        );
        assert!(domain
            .receive_tx("block-only-envelope", &domain.read_chain(), &raw)
            .is_err());
        assert!(evaluate(&domain, &raw).is_err());
    }
}

#[test]
fn indefinite_native_envelopes_preserve_bytes_but_extra_fields_reject() {
    let (domain, mut bytes) = support::fixture(false);
    bytes[0] = 0x9f;
    bytes.push(0xff);
    domain
        .receive_tx("indefinite-native", &domain.read_chain(), &bytes)
        .unwrap();
    assert_eq!(domain.mempool().peek_pending()[0].payload().1, bytes);
    bytes.insert(bytes.len() - 1, 0xf6);
    assert!(evaluate(&domain, &bytes).is_err());
}

#[path = "support/musashi_batch_dependencies.rs"]
mod batch_dependencies;
