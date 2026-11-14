use bytes::Bytes;
use dolos_core::{ClientFault, SubmitError};
use pallas::crypto::hash::Hash;
use tonic::Status;

pub fn bytes_to_hash32(data: &Bytes) -> Result<Hash<32>, Status> {
    let array: [u8; 32] = data
        .as_ref()
        .try_into()
        .map_err(|_| Status::invalid_argument("invalid hash value, needs to be 32-bytes long"))?;

    Ok(Hash::<32>::new(array))
}

/// The status a submit answers when receiving its transaction fails.
pub fn submit_error_status(error: SubmitError) -> Status {
    let message = format!("could not process tx: {error}");
    match error {
        SubmitError::Client(
            ClientFault::InvalidBytes(_)
            | ClientFault::Duplicate
            | ClientFault::InvalidTx(_)
            | ClientFault::ScriptFailure(_),
        ) => Status::invalid_argument(message),
        SubmitError::Client(ClientFault::MempoolFull(_)) => Status::resource_exhausted(message),
        SubmitError::Server(_) => Status::internal(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dolos_core::{DomainError, MempoolError, PassedBound};
    use dolos_testing::submit::wrong_answers;
    use tonic::Code;

    fn answer(error: DomainError) -> Code {
        submit_error_status(error.into()).code()
    }

    #[test]
    fn each_submit_error_answers_the_code_of_its_fault() {
        let client = Code::InvalidArgument;
        let wrong = wrong_answers(
            &[
                ("undecodable-bytes", client),
                ("bytes-of-no-era", client),
                ("trailing-bytes", client),
                ("bytes-of-another-era", client),
                ("duplicate", client),
                ("invalid-tx", client),
                ("phase-one-rejected", client),
                ("phase-two-unevaluable", client),
                ("script-failure", client),
                ("mempool-full", Code::ResourceExhausted),
                ("pparams-missing", Code::Internal),
                ("store-failed", Code::Internal),
                ("broken-invariant", Code::Internal),
                ("no-active-epoch", Code::Internal),
            ],
            answer,
        );
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[test]
    fn a_refused_transaction_answers_invalid_argument() {
        let status = submit_error_status(DomainError::from(MempoolError::DuplicateTx).into());
        assert_eq!(status.code(), Code::InvalidArgument);
        assert_eq!(status.message(), "could not process tx: duplicate tx");
    }

    #[test]
    fn a_full_mempool_answers_resource_exhausted_naming_the_bound() {
        let full = MempoolError::Full(PassedBound::Bytes {
            bound: 4_000_000,
            would_hold: 4_000_100,
        });
        let status = submit_error_status(DomainError::from(full).into());
        assert_eq!(status.code(), Code::ResourceExhausted);
        assert_eq!(
            status.message(),
            "could not process tx: mempool full: would hold 4000100 payload bytes, past the bound of 4000000"
        );
    }
}
