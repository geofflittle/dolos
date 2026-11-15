// Each RPC version runs these assertions over the same four outputs.
use super::*;
use dolos_testing::TestAddress;
use std::collections::HashSet;
use u5c::query::UtxoPredicate;

const HOLDERS: [TestAddress; 4] = [
    TestAddress::Alice,
    TestAddress::Bob,
    TestAddress::Carol,
    TestAddress::Dave,
];

/// A domain holding one output at each address of `HOLDERS`, with the
/// reference of each in the same order.
fn four_outputs() -> (ToyDomain, Vec<TxoRef>) {
    let mut delta = UtxoSetDelta::default();
    let mut refs = Vec::new();
    for (i, holder) in HOLDERS.into_iter().enumerate() {
        let txo = TxoRef(dolos_testing::tx_sequence_to_hash(7000 + i as u64), 0);
        let output = dolos_testing::utxo_with_value(
            holder,
            pallas::ledger::primitives::conway::Value::Coin(dolos_testing::MIN_UTXO_AMOUNT),
        );
        delta
            .produced_utxo
            .insert(txo.clone(), std::sync::Arc::new(output));
        refs.push(txo);
    }
    (ToyDomain::new(Some(delta), None), refs)
}

/// The payment part of the address of `holder`.
fn payment_of(holder: &TestAddress) -> Vec<u8> {
    match pallas::ledger::addresses::Address::from_bytes(&holder.to_bytes()).unwrap() {
        pallas::ledger::addresses::Address::Shelley(x) => x.payment().to_vec(),
        other => panic!("not a shelley address {other:?}"),
    }
}

/// A predicate whose match is the address pattern `address`.
fn matching(address: u5c::cardano::AddressPattern) -> UtxoPredicate {
    output_pattern(Some(address), None)
}

/// A predicate whose match is the output pattern of `address` and `asset`.
fn output_pattern(
    address: Option<u5c::cardano::AddressPattern>,
    asset: Option<u5c::cardano::AssetPattern>,
) -> UtxoPredicate {
    UtxoPredicate {
        r#match: Some(u5c::query::AnyUtxoPattern {
            utxo_pattern: Some(UtxoPattern::Cardano(u5c::cardano::TxOutputPattern {
                address,
                asset,
            })),
        }),
        ..Default::default()
    }
}

/// The address pattern of exactly the address of the holder at `i`.
// The address pattern fields are optional at v1 and v1beta and plain at v1alpha.
#[allow(clippy::useless_conversion)]
fn exact(i: usize) -> u5c::cardano::AddressPattern {
    u5c::cardano::AddressPattern {
        exact_address: bytes::Bytes::from(HOLDERS[i].to_bytes()).into(),
        ..Default::default()
    }
}

/// A predicate that matches the output at the address of the holder at `i`.
fn at(i: usize) -> UtxoPredicate {
    matching(exact(i))
}

/// A predicate whose match is a pattern that names no criterion.
fn match_anything() -> UtxoPredicate {
    UtxoPredicate {
        r#match: Some(u5c::query::AnyUtxoPattern { utxo_pattern: None }),
        ..Default::default()
    }
}

/// The references SearchUtxos answers for `predicate`, or the code it refuses
/// it with.
async fn searched(
    domain: &ToyDomain,
    predicate: Option<UtxoPredicate>,
) -> Result<HashSet<TxoRef>, tonic::Code> {
    let response = QueryServiceImpl::new(domain.clone())
        .search_utxos(Request::new(u5c::query::SearchUtxosRequest {
            predicate,
            ..Default::default()
        }))
        .await
        .map_err(|e| e.code())?;
    Ok(response
        .into_inner()
        .items
        .into_iter()
        .map(|x| from_u5c_txoref(x.txo_ref.unwrap()).unwrap())
        .collect())
}

#[tokio::test]
async fn search_utxos_refuses_a_predicate_that_admits_outputs_no_criterion_names() {
    let (domain, _) = four_outputs();
    let refused = Err(tonic::Code::InvalidArgument);

    for predicate in [
        None,
        Some(UtxoPredicate::default()),
        Some(match_anything()),
        Some(UtxoPredicate {
            not: vec![at(0)],
            ..Default::default()
        }),
        Some(UtxoPredicate {
            any_of: vec![at(0), UtxoPredicate {
                not: vec![at(1)],
                ..Default::default()
            }],
            ..Default::default()
        }),
        Some(matching(Default::default())),
        Some(output_pattern(None, Some(Default::default()))),
    ] {
        assert_eq!(
            searched(&domain, predicate.clone()).await,
            refused,
            "{predicate:?}"
        );
    }
}

#[tokio::test]
async fn search_utxos_answers_each_clause_as_the_spec_defines_it() {
    let (domain, refs) = four_outputs();
    let only = |held: &[usize]| Ok(held.iter().map(|i| refs[*i].clone()).collect());
    let any_of = |list: Vec<UtxoPredicate>| UtxoPredicate {
        any_of: list,
        ..Default::default()
    };
    let all_of = |list: Vec<UtxoPredicate>| UtxoPredicate {
        all_of: list,
        ..Default::default()
    };
    let none_of = |list: Vec<UtxoPredicate>| UtxoPredicate {
        not: list,
        ..Default::default()
    };
    #[allow(clippy::useless_conversion)]
    let exact_and_payment = |address: usize, payment: usize| {
        matching(u5c::cardano::AddressPattern {
            payment_part: bytes::Bytes::from(payment_of(&HOLDERS[payment])).into(),
            ..exact(address)
        })
    };

    for (predicate, expected) in [
        (at(1), only(&[1])),
        (
            UtxoPredicate {
                not: vec![at(0)],
                ..at(0)
            },
            only(&[]),
        ),
        (exact_and_payment(0, 1), only(&[])),
        (exact_and_payment(0, 0), only(&[0])),
        (output_pattern(Some(exact(2)), Some(Default::default())), only(&[2])),
        (any_of(vec![at(0), at(1)]), only(&[0, 1])),
        (
            all_of(vec![any_of(vec![at(0), at(1)]), any_of(vec![at(1), at(2)])]),
            only(&[1]),
        ),
        (
            UtxoPredicate {
                not: vec![at(1)],
                ..any_of(vec![at(0), at(1), at(2)])
            },
            only(&[0, 2]),
        ),
        (all_of(vec![at(0), match_anything()]), only(&[0])),
        (
            UtxoPredicate {
                any_of: vec![at(0), none_of(vec![at(1)])],
                ..at(2)
            },
            only(&[2]),
        ),
        (
            UtxoPredicate {
                any_of: vec![at(0), none_of(vec![at(1)])],
                ..at(1)
            },
            only(&[]),
        ),
    ] {
        assert_eq!(
            searched(&domain, Some(predicate.clone())).await,
            expected,
            "{predicate:?}"
        );
    }
}
