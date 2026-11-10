use std::{fmt, num::NonZeroU64};

use serde::{de::Error as _, Deserialize, Deserializer};

use crate::{
    accounts::LedgerAccounts,
    cbor::{self, Purpose, TxIn},
    refusal::{LedgerLeaf, RefusalClass},
    state::{Certs, Gov},
};

/// The corpus a record comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CorpusKind {
    Ic,
    Supp,
    P1,
}

impl CorpusKind {
    /// Every corpus, in the order a run judges them.
    pub const ALL: [CorpusKind; 3] = [CorpusKind::Ic, CorpusKind::Supp, CorpusKind::P1];

    /// The short name used in record ids and printed lines.
    pub fn name(self) -> &'static str {
        match self {
            CorpusKind::Ic => "ic",
            CorpusKind::Supp => "supp",
            CorpusKind::P1 => "p1",
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

/// One data argument a script is applied to, as its bytes and the value they
/// decode to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Argument {
    pub bytes: Vec<u8>,
    pub data: cbor::Data,
}

impl<'de> Deserialize<'de> for Argument {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let Bytes(bytes) = Bytes::deserialize(d)?;
        let data = cbor::data(&bytes).map_err(D::Error::custom)?;
        Ok(Argument { bytes, data })
    }
}

fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    Option::deserialize(d)
}

/// The ledger's evaluation of one redeemer, with the redeemer's purpose.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerRedeemer {
    pub purpose: Purpose,
    /// The arguments of the script context the ledger built for the redeemer,
    /// none when it built none.
    #[serde(deserialize_with = "present")]
    pub args: Option<Vec<Argument>>,
    pub evaluation: Evaluation,
    /// The trace lines the script wrote.
    pub logs: Vec<String>,
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

/// One ledger failure, by the leaf its constructor names end in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailurePath {
    pub leaf: LedgerLeaf,
}

impl FailurePath {
    /// The class of the failure's rule.
    pub fn class(&self) -> RefusalClass {
        self.leaf.class()
    }
}

impl<'de> Deserialize<'de> for FailurePath {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let names = NonEmpty::<String>::deserialize(d)?;
        let name = names.last();
        let leaf = LedgerLeaf::of_name(name)
            .ok_or_else(|| D::Error::custom(format!("unclassified ledger leaf {name}")))?;
        Ok(FailurePath { leaf })
    }
}

/// The protocol parameters the ledger ran a record under.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PParams(pub serde_json::Map<String, serde_json::Value>);

/// Whether a transaction declares its scripts to pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase2 {
    Valid,
    Invalid,
}

/// The scripts' declared result, with the ledger's answer for the same
/// transaction under a true flag when it declares they fail.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Declared {
    Valid,
    Invalid(LedgerOutcome),
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
    /// The UTxO, written whole because it is not the one the previous record
    /// left.
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

impl LedgerOutcome {
    /// Whether the ledger's decoder refused the transaction's bytes.
    pub fn undecoded(&self) -> bool {
        match self {
            LedgerOutcome::Accepted { .. } => false,
            LedgerOutcome::Refused { failure_names, .. } => failure_names
                .iter()
                .any(|path| path.class() == RefusalClass::Decoding),
        }
    }
}

/// The slot to time map of the era the ledger ran a record in.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EraTime {
    pub system_start_ms: u64,
    pub slot_length_ms: u64,
    pub epoch_size: u64,
    pub security_param: u64,
    pub active_slot_coeff: (u64, NonZeroU64),
}

/// The time the ledger read a record under, with the first slot it could not
/// translate when one was within two epochs past the stability window.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Time {
    pub era: EraTime,
    pub horizon_slot: Option<u64>,
}

/// The redeemers the ledger evaluated at a transaction's top level and in each
/// of its sub transactions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redeemers {
    pub top: Vec<LedgerRedeemer>,
    pub subs: Vec<Vec<LedgerRedeemer>>,
}

/// The redeemers the ledger evaluated for a record, or that its decoder refused
/// the record's bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerRedeemers {
    Read(Redeemers),
    Undecoded,
}

/// One LEDGER rule run as the extractor wrote it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "RecordLine")]
pub struct Record {
    pub n: u32,
    pub slot: u64,
    pub epoch: Option<u64>,
    pub phase2: Declared,
    pub tx: Bytes,
    pub pparams: PParams,
    pub pre: Pre,
    pub accounts_pre: LedgerAccounts,
    pub certs_pre: Certs,
    pub gov_pre: Gov,
    pub treasury: u64,
    pub outcome: LedgerOutcome,
    pub redeemers: LedgerRedeemers,
    pub min_fee: Option<u64>,
    pub time: Time,
}

/// One corpus line, the record with its redeemers as the extractor lists them.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordLine {
    n: u32,
    slot: u64,
    epoch: Option<u64>,
    phase2: Declared,
    tx: Bytes,
    pparams: PParams,
    pre: Pre,
    accounts_pre: LedgerAccounts,
    certs_pre: Certs,
    gov_pre: Gov,
    treasury: u64,
    outcome: LedgerOutcome,
    ex_units: Vec<LedgerRedeemer>,
    sub_ex_units: Vec<Vec<LedgerRedeemer>>,
    min_fee: Option<u64>,
    time: Time,
}

impl TryFrom<RecordLine> for Record {
    type Error = String;

    fn try_from(line: RecordLine) -> Result<Self, String> {
        let RecordLine {
            n,
            slot,
            epoch,
            phase2,
            tx,
            pparams,
            pre,
            accounts_pre,
            certs_pre,
            gov_pre,
            treasury,
            outcome,
            ex_units,
            sub_ex_units,
            min_fee,
            time,
        } = line;
        let listed = ex_units.len() + sub_ex_units.iter().map(Vec::len).sum::<usize>();
        let redeemers = match (outcome.undecoded(), listed) {
            (false, _) => LedgerRedeemers::Read(Redeemers {
                top: ex_units,
                subs: sub_ex_units,
            }),
            (true, 0) => LedgerRedeemers::Undecoded,
            (true, _) => {
                return Err(format!(
                    "the ledger's decoder refused the bytes and {listed} redeemers are listed"
                ))
            }
        };
        Ok(Record {
            n,
            slot,
            epoch,
            phase2,
            tx,
            pparams,
            pre,
            accounts_pre,
            certs_pre,
            gov_pre,
            treasury,
            outcome,
            redeemers,
            min_fee,
            time,
        })
    }
}

impl Record {
    /// Whether the transaction declares its scripts to pass.
    pub fn flag(&self) -> Phase2 {
        match self.phase2 {
            Declared::Valid => Phase2::Valid,
            Declared::Invalid(_) => Phase2::Invalid,
        }
    }

    /// The ledger's answer for the transaction as a node receives it, under a
    /// true flag.
    pub fn submitted(&self) -> &LedgerOutcome {
        match &self.phase2 {
            Declared::Valid => &self.outcome,
            Declared::Invalid(submitted) => submitted,
        }
    }
}
