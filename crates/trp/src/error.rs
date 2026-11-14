use dolos_core::{ClientFault, SubmitError};
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

impl From<SubmitError> for Error {
    fn from(error: SubmitError) -> Self {
        match error {
            SubmitError::Client(
                x @ (ClientFault::InvalidBytes(_)
                | ClientFault::Duplicate
                | ClientFault::InvalidTx(_)),
            ) => Error::tx_not_accepted(x),
            SubmitError::Client(ClientFault::ScriptFailure(logs)) => Error::tx_script_failure(logs),
            SubmitError::Client(x @ ClientFault::MempoolFull(_)) => Error::transient(x),
            SubmitError::Server(x) => Error::internal(x),
        }
    }
}

impl From<dolos_core::ChainError> for Error {
    fn from(error: dolos_core::ChainError) -> Self {
        Error::internal(error)
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
    use dolos_core::{DomainError, MempoolError, PassedBound};
    use dolos_testing::submit::wrong_answers;
    use jsonrpsee::types::ErrorObjectOwned;
    use tx3_resolver::trp::errors::{
        CODE_TRANSIENT_ERROR, CODE_TX_NOT_ACCEPTED, CODE_TX_SCRIPT_FAILURE,
    };

    fn submit_error(error: MempoolError) -> ErrorObjectOwned {
        Error::from(SubmitError::from(DomainError::MempoolError(error))).into()
    }

    fn answer(error: DomainError) -> i32 {
        ErrorObjectOwned::from(Error::from(SubmitError::from(error))).code()
    }

    #[test]
    fn each_submit_error_answers_the_code_of_its_fault() {
        let refused = CODE_TX_NOT_ACCEPTED;
        let internal = ErrorCode::InternalError.code();
        let wrong = wrong_answers(
            &[
                ("undecodable-bytes", refused),
                ("bytes-of-no-era", refused),
                ("trailing-bytes", refused),
                ("bytes-of-another-era", refused),
                ("duplicate", refused),
                ("invalid-tx", refused),
                ("phase-one-rejected", refused),
                ("phase-two-unevaluable", refused),
                ("phase-two-unbuilt", internal),
                ("script-failure", CODE_TX_SCRIPT_FAILURE),
                ("mempool-full", CODE_TRANSIENT_ERROR),
                ("pparams-missing", internal),
                ("store-failed", internal),
                ("broken-invariant", internal),
                ("no-active-epoch", internal),
            ],
            answer,
        );
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    #[test]
    fn an_invalid_transaction_answers_not_accepted() {
        let error = submit_error(MempoolError::InvalidTx("bad witness".into()));
        assert_eq!(error.code(), CODE_TX_NOT_ACCEPTED);
        assert_eq!(error.message(), "tx was not accepted: bad witness");
    }

    #[test]
    fn a_duplicate_transaction_answers_not_accepted() {
        let error = submit_error(MempoolError::DuplicateTx);
        assert_eq!(error.code(), CODE_TX_NOT_ACCEPTED);
        assert_eq!(error.message(), "tx was not accepted: duplicate tx");
    }

    #[test]
    fn undecodable_transaction_bytes_answer_not_accepted() {
        let error = submit_error(MempoolError::DecodeError(
            pallas::codec::minicbor::decode::Error::message("end of input bytes"),
        ));
        assert_eq!(error.code(), CODE_TX_NOT_ACCEPTED);
    }

    #[test]
    fn bytes_of_no_known_era_answer_not_accepted() {
        let error = submit_error(MempoolError::TraverseError(
            pallas::ledger::traverse::Error::UnknownCbor("not a transaction".into()),
        ));
        assert_eq!(error.code(), CODE_TX_NOT_ACCEPTED);
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
