//! Synthetic admission regressions; these do not assert upstream node acceptance.
use super::*;
use pallas::{
    codec::utils::MaybeIndefArray,
    crypto::{hash::Hasher, key::ed25519::SecretKey},
    ledger::{
        addresses::{Network, ShelleyAddress, ShelleyDelegationPart, ShelleyPaymentPart},
        validate::utils::{PostAlonzoError, ValidationError},
    },
};
use std::sync::Arc;

fn input(index: u64) -> n::TransactionInput {
    n::TransactionInput {
        transaction_id: [42; 32].into(),
        index,
    }
}

fn fixture(children: &[u64], references: &[u64], missing: Option<u64>) -> (ToyDomain, Vec<u8>) {
    // Reuse the explicit protocol-12 test environment, but construct all batch
    // bodies, signatures and UTxOs here. None of these are network captures.
    let (domain, _) = support::fixture(false);
    let key = SecretKey::from([71; 32]);
    let address = ShelleyAddress::new(
        Network::Testnet,
        ShelleyPaymentPart::Key(Hasher::<224>::hash(key.public_key().as_ref())),
        ShelleyDelegationPart::Null,
    )
    .to_vec();
    let output = |coin| {
        n::TransactionOutput::PostAlonzo(KeepRaw::from(n::PostAlonzoTransactionOutput {
            address: address.clone().into(),
            value: n::Value::Coin(coin),
            datum_option: None,
            script_ref: None,
        }))
    };
    let witnesses = |body: Vec<u8>| {
        n::NonEmptySet::from_vec(vec![n::VKeyWitness {
            vkey: key.public_key().as_ref().to_vec().into(),
            signature: key
                .sign(Hasher::<256>::hash(&body))
                .as_ref()
                .to_vec()
                .into(),
        }])
    };
    let mut parent: n::BlockTransaction =
        minicbor::decode(&[0x84, 0xa3, 0, 0x80, 1, 0x80, 2, 0, 0xa0, 0xf6, 0xf5]).unwrap();
    parent.transaction_body.inputs = vec![input(0)].into();
    parent.transaction_body.outputs = MaybeIndefArray::Def(vec![output(19_000_000)]);
    parent.transaction_body.fee = 1_000_000;
    parent.transaction_body.reference_inputs =
        n::NonEmptySet::from_vec(references.iter().copied().map(input).collect());
    parent.transaction_body.sub_transactions = n::NonEmptySet::from_vec(
        children
            .iter()
            .map(|index| {
                let mut child: n::SubTransaction =
                    minicbor::decode(&[0x83, 0xa2, 0, 0x80, 1, 0x80, 0xa0, 0xf6]).unwrap();
                child.sub_transaction_body.inputs = vec![input(*index)].into();
                child.transaction_witness_set.vkeywitness =
                    witnesses(minicbor::to_vec(&child.sub_transaction_body).unwrap());
                child
            })
            .collect(),
    );
    parent.transaction_witness_set.vkeywitness =
        witnesses(minicbor::to_vec(&parent.transaction_body).unwrap());
    let bytes = minicbor::to_vec(parent.to_mempool_transaction()).unwrap();
    let mut delta = UtxoSetDelta::default();
    for (index, coin) in [
        (0, 10_000_000),
        (1, 5_000_000),
        (2, 5_000_000),
        (3, 10_000_000),
    ] {
        if Some(index) != missing {
            delta.produced_utxo.insert(
                TxoRef(input(index).transaction_id, index as u32),
                Arc::new(EraCbor(8, minicbor::to_vec(output(coin)).unwrap())),
            );
        }
    }
    let writer = domain.state().start_writer().unwrap();
    writer.apply_utxoset(&delta).unwrap();
    writer.commit().unwrap();
    (domain, bytes)
}

#[test]
fn batch_validates_without_child_inputs_in_parent_references() {
    let (domain, bytes) = fixture(&[1, 2], &[], None);
    let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &bytes).unwrap();
    assert!(tx.reference_inputs().is_empty());
    assert_eq!(tx.inputs().len(), 1);
    assert_eq!(tx.sub_transactions().len(), 2);
    assert!(evaluate(&domain, &bytes).unwrap().is_empty());
    let hash = domain
        .receive_tx("synthetic-key-batch", &domain.read_chain(), &bytes)
        .expect("Dolos must load the children's spending UTxOs through requires()");
    assert_eq!(hash, tx.hash());
    let accepted = domain.mempool().peek_pending();
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0].payload().0, 8);
    assert_eq!(accepted[0].payload().1, bytes);
}

#[test]
fn batch_missing_parent_child_or_reference_input_rejects() {
    // First prove the otherwise identical signed transaction is admissible.
    let (domain, bytes) = fixture(&[1, 2], &[3], None);
    domain.validate_tx(&domain.read_chain(), &bytes).unwrap();
    for missing in [0, 1, 2, 3] {
        let (domain, bytes) = fixture(&[1, 2], &[3], Some(missing));
        let error = domain
            .receive_tx("missing-batch-input", &domain.read_chain(), &bytes)
            .unwrap_err();
        assert!(
            matches!(
                (missing, &error),
                (
                    0..=2,
                    DomainError::ChainError(ChainError::Phase1ValidationRejected(
                        ValidationError::PostAlonzo(PostAlonzoError::InputNotInUTxO)
                    ))
                ) | (
                    3,
                    DomainError::ChainError(ChainError::Phase1ValidationRejected(
                        ValidationError::PostAlonzo(PostAlonzoError::ReferenceInputNotInUTxO)
                    ))
                )
            ),
            "missing input {missing}: {error:?}"
        );
        assert!(!domain.mempool().has_pending());
    }
}

#[test]
fn batch_duplicate_spending_references_still_reject() {
    for children in [[1, 1], [0, 1]] {
        let (domain, bytes) = fixture(&children, &[], None);
        let error = domain
            .receive_tx("duplicate-batch-input", &domain.read_chain(), &bytes)
            .unwrap_err();
        assert!(
            matches!(
                error,
                DomainError::ChainError(ChainError::Phase1ValidationRejected(
                    ValidationError::DijkstraInputAlreadySpent
                ))
            ),
            "{error:?}"
        );
        assert!(!domain.mempool().has_pending());
    }
}
