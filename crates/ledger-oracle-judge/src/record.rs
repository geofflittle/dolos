use std::fmt;

use serde::{de::Error as _, Deserialize, Deserializer};

use crate::{
    accounts::LedgerAccounts,
    cbor::{self, Purpose, TxIn},
    refusal::RefusalClass,
};

/// The corpus a record comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CorpusKind {
    Ic,
    Supp,
}

impl CorpusKind {
    /// The short name used in record ids and printed lines.
    pub fn name(self) -> &'static str {
        match self {
            CorpusKind::Ic => "ic",
            CorpusKind::Supp => "supp",
        }
    }
}

/// One record of one corpus, printed as `ic#851`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecordId {
    pub corpus: CorpusKind,
    pub n: u32,
}

impl fmt::Display for RecordId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.corpus.name(), self.n)
    }
}

/// Bytes written as a hex string.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bytes(pub Vec<u8>);

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        hex::decode(&s).map(Bytes).map_err(D::Error::custom)
    }
}

impl<'de> Deserialize<'de> for TxIn {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let Bytes(raw) = Bytes::deserialize(d)?;
        cbor::txin(&raw).map_err(D::Error::custom)
    }
}

impl<'de> Deserialize<'de> for Purpose {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let Bytes(raw) = Bytes::deserialize(d)?;
        cbor::purpose(&raw).map_err(D::Error::custom)
    }
}

/// Execution units, memory and steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(from = "(u64, u64)")]
pub struct Units {
    pub mem: u64,
    pub steps: u64,
}

impl From<(u64, u64)> for Units {
    fn from((mem, steps): (u64, u64)) -> Self {
        Units { mem, steps }
    }
}

impl fmt::Display for Units {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "mem={} steps={}", self.mem, self.steps)
    }
}

/// What one evaluation of a redeemer returned.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Evaluation {
    Ran(Units),
    Failed(String),
}

impl fmt::Display for Evaluation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Evaluation::Ran(units) => write!(f, "ran {units}"),
            Evaluation::Failed(why) => {
                write!(f, "failed {}", why.chars().take(120).collect::<String>())
            }
        }
    }
}

/// The ledger's evaluation of one redeemer, with the redeemer's purpose.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerRedeemer {
    pub purpose: Purpose,
    pub evaluation: Evaluation,
}

/// A list with at least one element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonEmpty<T> {
    pub head: T,
    pub tail: Vec<T>,
}

impl<T> NonEmpty<T> {
    /// The list of the items in order, none when there are none.
    pub fn of(items: impl IntoIterator<Item = T>) -> Option<Self> {
        let mut items = items.into_iter();
        let head = items.next()?;
        Some(NonEmpty {
            head,
            tail: items.collect(),
        })
    }

    /// The elements in order.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        std::iter::once(&self.head).chain(&self.tail)
    }

    /// The final element.
    pub fn last(&self) -> &T {
        self.tail.last().unwrap_or(&self.head)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for NonEmpty<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        NonEmpty::of(Vec::<T>::deserialize(d)?)
            .ok_or_else(|| D::Error::invalid_length(0, &"at least one element"))
    }
}

/// The constructor names of one ledger failure, from the outer wrapper to the leaf, and the class of its leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailurePath {
    pub names: NonEmpty<String>,
    pub class: RefusalClass,
}

impl FailurePath {
    /// The constructor name of the failure itself.
    pub fn leaf(&self) -> &str {
        self.names.last()
    }
}

impl<'de> Deserialize<'de> for FailurePath {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let names = NonEmpty::<String>::deserialize(d)?;
        let leaf = names.last();
        let class = RefusalClass::of_ledger_leaf(leaf)
            .ok_or_else(|| D::Error::custom(format!("unclassified ledger leaf {leaf}")))?;
        Ok(FailurePath { names, class })
    }
}

/// The protocol parameters the ledger ran a record under.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PParams(pub serde_json::Map<String, serde_json::Value>);

/// Whether a transaction declares its scripts to pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(from = "bool")]
pub enum Phase2 {
    Valid,
    Invalid,
}

impl From<bool> for Phase2 {
    fn from(valid: bool) -> Self {
        if valid {
            Phase2::Valid
        } else {
            Phase2::Invalid
        }
    }
}

/// One unspent output, its input key and its output bytes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "(TxIn, Bytes)")]
pub struct UtxoEntry {
    pub input: TxIn,
    pub output: Bytes,
}

impl From<(TxIn, Bytes)> for UtxoEntry {
    fn from((input, output): (TxIn, Bytes)) -> Self {
        UtxoEntry { input, output }
    }
}

/// The UTxO the ledger ran one record from.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pre {
    /// The UTxO, written whole because it is not the one the previous record left.
    Fresh(Vec<UtxoEntry>),
    /// The UTxO the previous record left.
    FollowsPrev,
}

/// The ledger's answer for one record.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum LedgerOutcome {
    Accepted {
        consumed: Vec<TxIn>,
        produced: Vec<UtxoEntry>,
        accounts_post: LedgerAccounts,
    },
    Refused {
        failures: String,
        failure_names: NonEmpty<FailurePath>,
    },
}

/// The chain values the ledger ran the corpus under, written on its first record.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Globals {
    pub network: String,
    pub epoch_size: u64,
    pub security_param: u64,
}

/// One LEDGER rule run as the extractor wrote it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub n: u32,
    pub slot: u64,
    pub epoch: Option<u64>,
    pub phase2_valid: Phase2,
    pub tx: Bytes,
    pub pparams: PParams,
    pub pre: Pre,
    pub accounts_pre: LedgerAccounts,
    pub outcome: LedgerOutcome,
    pub ex_units: Vec<LedgerRedeemer>,
    pub sub_ex_units: Vec<Vec<LedgerRedeemer>>,
    pub min_fee: Option<u64>,
    #[serde(default)]
    pub globals: Option<Globals>,
}
