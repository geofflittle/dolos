//! Transaction validation and mempool submission.
//!
//! This module provides functionality for validating transactions against
//! the current ledger state and submitting them to the mempool.

use tracing::{debug, instrument};

use crate::{
    ChainError, ChainLogic, Domain, DomainError, MempoolAwareUtxoStore, MempoolError, MempoolStore,
    MempoolTx, PassedBound, Phase2Log, StateStore, TxHash,
};

/// Extension trait for transaction submission operations.
///
/// This trait extends any `Domain` implementation with methods for
/// validating and submitting transactions to the mempool.
pub trait SubmitExt: Domain {
    /// Estimate script costs using ledger and mempool inputs, without admission
    /// validation or inserting the transaction into the mempool.
    #[instrument(skip_all)]
    fn estimate_tx(
        &self,
        chain: &Self::Chain,
        cbor: &[u8],
    ) -> Result<crate::mempool::MultiEraEvalReport, DomainError> {
        let utxos = MempoolAwareUtxoStore::<'_, Self>::new(self.state(), self.mempool());
        Ok(chain.estimate_tx(cbor, &utxos, &self.genesis())?)
    }

    /// Validate a transaction against the current ledger state.
    ///
    /// Checks that the transaction is valid according to the current
    /// ledger state and mempool contents.
    ///
    /// # Arguments
    ///
    /// * `chain` - Reference to the chain logic for validation
    /// * `cbor` - CBOR-encoded transaction bytes
    ///
    /// # Returns
    ///
    /// The validated mempool transaction if valid.
    #[instrument(skip_all)]
    fn validate_tx(&self, chain: &Self::Chain, cbor: &[u8]) -> Result<MempoolTx, DomainError> {
        let tip = self.state().read_cursor()?;

        let utxos = MempoolAwareUtxoStore::<'_, Self>::new(self.state(), self.mempool());

        let tx = chain.validate_tx(cbor, &utxos, tip, &self.genesis())?;

        Ok(tx)
    }

    /// Validate and receive a transaction into the mempool.
    ///
    /// Validates the transaction and, if valid, adds it to the mempool
    /// for potential inclusion in a future block.
    ///
    /// # Arguments
    ///
    /// * `source` - A reference to the mechanims from which the tx was received
    /// * `chain` - Reference to the chain logic for validation
    /// * `cbor` - CBOR-encoded transaction bytes
    ///
    /// # Returns
    ///
    /// The transaction hash if successfully submitted.
    #[instrument(skip_all)]
    fn receive_tx(
        &self,
        source: &str,
        chain: &Self::Chain,
        cbor: &[u8],
    ) -> Result<TxHash, DomainError> {
        let _guard = match self.mempool().submit_lock().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        let tx = self.validate_tx(chain, cbor)?;
        let hash = tx.hash();

        debug!(tx.hash = %hash, source=source, "tx received");

        self.mempool().receive(tx)?;

        Ok(hash)
    }

    /// Validates each pending transaction again, in queue order, and drops
    /// those the ledger now refuses.
    fn revalidate_pending(&self, chain: &Self::Chain) -> Result<Vec<TxHash>, DomainError> {
        if !self.mempool().has_pending() {
            return Ok(vec![]);
        }

        let _guard = match self.mempool().submit_lock().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        let tip = self.state().read_cursor()?;
        let genesis = self.genesis();
        let mut kept = Vec::new();
        let mut refused = Vec::new();

        // Each transaction is judged after the pending transactions kept before
        // it.
        for tx in self.mempool().peek_pending() {
            let utxos = MempoolAwareUtxoStore::<'_, Self>::with_pending(
                self.state(),
                self.mempool(),
                kept.clone(),
            );
            match chain.validate_tx(&tx.payload().1, &utxos, tip.clone(), &genesis) {
                Ok(_) => kept.push(tx),
                Err(error) => {
                    debug!(tx.hash = %tx.hash(), %error, "pending tx refused on revalidation");
                    refused.push(tx.hash());
                }
            }
        }

        self.mempool().drop_pending(&refused)?;

        Ok(refused)
    }
}

impl<D: Domain> SubmitExt for D {}

/// A failed submit, split by whose fault it is.
#[derive(Debug, thiserror::Error)]
pub enum SubmitError {
    #[error(transparent)]
    Client(#[from] ClientFault),

    #[error(transparent)]
    Server(DomainError),
}

/// A submit the client can correct or retry.
#[derive(Debug, thiserror::Error)]
pub enum ClientFault {
    #[error("{0}")]
    InvalidBytes(String),

    #[error("duplicate tx")]
    Duplicate,

    #[error("{0}")]
    InvalidTx(String),

    #[error("phase-2 script rejected the transaction")]
    ScriptFailure(Phase2Log),

    #[error("mempool full: {0}")]
    MempoolFull(PassedBound),
}

impl From<DomainError> for SubmitError {
    fn from(error: DomainError) -> Self {
        match error {
            DomainError::ChainError(x) => from_chain(x),
            DomainError::MempoolError(x) => from_mempool(x),
            x @ (DomainError::WalError(_)
            | DomainError::StateError(_)
            | DomainError::ArchiveError(_)
            | DomainError::InconsistentState { .. }
            | DomainError::Internal(_)
            | DomainError::WalIsEmpty
            | DomainError::RollbackTargetNotInWal(_)
            | DomainError::ArchiveWalGap(_)
            | DomainError::StopEpochReached) => SubmitError::Server(x),
        }
    }
}

fn from_chain(error: ChainError) -> SubmitError {
    let fault = match error {
        ChainError::BrokenInvariant(x) => ClientFault::InvalidTx(x.to_string()),
        ChainError::DecodingError(x) => ClientFault::InvalidBytes(x.to_string()),
        ChainError::CborDecodingError(x) => ClientFault::InvalidBytes(x.to_string()),
        ChainError::AddressDecoding(x) => ClientFault::InvalidBytes(x.to_string()),
        ChainError::Phase1ValidationRejected(x) => ClientFault::InvalidTx(x.to_string()),
        x @ ChainError::Phase2EvaluationError(_) => ClientFault::InvalidTx(x.to_string()),
        ChainError::Phase2ValidationRejected(x) => ClientFault::ScriptFailure(x),
        x @ (ChainError::CantReceiveBlock(_)
        | ChainError::InvalidNamespace(_)
        | ChainError::StateError(_)
        | ChainError::ArchiveError(_)
        | ChainError::GenesisFieldMissing(_)
        | ChainError::InvalidConfig(_)
        | ChainError::PParamsNotFound(_)
        | ChainError::NoActiveEpoch
        | ChainError::MissingGovState
        | ChainError::EraNotFound
        | ChainError::EpochValueVersionNotFound(_)
        | ChainError::PoolSnapshotLagging { .. }
        | ChainError::MissingRewards
        | ChainError::InvalidPoolParams
        | ChainError::InvalidProposalParams
        | ChainError::UnrecordableProposalPart(_)
        | ChainError::Phase2EvaluationUnsupported(_)
        | ChainError::Consensus(_)) => return SubmitError::Server(x.into()),
    };
    fault.into()
}

fn from_mempool(error: MempoolError) -> SubmitError {
    let fault = match error {
        MempoolError::TraverseError(x) => ClientFault::InvalidBytes(x.to_string()),
        MempoolError::DecodeError(x) => ClientFault::InvalidBytes(x.to_string()),
        MempoolError::DuplicateTx => ClientFault::Duplicate,
        MempoolError::InvalidTx(x) => ClientFault::InvalidTx(x),
        MempoolError::Full(x) => ClientFault::MempoolFull(x),
        x @ (MempoolError::Internal(_)
        | MempoolError::StateError(_)
        | MempoolError::PlutusNotSupported
        | MempoolError::PParamsNotAvailable) => return SubmitError::Server(x.into()),
    };
    fault.into()
}
