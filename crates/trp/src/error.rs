use dolos_core::DomainError;
use jsonrpsee::types::ErrorCode;
use serde_json::Value;
use tx3_resolver::trp::errors::TrpError as _;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    InternalError(String),

    #[error(transparent)]
    MinicborError(#[from] pallas::codec::minicbor::decode::Error),

    #[error(transparent)]
    TraverseError(#[from] pallas::ledger::traverse::Error),

    #[error(transparent)]
    Tx3Error(#[from] Box<tx3_resolver::Error>),

    #[error(transparent)]
    JsonRpcError(#[from] jsonrpsee::types::ErrorObjectOwned),

    #[error("invalid params: {0}")]
    InvalidParams(String),

    #[error("only txs from Conway era are supported")]
    UnsupportedTxEra,

    #[error("node can't resolve txs while running at era {era}")]
    UnsupportedEra { era: String },
}

impl From<dolos_core::StateError> for Error {
    fn from(error: dolos_core::StateError) -> Self {
        Error::InternalError(error.to_string())
    }
}

impl Error {
    pub fn code(&self) -> i32 {
        match self {
            Error::InvalidParams(_) => ErrorCode::InvalidParams.code(),
            Error::Tx3Error(x) => x.code(),
            Error::JsonRpcError(err) => err.code(),
            _ => ErrorCode::InternalError.code(),
        }
    }

    pub fn data(&self) -> Option<Value> {
        match self {
            Error::JsonRpcError(err) => err.data().and_then(|v| serde_json::to_value(v).ok()),
            Error::Tx3Error(x) => x.data(),
            _ => None,
        }
    }

    fn tx_not_accepted(msg: impl std::fmt::Display) -> Self {
        Error::Tx3Error(Box::new(tx3_resolver::Error::TxNotAccepted(
            msg.to_string(),
        )))
    }

    fn tx_script_failure(logs: Vec<String>) -> Self {
        Error::Tx3Error(Box::new(tx3_resolver::Error::TxScriptFailure(logs)))
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        Error::InternalError(error.to_string())
    }

    fn transient(error: impl std::fmt::Display) -> Self {
        Error::Tx3Error(Box::new(tx3_resolver::Error::TransientError(
            error.to_string(),
        )))
    }
}

impl From<dolos_core::MempoolError> for Error {
    fn from(error: dolos_core::MempoolError) -> Self {
        use dolos_core::MempoolError;
        match error {
            MempoolError::PlutusNotSupported => Error::tx_not_accepted("Plutus not supported"),
            MempoolError::InvalidTx(x) => Error::tx_not_accepted(x),
            x @ MempoolError::Full(_) => Error::transient(x),
            x @ (MempoolError::Internal(_)
            | MempoolError::TraverseError(_)
            | MempoolError::DecodeError(_)
            | MempoolError::StateError(_)
            | MempoolError::PParamsNotAvailable
            | MempoolError::DuplicateTx) => Error::internal(x),
        }
    }
}

impl From<dolos_core::ChainError> for Error {
    fn from(error: dolos_core::ChainError) -> Self {
        match error {
            dolos_core::ChainError::BrokenInvariant(x) => Error::tx_not_accepted(x),
            dolos_core::ChainError::DecodingError(x) => Error::tx_not_accepted(x),
            dolos_core::ChainError::CborDecodingError(x) => Error::tx_not_accepted(x),
            dolos_core::ChainError::Phase1ValidationRejected(x) => Error::tx_not_accepted(x),
            dolos_core::ChainError::Phase2ValidationRejected(x) => Error::tx_script_failure(x),
            x => Error::internal(x),
        }
    }
}

impl From<DomainError> for Error {
    fn from(error: DomainError) -> Self {
        match error {
            dolos_core::DomainError::ChainError(e) => Error::from(e),
            dolos_core::DomainError::MempoolError(e) => Error::from(e),
            _ => Error::internal(error),
        }
    }
}

impl From<tx3_resolver::Error> for Error {
    fn from(error: tx3_resolver::Error) -> Self {
        Error::Tx3Error(Box::new(error))
    }
}

impl From<Error> for jsonrpsee::types::ErrorObject<'_> {
    fn from(error: Error) -> Self {
        let message = error.to_string();

        jsonrpsee::types::ErrorObject::owned(error.code(), message, error.data())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dolos_core::{MempoolError, PassedBound};
    use jsonrpsee::types::ErrorObjectOwned;
    use tx3_resolver::trp::errors::{CODE_TRANSIENT_ERROR, CODE_TX_NOT_ACCEPTED};

    fn submit_error(error: MempoolError) -> ErrorObjectOwned {
        Error::from(DomainError::MempoolError(error)).into()
    }

    #[test]
    fn an_invalid_transaction_answers_not_accepted() {
        let error = submit_error(MempoolError::InvalidTx("bad witness".into()));
        assert_eq!(error.code(), CODE_TX_NOT_ACCEPTED);
        assert_eq!(error.message(), "tx was not accepted: bad witness");
    }

    #[test]
    fn a_missing_protocol_parameter_set_answers_internal_error() {
        let error = submit_error(MempoolError::PParamsNotAvailable);
        assert_eq!(error.code(), ErrorCode::InternalError.code());
    }

    #[test]
    fn a_full_mempool_answers_transient_naming_the_bound() {
        let error = submit_error(MempoolError::Full(PassedBound::Txs {
            bound: 2,
            would_hold: 3,
        }));
        assert_eq!(error.code(), CODE_TRANSIENT_ERROR);
        assert_eq!(
            error.message(),
            "transient error: mempool full: would hold 3 transactions, past the bound of 2"
        );
    }
}
