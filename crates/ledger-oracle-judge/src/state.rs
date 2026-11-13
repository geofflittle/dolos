//! The pool, vote and governance state the ledger ran a record from.

use std::collections::BTreeMap;

use serde::{de::Error as _, Deserialize, Deserializer};

use crate::{
    accounts::Credential,
    params::Canon,
    record::{Bytes, PParams},
};

/// A fixed length hash written as a hex string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hex<const N: usize>(pub [u8; N]);

impl<'de, const N: usize> Deserialize<'de> for Hex<N> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let Bytes(raw) = Bytes::deserialize(d)?;
        let len = raw.len();
        raw.try_into()
            .map(Hex)
            .map_err(|_| D::Error::custom(format!("{len} bytes where {N} are expected")))
    }
}

/// A shape no corpus record holds, so a record holding one fails to load.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub enum Unseen {}

/// A credential written as an object keyed by its kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CredentialObject {
    KeyHash(Hex<28>),
    ScriptHash(Hex<28>),
}

impl From<CredentialObject> for Credential {
    fn from(c: CredentialObject) -> Self {
        match c {
            CredentialObject::KeyHash(Hex(h)) => Credential::Key(h),
            CredentialObject::ScriptHash(Hex(h)) => Credential::Script(h),
        }
    }
}

/// A fraction the ledger writes as a decimal number or as its numerator and
/// denominator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fraction {
    pub numerator: u64,
    pub denominator: u64,
}

impl<'de> Deserialize<'de> for Fraction {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        let (numerator, denominator) =
            match Canon::of_json("fraction", &value).map_err(D::Error::custom)? {
                Canon::Int(n) => (n, 1),
                Canon::Ratio(n, d) => (n, d),
                other => return Err(D::Error::custom(format!("{other} is not a fraction"))),
            };
        let part = |x: i128| {
            u64::try_from(x).map_err(|_| D::Error::custom(format!("{value} is not a fraction")))
        };
        Ok(Fraction {
            numerator: part(numerator)?,
            denominator: part(denominator)?,
        })
    }
}

/// A metadata anchor.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Anchor {
    pub data_hash: Hex<32>,
    pub url: String,
}

/// A reward account written as its credential and network.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RewardAccount {
    pub credential: CredentialObject,
    pub network: String,
}

/// A registered pool's BLS key and the epoch it was registered in.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlsKey {
    pub bks_key: BlsKeyPair,
    pub bks_registered_in: u64,
}

/// A BLS public key with its proof of possession.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlsKeyPair {
    pub bls_possession_proof: Bytes,
    pub bls_pub_key: Bytes,
}

/// A registered pool as the ledger holds it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StakePool {
    pub sps_account_id: CredentialObject,
    pub sps_bls_key: Option<BlsKey>,
    pub sps_cost: u64,
    pub sps_delegators: Vec<CredentialObject>,
    pub sps_deposit: u64,
    pub sps_margin: Fraction,
    pub sps_metadata: Option<Unseen>,
    pub sps_owners: Vec<Hex<28>>,
    pub sps_pledge: u64,
    pub sps_relays: Vec<Unseen>,
    pub sps_vrf: Hex<32>,
}

/// The parameters a pool registered for the next epoch.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PoolParams {
    pub account_address: RewardAccount,
    pub bls_key: Option<Unseen>,
    pub cost: u64,
    pub margin: Fraction,
    pub metadata: Option<Unseen>,
    pub owners: Vec<Hex<28>>,
    pub pledge: u64,
    pub pool_id: Hex<28>,
    pub relays: Vec<Unseen>,
    pub vrf: Hex<32>,
}

/// The pools the ledger holds.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Pools {
    pub future_stake_pool_params: BTreeMap<Hex<28>, PoolParams>,
    pub retiring: BTreeMap<Hex<28>, u64>,
    pub stake_pools: BTreeMap<Hex<28>, StakePool>,
    pub vrf_key_hashes: BTreeMap<Hex<32>, u64>,
}

/// A registered DRep.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DRepState {
    #[serde(default)]
    pub anchor: Option<Anchor>,
    pub deposit: u64,
    pub expiry: u64,
}

/// What a committee cold credential has authorized.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "tag", content = "contents", deny_unknown_fields)]
pub enum CommitteeAuthorization {
    CommitteeHotCredential(CredentialObject),
    CommitteeMemberResigned(Option<Anchor>),
}

/// The committee authorizations the ledger holds.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommitteeState {
    pub cs_committee_creds: BTreeMap<Credential, CommitteeAuthorization>,
}

/// The DReps, committee authorizations and dormant epochs the ledger holds.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Votes {
    pub committee_state: CommitteeState,
    pub dreps: BTreeMap<Credential, DRepState>,
    pub num_dormant_epochs: u64,
}

/// The certificate state the ledger ran a record from, without its accounts.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Certs {
    pub pstate: Pools,
    pub vstate: Votes,
}

/// A governance action id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionId {
    pub gov_action_ix: u32,
    pub tx_id: Hex<32>,
}

/// A protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolVersion {
    pub major: u64,
    pub minor: u64,
}

/// A new constitution as a proposal or the enacted state writes it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Constitution {
    pub anchor: Anchor,
    #[serde(default)]
    pub script: Option<Hex<28>>,
}

/// A governance action.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "tag", content = "contents", deny_unknown_fields)]
pub enum GovAction {
    ParameterChange(Option<ActionId>, PParams, Option<Hex<28>>),
    HardForkInitiation(Option<ActionId>, ProtocolVersion),
    TreasuryWithdrawals(Vec<(RewardAccount, u64)>, Option<Hex<28>>),
    NoConfidence(Option<ActionId>),
    UpdateCommittee(
        Option<ActionId>,
        Vec<CredentialObject>,
        BTreeMap<Credential, u64>,
        Fraction,
    ),
    NewConstitution(Option<ActionId>, Constitution),
    InfoAction,
}

/// A vote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Vote {
    VoteYes,
    VoteNo,
    Abstain,
}

/// What a proposal asks for.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProposalProcedure {
    pub anchor: Anchor,
    pub deposit: u64,
    pub gov_action: GovAction,
    pub return_addr: RewardAccount,
}

/// A live proposal with the votes cast on it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Proposal {
    pub action_id: ActionId,
    pub committee_votes: BTreeMap<Credential, Vote>,
    #[serde(rename = "dRepVotes")]
    pub drep_votes: BTreeMap<Credential, Vote>,
    pub expires_after: u64,
    pub proposal_procedure: ProposalProcedure,
    pub proposed_in: u64,
    pub stake_pool_votes: BTreeMap<Hex<28>, Vote>,
}

/// The last enacted action of each purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Roots {
    pub committee: Option<ActionId>,
    pub constitution: Option<ActionId>,
    pub hard_fork: Option<ActionId>,
    pub pparam_update: Option<ActionId>,
}

/// The enacted committee.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Committee {
    pub members: BTreeMap<Credential, u64>,
    pub threshold: Fraction,
}

/// The governance state the ledger ran a record from.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gov {
    pub committee: Option<Committee>,
    pub constitution: Constitution,
    pub proposals: Vec<Proposal>,
    pub roots: Roots,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fraction_reads_from_a_decimal_or_a_numerator_and_denominator() {
        let read = |s: &str| serde_json::from_str::<Fraction>(s).map_err(|e| e.to_string());
        let f = |numerator, denominator| Fraction {
            numerator,
            denominator,
        };
        assert_eq!(read("0"), Ok(f(0, 1)));
        assert_eq!(read("0.5"), Ok(f(1, 2)));
        assert_eq!(read(r#"{"numerator": 1, "denominator": 3}"#), Ok(f(1, 3)));
        assert!(read("-1").is_err());
        assert!(read(r#"{"a": 1}"#).is_err());
    }

    #[test]
    fn a_governance_action_reads_each_written_shape() {
        let read = |s: &str| serde_json::from_str::<GovAction>(s).unwrap();
        let id = r#"{"govActionIx": 0, "txId": "a7a83e6f62d0018de0c6484a85833b520d6dadcf41acd05179f592d0efa96d46"}"#;
        assert!(matches!(
            read(&format!(
                r#"{{"contents": [{id}, {{"major": 12, "minor": 1}}], "tag": "HardForkInitiation"}}"#
            )),
            GovAction::HardForkInitiation(
                Some(_),
                ProtocolVersion {
                    major: 12,
                    minor: 1
                }
            )
        ));
        assert_eq!(read(r#"{"tag": "InfoAction"}"#), GovAction::InfoAction);
        assert_eq!(
            read(r#"{"contents": null, "tag": "NoConfidence"}"#),
            GovAction::NoConfidence(None)
        );
        let committee = read(
            r#"{"contents": [null, [], {"keyHash-1cca48da7f291b81a09ddc0e132fce0c5a1ef3410e5a70f9fcd5f777": 1209}, 0.5], "tag": "UpdateCommittee"}"#,
        );
        let GovAction::UpdateCommittee(None, removed, added, threshold) = committee else {
            panic!("{committee:?}")
        };
        assert!(removed.is_empty());
        assert_eq!(added.values().copied().collect::<Vec<_>>(), vec![1209]);
        assert_eq!(
            threshold,
            Fraction {
                numerator: 1,
                denominator: 2
            }
        );
    }

    #[test]
    fn a_pool_with_a_relay_fails_to_load() {
        let pool = |relays: &str| {
            format!(
                r#"{{"spsAccountId": {{"keyHash": "2090a1afc08a9c4abc6035064edf1b1b2c96d099aac7dfecdf79aede"}}, "spsBlsKey": null, "spsCost": 1, "spsDelegators": [], "spsDeposit": 2, "spsMargin": 0, "spsMetadata": null, "spsOwners": [], "spsPledge": 3, "spsRelays": {relays}, "spsVrf": "0c5cf58d57299b077597e27000775ed31ce6f580f98fe4f5dcd2790b57bbb3ee"}}"#
            )
        };
        assert!(serde_json::from_str::<StakePool>(&pool("[]")).is_ok());
        assert!(serde_json::from_str::<StakePool>(&pool(r#"[{"single host addr": []}]"#)).is_err());
    }
}
