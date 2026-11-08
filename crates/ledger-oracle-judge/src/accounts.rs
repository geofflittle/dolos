use std::{collections::BTreeMap, fmt};

use serde::{de::Error as _, Deserialize, Deserializer};

use crate::{
    compare::Tally,
    judgement::Finding,
    record::{Bytes, RecordId},
};

/// A stake credential, printed as the ledger keys it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Credential {
    Key([u8; 28]),
    Script([u8; 28]),
}

/// A vote delegation target, printed as the ledger writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DRep {
    Key([u8; 28]),
    Script([u8; 28]),
    AlwaysAbstain,
    AlwaysNoConfidence,
}

fn hash28(hex: &str) -> Result<[u8; 28], String> {
    let raw = hex::decode(hex).map_err(|e| e.to_string())?;
    raw.try_into().map_err(|_| format!("{hex} is not 28 bytes"))
}

impl std::str::FromStr for Credential {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.split_once('-') {
            Some(("keyHash", hex)) => hash28(hex).map(Credential::Key),
            Some(("scriptHash", hex)) => hash28(hex).map(Credential::Script),
            _ => Err(format!("{s} is not a stake credential")),
        }
    }
}

impl fmt::Display for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Credential::Key(h) => write!(f, "keyHash-{}", hex::encode(h)),
            Credential::Script(h) => write!(f, "scriptHash-{}", hex::encode(h)),
        }
    }
}

impl std::str::FromStr for DRep {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.strip_prefix("drep-") {
            Some("alwaysAbstain") => Ok(DRep::AlwaysAbstain),
            Some("alwaysNoConfidence") => Ok(DRep::AlwaysNoConfidence),
            Some(rest) => match rest.parse()? {
                Credential::Key(h) => Ok(DRep::Key(h)),
                Credential::Script(h) => Ok(DRep::Script(h)),
            },
            None => Err(format!("{s} is not a vote delegation")),
        }
    }
}

impl fmt::Display for DRep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DRep::Key(h) => write!(f, "drep-{}", Credential::Key(*h)),
            DRep::Script(h) => write!(f, "drep-{}", Credential::Script(*h)),
            DRep::AlwaysAbstain => write!(f, "drep-alwaysAbstain"),
            DRep::AlwaysNoConfidence => write!(f, "drep-alwaysNoConfidence"),
        }
    }
}

fn parsed<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: std::str::FromStr<Err = String>,
{
    String::deserialize(d)?.parse().map_err(D::Error::custom)
}

fn parsed_option<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: std::str::FromStr<Err = String>,
{
    Option::<String>::deserialize(d)?
        .map(|s| s.parse().map_err(D::Error::custom))
        .transpose()
}

impl<'de> Deserialize<'de> for Credential {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        parsed(d)
    }
}

fn pool_option<'de, D: Deserializer<'de>>(d: D) -> Result<Option<[u8; 28]>, D::Error> {
    match Option::<Bytes>::deserialize(d)? {
        None => Ok(None),
        Some(Bytes(raw)) => raw
            .try_into()
            .map(Some)
            .map_err(|raw: Vec<u8>| D::Error::custom(format!("a pool id of {} bytes", raw.len()))),
    }
}

/// One registered account as the ledger dumped it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerAccount {
    pub balance: u64,
    pub deposit: u64,
    #[serde(deserialize_with = "parsed_option")]
    pub drep: Option<DRep>,
    pub reward: u64,
    #[serde(deserialize_with = "pool_option")]
    pub spool: Option<[u8; 28]>,
}

/// The registered accounts the ledger dumped, by credential.
pub type LedgerAccounts = BTreeMap<Credential, LedgerAccount>;

/// The fields of a registered account that both the ledger and Dolos hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub balance: u64,
    pub pool: Option<[u8; 28]>,
    pub drep: Option<DRep>,
}

impl From<&LedgerAccount> for Account {
    fn from(a: &LedgerAccount) -> Self {
        Account {
            balance: a.balance,
            pool: a.spool,
            drep: a.drep,
        }
    }
}

impl fmt::Display for Account {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "balance={}", self.balance)?;
        match &self.pool {
            Some(pool) => write!(f, ",pool={}", hex::encode(pool))?,
            None => write!(f, ",pool=none")?,
        }
        match &self.drep {
            Some(drep) => write!(f, ",{drep}"),
            None => write!(f, ",drep=none"),
        }
    }
}

/// Counts each account either side holds after record `id` against the other side's account for that credential.
pub fn compare(
    id: RecordId,
    ledger: &LedgerAccounts,
    dolos: &BTreeMap<Credential, Account>,
    tally: &mut Tally,
) {
    let ledger: BTreeMap<&Credential, Account> =
        ledger.iter().map(|(c, a)| (c, a.into())).collect();
    let mut credentials: Vec<&Credential> = ledger.keys().copied().chain(dolos.keys()).collect();
    credentials.sort();
    credentials.dedup();
    let shown = |a: Option<&Account>| a.map_or("none".to_string(), Account::to_string);
    for credential in credentials {
        let (l, d) = (ledger.get(credential), dolos.get(credential));
        tally.add(l == d, || {
            let detail = format!(
                "account={credential} ledger={} dolos={}",
                shown(l),
                shown(d)
            );
            Finding::new(id, Vec::new(), detail)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "1cca48da7f291b81a09ddc0e132fce0c5a1ef3410e5a70f9fcd5f777";

    fn ledger(json: &str) -> LedgerAccounts {
        serde_json::from_str(json).unwrap()
    }

    fn account(balance: u64) -> String {
        format!(
            r#"{{"keyHash-{KEY}": {{"balance": {balance}, "deposit": 2000000, "drep": "drep-keyHash-{KEY}", "reward": {balance}, "spool": "{KEY}"}}}}"#
        )
    }

    #[test]
    fn a_ledger_account_reads_its_credential_pool_and_vote_delegation() {
        let read = ledger(&account(5));
        let (credential, account) = read.iter().next().unwrap();
        assert_eq!(credential.to_string(), format!("keyHash-{KEY}"));
        assert_eq!(account.spool, Some(hash28(KEY).unwrap()));
        assert_eq!(account.drep, Some(DRep::Key(hash28(KEY).unwrap())));
        let bad = r#"{"pointer-00": {"balance": 0, "deposit": 0, "drep": null, "reward": 0, "spool": null}}"#;
        assert!(serde_json::from_str::<LedgerAccounts>(bad).is_err());
    }

    #[test]
    fn equal_accounts_agree_and_one_lovelace_or_a_missing_account_disagrees() {
        let ledger = ledger(&account(5));
        let same: BTreeMap<_, _> = ledger.iter().map(|(c, a)| (*c, a.into())).collect();
        let id: RecordId = "supp#1".parse().unwrap();
        let mut tally = Tally::default();
        compare(id, &ledger, &same, &mut tally);
        assert_eq!(
            tally,
            Tally {
                compared: 1,
                disagree: vec![]
            }
        );

        let mut off = same.clone();
        off.values_mut().for_each(|a: &mut Account| a.balance += 1);
        let mut tally = Tally::default();
        compare(id, &ledger, &off, &mut tally);
        assert_eq!(tally.compared, 1);
        assert_eq!(
            tally.disagree,
            vec![Finding::new(id, Vec::new(), format!(
                "account=keyHash-{KEY} ledger=balance=5,pool={KEY},drep-keyHash-{KEY} dolos=balance=6,pool={KEY},drep-keyHash-{KEY}"
            ))]
        );

        let mut tally = Tally::default();
        compare(id, &LedgerAccounts::new(), &same, &mut tally);
        assert_eq!(tally.compared, 1);
        assert!(tally.disagree[0].detail.ends_with(" ledger=none dolos=balance=5,pool=1cca48da7f291b81a09ddc0e132fce0c5a1ef3410e5a70f9fcd5f777,drep-keyHash-1cca48da7f291b81a09ddc0e132fce0c5a1ef3410e5a70f9fcd5f777"));
    }
}
