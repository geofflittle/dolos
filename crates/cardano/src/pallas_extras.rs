use dolos_core::BlockSlot;
use pallas::crypto::hash::Hash;
use pallas::ledger::addresses::{
    Address, Network, ShelleyAddress, ShelleyDelegationPart, StakeAddress, StakePayload,
};
use pallas::ledger::primitives::alonzo::MoveInstantaneousReward;
use pallas::ledger::primitives::conway::{
    CostModels, DRep, DRepVotingThresholds, PoolVotingThresholds,
};
use pallas::ledger::primitives::{PoolMetadata, RationalNumber, Relay, StakeCredential};
use pallas::ledger::primitives::{Epoch, ExUnitPrices, ExUnits, Nonce, NonceVariant};
use pallas::ledger::traverse::cert::BlsKeySlot;
use pallas::ledger::traverse::{MultiEraCert, MultiEraCertKind, MultiEraScriptRef, MultiEraTx};
use serde::{Deserialize, Serialize};

use crate::eras::ChainSummary;
use crate::{hacks, Lovelace};

/// What a pool registration wrote in the BLS key slot the Dijkstra era adds.
///
/// A consumer acts differently on each of these, so they stay apart. A
/// registration of an era before Dijkstra has no slot to write, a Dijkstra
/// registration may write the slot as nil, and one may write a key. One absent
/// value for all three would report a pool that declined a key and a pool that
/// could not have had one as the same pool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MultiEraBlsKey {
    NoSlot,
    Null,
    Key {
        pubkey: Vec<u8>,
        possession_proof: Vec<u8>,
    },
    /// A slot state this build has no name for, which is what a state added to
    /// the era neutral view after this was written reads as.
    Unrecognized,
}

impl From<BlsKeySlot<'_>> for MultiEraBlsKey {
    fn from(slot: BlsKeySlot<'_>) -> Self {
        match slot {
            BlsKeySlot::NoSlot => MultiEraBlsKey::NoSlot,
            BlsKeySlot::Null => MultiEraBlsKey::Null,
            BlsKeySlot::Key(key) => MultiEraBlsKey::Key {
                pubkey: key.bls_pubkey.to_vec(),
                possession_proof: key.bls_possession_proof.to_vec(),
            },
            _ => MultiEraBlsKey::Unrecognized,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MultiEraPoolRegistration {
    pub operator: Hash<28>,
    pub vrf_keyhash: Hash<32>,
    pub pledge: u64,
    pub cost: u64,
    pub margin: RationalNumber,
    pub reward_account: Vec<u8>,
    pub pool_owners: Vec<Hash<28>>,
    pub relays: Vec<Relay>,
    pub pool_metadata: Option<PoolMetadata>,
    pub bls_key: MultiEraBlsKey,
}

pub fn cert_as_pool_registration(cert: &MultiEraCert) -> Option<MultiEraPoolRegistration> {
    match cert.kind()? {
        MultiEraCertKind::PoolRegistration(params) => Some(MultiEraPoolRegistration {
            operator: *params.operator,
            vrf_keyhash: *params.vrf_keyhash,
            pledge: params.pledge,
            cost: params.cost,
            margin: params.margin.clone(),
            reward_account: params.reward_account.to_vec(),
            pool_owners: params.pool_owners.to_vec(),
            relays: params.relays.to_vec(),
            pool_metadata: params.pool_metadata.cloned(),
            bls_key: params.bls_key.into(),
        }),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MultiEraPoolRetirement {
    pub operator: Hash<28>,
    pub epoch: Epoch,
}

pub fn cert_as_pool_retirement(cert: &MultiEraCert) -> Option<MultiEraPoolRetirement> {
    match cert.kind()? {
        MultiEraCertKind::PoolRetirement(operator, epoch) => Some(MultiEraPoolRetirement {
            operator: *operator,
            epoch,
        }),
        _ => None,
    }
}

pub struct MultiEraVoteDelegation {
    pub delegator: StakeCredential,
    pub drep: DRep,
}

pub fn cert_as_vote_delegation(cert: &MultiEraCert) -> Option<MultiEraVoteDelegation> {
    let (delegator, drep) = match cert.kind()? {
        MultiEraCertKind::VoteDeleg(delegator, drep) => (delegator, drep),
        MultiEraCertKind::VoteRegDeleg(delegator, drep, _) => (delegator, drep),
        MultiEraCertKind::StakeVoteRegDeleg(delegator, _, drep, _) => (delegator, drep),
        MultiEraCertKind::StakeVoteDeleg(delegator, _, drep) => (delegator, drep),
        _ => return None,
    };

    Some(MultiEraVoteDelegation {
        delegator: delegator.clone(),
        drep: drep.clone(),
    })
}

pub struct MultiEraDRepRegistration {
    pub cred: StakeCredential,
    pub deposit: Lovelace,
}

pub fn cert_as_drep_registration(cert: &MultiEraCert) -> Option<MultiEraDRepRegistration> {
    match cert.kind()? {
        MultiEraCertKind::RegDRep(cred, deposit, _) => Some(MultiEraDRepRegistration {
            cred: cred.clone(),
            deposit,
        }),
        _ => None,
    }
}

pub type MultiEraDRepUnRegistration = MultiEraDRepRegistration;

pub fn cert_as_drep_unregistration(cert: &MultiEraCert) -> Option<MultiEraDRepUnRegistration> {
    match cert.kind()? {
        MultiEraCertKind::UnRegDRep(cred, deposit) => Some(MultiEraDRepRegistration {
            cred: cred.clone(),
            deposit,
        }),
        _ => None,
    }
}

pub struct MultiEraCommitteeAuth {
    pub cold: StakeCredential,
    pub hot: StakeCredential,
}

pub fn cert_as_committee_auth(cert: &MultiEraCert) -> Option<MultiEraCommitteeAuth> {
    match cert.kind()? {
        MultiEraCertKind::AuthCommitteeHot(cold, hot) => Some(MultiEraCommitteeAuth {
            cold: cold.clone(),
            hot: hot.clone(),
        }),
        _ => None,
    }
}

pub struct MultiEraCommitteeResign {
    pub cold: StakeCredential,
    pub anchor: Option<pallas::ledger::primitives::conway::Anchor>,
}

pub fn cert_as_committee_resign(cert: &MultiEraCert) -> Option<MultiEraCommitteeResign> {
    match cert.kind()? {
        MultiEraCertKind::ResignCommitteeCold(cold, anchor) => Some(MultiEraCommitteeResign {
            cold: cold.clone(),
            anchor: anchor.cloned(),
        }),
        _ => None,
    }
}

#[derive(Debug)]
pub struct MultiEraStakeDelegation {
    pub delegator: StakeCredential,
    pub pool: Hash<28>,
}

pub fn cert_as_stake_delegation(cert: &MultiEraCert) -> Option<MultiEraStakeDelegation> {
    let (delegator, pool) = match cert.kind()? {
        MultiEraCertKind::StakeDelegation(delegator, pool) => (delegator, pool),
        MultiEraCertKind::StakeRegDeleg(delegator, pool, _) => (delegator, pool),
        MultiEraCertKind::StakeVoteRegDeleg(delegator, pool, _, _) => (delegator, pool),
        MultiEraCertKind::StakeVoteDeleg(delegator, pool, _) => (delegator, pool),
        _ => return None,
    };

    Some(MultiEraStakeDelegation {
        delegator: delegator.clone(),
        pool: *pool,
    })
}

pub fn cert_as_stake_registration(cert: &MultiEraCert) -> Option<StakeCredential> {
    match cert.kind()? {
        MultiEraCertKind::StakeRegistration(credential) => Some(credential.clone()),
        MultiEraCertKind::Reg(credential, _) => Some(credential.clone()),
        MultiEraCertKind::StakeRegDeleg(credential, _, _) => Some(credential.clone()),
        MultiEraCertKind::VoteRegDeleg(credential, _, _) => Some(credential.clone()),
        MultiEraCertKind::StakeVoteRegDeleg(credential, _, _, _) => Some(credential.clone()),
        _ => None,
    }
}

pub fn cert_as_stake_deregistration(cert: &MultiEraCert) -> Option<StakeCredential> {
    match cert.kind()? {
        MultiEraCertKind::StakeDeregistration(credential) => Some(credential.clone()),
        MultiEraCertKind::UnReg(credential, _) => Some(credential.clone()),
        _ => None,
    }
}

pub fn cert_as_mir_certificate(cert: &MultiEraCert) -> Option<MoveInstantaneousReward> {
    match cert.kind()? {
        MultiEraCertKind::MoveInstantaneousRewards(mir) => Some(mir.clone()),
        _ => None,
    }
}

pub fn stake_credential_to_address(network: Network, credential: &StakeCredential) -> StakeAddress {
    match credential {
        StakeCredential::ScriptHash(x) => StakeAddress::new(network, StakePayload::Script(*x)),
        StakeCredential::AddrKeyhash(x) => StakeAddress::new(network, StakePayload::Stake(*x)),
    }
}

pub fn stake_address_to_cred(address: &StakeAddress) -> StakeCredential {
    match address.payload() {
        StakePayload::Stake(x) => StakeCredential::AddrKeyhash(*x),
        StakePayload::Script(x) => StakeCredential::ScriptHash(*x),
    }
}

pub fn shelley_address_to_stake_cred(
    address: &ShelleyAddress,
) -> Option<(StakeCredential, IsPointer)> {
    match address.delegation() {
        ShelleyDelegationPart::Key(x) => Some((StakeCredential::AddrKeyhash(*x), false)),
        ShelleyDelegationPart::Script(x) => Some((StakeCredential::ScriptHash(*x), false)),
        ShelleyDelegationPart::Pointer(x) => hacks::pointers::pointer_to_cred(x).map(|x| (x, true)),
        ShelleyDelegationPart::Null => None,
    }
}

pub fn shelley_address_to_stake_address(address: &ShelleyAddress) -> Option<StakeAddress> {
    match address.delegation() {
        ShelleyDelegationPart::Key(x) => Some(StakeAddress::new(
            address.network(),
            StakePayload::Stake(*x),
        )),
        ShelleyDelegationPart::Script(x) => Some(StakeAddress::new(
            address.network(),
            StakePayload::Script(*x),
        )),
        _ => None,
    }
}

pub type IsPointer = bool;

pub fn address_as_stake_cred(address: &Address) -> Option<(StakeCredential, IsPointer)> {
    match &address {
        Address::Shelley(x) => shelley_address_to_stake_cred(x),
        Address::Stake(x) => Some((stake_address_to_cred(x), false)),
        _ => None,
    }
}

pub fn epoch_boundary(
    chain_summary: &ChainSummary,
    prev_slot: BlockSlot,
    next_slot: BlockSlot,
) -> Option<(Epoch, BlockSlot, Epoch)> {
    let (prev_epoch, _) = chain_summary.slot_epoch(prev_slot);
    let (next_epoch, _) = chain_summary.slot_epoch(next_slot);

    if prev_epoch != next_epoch {
        let boundary = chain_summary.epoch_start(next_epoch);
        Some((prev_epoch, boundary, next_epoch))
    } else {
        None
    }
}

pub fn rupd_boundary(
    stability_window: u64,
    chain_summary: &ChainSummary,
    prev_slot: BlockSlot,
    next_slot: BlockSlot,
) -> Option<BlockSlot> {
    let (prev_epoch, _) = chain_summary.slot_epoch(prev_slot);

    let epoch_start = chain_summary.epoch_start(prev_epoch);

    let boundary = epoch_start + stability_window;

    if prev_slot <= boundary && boundary < next_slot {
        Some(boundary)
    } else {
        None
    }
}

pub fn default_rational_number() -> RationalNumber {
    RationalNumber {
        numerator: 0,
        denominator: 1,
    }
}

pub fn default_pool_voting_thresholds() -> PoolVotingThresholds {
    PoolVotingThresholds {
        motion_no_confidence: default_rational_number(),
        committee_normal: default_rational_number(),
        committee_no_confidence: default_rational_number(),
        hard_fork_initiation: default_rational_number(),
        security_voting_threshold: default_rational_number(),
    }
}

pub fn default_drep_voting_thresholds() -> DRepVotingThresholds {
    DRepVotingThresholds {
        motion_no_confidence: default_rational_number(),
        committee_normal: default_rational_number(),
        committee_no_confidence: default_rational_number(),
        hard_fork_initiation: default_rational_number(),
        pp_network_group: default_rational_number(),
        pp_economic_group: default_rational_number(),
        pp_technical_group: default_rational_number(),
        treasury_withdrawal: default_rational_number(),
        update_constitution: default_rational_number(),
        pp_governance_group: default_rational_number(),
    }
}

pub fn default_nonce() -> Nonce {
    Nonce {
        variant: NonceVariant::NeutralNonce,
        hash: None,
    }
}

pub fn default_ex_units() -> ExUnits {
    ExUnits { mem: 0, steps: 0 }
}

pub fn default_ex_unit_prices() -> ExUnitPrices {
    ExUnitPrices {
        mem_price: default_rational_number(),
        step_price: default_rational_number(),
    }
}

/// The cost model map key that names PlutusV4.
///
/// The Conway cost model type names keys 0 to 2 and reads every further key
/// into its wildcard map, so a PlutusV4 model is found here by key rather than
/// by field.
pub const PLUTUS_V4_COST_MODEL_KEY: u64 = 3;

pub fn default_cost_models() -> CostModels {
    CostModels {
        plutus_v1: None,
        plutus_v2: None,
        plutus_v3: None,
        unknown: Default::default(),
    }
}

/// The language of a script, across every era that can carry one.
///
/// Taken from pallas, which now names the same five languages and is the side
/// that decides what a reference script reports. The type is exhaustive there
/// too, so a language no consumer has a case for is still a compile error
/// rather than a catch-all reporting a real script as absent.
pub use pallas::ledger::traverse::script_ref::ScriptLanguage;

/// A reference script decomposed into everything a consumer needs from it.
///
/// The three fields travel together because they are only correct together.
/// Each language hashes its own tagged serialization over its own bytes, so a
/// hash taken from one variant and bytes taken from another describe no script
/// that exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptRefParts {
    pub language: ScriptLanguage,
    pub hash: Hash<28>,
    pub bytes: Vec<u8>,
}

/// Decompose a reference script from any era into its language, on-chain hash
/// and bytes.
///
/// The language drives the read, because it is the one answer that covers
/// every variant of every era. A native script is the only kind whose bytes
/// are not the script itself, and it is asked for by name.
pub fn script_ref_parts(script_ref: &MultiEraScriptRef) -> ScriptRefParts {
    let language = script_ref.language();

    let bytes = match language {
        ScriptLanguage::Native => script_ref.native_script().map(|script| script.encode()),
        ScriptLanguage::PlutusV1
        | ScriptLanguage::PlutusV2
        | ScriptLanguage::PlutusV3
        | ScriptLanguage::PlutusV4 => script_ref.plutus_bytes().map(|bytes| bytes.to_vec()),
        other => panic!("script_ref_parts has no bytes rule for {other:?}"),
    };

    ScriptRefParts {
        language,
        hash: script_ref.hash(),
        // Reachable only if the language a reference script reports and the
        // bytes it returns disagree. No variant of ScriptRefParts can state
        // that, and serving a script with the wrong bytes under the right hash
        // is worse than stopping.
        bytes: bytes.expect("a reference script reporting a language carries that language's bytes"),
    }
}

/// Compute the on-chain script hash of a reference script.
pub fn script_ref_hash(script_ref: &MultiEraScriptRef) -> Hash<28> {
    script_ref.hash()
}

pub const DREP_KEY_PREFIX: u8 = 0b00100010;
pub const DREP_SCRIPT_PREFIX: u8 = 0b00100011;

/// Check that the first byte of the drep id finishes with the 0011 bytes.
pub fn drep_id_is_script(drep_id: &[u8]) -> bool {
    let first = drep_id.first().unwrap();
    first & 0b00001111 == 0b00000011
}

pub fn stake_cred_to_drep(cred: &StakeCredential) -> DRep {
    match cred {
        StakeCredential::AddrKeyhash(key) => DRep::Key(*key),
        StakeCredential::ScriptHash(key) => DRep::Script(*key),
    }
}

pub fn parse_reward_account(reward_account: &[u8]) -> Option<StakeCredential> {
    let pool_address = Address::from_bytes(reward_account).ok()?;
    let (cred, _) = address_as_stake_cred(&pool_address)?;

    Some(cred)
}

pub fn keyhash_to_stake_cred(keyhash: Hash<28>) -> StakeCredential {
    StakeCredential::AddrKeyhash(keyhash)
}

pub fn cred_matches_hash(cred: &StakeCredential, hash: &str) -> bool {
    let hash: Hash<28> = hash.parse().unwrap();

    match cred {
        StakeCredential::AddrKeyhash(x) => x == &hash,
        StakeCredential::ScriptHash(x) => x == &hash,
    }
}

pub fn tx_treasury_donation(tx: &MultiEraTx) -> Option<Lovelace> {
    match tx {
        MultiEraTx::Conway(x) => x.transaction_body.donation.map(|x| x.into()),
        // Dijkstra keeps the donation at the same body key as Conway. An era
        // that carries the field has to read it, because the wildcard below
        // stops the node rather than answering, and a chain past the Dijkstra
        // hard fork puts every one of its transactions through here.
        MultiEraTx::Dijkstra(x) => x.transaction_body.donation.map(|x| x.into()),
        MultiEraTx::DijkstraSub(x, _) => x.sub_transaction_body.donation.map(|x| x.into()),
        MultiEraTx::AlonzoCompatible(..) => None,
        MultiEraTx::Babbage(..) => None,
        MultiEraTx::Byron(..) => None,
        _ => panic!("unexpected tx era"),
    }
}

/// The direct deposits of `tx`, each a reward account and the coin it receives.
pub fn tx_direct_deposits<'a>(tx: &'a MultiEraTx) -> Vec<(&'a [u8], Lovelace)> {
    let deposits = match tx {
        MultiEraTx::Dijkstra(x) => x.transaction_body.direct_deposits.as_ref(),
        MultiEraTx::DijkstraSub(x, _) => x.sub_transaction_body.direct_deposits.as_ref(),
        _ => None,
    };

    deposits
        .into_iter()
        .flatten()
        .map(|(account, amount)| (account.as_slice(), *amount))
        .collect()
}

/// Calls `f` on each sub transaction of `tx` and then on `tx`, the order in
/// which the ledger applies them.
pub fn for_each_applied_tx<E>(
    tx: &MultiEraTx<'_>,
    mut f: impl FnMut(&MultiEraTx<'_>) -> Result<(), E>,
) -> Result<(), E> {
    for sub in tx.sub_transactions() {
        f(&sub)?;
    }

    f(tx)
}

#[cfg(test)]
mod script_ref_tests {
    use super::*;
    use pallas::crypto::hash::Hasher;
    use pallas::ledger::traverse::Era;

    /// The three bytes of a minimal Plutus script, the same ones pallas uses
    /// in its own reference script tests.
    const PLUTUS_BYTES: [u8; 3] = [0x4d, 0x01, 0x00];

    /// `native_script = [0, addr_keyhash]`, the pubkey clause over 28 bytes.
    fn native_script() -> Vec<u8> {
        let mut out = vec![0x82, 0x00, 0x58, 0x1c];
        out.extend_from_slice(&[0xaa; 28]);
        out
    }

    /// `script = [n, script_n]`, the reference script rule of Babbage on.
    fn script(tag: u8, inner: &[u8]) -> Vec<u8> {
        let mut out = vec![0x82, tag];
        out.extend_from_slice(inner);
        out
    }

    /// The hash the ledger keys a script by, computed from the rule rather
    /// than from the code under test: blake2b-224 over the language tag
    /// followed by the script's own bytes.
    fn ledger_hash(tag: u8, bytes: &[u8]) -> Hash<28> {
        let mut input = vec![tag];
        input.extend_from_slice(bytes);
        Hasher::<224>::hash(&input)
    }

    #[test]
    fn a_dijkstra_plutus_v4_reference_script_reports_its_language_hash_and_bytes() {
        let cbor = script(4, &[0x43, 0x4d, 0x01, 0x00]);
        let script_ref = MultiEraScriptRef::decode(Era::Dijkstra, &cbor).expect("must decode");

        let parts = script_ref_parts(&script_ref);

        assert_eq!(parts.language, ScriptLanguage::PlutusV4);
        assert_eq!(parts.bytes, PLUTUS_BYTES.to_vec());
        assert_eq!(parts.hash, ledger_hash(4, &PLUTUS_BYTES));
        // The tag is what separates one language's hash from another's over
        // the same bytes, so a hash taken without it is the wrong hash.
        assert_ne!(parts.hash, Hasher::<224>::hash(&PLUTUS_BYTES));
        assert_ne!(parts.hash, ledger_hash(3, &PLUTUS_BYTES));
        // The whole tagged array is what a reference script is stored as, so
        // it has to come back with the tag on it and not as the body alone.
        assert_eq!(script_ref.encode(), cbor);
    }

    #[test]
    fn a_dijkstra_native_reference_script_reports_its_language_hash_and_bytes() {
        let inner = native_script();
        let cbor = script(0, &inner);
        let script_ref = MultiEraScriptRef::decode(Era::Dijkstra, &cbor).expect("must decode");

        let parts = script_ref_parts(&script_ref);

        assert_eq!(parts.language, ScriptLanguage::Native);
        assert_eq!(parts.bytes, inner);
        assert_eq!(parts.hash, ledger_hash(0, &inner));
    }

    #[test]
    fn a_conway_native_reference_script_reports_its_language_hash_and_bytes() {
        let inner = native_script();
        let cbor = script(0, &inner);
        let script_ref = MultiEraScriptRef::decode(Era::Conway, &cbor).expect("must decode");

        let parts = script_ref_parts(&script_ref);

        assert_eq!(parts.language, ScriptLanguage::Native);
        assert_eq!(parts.bytes, inner);
        assert_eq!(parts.hash, ledger_hash(0, &inner));
        assert_eq!(script_ref_hash(&script_ref), parts.hash);
        assert_eq!(script_ref.encode(), cbor);
    }

    #[test]
    fn a_conway_plutus_v2_reference_script_reports_its_language_hash_and_bytes() {
        let cbor = script(2, &[0x43, 0x4d, 0x01, 0x00]);
        let script_ref = MultiEraScriptRef::decode(Era::Conway, &cbor).expect("must decode");

        let parts = script_ref_parts(&script_ref);

        assert_eq!(parts.language, ScriptLanguage::PlutusV2);
        assert_eq!(parts.bytes, PLUTUS_BYTES.to_vec());
        assert_eq!(parts.hash, ledger_hash(2, &PLUTUS_BYTES));
    }
}

#[cfg(test)]
mod dijkstra_certificate_tests {
    use super::*;
    use pallas::codec::utils::Nullable;
    use pallas::ledger::primitives::dijkstra::{
        BlsKey as DijkstraBlsKey, Certificate as DijkstraCert,
    };
    use std::borrow::Cow;

    const POOL: &str = "747aca09f322d2dfc56243b839e2d573ab92287684e5e37d66ec0f87";
    const VRF: &str = "d8252bd637a90ba4dbd2cf63afda20a19888b7895ede067081ce7fb7411a972b";
    const CRED: &str = "5e81366cb6f3c0d14837614afcea669d51b8be9519eaec4a237504f8";

    fn wrap(cert: DijkstraCert) -> MultiEraCert<'static> {
        MultiEraCert::Dijkstra(Box::new(Cow::Owned(cert)))
    }

    fn cred() -> StakeCredential {
        StakeCredential::AddrKeyhash(CRED.parse().unwrap())
    }

    fn pool_registration() -> DijkstraCert {
        pool_registration_with(Some(pallas::codec::utils::Nullable::Null))
    }

    fn pool_registration_with(bls_key: Option<Nullable<DijkstraBlsKey>>) -> DijkstraCert {
        DijkstraCert::PoolRegistration {
            operator: POOL.parse().unwrap(),
            vrf_keyhash: VRF.parse().unwrap(),
            bls_key,
            pledge: 1_000_000,
            cost: 340_000_000,
            margin: RationalNumber {
                numerator: 3,
                denominator: 100,
            },
            reward_account: vec![0xe0].into(),
            pool_owners: vec![CRED.parse::<Hash<28>>().unwrap()].into(),
            relays: vec![],
            pool_metadata: None,
        }
    }

    /// Which of the eleven accessors answers for a given certificate. Any
    /// accessor that answers is named, so a test can say both which one fired
    /// and that no other one did.
    fn answered_by(cert: &MultiEraCert) -> Vec<&'static str> {
        let mut out = vec![];

        if cert_as_pool_registration(cert).is_some() {
            out.push("pool_registration");
        }
        if cert_as_pool_retirement(cert).is_some() {
            out.push("pool_retirement");
        }
        if cert_as_vote_delegation(cert).is_some() {
            out.push("vote_delegation");
        }
        if cert_as_drep_registration(cert).is_some() {
            out.push("drep_registration");
        }
        if cert_as_drep_unregistration(cert).is_some() {
            out.push("drep_unregistration");
        }
        if cert_as_committee_auth(cert).is_some() {
            out.push("committee_auth");
        }
        if cert_as_committee_resign(cert).is_some() {
            out.push("committee_resign");
        }
        if cert_as_stake_delegation(cert).is_some() {
            out.push("stake_delegation");
        }
        if cert_as_stake_registration(cert).is_some() {
            out.push("stake_registration");
        }
        if cert_as_stake_deregistration(cert).is_some() {
            out.push("stake_deregistration");
        }
        if cert_as_mir_certificate(cert).is_some() {
            out.push("mir");
        }

        out
    }

    /// The must-fire case, over every certificate a Dijkstra chain can carry.
    /// Musashi is Dijkstra from slot 86400, so every certificate the node ever
    /// applies past that point arrives through this type.
    #[test]
    fn every_dijkstra_certificate_kind_is_read() {
        let cases: Vec<(DijkstraCert, &str)> = vec![
            (pool_registration(), "pool_registration"),
            (
                DijkstraCert::PoolRetirement(POOL.parse().unwrap(), 42),
                "pool_retirement",
            ),
            (DijkstraCert::Reg(cred(), 2_000_000), "stake_registration"),
            (DijkstraCert::UnReg(cred(), 2_000_000), "stake_deregistration"),
            (
                DijkstraCert::StakeDelegation(cred(), POOL.parse().unwrap()),
                "stake_delegation",
            ),
            (
                DijkstraCert::VoteDeleg(cred(), DRep::Abstain),
                "vote_delegation",
            ),
            (
                DijkstraCert::RegDRepCert(cred(), 500_000_000, None),
                "drep_registration",
            ),
            (
                DijkstraCert::UnRegDRepCert(cred(), 500_000_000),
                "drep_unregistration",
            ),
            (
                DijkstraCert::AuthCommitteeHot(cred(), cred()),
                "committee_auth",
            ),
            (
                DijkstraCert::ResignCommitteeCold(cred(), None),
                "committee_resign",
            ),
        ];

        for (cert, expected) in cases {
            let wrapped = wrap(cert);
            let answered = answered_by(&wrapped);

            assert!(
                answered.contains(&expected),
                "{expected} did not answer for {wrapped:?}, answered: {answered:?}",
            );
        }
    }

    /// The must-not case. Reading Dijkstra certificates must not turn every
    /// accessor into one that answers for everything, so a pool retirement has
    /// to be read as a pool retirement and as nothing else.
    #[test]
    fn a_dijkstra_certificate_answers_only_its_own_accessor() {
        let cert = wrap(DijkstraCert::PoolRetirement(POOL.parse().unwrap(), 42));

        assert_eq!(answered_by(&cert), vec!["pool_retirement"]);

        let cert = wrap(DijkstraCert::Reg(cred(), 2_000_000));

        assert_eq!(answered_by(&cert), vec!["stake_registration"]);

        // A registration that also delegates is genuinely two things, and both
        // accessors are meant to answer.
        let cert = wrap(DijkstraCert::StakeVoteRegDeleg(
            cred(),
            POOL.parse().unwrap(),
            DRep::Abstain,
            2_000_000,
        ));

        let answered = answered_by(&cert);

        assert!(answered.contains(&"vote_delegation"));
        assert!(answered.contains(&"stake_delegation"));
        assert!(!answered.contains(&"pool_retirement"));
        assert!(!answered.contains(&"committee_auth"));
    }

    /// The pool parameters have to survive the read, not merely be present.
    /// An accessor that answered with a default would pass the test above.
    #[test]
    fn a_dijkstra_pool_registration_carries_its_parameters() {
        let cert = wrap(pool_registration());

        let read = cert_as_pool_registration(&cert).expect("must be read");

        assert_eq!(read.operator, POOL.parse::<Hash<28>>().unwrap());
        assert_eq!(read.vrf_keyhash, VRF.parse::<Hash<32>>().unwrap());
        assert_eq!(read.pledge, 1_000_000);
        assert_eq!(read.cost, 340_000_000);
        assert_eq!(read.margin.numerator, 3);
        assert_eq!(read.margin.denominator, 100);
        assert_eq!(read.reward_account, vec![0xe0]);
        assert_eq!(read.pool_owners, vec![CRED.parse::<Hash<28>>().unwrap()]);
    }

    /// The key a Dijkstra pool registration writes reaches the caller. Both
    /// byte strings are checked, because a read that carried one of them and
    /// defaulted the other would pass a test that only looked for a key.
    #[test]
    fn a_dijkstra_pool_registration_carries_the_key_it_wrote() {
        let cert = wrap(pool_registration_with(Some(Nullable::Some(
            DijkstraBlsKey {
                bls_pubkey: vec![0xab; 96].into(),
                bls_possession_proof: vec![0xcd; 48].into(),
            },
        ))));

        let read = cert_as_pool_registration(&cert).expect("must be read");

        assert_eq!(
            read.bls_key,
            MultiEraBlsKey::Key {
                pubkey: vec![0xab; 96],
                possession_proof: vec![0xcd; 48],
            }
        );
    }

    /// The must-not case for the key. Three states of the slot have to stay
    /// three answers: a registration that wrote nil declined a key, one of an
    /// era with no slot could not have written one, and only a registration
    /// that wrote bytes has a key. Reporting any of these as another would
    /// tell a caller something the chain does not say.
    #[test]
    fn the_three_states_of_the_key_slot_stay_three_answers() {
        let nil = wrap(pool_registration_with(Some(Nullable::Null)));
        let omitted = wrap(pool_registration_with(None));
        let written = wrap(pool_registration_with(Some(Nullable::Some(
            DijkstraBlsKey {
                bls_pubkey: vec![0x01; 96].into(),
                bls_possession_proof: vec![0x02; 48].into(),
            },
        ))));

        let read = |cert: &MultiEraCert| cert_as_pool_registration(cert).expect("must be read").bls_key;

        assert_eq!(read(&nil), MultiEraBlsKey::Null);
        assert_eq!(read(&omitted), MultiEraBlsKey::NoSlot);
        assert!(matches!(read(&written), MultiEraBlsKey::Key { .. }));
    }

    /// A certificate that is not a pool registration has no key slot to read,
    /// and the accessor for pool registrations is the only place the key is
    /// reachable from, so no other accessor can report one.
    #[test]
    fn a_certificate_that_is_not_a_pool_registration_has_no_key() {
        let cert = wrap(DijkstraCert::PoolRetirement(POOL.parse().unwrap(), 42));

        assert!(cert_as_pool_registration(&cert).is_none());
    }
}

#[cfg(test)]
mod real_pool_registration_tests {
    use super::*;
    use pallas::ledger::traverse::MultiEraBlock;

    /// The block cut from the prototype chain at slot 86855 whose single
    /// transaction registers a pool and fills the BLS key slot. Its provenance
    /// entry is `pool_registration_with_bls_key`.
    const BLOCK: &str = include_str!("../../../test_data/musashi-w36/pool-registration-bls.block");

    /// The two byte strings that block's key slot holds, read off the CBOR at
    /// offset 1022 as a 96 byte string followed by a 48 byte one.
    const PUBKEY: &str = "b0d04d6492c59fa7aae9354078c77adc8ba04db59982fe1a842b0356ac720846ea098aa1e93972c027ae10c8b91f30e30c541a6675e2feeeb177c60f6a67159eb5751666ea5875a4983bfea53fed819958bc530cc0ad884eeaf0b85e8f3a46a5";
    const PROOF: &str = "a9964d2780f1fb6f7ba8553a89bfc4ddf831021ec051744b13dddfea412c7bd7314d65c1f276e22c114c523645af1854";

    fn registrations() -> Vec<MultiEraPoolRegistration> {
        let cbor = hex::decode(BLOCK.trim()).expect("the fixture is hex");
        let block = MultiEraBlock::decode(&cbor).expect("the fixture decodes");

        block
            .txs()
            .iter()
            .flat_map(|tx| tx.certs())
            .filter_map(|cert| cert_as_pool_registration(&cert))
            .collect()
    }

    /// The must-fire case against the chain rather than against a certificate
    /// built here. A registration a node accepted carries a key, and the read
    /// a consumer of this module gets has to carry the same bytes.
    #[test]
    fn the_key_a_real_pool_registration_wrote_reaches_a_consumer() {
        let read = registrations();

        assert_eq!(read.len(), 1, "the fixture holds one pool registration");

        let carried = serde_json::to_value(&read[0]).expect("the read serializes");

        assert_eq!(
            carried["bls_key"]["Key"]["pubkey"],
            serde_json::json!(hex::decode(PUBKEY).expect("the key is hex")),
        );
        assert_eq!(
            carried["bls_key"]["Key"]["possession_proof"],
            serde_json::json!(hex::decode(PROOF).expect("the proof is hex")),
        );
    }

    /// Every other parameter of the same registration, each compared to what
    /// the certificate itself says rather than to a value written here, so a
    /// read that carried the key and defaulted or swapped a field does not
    /// pass on the key alone.
    #[test]
    fn the_rest_of_a_real_pool_registration_reaches_a_consumer_too() {
        let cbor = hex::decode(BLOCK.trim()).expect("the fixture is hex");
        let block = MultiEraBlock::decode(&cbor).expect("the fixture decodes");

        let mut compared = 0;

        for tx in block.txs().iter() {
            for cert in tx.certs() {
                let Some(MultiEraCertKind::PoolRegistration(params)) = cert.kind() else {
                    continue;
                };

                let read = cert_as_pool_registration(&cert).expect("must be read");

                assert_eq!(&read.operator, params.operator);
                assert_eq!(&read.vrf_keyhash, params.vrf_keyhash);
                assert_eq!(read.pledge, params.pledge);
                assert_eq!(read.cost, params.cost);
                assert_eq!(&read.margin, params.margin);
                assert_eq!(read.reward_account, params.reward_account.to_vec());
                assert_eq!(read.pool_owners, params.pool_owners.to_vec());
                assert_eq!(read.relays, params.relays.to_vec());
                assert_eq!(read.pool_metadata.as_ref(), params.pool_metadata);

                compared += 1;
            }
        }

        assert_eq!(compared, 1, "the fixture holds one pool registration");
    }
}

#[cfg(test)]
mod earlier_era_certificate_tests {
    use super::*;
    use pallas::ledger::primitives::alonzo::Certificate as AlonzoCert;
    use pallas::ledger::primitives::conway::Certificate as ConwayCert;
    use std::borrow::Cow;

    const POOL: &str = "747aca09f322d2dfc56243b839e2d573ab92287684e5e37d66ec0f87";
    const VRF: &str = "d8252bd637a90ba4dbd2cf63afda20a19888b7895ede067081ce7fb7411a972b";
    const CRED: &str = "5e81366cb6f3c0d14837614afcea669d51b8be9519eaec4a237504f8";

    fn alonzo(cert: AlonzoCert) -> MultiEraCert<'static> {
        MultiEraCert::AlonzoCompatible(Box::new(Cow::Owned(cert)))
    }

    fn conway(cert: ConwayCert) -> MultiEraCert<'static> {
        MultiEraCert::Conway(Box::new(Cow::Owned(cert)))
    }

    fn cred() -> StakeCredential {
        StakeCredential::AddrKeyhash(CRED.parse().unwrap())
    }

    /// A move instantaneous rewards certificate is named by the type serving
    /// Shelley through Babbage and by no later era's, so it is the one kind
    /// whose reader has to keep answering for an early era and for no other.
    #[test]
    fn a_move_instantaneous_rewards_certificate_is_read_for_the_era_that_names_it() {
        let mir = pallas::ledger::primitives::alonzo::MoveInstantaneousReward {
            source: pallas::ledger::primitives::alonzo::InstantaneousRewardSource::Reserves,
            target: pallas::ledger::primitives::alonzo::InstantaneousRewardTarget::OtherAccountingPot(
                1_000_000,
            ),
        };

        let cert = alonzo(AlonzoCert::MoveInstantaneousRewardsCert(mir));

        assert!(cert_as_mir_certificate(&cert).is_some());
        assert!(cert_as_stake_registration(&cert).is_none());
        assert!(cert_as_pool_registration(&cert).is_none());

        let later = conway(ConwayCert::Reg(cred(), 2_000_000));

        assert!(cert_as_mir_certificate(&later).is_none());
    }

    /// The two stake certificates the Conway type still names and no later era
    /// does, read for both eras that name them.
    #[test]
    fn the_stake_certificates_of_an_earlier_era_are_read() {
        assert_eq!(
            cert_as_stake_registration(&alonzo(AlonzoCert::StakeRegistration(cred()))),
            Some(cred())
        );
        assert_eq!(
            cert_as_stake_deregistration(&alonzo(AlonzoCert::StakeDeregistration(cred()))),
            Some(cred())
        );
        assert_eq!(
            cert_as_stake_registration(&conway(ConwayCert::StakeRegistration(cred()))),
            Some(cred())
        );
        assert_eq!(
            cert_as_stake_deregistration(&conway(ConwayCert::StakeDeregistration(cred()))),
            Some(cred())
        );
    }

    /// A pool registration of an era with no key slot reads as having none,
    /// and every other parameter still arrives.
    #[test]
    fn a_conway_pool_registration_has_no_key_slot_and_keeps_its_parameters() {
        let cert = conway(ConwayCert::PoolRegistration {
            operator: POOL.parse().unwrap(),
            vrf_keyhash: VRF.parse().unwrap(),
            pledge: 1_000_000,
            cost: 340_000_000,
            margin: RationalNumber {
                numerator: 3,
                denominator: 100,
            },
            reward_account: vec![0xe0].into(),
            pool_owners: vec![CRED.parse::<Hash<28>>().unwrap()].into(),
            relays: vec![],
            pool_metadata: None,
        });

        let read = cert_as_pool_registration(&cert).expect("must be read");

        assert_eq!(read.bls_key, MultiEraBlsKey::NoSlot);
        assert_eq!(read.operator, POOL.parse::<Hash<28>>().unwrap());
        assert_eq!(read.pledge, 1_000_000);
        assert_eq!(read.pool_owners, vec![CRED.parse::<Hash<28>>().unwrap()]);
    }

    /// An era that carries no certificates at all answers no accessor, which
    /// is the one case a reader is allowed to answer nothing for.
    #[test]
    fn an_era_with_no_certificates_answers_no_accessor() {
        let cert = MultiEraCert::NotApplicable;

        assert!(cert_as_pool_registration(&cert).is_none());
        assert!(cert_as_stake_registration(&cert).is_none());
        assert!(cert_as_mir_certificate(&cert).is_none());
    }
}

#[cfg(test)]
mod treasury_donation_tests {
    use super::*;

    /// The smallest Dijkstra transaction that carries a treasury donation:
    /// one input, no outputs, zero fee, and body key 22 set to 1000000. Built
    /// by hand because no transaction on any Dijkstra chain has ever set that
    /// key, so there are no real bytes to take it from.
    ///
    /// Four elements, ending `f5`. The w36 ledger deleted the block body's
    /// leading list of transactions the producer rejected and put each
    /// producer's verdict on the transaction instead, so a transaction as a
    /// block carries it is `[body, witness_set, auxiliary_data / nil, bool]`
    /// and `decode_for_era` reads that shape. Three elements is the mempool
    /// form, which is what a client submits and not what a block holds.
    const DIJKSTRA_TX_WITH_DONATION: &str = "84a40081825820000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f0001800200161a000f4240a0f6f5";

    /// The same transaction with body key 22 absent.
    const DIJKSTRA_TX_WITHOUT_DONATION: &str = "84a30081825820000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f0001800200a0f6f5";

    fn dijkstra_tx(hex: &str) -> MultiEraTx<'static> {
        let bytes: &'static [u8] = hex::decode(hex).unwrap().leak();
        MultiEraTx::decode_for_era(pallas::ledger::traverse::Era::Dijkstra, bytes).unwrap()
    }

    /// The must-fire case. A Dijkstra transaction's donation has to come back
    /// as the amount it carries. Musashi is a Dijkstra chain from slot 86400
    /// on, so every transaction the node applies reaches this accessor.
    #[test]
    fn a_dijkstra_donation_is_read() {
        let tx = dijkstra_tx(DIJKSTRA_TX_WITH_DONATION);
        assert!(matches!(tx, MultiEraTx::Dijkstra(..)));
        assert_eq!(tx_treasury_donation(&tx), Some(1_000_000));
    }

    /// The must-not case. Reading the field must not turn every Dijkstra
    /// transaction into a donation, so one without the key has to come back
    /// as none.
    #[test]
    fn a_dijkstra_transaction_without_a_donation_reports_none() {
        let tx = dijkstra_tx(DIJKSTRA_TX_WITHOUT_DONATION);
        assert!(matches!(tx, MultiEraTx::Dijkstra(..)));
        assert_eq!(tx_treasury_donation(&tx), None);
    }

    /// A sub transaction, `[body, witness_set, auxiliary_data / nil]`, whose
    /// body holds one input, no outputs, and a donation of one ada at key 22.
    const DIJKSTRA_SUB_WITH_DONATION: &str = "83a30081825820000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f000180161a000f4240a0f6";

    /// The same sub transaction with body key 22 absent.
    const DIJKSTRA_SUB_WITHOUT_DONATION: &str =
        "83a20081825820000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f000180a0f6";

    /// The epoch visitor asks each sub transaction for its donation, so a block
    /// with a sub transaction reaches this function.
    #[test]
    fn a_sub_transaction_donation_is_read() {
        let bytes = hex::decode(DIJKSTRA_SUB_WITH_DONATION).unwrap();
        let sub: pallas::ledger::primitives::dijkstra::SubTransaction =
            pallas::codec::minicbor::decode(&bytes).unwrap();
        let tx = MultiEraTx::from_dijkstra_sub(&sub, true);

        assert!(matches!(tx, MultiEraTx::DijkstraSub(..)));
        assert_eq!(tx_treasury_donation(&tx), Some(1_000_000));
    }

    /// The must-not case for a sub transaction. One without the key has to come
    /// back as none, or every sub transaction of a block would read as a
    /// donation.
    #[test]
    fn a_sub_transaction_without_a_donation_reports_none() {
        let bytes = hex::decode(DIJKSTRA_SUB_WITHOUT_DONATION).unwrap();
        let sub: pallas::ledger::primitives::dijkstra::SubTransaction =
            pallas::codec::minicbor::decode(&bytes).unwrap();
        let tx = MultiEraTx::from_dijkstra_sub(&sub, true);

        assert!(matches!(tx, MultiEraTx::DijkstraSub(..)));
        assert_eq!(tx_treasury_donation(&tx), None);
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::model::pools::testing::any_pool_params;
    use crate::model::testing as root;
    use proptest::prelude::*;

    /// All four states of the key slot, so a roundtrip is asserted over each
    /// rather than over whichever one a fixed value picked.
    pub fn any_bls_key() -> impl Strategy<Value = MultiEraBlsKey> {
        prop_oneof![
            Just(MultiEraBlsKey::NoSlot),
            Just(MultiEraBlsKey::Null),
            Just(MultiEraBlsKey::Unrecognized),
            (
                prop::collection::vec(any::<u8>(), 96..97),
                prop::collection::vec(any::<u8>(), 48..49),
            )
                .prop_map(|(pubkey, possession_proof)| MultiEraBlsKey::Key {
                    pubkey,
                    possession_proof,
                }),
        ]
    }

    prop_compose! {
        pub fn any_multi_era_pool_registration()(
            operator in root::any_hash_28(),
            params in any_pool_params(),
            bls_key in any_bls_key(),
        ) -> MultiEraPoolRegistration {
            MultiEraPoolRegistration {
                operator,
                vrf_keyhash: params.vrf_keyhash,
                pledge: params.pledge,
                cost: params.cost,
                margin: params.margin,
                reward_account: params.reward_account,
                pool_owners: params.pool_owners,
                relays: params.relays,
                pool_metadata: params.pool_metadata,
                bls_key,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static REWARD_ACCOUNT: [u8; 29] = [
        224, 185, 111, 206, 243, 185, 53, 26, 246, 131, 75, 216, 80, 227, 169, 120, 89, 215, 189,
        91, 114, 157, 36, 191, 54, 70, 174, 172, 207,
    ];

    #[test]
    fn test_pool_reward_account() {
        let parsed = parse_reward_account(&REWARD_ACCOUNT).unwrap();
        dbg!(&parsed);
    }
}
