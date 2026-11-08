use std::fmt;

use crate::{
    coverage::{Member, Reach},
    eval::LeafEval,
    judgement::{Axis, CellKey},
};

/// The rule a refusal names, shared by the ledger's failure leaves and the node's refusal kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RefusalClass {
    Inputs,
    Value,
    FeeAndSize,
    Witness,
    Script,
    Datum,
    Metadata,
    Interval,
    Network,
    AccountAndWithdrawal,
    CertificateAndGovernance,
    Collateral,
    Decoding,
}

/// The reach of a leaf row, counted unless the row names why no record can raise it.
macro_rules! leaf_reach {
    () => {
        Reach::Counted
    };
    ($reason:literal, $command:literal) => {
        Reach::Excluded {
            reason: $reason.to_string(),
            command: $command.to_string(),
        }
    };
}

/// Declares the ledger failure leaves, each with its refusal class, what an evaluation answers under it, and why no record can raise it when none can.
macro_rules! ledger_leaves {
    ($($leaf:ident: $class:ident, $eval:ident $(, excluded $reason:literal, $command:literal)?;)*) => {
        /// A ledger failure leaf, by its constructor name.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum LedgerLeaf {
            $($leaf,)*
        }

        impl LedgerLeaf {
            /// Every leaf in declaration order.
            pub const ALL: &'static [LedgerLeaf] = &[$(LedgerLeaf::$leaf,)*];

            /// Whether a record of the corpora can raise the leaf.
            pub fn reach(self) -> Reach {
                match self {
                    $(LedgerLeaf::$leaf => leaf_reach!($($reason, $command)?),)*
                }
            }

            /// The leaf a constructor name names, none for a name no leaf has.
            pub fn of_name(name: &str) -> Option<Self> {
                match name {
                    $(stringify!($leaf) => Some(LedgerLeaf::$leaf),)*
                    _ => None,
                }
            }

            /// The constructor name.
            pub fn name(self) -> &'static str {
                match self {
                    $(LedgerLeaf::$leaf => stringify!($leaf),)*
                }
            }

            /// The rule the leaf names.
            pub fn class(self) -> RefusalClass {
                match self {
                    $(LedgerLeaf::$leaf => RefusalClass::$class,)*
                }
            }

            /// What an evaluation of a transaction the ledger refused under the leaf answers.
            pub fn eval(self) -> LeafEval {
                match self {
                    $(LedgerLeaf::$leaf => LeafEval::$eval,)*
                }
            }
        }
    };
}

ledger_leaves! {
    BadInputsUTxO: Inputs, Undecided;
    SubBadInputsUTxO: Inputs, Undecided;
    InputSetEmptyUTxO: Inputs, Undecided;
    SubInputSetEmptyUTxO: Inputs, Undecided;
    UnspendableUTxONoDatumHash: Inputs, Undecided;
    SubUnspendableUTxONoDatumHash: Inputs, Undecided;
    BabbageNonDisjointRefInputs: Inputs, Undecided, excluded
        "raised only above protocol 9 and below protocol 11",
        "git -C cardano-ledger show 9df9f98:eras/babbage/impl/src/Cardano/Ledger/Babbage/Rules/Utxo.hs | grep -n -B2 -A2 'natVersion @11'";
    ValueNotConservedUTxO: Value, Undecided;
    ValueNotConservedInLegacyMode: Value, Undecided;
    SubBabbageOutputTooSmallUTxO: Value, Undecided;
    SubOutputTooBigUTxO: Value, Undecided;
    SubOutputBootAddrAttrsTooBig: Value, Undecided;
    BabbageOutputTooSmallUTxO: Value, Undecided;
    OutputTooBigUTxO: Value, Undecided;
    OutputBootAddrAttrsTooBig: Value, Undecided;
    MaxTxSizeUTxO: FeeAndSize, Undecided;
    SubMaxTxSizeUTxO: FeeAndSize, Undecided, excluded
        "named only by the translation of MaxTxSizeUTxO, which a sub transaction never raises",
        "git -C cardano-ledger show 9df9f98:eras/dijkstra/impl/src/Cardano/Ledger/Dijkstra/Rules/SubUtxo.hs | grep -n MaxTxSize";
    DijkstraTxRefScriptsSizeTooBig: FeeAndSize, Undecided;
    FeeTooSmallUTxO: FeeAndSize, Evaluate;
    ExUnitsTooBigUTxO: FeeAndSize, Evaluate;
    MissingVKeyWitnessesUTXOW: Witness, Evaluate;
    SubMissingVKeyWitnessesUTXOW: Witness, Evaluate;
    InvalidWitnessesUTXOW: Witness, Evaluate;
    SubInvalidWitnessesUTXOW: Witness, Evaluate;
    MissingRequiredGuards: Witness, Undecided;
    ScriptWitnessNotValidatingUTXOW: Script, Evaluate;
    SubScriptWitnessNotValidatingUTXOW: Script, Evaluate;
    MissingScriptWitnessesUTXOW: Script, Refuse;
    ExtraneousScriptWitnessesUTXOW: Script, Undecided;
    ValidationTagMismatch: Script, Undecided;
    SubScriptIntegrityHashMismatch: Script, Undecided;
    ScriptIntegrityHashMismatch: Script, Undecided;
    PPViewHashesDontMatch: Script, Undecided, excluded
        "raised only below protocol 11",
        "git -C cardano-ledger show 9df9f98:eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Rules/Utxow.hs | grep -n -A2 '< natVersion @11'";
    SubPPViewHashesDontMatch: Script, Undecided, excluded
        "the translation of PPViewHashesDontMatch, raised only below protocol 11",
        "git -C cardano-ledger show 9df9f98:eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Rules/Utxow.hs | grep -n -A2 '< natVersion @11'";
    MalformedScriptWitnesses: Script, Undecided;
    SubMalformedScriptWitnesses: Script, Undecided;
    MalformedReferenceScripts: Script, Undecided;
    SubMalformedReferenceScripts: Script, Undecided;
    MissingRedeemers: Script, Refuse;
    SubMissingRedeemers: Script, Refuse;
    ExtraRedeemers: Script, Undecided;
    SubExtraRedeemers: Script, Undecided;
    CollectErrors: Script, Refuse;
    MissingRequiredDatums: Datum, Undecided;
    SubMissingRequiredDatums: Datum, Undecided;
    NotAllowedSupplementalDatums: Datum, Undecided;
    SubNotAllowedSupplementalDatums: Datum, Undecided;
    MalformedGuardDatums: Datum, Undecided;
    SubMalformedGuardDatums: Datum, Undecided;
    MissingTxMetadata: Metadata, Undecided;
    SubMissingTxMetadata: Metadata, Undecided;
    MissingTxBodyMetadataHash: Metadata, Undecided;
    SubMissingTxBodyMetadataHash: Metadata, Undecided;
    ConflictingMetadataHash: Metadata, Undecided;
    SubConflictingMetadataHash: Metadata, Undecided;
    InvalidMetadata: Metadata, Undecided;
    SubInvalidMetadata: Metadata, Undecided;
    OutsideValidityIntervalUTxO: Interval, Undecided;
    SubOutsideValidityIntervalUTxO: Interval, Undecided;
    OutsideForecast: Interval, Undecided, excluded
        "the ledger extends the epoch info to the slot, so every validity bound translates",
        "git -C cardano-ledger show 9df9f98:eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Rules/Utxo.hs | grep -n 'unsafeLinearExtendEpochInfo\\|OutsideForecast'";
    SubOutsideForecast: Interval, Undecided, excluded
        "the translation of OutsideForecast, which the extended epoch info never raises",
        "git -C cardano-ledger show 9df9f98:eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Rules/Utxo.hs | grep -n 'unsafeLinearExtendEpochInfo\\|OutsideForecast'";
    SubWrongNetwork: Network, Undecided;
    WrongNetwork: Network, Undecided;
    WrongNetworkInTxBody: Network, Undecided;
    SubWrongNetworkInTxBody: Network, Undecided;
    WrongNetworkPOOL: Network, Undecided;
    WithdrawalAddressesWithWrongNetwork: Network, Undecided;
    SubWithdrawalAddressesWithWrongNetwork: Network, Undecided;
    DirectDepositAddressesWithWrongNetwork: Network, Undecided;
    SubDirectDepositAddressesWithWrongNetwork: Network, Undecided;
    WrongNetworkInAccountBalanceIntervals: Network, Undecided;
    SubWrongNetworkInAccountBalanceIntervals: Network, Undecided;
    WrongNetworkInStartingAccountBalanceIntervals: Network, Undecided;
    ProposalProcedureNetworkIdMismatch: Network, Undecided;
    TreasuryWithdrawalsNetworkIdMismatch: Network, Undecided;
    WithdrawalAccountsMissing: AccountAndWithdrawal, Undecided;
    SubWithdrawalAccountsMissing: AccountAndWithdrawal, Undecided;
    WithdrawalAccountsMissingFromOriginal: AccountAndWithdrawal, Undecided;
    SubWithdrawalAccountsMissingFromOriginal: AccountAndWithdrawal, Undecided;
    WithdrawalAmountsExceedingOriginalBalance: AccountAndWithdrawal, Undecided;
    WithdrawalAmountsInexactInLegacyMode: AccountAndWithdrawal, Undecided;
    BalancesOutsideAccountBalanceIntervals: AccountAndWithdrawal, Undecided;
    SubBalancesOutsideAccountBalanceIntervals: AccountAndWithdrawal, Undecided;
    BalancesOutsideStartingAccountBalanceIntervals: AccountAndWithdrawal, Undecided;
    MissingAccountsInAccountBalanceIntervals: AccountAndWithdrawal, Undecided;
    SubMissingAccountsInAccountBalanceIntervals: AccountAndWithdrawal, Undecided;
    MissingAccountsInStartingAccountBalanceIntervals: AccountAndWithdrawal, Undecided;
    DirectDepositAccountsMissing: AccountAndWithdrawal, Undecided;
    SubDirectDepositAccountsMissing: AccountAndWithdrawal, Undecided;
    StakeKeyNotRegisteredDELEG: AccountAndWithdrawal, Undecided;
    RefundIncorrectDELEG: CertificateAndGovernance, Undecided;
    DepositIncorrectDELEG: CertificateAndGovernance, Undecided;
    IncorrectDepositDELEG: CertificateAndGovernance, Undecided, excluded
        "raised only at protocol 10 and below",
        "git -C cardano-ledger show 9df9f98:eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Deleg.hs | grep -n 'IncorrectDepositDELEG\\|hardforkConwayDELEGIncorrectDepositsAndRefunds'";
    DelegAccountAlreadyRegistered: CertificateAndGovernance, Undecided;
    StakeKeyHasNonZeroAccountBalanceDELEG: CertificateAndGovernance, Undecided;
    DelegateeDRepNotRegisteredDELEG: CertificateAndGovernance, Undecided;
    DelegateeStakePoolNotRegisteredDELEG: CertificateAndGovernance, Undecided;
    StakePoolCostTooLowPOOL: CertificateAndGovernance, Undecided;
    StakePoolNotRegisteredOnKeyPOOL: CertificateAndGovernance, Undecided;
    StakePoolRetirementWrongEpochPOOL: CertificateAndGovernance, Undecided;
    PoolMedataHashTooBig: CertificateAndGovernance, Undecided;
    DijkstraDRepAlreadyRegistered: CertificateAndGovernance, Undecided;
    DijkstraDRepNotRegistered: CertificateAndGovernance, Undecided;
    DijkstraDRepIncorrectDeposit: CertificateAndGovernance, Undecided;
    DijkstraDRepIncorrectRefund: CertificateAndGovernance, Undecided;
    DijkstraCommitteeIsUnknown: CertificateAndGovernance, Undecided;
    DijkstraCommitteeHasPreviouslyResigned: CertificateAndGovernance, Undecided;
    BlsKeyInvalidProofOfPossession: CertificateAndGovernance, Undecided;
    VRFKeyHashAlreadyRegistered: CertificateAndGovernance, Undecided;
    DijkstraTreasuryValueMismatch: CertificateAndGovernance, Undecided;
    SubTreasuryValueMismatch: CertificateAndGovernance, Undecided;
    ProposalReturnAccountDoesNotExist: CertificateAndGovernance, Undecided;
    TreasuryWithdrawalReturnAccountsDoNotExist: CertificateAndGovernance, Undecided;
    ProposalDepositIncorrect: CertificateAndGovernance, Undecided;
    ProposalCantFollow: CertificateAndGovernance, Undecided;
    MalformedProposal: CertificateAndGovernance, Undecided;
    DisallowedProposalDuringBootstrap: CertificateAndGovernance, Undecided, excluded
        "raised only in the bootstrap phase, protocol 9",
        "git -C cardano-ledger show 9df9f98:eras/conway/impl/src/Cardano/Ledger/Conway/Era.hs | grep -n -A1 '^hardforkConwayBootstrapPhase'";
    DisallowedVotesDuringBootstrap: CertificateAndGovernance, Undecided, excluded
        "raised only in the bootstrap phase, protocol 9",
        "git -C cardano-ledger show 9df9f98:eras/conway/impl/src/Cardano/Ledger/Conway/Era.hs | grep -n -A1 '^hardforkConwayBootstrapPhase'";
    InvalidPrevGovActionId: CertificateAndGovernance, Undecided;
    InvalidGuardrailsScriptHash: CertificateAndGovernance, Undecided;
    GovActionsDoNotExist: CertificateAndGovernance, Undecided;
    ConflictingCommitteeUpdate: CertificateAndGovernance, Undecided;
    ExpirationEpochTooSmall: CertificateAndGovernance, Undecided;
    ZeroTreasuryWithdrawals: CertificateAndGovernance, Undecided;
    DisallowedVoters: CertificateAndGovernance, Undecided;
    VotersDoNotExist: CertificateAndGovernance, Undecided;
    UnelectedCommitteeVoters: CertificateAndGovernance, Undecided;
    VotingOnExpiredGovAction: CertificateAndGovernance, Undecided;
    InsufficientCollateral: Collateral, Undecided;
    ScriptsNotPaidUTxO: Collateral, Undecided;
    CollateralContainsNonADA: Collateral, Undecided;
    TooManyCollateralInputs: Collateral, Undecided;
    NoCollateralInputs: Collateral, Undecided;
    IncorrectTotalCollateralField: Collateral, Undecided;
    PtrPresentInCollateralReturn: Collateral, Undecided;
    DecoderErrorDeserialiseFailure: Decoding, Undecided;
    DecoderErrorCustom: Decoding, Undecided;
    DecoderErrorLeftover: Decoding, Undecided;
}

impl fmt::Display for LedgerLeaf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The member of the leaf axis of each leaf.
pub fn leaf_members() -> Vec<Member> {
    let member = |leaf: &LedgerLeaf| Member {
        cell: CellKey::new(Axis::Leaf, leaf),
        reach: leaf.reach(),
    };
    LedgerLeaf::ALL.iter().map(member).collect()
}

impl RefusalClass {
    /// The name printed in judgement lines.
    pub fn name(self) -> &'static str {
        match self {
            RefusalClass::Inputs => "inputs",
            RefusalClass::Value => "value",
            RefusalClass::FeeAndSize => "fee_and_size",
            RefusalClass::Witness => "witness",
            RefusalClass::Script => "script",
            RefusalClass::Datum => "datum",
            RefusalClass::Metadata => "metadata",
            RefusalClass::Interval => "interval",
            RefusalClass::Network => "network",
            RefusalClass::AccountAndWithdrawal => "account_and_withdrawal",
            RefusalClass::CertificateAndGovernance => "certificate_and_governance",
            RefusalClass::Collateral => "collateral",
            RefusalClass::Decoding => "decoding",
        }
    }
}

impl fmt::Display for RefusalClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The class of a node refusal by its error variant, unknown for a variant the class table does not name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DolosClass {
    Rule(RefusalClass),
    NoRule,
    Unknown,
}

impl DolosClass {
    /// The name printed in judgement lines.
    pub fn name(self) -> &'static str {
        match self {
            DolosClass::Rule(class) => class.name(),
            DolosClass::NoRule => "none",
            DolosClass::Unknown => "unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    const DUMPED: &str = include_str!("../../../test_data/ledger-oracle/leaves.txt");

    fn dumped(text: &str) -> BTreeSet<&str> {
        text.lines().filter_map(|l| l.split(' ').next()).collect()
    }

    /// The names of the leaves a ledger rule raises, every leaf but the decoder's.
    fn rule_leaves() -> BTreeSet<&'static str> {
        let rules = LedgerLeaf::ALL
            .iter()
            .filter(|l| l.class() != RefusalClass::Decoding);
        rules.map(|l| l.name()).collect()
    }

    #[test]
    fn a_dump_missing_one_leaf_or_naming_one_more_differs_from_the_leaves() {
        let mut lines: Vec<&str> = DUMPED.lines().collect();
        let first = lines.remove(0);
        assert_ne!(dumped(&lines.join("\n")), rule_leaves(), "{first} removed");
        lines.push(first);
        lines.push("NoSuchLeaf DijkstraUtxoPredFailure");
        assert_ne!(dumped(&lines.join("\n")), rule_leaves());
    }

    #[test]
    fn the_rule_leaves_are_the_leaves_the_ledger_dumper_writes() {
        assert_eq!(dumped(DUMPED).len(), DUMPED.lines().count());
        assert_eq!(rule_leaves(), dumped(DUMPED));
    }

    #[test]
    fn each_leaf_is_a_member_once_and_a_leaf_no_record_can_raise_names_its_command() {
        let members = leaf_members();
        let cells: BTreeSet<_> = members.iter().map(|m| m.cell.to_string()).collect();
        assert_eq!(cells.len(), LedgerLeaf::ALL.len());
        let excluded = members.iter().filter_map(|m| match &m.reach {
            Reach::Excluded { command, .. } => Some((m.cell.to_string(), command)),
            Reach::Counted => None,
        });
        let excluded: Vec<_> = excluded.collect();
        assert_eq!(excluded.len(), 9, "{excluded:?}");
        assert!(excluded
            .iter()
            .all(|(_, c)| c.starts_with("git -C cardano-ledger show 9df9f98:")));
    }
}
