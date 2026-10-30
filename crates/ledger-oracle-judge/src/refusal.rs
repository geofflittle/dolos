use std::fmt;

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

impl RefusalClass {
    /// The class of a ledger failure leaf, by its constructor name.
    pub fn of_ledger_leaf(leaf: &str) -> Option<Self> {
        use RefusalClass as C;
        let class = match leaf {
            "BadInputsUTxO"
            | "SubBadInputsUTxO"
            | "SubInputSetEmptyUTxO"
            | "UnspendableUTxONoDatumHash"
            | "SubUnspendableUTxONoDatumHash" => C::Inputs,
            "ValueNotConservedUTxO"
            | "ValueNotConservedInLegacyMode"
            | "SubBabbageOutputTooSmallUTxO"
            | "SubOutputTooBigUTxO"
            | "SubOutputBootAddrAttrsTooBig" => C::Value,
            "MaxTxSizeUTxO" | "DijkstraTxRefScriptsSizeTooBig" => C::FeeAndSize,
            "MissingVKeyWitnessesUTXOW"
            | "SubMissingVKeyWitnessesUTXOW"
            | "InvalidWitnessesUTXOW"
            | "SubInvalidWitnessesUTXOW"
            | "MissingRequiredGuards" => C::Witness,
            "ScriptWitnessNotValidatingUTXOW"
            | "SubScriptWitnessNotValidatingUTXOW"
            | "MissingScriptWitnessesUTXOW"
            | "ExtraneousScriptWitnessesUTXOW"
            | "ValidationTagMismatch"
            | "SubScriptIntegrityHashMismatch"
            | "MissingRedeemers"
            | "SubMissingRedeemers"
            | "ExtraRedeemers"
            | "SubExtraRedeemers"
            | "CollectErrors" => C::Script,
            "MissingRequiredDatums"
            | "SubMissingRequiredDatums"
            | "NotAllowedSupplementalDatums"
            | "SubNotAllowedSupplementalDatums"
            | "MalformedGuardDatums"
            | "SubMalformedGuardDatums" => C::Datum,
            "MissingTxMetadata"
            | "SubMissingTxMetadata"
            | "MissingTxBodyMetadataHash"
            | "SubMissingTxBodyMetadataHash"
            | "ConflictingMetadataHash"
            | "SubConflictingMetadataHash" => C::Metadata,
            "OutsideValidityIntervalUTxO" | "SubOutsideValidityIntervalUTxO" => C::Interval,
            "SubWrongNetwork"
            | "WrongNetworkInTxBody"
            | "SubWrongNetworkInTxBody"
            | "WrongNetworkPOOL"
            | "WithdrawalAddressesWithWrongNetwork"
            | "SubWithdrawalAddressesWithWrongNetwork"
            | "DirectDepositAddressesWithWrongNetwork"
            | "SubDirectDepositAddressesWithWrongNetwork"
            | "WrongNetworkInAccountBalanceIntervals"
            | "SubWrongNetworkInAccountBalanceIntervals"
            | "WrongNetworkInStartingAccountBalanceIntervals"
            | "ProposalProcedureNetworkIdMismatch"
            | "TreasuryWithdrawalsNetworkIdMismatch" => C::Network,
            "WithdrawalAccountsMissing"
            | "SubWithdrawalAccountsMissing"
            | "WithdrawalAccountsMissingFromOriginal"
            | "SubWithdrawalAccountsMissingFromOriginal"
            | "WithdrawalAmountsExceedingOriginalBalance"
            | "WithdrawalAmountsInexactInLegacyMode"
            | "BalancesOutsideAccountBalanceIntervals"
            | "SubBalancesOutsideAccountBalanceIntervals"
            | "BalancesOutsideStartingAccountBalanceIntervals"
            | "MissingAccountsInAccountBalanceIntervals"
            | "SubMissingAccountsInAccountBalanceIntervals"
            | "MissingAccountsInStartingAccountBalanceIntervals"
            | "DirectDepositAccountsMissing"
            | "SubDirectDepositAccountsMissing"
            | "StakeKeyNotRegisteredDELEG" => C::AccountAndWithdrawal,
            "RefundIncorrectDELEG"
            | "DepositIncorrectDELEG"
            | "DelegAccountAlreadyRegistered"
            | "StakeKeyHasNonZeroAccountBalanceDELEG"
            | "DelegateeDRepNotRegisteredDELEG"
            | "DelegateeStakePoolNotRegisteredDELEG"
            | "StakePoolCostTooLowPOOL"
            | "StakePoolNotRegisteredOnKeyPOOL"
            | "StakePoolRetirementWrongEpochPOOL"
            | "DijkstraDRepAlreadyRegistered"
            | "DijkstraDRepNotRegistered"
            | "DijkstraDRepIncorrectDeposit"
            | "DijkstraDRepIncorrectRefund"
            | "DijkstraCommitteeIsUnknown"
            | "DijkstraCommitteeHasPreviouslyResigned"
            | "BlsKeyInvalidProofOfPossession"
            | "VRFKeyHashAlreadyRegistered"
            | "DijkstraTreasuryValueMismatch"
            | "SubTreasuryValueMismatch"
            | "ProposalReturnAccountDoesNotExist"
            | "TreasuryWithdrawalReturnAccountsDoNotExist"
            | "ProposalDepositIncorrect"
            | "ProposalCantFollow"
            | "MalformedProposal"
            | "InvalidPrevGovActionId"
            | "InvalidGuardrailsScriptHash"
            | "GovActionsDoNotExist"
            | "ConflictingCommitteeUpdate"
            | "ExpirationEpochTooSmall"
            | "ZeroTreasuryWithdrawals"
            | "DisallowedVoters"
            | "VotersDoNotExist"
            | "UnelectedCommitteeVoters"
            | "VotingOnExpiredGovAction" => C::CertificateAndGovernance,
            "InsufficientCollateral" => C::Collateral,
            "DecoderErrorDeserialiseFailure" | "DecoderErrorCustom" | "DecoderErrorLeftover" => {
                C::Decoding
            }
            _ => return None,
        };
        Some(class)
    }

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
