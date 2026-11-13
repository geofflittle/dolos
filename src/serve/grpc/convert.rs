use bytes::Bytes;
use dolos_core::{DomainError, MempoolError};
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
pub fn submit_error_status(error: DomainError) -> Status {
    let message = format!("could not process tx: {error}");
    match error {
        DomainError::MempoolError(error) => match error {
            MempoolError::Full(_) => Status::resource_exhausted(message),
            MempoolError::Internal(_)
            | MempoolError::TraverseError(_)
            | MempoolError::DecodeError(_)
            | MempoolError::StateError(_)
            | MempoolError::PlutusNotSupported
            | MempoolError::InvalidTx(_)
            | MempoolError::PParamsNotAvailable
            | MempoolError::DuplicateTx => Status::invalid_argument(message),
        },
        _ => Status::invalid_argument(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dolos_core::PassedBound;
    use tonic::Code;

    #[test]
    fn a_refused_transaction_answers_invalid_argument() {
        let status = submit_error_status(MempoolError::DuplicateTx.into());
        assert_eq!(status.code(), Code::InvalidArgument);
        assert_eq!(
            status.message(),
            "could not process tx: mempool error: duplicate tx"
        );
    }

    #[test]
    fn a_full_mempool_answers_resource_exhausted_naming_the_bound() {
        let full = MempoolError::Full(PassedBound::Bytes {
            bound: 4_000_000,
            would_hold: 4_000_100,
        });
        let status = submit_error_status(full.into());
        assert_eq!(status.code(), Code::ResourceExhausted);
        assert_eq!(
            status.message(),
            "could not process tx: mempool error: mempool full: would hold 4000100 payload bytes, past the bound of 4000000"
        );
    }
}
