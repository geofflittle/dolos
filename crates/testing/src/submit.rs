use std::fmt::Debug;

use dolos_core::{BrokenInvariant, ChainError, DomainError, MempoolError, PassedBound, StateError};
use pallas::{
    codec::minicbor::decode,
    ledger::{
        traverse,
        validate::utils::{PostAlonzoError, ValidationError},
    },
};

/// One error of each source a transaction submit can fail with, each under a name.
pub fn submit_errors() -> Vec<(&'static str, DomainError)> {
    vec![
        (
            "undecodable-bytes",
            MempoolError::DecodeError(decode::Error::message("end of input bytes")).into(),
        ),
        (
            "bytes-of-no-era",
            MempoolError::TraverseError(traverse::Error::UnknownCbor("not a tx".into())).into(),
        ),
        (
            "trailing-bytes",
            ChainError::CborDecodingError(decode::Error::message("trailing transaction bytes"))
                .into(),
        ),
        (
            "bytes-of-another-era",
            ChainError::DecodingError(traverse::Error::UnknownCbor("not a tx".into())).into(),
        ),
        ("duplicate", MempoolError::DuplicateTx.into()),
        (
            "invalid-tx",
            MempoolError::InvalidTx("bad witness".into()).into(),
        ),
        (
            "phase-one-rejected",
            ChainError::Phase1ValidationRejected(ValidationError::PostAlonzo(
                PostAlonzoError::InputNotInUTxO,
            ))
            .into(),
        ),
        (
            "phase-two-unevaluable",
            ChainError::Phase2EvaluationError("can't eval without redeemers".into()).into(),
        ),
        (
            "script-failure",
            ChainError::Phase2ValidationRejected(vec!["the trace".into()]).into(),
        ),
        (
            "mempool-full",
            MempoolError::Full(PassedBound::Txs {
                bound: 2,
                would_hold: 3,
            })
            .into(),
        ),
        ("pparams-missing", MempoolError::PParamsNotAvailable.into()),
        (
            "store-failed",
            StateError::InternalStoreError("disk".into()).into(),
        ),
        (
            "broken-invariant",
            ChainError::BrokenInvariant(BrokenInvariant::BadBootstrap).into(),
        ),
        ("no-active-epoch", ChainError::NoActiveEpoch.into()),
    ]
}

/// A line for each sample of [`submit_errors`] that `answer` answers other than `want` names, and for each name only one side has.
pub fn wrong_answers<A: PartialEq + Debug>(
    want: &[(&str, A)],
    answer: impl Fn(DomainError) -> A,
) -> Vec<String> {
    let samples = submit_errors();
    let mut wrong: Vec<String> = want
        .iter()
        .filter(|(name, _)| !samples.iter().any(|(sample, _)| sample == name))
        .map(|(name, _)| format!("{name}: no such sample"))
        .collect();
    for (name, error) in samples {
        match want.iter().find(|(wanted, _)| *wanted == name) {
            None => wrong.push(format!("{name}: no answer wanted")),
            Some((_, wanted)) => {
                let got = answer(error);
                if got != *wanted {
                    wrong.push(format!("{name}: got {got:?}, want {wanted:?}"));
                }
            }
        }
    }
    wrong
}
