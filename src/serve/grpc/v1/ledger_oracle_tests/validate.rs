use std::{
    cell::RefCell,
    fmt,
    panic::{self, AssertUnwindSafe},
    sync::{Arc, Once, PoisonError},
};

use dolos_cardano::CardanoLogic;
use dolos_core::{
    BlockHash, BlockSlot, ChainError, ChainLogic as _, ChainPoint, Domain as _, DomainError,
    EraCbor, MempoolAwareUtxoStore, MempoolStore as _, RawBlock, StateStore as _, StateWriter as _,
    SubmitExt as _, SyncExt as _, TxHash, TxoRef, UtxoSetDelta,
};
use ledger_oracle_judge::{
    record::{Phase2, UtxoEntry},
    refusal::{DolosClass, RefusalClass},
    verdict::DolosVerdict,
};
use pallas::{
    codec::minicbor,
    ledger::{
        primitives::{
            dijkstra::{
                Block, BlockBody, BlockTransaction, Header, HeaderBody, HeaderVersionInfo,
                MempoolTransaction, OperationalCert,
            },
            KeepRaw, MaybeIndefArray, Nullable, VrfCert,
        },
        traverse::MultiEraBlock,
        validate::{
            phase2::{tx::TxEvalResult, Builtin},
            utils::{PostAlonzoError, ValidationError},
        },
    },
};

use super::seed::{txo_ref, DIJKSTRA};
use crate::adapters::{DomainAdapter, MempoolBackend};

/// The hard fork variant that tags a Dijkstra block.
const DIJKSTRA_BLOCK: u16 = 8;

/// Why Dolos did not apply a block.
#[derive(Debug)]
pub(super) enum RollFailure {
    Error(DomainError),
    Panic { at: PanicAt, message: String },
}

impl fmt::Display for RollFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RollFailure::Error(e) => write!(f, "error {e}"),
            RollFailure::Panic { at, message } => write!(f, "panic at={at} {message}"),
        }
    }
}

/// The source location of a panic, as the panic hook reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PanicAt {
    Source(String),
    Unreported,
}

impl fmt::Display for PanicAt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PanicAt::Source(location) => write!(f, "{location}"),
            PanicAt::Unreported => write!(f, "unreported"),
        }
    }
}

thread_local! {
    /// The location of the last panic on this thread.
    static LAST_PANIC: RefCell<PanicAt> = const { RefCell::new(PanicAt::Unreported) };
}

/// Installs, once, a panic hook that records each panic's location for its thread and then runs the hook it replaces.
fn record_panic_locations() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let at = match info.location() {
                Some(l) => PanicAt::Source(format!("{}:{}:{}", l.file(), l.line(), l.column())),
                None => PanicAt::Unreported,
            };
            LAST_PANIC.with(|last| *last.borrow_mut() = at);
            previous(info);
        }));
    });
}

/// The header hash and block number of the last block a run applied.
#[derive(Debug, Clone, Copy)]
pub(super) struct Tip {
    hash: BlockHash,
    number: u64,
}

impl Tip {
    /// The header hash of the block.
    pub(super) fn hash(&self) -> BlockHash {
        self.hash
    }
}

/// A block at the slot whose parent is the tip, holding each transaction with its scripts flag, and the tip it makes, or the error of a transaction Dolos does not decode.
pub(super) fn block(
    slot: u64,
    tip: Option<Tip>,
    txs: &[(&[u8], Phase2)],
) -> Result<(Tip, RawBlock), minicbor::decode::Error> {
    let transactions = txs
        .iter()
        .map(|(raw, phase2)| {
            let tx: MempoolTransaction = minicbor::decode(raw)?;
            let success = match phase2 {
                Phase2::Valid => true,
                Phase2::Invalid => false,
            };
            Ok(BlockTransaction {
                success,
                ..BlockTransaction::from(tx)
            })
        })
        .collect::<Result<_, _>>()?;
    let number = tip.map_or(1, |t| t.number + 1);
    let header = Header {
        header_body: HeaderBody {
            block_number: number,
            slot,
            prev_hash: tip.map(|t| t.hash),
            issuer_vkey: vec![0; 32].into(),
            vrf_vkey: vec![0; 32].into(),
            vrf_result: VrfCert(vec![0; 64].into(), vec![0; 80].into()),
            block_body_size: 0,
            block_body_hash: [0; 32].into(),
            operational_cert: OperationalCert {
                operational_cert_hot_vkey: vec![0; 32].into(),
                operational_cert_sequence_number: 0,
                operational_cert_kes_period: 0,
                operational_cert_sigma: vec![0; 64].into(),
            },
            header_version_info: HeaderVersionInfo {
                highest_supported_major_version: 12,
                self_reported_software_tag: 0,
            },
            block_body_contains_leios_cert: false,
            eb_announcement: Nullable::Null,
        },
        body_signature: vec![0; 448].into(),
    };
    let block = Block {
        header: KeepRaw::from(header),
        block_body: BlockBody {
            transactions: MaybeIndefArray::Def(transactions),
            leios_certificate: Nullable::Null,
            peras_certificate: Nullable::Null,
        },
    };
    let raw = minicbor::to_vec((DIJKSTRA_BLOCK, &block)).expect("a block encodes");
    let hash = MultiEraBlock::decode(&raw)
        .expect("a written block decodes")
        .hash();
    Ok((Tip { hash, number }, Arc::new(raw)))
}

/// The mempool state a group puts each transaction it accepts in.
#[derive(Clone, Copy)]
pub(super) enum Relay {
    /// Accepted transactions stay pending.
    Held,
    /// Accepted transactions are marked inflight at once.
    Inflight,
}

/// A domain over the seeded stores with a mempool of its own and its cursor at one slot.
pub(super) struct Group {
    domain: DomainAdapter,
    relay: Relay,
}

/// The group for the records at one slot, whose mempool starts empty and whose cursor keeps the hash of the last applied block.
pub(super) fn group(base: &DomainAdapter, slot: u64, relay: Relay) -> Group {
    let hash = base
        .state()
        .read_cursor()
        .unwrap()
        .and_then(|point| point.hash())
        .unwrap_or_else(|| [0; 32].into());
    let writer = base.state().start_writer().unwrap();
    writer.set_cursor(ChainPoint::Specific(slot, hash)).unwrap();
    writer.commit().unwrap();
    let mut domain = base.clone();
    domain.mempool = MempoolBackend::Ephemeral(Default::default());
    Group { domain, relay }
}

impl Group {
    /// Submits one transaction to the group mempool and leaves it in the group's relay state.
    pub(super) fn receive(&self, tx: &[u8]) -> Result<TxHash, DomainError> {
        let chain = self.domain.read_chain();
        let hash = self.domain.receive_tx("ledger-oracle", &chain, tx)?;
        match self.relay {
            Relay::Held => {}
            Relay::Inflight => self.domain.mempool().mark_inflight(&[hash])?,
        }
        Ok(hash)
    }

    /// The redeemer evaluations Dolos estimates for one transaction against the group's stores and mempool, with the builtins each called.
    pub(super) fn estimate(
        &self,
        tx: &[u8],
    ) -> Result<Vec<(TxEvalResult, Vec<Builtin>)>, ChainError> {
        dolos_cardano::validate::estimate_tx_calls(tx, &self.utxos())
    }

    /// The minimum fee Dolos computes for one transaction against the group's stores and mempool.
    pub(super) fn min_fee(&self, tx: &[u8]) -> Result<Option<u64>, ChainError> {
        dolos_cardano::validate::min_fee(tx, &self.utxos())
    }

    fn utxos(&self) -> MempoolAwareUtxoStore<'_, DomainAdapter> {
        MempoolAwareUtxoStore::new(self.domain.state(), self.domain.mempool())
    }

    /// Writes ledger outputs into the state the group reads.
    pub(super) fn seed(&self, entries: &[&UtxoEntry]) {
        let delta = UtxoSetDelta {
            produced_utxo: entries
                .iter()
                .map(|e| {
                    let output = Arc::new(EraCbor(DIJKSTRA, e.output.0.clone()));
                    (txo_ref(&e.input), output)
                })
                .collect(),
            ..Default::default()
        };
        let writer = self.domain.state().start_writer().unwrap();
        writer.apply_utxoset(&delta).unwrap();
        writer.commit().unwrap();
    }

    /// Applies one block through the live sync path, reopening the chain logic from the state after a panic.
    pub(super) fn roll(&self, block: RawBlock) -> Result<BlockSlot, RollFailure> {
        record_panic_locations();
        LAST_PANIC.with(|last| *last.borrow_mut() = PanicAt::Unreported);
        let rolled = panic::catch_unwind(AssertUnwindSafe(|| self.domain.roll_forward(block)));
        let payload = match rolled {
            Ok(result) => return result.map_err(RollFailure::Error),
            Err(payload) => payload,
        };
        let at = LAST_PANIC.with(|last| last.replace(PanicAt::Unreported));
        let logic = CardanoLogic::initialize::<DomainAdapter>(
            Default::default(),
            &self.domain.state,
            &self.domain.genesis,
        )
        .unwrap();
        *self
            .domain
            .chain
            .write()
            .unwrap_or_else(PoisonError::into_inner) = logic;
        self.domain.chain.clear_poison();
        let message = match payload.downcast::<String>() {
            Ok(message) => *message,
            Err(payload) => payload
                .downcast_ref::<&str>()
                .map_or("non text payload", |m| m)
                .to_string(),
        };
        Err(RollFailure::Panic { at, message })
    }

    /// The domain the group submits to and rolls blocks into.
    pub(super) fn domain(&self) -> &DomainAdapter {
        &self.domain
    }

    /// Whether the group reads an output at the reference, from the stores or its mempool.
    pub(super) fn sees(&self, txo: TxoRef) -> bool {
        !self.utxos().get_utxos([txo].into()).unwrap().is_empty()
    }
}

/// The verdict for one submission.
pub(super) fn verdict(result: &Result<TxHash, DomainError>) -> DolosVerdict {
    let refused = |kind: &str| DolosVerdict::Refused {
        kind: kind.to_string(),
        class: DolosClass::NoRule,
    };
    let decoding = |kind: &str| DolosVerdict::Refused {
        kind: kind.to_string(),
        class: DolosClass::Rule(RefusalClass::Decoding),
    };
    let e = match result {
        Ok(_) => return DolosVerdict::Accepted,
        Err(DomainError::ChainError(e)) => e,
        Err(DomainError::WalError(_)) => return refused("WalError"),
        Err(DomainError::StateError(_)) => return refused("StateError"),
        Err(DomainError::ArchiveError(_)) => return refused("ArchiveError"),
        Err(DomainError::MempoolError(_)) => return refused("MempoolError"),
        Err(DomainError::InconsistentState { .. }) => return refused("InconsistentState"),
        Err(DomainError::Internal(_)) => return refused("Internal"),
        Err(DomainError::WalIsEmpty) => return refused("WalIsEmpty"),
        Err(DomainError::RollbackTargetNotInWal(_)) => return refused("RollbackTargetNotInWal"),
        Err(DomainError::ArchiveWalGap(_)) => return refused("ArchiveWalGap"),
        Err(DomainError::StopEpochReached) => return refused("StopEpochReached"),
    };
    match e {
        ChainError::Phase1ValidationRejected(v) => validation_verdict(v),
        ChainError::Phase2ValidationRejected(_) => DolosVerdict::Phase2Rejected,
        ChainError::Phase2EvaluationError(_) => refused("Phase2EvaluationError"),
        ChainError::Phase2EvaluationUnsupported(unsupported) => {
            DolosVerdict::Unsupported(unsupported.0.to_string())
        }
        ChainError::DijkstraWithdrawalConflict { .. } => refused("DijkstraWithdrawalConflict"),
        ChainError::CantReceiveBlock(_) => refused("CantReceiveBlock"),
        ChainError::BrokenInvariant(_) => refused("BrokenInvariant"),
        ChainError::DecodingError(_) => decoding("DecodingError"),
        ChainError::CborDecodingError(_) => decoding("CborDecodingError"),
        ChainError::InvalidNamespace(_) => refused("InvalidNamespace"),
        ChainError::AddressDecoding(_) => refused("AddressDecoding"),
        ChainError::StateError(_) => refused("StateError"),
        ChainError::ArchiveError(_) => refused("ArchiveError"),
        ChainError::GenesisFieldMissing(_) => refused("GenesisFieldMissing"),
        ChainError::InvalidConfig(_) => refused("InvalidConfig"),
        ChainError::PParamsNotFound(_) => refused("PParamsNotFound"),
        ChainError::NoActiveEpoch => refused("NoActiveEpoch"),
        ChainError::MissingGovState => refused("MissingGovState"),
        ChainError::EraNotFound => refused("EraNotFound"),
        ChainError::EpochValueVersionNotFound(_) => refused("EpochValueVersionNotFound"),
        ChainError::PoolSnapshotLagging { .. } => refused("PoolSnapshotLagging"),
        ChainError::MissingRewards => refused("MissingRewards"),
        ChainError::InvalidPoolParams => refused("InvalidPoolParams"),
        ChainError::InvalidProposalParams => refused("InvalidProposalParams"),
        ChainError::UnrecordableProposalPart(_) => refused("UnrecordableProposalPart"),
        ChainError::Consensus(_) => refused("Consensus"),
    }
}

/// The name of a Debug rendered variant, up to its first field.
fn variant_name(debug: String) -> String {
    let end = debug.find(['(', ' ', '{']).unwrap_or(debug.len());
    debug[..end].to_string()
}

/// The class of a post Alonzo validation error by its variant.
fn post_alonzo_class(e: &PostAlonzoError) -> DolosClass {
    use PostAlonzoError as P;
    use RefusalClass as C;
    let class = match e {
        P::InputNotInUTxO | P::ReferenceInputNotInUTxO | P::TxInsEmpty => C::Inputs,
        P::PreservationOfValue
        | P::NegativeValue
        | P::MinLovelaceUnreached
        | P::MaxValSizeExceeded => C::Value,
        P::FeeBelowMin | P::MaxTxSizeExceeded | P::UnknownTxSize => C::FeeAndSize,
        P::VKWitnessMissing | P::VKWrongSignature | P::ReqSignerMissing => C::Witness,
        P::ScriptWitnessMissing
        | P::NativeScriptDenial
        | P::ScriptIntegrityHash
        | P::RedeemerMissing
        | P::UnneededRedeemer
        | P::UnneededNativeScript
        | P::UnneededPlutusV1Script
        | P::UnneededPlutusV2Script
        | P::UnneededPlutusV3Script => C::Script,
        P::DatumMissing | P::UnneededDatum => C::Datum,
        P::MetadataHash => C::Metadata,
        P::BlockPrecedesValInt | P::BlockExceedsValInt => C::Interval,
        P::TxWrongNetworkID | P::OutputWrongNetworkID => C::Network,
        P::CollateralMissing
        | P::CollateralNotInUTxO
        | P::CollateralMinLovelace
        | P::NonLovelaceCollateral
        | P::TooManyCollaterals
        | P::CollateralNotVKeyLocked
        | P::CollateralAnnotation => C::Collateral,
        P::InputDecoding | P::AddressDecoding => C::Decoding,
        P::UnsupportedNativeScript
        | P::CollateralWrongAssets
        | P::TxExUnitsExceeded
        | P::MintingLacksPolicy(_)
        | P::ReqSignerWrongSig
        | P::UnsupportedPlutusLanguage => return DolosClass::NoRule,
        _ => return DolosClass::Unknown,
    };
    DolosClass::Rule(class)
}

/// The class of a validation error by its variant.
fn validation_class(v: &ValidationError) -> DolosClass {
    use RefusalClass as C;
    use ValidationError as V;
    let class = match v {
        V::PostAlonzo(e) => return post_alonzo_class(e),
        V::DijkstraInputAlreadySpent => C::Inputs,
        V::DijkstraReferenceScriptsTooLarge => C::FeeAndSize,
        V::DijkstraMalformedScript => C::Script,
        V::DijkstraMalformed(_) | V::DijkstraInvalidMetadata => C::Decoding,
        V::DijkstraInvalidWithdrawal(_) | V::DijkstraCertificateStateUnavailable => {
            C::AccountAndWithdrawal
        }
        V::DijkstraInvalidCertificate(_) => C::CertificateAndGovernance,
        V::DijkstraUnsupported(_)
        | V::DijkstraMissingParameters(_)
        | V::DijkstraAccountStateUnavailable(_)
        | V::TxAndProtParamsDiffer
        | V::PParamsByronDoesntNeedAccountState
        | V::EnvMissingAccountState
        | V::UnknownProtParams
        | V::Byron(_)
        | V::ShelleyMA(_)
        | V::Alonzo(_) => return DolosClass::NoRule,
        _ => return DolosClass::Unknown,
    };
    DolosClass::Rule(class)
}

fn validation_verdict(v: &ValidationError) -> DolosVerdict {
    match v {
        ValidationError::DijkstraUnsupported(label) => DolosVerdict::Unsupported(label.to_string()),
        ValidationError::PostAlonzo(e) => DolosVerdict::Refused {
            kind: variant_name(format!("{e:?}")),
            class: validation_class(v),
        },
        other => DolosVerdict::Refused {
            kind: variant_name(format!("{other:?}")),
            class: validation_class(other),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(e: ChainError) -> Result<TxHash, DomainError> {
        Err(e.into())
    }

    #[test]
    fn a_phase_two_evaluation_error_is_a_refusal() {
        let error = ChainError::Phase2EvaluationError("can't eval without redeemers".into());
        let expected = DolosVerdict::Refused {
            kind: "Phase2EvaluationError".to_string(),
            class: DolosClass::NoRule,
        };
        assert_eq!(verdict(&refusal(error)), expected);
    }

    #[test]
    fn an_unsupported_phase_two_evaluation_is_unsupported_with_its_label() {
        let label = "V3 builtin outside audited protocol-12 subset";
        let error = ChainError::Phase2EvaluationUnsupported(
            pallas::ledger::validate::phase2::error::Unsupported(label),
        );
        assert_eq!(
            verdict(&refusal(error)),
            DolosVerdict::Unsupported(label.to_string())
        );
    }
}
