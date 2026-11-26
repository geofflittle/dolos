//! The Dijkstra fields of each transaction and block v1 serves against the
//! values the ledger's bytes hold.

use std::{collections::BTreeMap, fmt};

use ledger_oracle_judge::{
    compare::ServedRoutes as _,
    coverage::Member,
    judgement::{self, Axis, CellKey, EntryPoint, Judge, Outcome, Subject, Verdict},
    record::{CorpusKind, RecordId},
};
use pallas::{
    codec::minicbor,
    crypto::hash::Hasher,
    ledger::primitives::{dijkstra, Nullable, PlutusData, StakeCredential},
};

use super::super::spec::cardano;
use super::{
    output::{self, Credential, Data, Integer},
    serve::RouteServed,
    Rolled,
};

/// Bytes a field states, shown in hexadecimal.
#[derive(Clone, PartialEq, Eq)]
struct Bytes(Vec<u8>);

impl fmt::Debug for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(&self.0))
    }
}

/// The value of one field, which holds a value when it differs from the value
/// an unset field states.
trait Value: PartialEq + fmt::Debug {
    /// Whether the value differs from the value an unset field states.
    fn held(&self) -> bool;
}

impl<T: PartialEq + fmt::Debug> Value for Vec<T> {
    fn held(&self) -> bool {
        !self.is_empty()
    }
}

impl<T: PartialEq + fmt::Debug> Value for Option<T> {
    fn held(&self) -> bool {
        self.is_some()
    }
}

impl Value for Bytes {
    fn held(&self) -> bool {
        !self.0.is_empty()
    }
}

impl Value for bool {
    fn held(&self) -> bool {
        *self
    }
}

/// The interval an account balance must fall within.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Interval {
    Unset,
    Exact(Option<Integer>),
    Range(Option<Integer>, Option<Integer>),
}

/// Whether the ledger's value of a field and the served one are equal.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Values {
    Equal,
    Differ { ledger: String, dolos: String },
}

/// One field of a served message against the ledger's value of it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Compared {
    path: String,
    /// Whether either side holds a value.
    held: bool,
    values: Values,
}

/// The field `name` at `at` of the ledger's value and the served one.
fn compare<T: Value>(at: &str, name: &str, ledger: &T, served: &T) -> Compared {
    let values = match ledger == served {
        true => Values::Equal,
        false => Values::Differ {
            ledger: format!("{ledger:?}"),
            dolos: format!("{served:?}"),
        },
    };
    Compared {
        path: format!("{at}.{name}"),
        held: ledger.held() || served.held(),
        values,
    }
}

/// Declares the fields of one view with the comparison of each of them.
macro_rules! fields {
    ($(#[$doc:meta])* $name:ident { $($field:ident: $ty:ty,)* }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq)]
        struct $name {
            $($field: $ty,)*
        }

        impl $name {
            /// The name of each field.
            const NAMES: &'static [&'static str] = &[$(stringify!($field)),*];

            /// Each field of the served view against the ledger's, at `at`.
            fn compared(ledger: &Self, served: &Self, at: &str) -> Vec<Compared> {
                vec![$(compare(at, stringify!($field), &ledger.$field, &served.$field)),*]
            }
        }
    };
}

fields! {
    /// The Dijkstra fields of one transaction or sub transaction body.
    Common {
        guards: Vec<Option<Credential>>,
        required_top_level_guards: Vec<(Option<Credential>, Option<Option<Data>>)>,
        direct_deposits: Vec<(Bytes, Option<Integer>)>,
        account_balance_intervals: Vec<(Bytes, Interval)>,
        auxiliary_data_hash: Bytes,
        script_data_hash: Bytes,
        network_id: Option<u32>,
        treasury_value: Option<Integer>,
        treasury_donation: Option<Integer>,
    }
}

fields! {
    /// The Dijkstra fields a transaction states beside its body's.
    Own {
        sub_transactions: Vec<Bytes>,
        starting_account_balance_intervals: Vec<(Bytes, Interval)>,
    }
}

fields! {
    /// The Dijkstra fields of a block header.
    Header {
        endorser_block_announcement: Option<(Bytes, u32)>,
        body_contains_leios_certificate: bool,
    }
}

fields! {
    /// The Dijkstra fields of a block body.
    Body {
        leios_certificate: Option<(Bytes, Bytes)>,
        peras_certificate: Bytes,
    }
}

/// The Dijkstra fields of one transaction, with each sub transaction body's
/// by the sub transaction's hash.
#[derive(Debug, Clone, PartialEq)]
struct TxFields {
    own: Own,
    common: Common,
    subs: Vec<(Bytes, Common)>,
}

impl TxFields {
    /// Each field of the served transaction against the ledger's, a sub
    /// transaction's where the served one lists its hash.
    fn compared(ledger: &Self, served: &Self) -> Vec<Compared> {
        let mut compared = Own::compared(&ledger.own, &served.own, "Tx");
        compared.extend(Common::compared(
            &ledger.common,
            &served.common,
            "Tx.common",
        ));
        for (hash, sub) in &ledger.subs {
            if let Some((_, served)) = served.subs.iter().find(|(h, _)| h == hash) {
                compared.extend(Common::compared(sub, served, "Tx.sub_transactions"));
            }
        }
        compared
    }
}

/// The Dijkstra fields of one block.
#[derive(Debug, Clone, PartialEq)]
struct BlockFields {
    header: Header,
    body: Body,
}

impl BlockFields {
    /// Each field of the served block against the ledger's.
    fn compared(ledger: &Self, served: &Self) -> Vec<Compared> {
        let mut compared = Header::compared(&ledger.header, &served.header, "Block.header");
        compared.extend(Body::compared(&ledger.body, &served.body, "Block.body"));
        compared
    }
}

/// The credential a served stake credential states, none when it states none.
fn served_credential(credential: &cardano::StakeCredential) -> Option<Credential> {
    use cardano::stake_credential::StakeCredential as C;
    let cardano::StakeCredential { stake_credential } = credential;
    Some(match stake_credential.as_ref()? {
        C::AddrKeyHash(h) => Credential::Key(h.to_vec()),
        C::ScriptHash(h) => Credential::Script(h.to_vec()),
    })
}

/// The account and interval a served account balance interval states.
fn served_interval(interval: &cardano::AccountBalanceInterval) -> (Bytes, Interval) {
    use cardano::account_balance_interval::Interval as I;
    let cardano::AccountBalanceInterval {
        reward_account,
        interval,
    } = interval;
    let interval = match interval {
        None => Interval::Unset,
        Some(I::Exact(coin)) => Interval::Exact(output::v1::integer(coin)),
        Some(I::Range(range)) => {
            let cardano::AccountBalanceRange {
                inclusive_lower_bound,
                exclusive_upper_bound,
            } = range;
            let bound = |b: &Option<cardano::BigInt>| b.as_ref().and_then(output::v1::integer);
            Interval::Range(bound(inclusive_lower_bound), bound(exclusive_upper_bound))
        }
    };
    (Bytes(reward_account.to_vec()), interval)
}

impl Common {
    /// The fields a served `TxCommon` states.
    fn served(common: &cardano::TxCommon) -> Self {
        let cardano::TxCommon {
            inputs: _,
            outputs: _,
            certificates: _,
            withdrawals: _,
            mint: _,
            reference_inputs: _,
            witnesses: _,
            validity: _,
            auxiliary: _,
            hash: _,
            proposals: _,
            votes: _,
            guards,
            required_top_level_guards,
            direct_deposits,
            account_balance_intervals,
            auxiliary_data_hash,
            script_data_hash,
            network_id,
            treasury_value,
            treasury_donation,
        } = common;
        let guard = |g: &cardano::RequiredTopLevelGuard| {
            let cardano::RequiredTopLevelGuard { credential, datum } = g;
            let credential = credential.as_ref().and_then(served_credential);
            (credential, datum.as_ref().map(output::v1::data))
        };
        let deposit = |d: &cardano::DirectDeposit| {
            let cardano::DirectDeposit {
                reward_account,
                coin,
            } = d;
            let coin = coin.as_ref().and_then(output::v1::integer);
            (Bytes(reward_account.to_vec()), coin)
        };
        let integer = |i: &Option<cardano::BigInt>| i.as_ref().and_then(output::v1::integer);
        Common {
            guards: guards.iter().map(served_credential).collect(),
            required_top_level_guards: required_top_level_guards.iter().map(guard).collect(),
            direct_deposits: direct_deposits.iter().map(deposit).collect(),
            account_balance_intervals: account_balance_intervals
                .iter()
                .map(served_interval)
                .collect(),
            auxiliary_data_hash: Bytes(auxiliary_data_hash.to_vec()),
            script_data_hash: Bytes(script_data_hash.to_vec()),
            network_id: *network_id,
            treasury_value: integer(treasury_value),
            treasury_donation: integer(treasury_donation),
        }
    }
}

impl TxFields {
    /// The fields a served transaction states with the hash it states, none
    /// when it states no common fields.
    fn served(tx: &cardano::Tx) -> Option<(Bytes, Self)> {
        let cardano::Tx {
            common,
            collateral: _,
            fee: _,
            successful: _,
            sub_transactions,
            starting_account_balance_intervals,
        } = tx;
        let common = common.as_ref()?;
        let subs: Vec<(Bytes, Common)> = sub_transactions
            .iter()
            .map(|s| (Bytes(s.hash.to_vec()), Common::served(s)))
            .collect();
        let own = Own {
            sub_transactions: subs.iter().map(|(hash, _)| hash.clone()).collect(),
            starting_account_balance_intervals: starting_account_balance_intervals
                .iter()
                .map(served_interval)
                .collect(),
        };
        let fields = TxFields {
            own,
            common: Common::served(common),
            subs,
        };
        Some((Bytes(common.hash.to_vec()), fields))
    }
}

/// The fields a served block states with the hash its header states and its
/// transactions, or the part it lacks.
fn served_block(
    block: &cardano::Block,
) -> Result<(Bytes, BlockFields, &[cardano::Tx]), &'static str> {
    let cardano::Block {
        header,
        body,
        timestamp: _,
    } = block;
    let cardano::BlockHeader {
        slot: _,
        hash,
        height: _,
        endorser_block_announcement,
        body_contains_leios_certificate,
    } = header.as_ref().ok_or("header=none")?;
    let cardano::BlockBody {
        tx,
        leios_certificate,
        peras_certificate,
    } = body.as_ref().ok_or("body=none")?;
    let announcement = |a: &cardano::EndorserBlockAnnouncement| {
        let cardano::EndorserBlockAnnouncement {
            references_hash,
            references_size,
        } = a;
        (Bytes(references_hash.to_vec()), *references_size)
    };
    let certificate = |c: &cardano::LeiosCertificate| {
        let cardano::LeiosCertificate { signers, signature } = c;
        (Bytes(signers.to_vec()), Bytes(signature.to_vec()))
    };
    let fields = BlockFields {
        header: Header {
            endorser_block_announcement: endorser_block_announcement.as_ref().map(announcement),
            body_contains_leios_certificate: *body_contains_leios_certificate,
        },
        body: Body {
            leios_certificate: leios_certificate.as_ref().map(certificate),
            peras_certificate: Bytes(peras_certificate.to_vec()),
        },
    };
    Ok((Bytes(hash.to_vec()), fields, tx.as_slice()))
}

/// A ledger credential.
fn ledger_credential(credential: &StakeCredential) -> Credential {
    match credential {
        StakeCredential::AddrKeyhash(h) => Credential::Key(h.to_vec()),
        StakeCredential::ScriptHash(h) => Credential::Script(h.to_vec()),
    }
}

/// The credentials that guard a body, a bare key hash as a key credential.
fn ledger_guards(guards: Option<&dijkstra::Guards>) -> Vec<Option<Credential>> {
    match guards {
        None => Vec::new(),
        Some(dijkstra::Guards::AddrKeyhashes(keys)) => keys
            .iter()
            .map(|k| Some(Credential::Key(k.to_vec())))
            .collect(),
        Some(dijkstra::Guards::Credentials(credentials)) => credentials
            .iter()
            .map(|c| Some(ledger_credential(c)))
            .collect(),
    }
}

/// The datum a required guard names, none for a nil one.
fn ledger_datum(datum: &Nullable<PlutusData>) -> Option<Option<Data>> {
    match datum {
        Nullable::Some(datum) => Some(Some(Data::ledger(datum))),
        Nullable::Null | Nullable::Undefined => None,
    }
}

/// The account and interval of one ledger account balance interval.
fn ledger_interval(
    (account, interval): (
        &pallas::ledger::primitives::RewardAccount,
        &dijkstra::AccountBalanceInterval,
    ),
) -> (Bytes, Interval) {
    use dijkstra::AccountBalanceInterval as A;
    let coin = |c: u64| Some(Integer::of(c.into()));
    let interval = match *interval {
        A::LowerBound(l) => Interval::Range(coin(l), None),
        A::Bounded(l, u) => Interval::Range(coin(l), coin(u)),
        A::UpperBound(u) => Interval::Range(None, coin(u)),
        A::Exact(c) => Interval::Exact(coin(c)),
    };
    (Bytes(account.to_vec()), interval)
}

/// A ledger hash, empty when the body states none.
fn ledger_hash(hash: Option<&pallas::crypto::hash::Hash<32>>) -> Bytes {
    Bytes(hash.map(|h| h.to_vec()).unwrap_or_default())
}

/// The Dijkstra fields of a transaction or sub transaction body.
macro_rules! ledger_common {
    ($body:expr) => {{
        let body = $body;
        let deposit = |(account, coin): (&pallas::ledger::primitives::RewardAccount, &u64)| {
            (Bytes(account.to_vec()), Some(Integer::of((*coin).into())))
        };
        let guards = body.required_top_level_guards.iter().flatten();
        let deposits = body.direct_deposits.iter().flatten();
        let intervals = body.account_balance_intervals.iter().flatten();
        Common {
            guards: ledger_guards(body.guards.as_ref()),
            required_top_level_guards: guards
                .map(|(c, d)| (Some(ledger_credential(c)), ledger_datum(d)))
                .collect(),
            direct_deposits: deposits.map(deposit).collect(),
            account_balance_intervals: intervals.map(ledger_interval).collect(),
            auxiliary_data_hash: ledger_hash(body.auxiliary_data_hash.as_ref()),
            script_data_hash: ledger_hash(body.script_data_hash.as_ref()),
            network_id: body.network_id.map(|n| u32::from(u8::from(n))),
            treasury_value: body.treasury_value.map(|v| Integer::of(v.into())),
            treasury_donation: body.donation.map(|v| Integer::of(u64::from(v).into())),
        }
    }};
}

/// The Dijkstra fields a transaction's bytes state, with its hash.
fn ledger_tx(bytes: &[u8]) -> Result<([u8; 32], TxFields), minicbor::decode::Error> {
    let tx: dijkstra::MempoolTransaction = minicbor::decode(bytes)?;
    let body = &tx.transaction_body;
    let subs: Vec<(Bytes, Common)> = body
        .sub_transactions
        .iter()
        .flatten()
        .map(|s| {
            let sub = &s.sub_transaction_body;
            let hash = Hasher::<256>::hash(sub.raw_cbor());
            (Bytes(hash.to_vec()), ledger_common!(sub))
        })
        .collect();
    let starting = body.starting_account_balance_intervals.iter().flatten();
    let own = Own {
        sub_transactions: subs.iter().map(|(hash, _)| hash.clone()).collect(),
        starting_account_balance_intervals: starting.map(ledger_interval).collect(),
    };
    let fields = TxFields {
        own,
        common: ledger_common!(body),
        subs,
    };
    Ok((*Hasher::<256>::hash(body.raw_cbor()), fields))
}

/// The Dijkstra fields a block's bytes state.
fn ledger_block(bytes: &[u8]) -> Result<BlockFields, minicbor::decode::Error> {
    let (_, block): (u16, dijkstra::Block) = minicbor::decode(bytes)?;
    let dijkstra::HeaderBody::Leios(header) = &block.header.header_body else {
        return Err(minicbor::decode::Error::message(
            "a Praos header body states no Leios field",
        ));
    };
    let body = &block.block_body;
    let announcement = match &header.eb_announcement {
        Nullable::Some(a) => Some((Bytes(a.eb_hash.to_vec()), a.eb_size)),
        Nullable::Null | Nullable::Undefined => None,
    };
    let certificate = match &body.leios_certificate {
        Nullable::Some(c) => Some((Bytes(c.signers.to_vec()), Bytes(c.signature.to_vec()))),
        Nullable::Null | Nullable::Undefined => None,
    };
    let peras = match &body.peras_certificate {
        Nullable::Some(p) => p.to_vec(),
        Nullable::Null | Nullable::Undefined => Vec::new(),
    };
    Ok(BlockFields {
        header: Header {
            endorser_block_announcement: announcement,
            body_contains_leios_certificate: header.block_body_contains_leios_cert,
        },
        body: Body {
            leios_certificate: certificate,
            peras_certificate: Bytes(peras),
        },
    })
}

/// Adds the verdict on each compared field, at `entry` on `subject`.
fn add(out: &mut judgement::Tally, entry: EntryPoint, subject: Subject, compared: Vec<Compared>) {
    for Compared { path, held, values } in compared {
        let cells = match held {
            true => vec![CellKey::new(Axis::Field, &path)],
            false => vec![CellKey::new(Axis::Ledger, "none")],
        };
        let outcome = match values {
            Values::Equal => Outcome::Agree,
            Values::Differ { ledger, dolos } => Outcome::Disagree(format!(
                "DIJKSTRA-FIELD path={path} ledger={ledger} dolos={dolos}"
            )),
        };
        out.add(Verdict {
            judge: Judge::DijkstraField,
            entry: Some(entry),
            subject,
            cells,
            outcome,
        });
    }
}

/// The ledger's fields of each transaction and block the rolled blocks hold,
/// by hash.
struct Ledger {
    txs: BTreeMap<Vec<u8>, (RecordId, TxFields)>,
    blocks: BTreeMap<Vec<u8>, (Subject, BlockFields)>,
}

impl Ledger {
    /// The fields of the rolled blocks and the records they hold, a block's
    /// verdicts on its last record.
    fn of(kind: CorpusKind, rolled: &[&Rolled<'_>]) -> Self {
        let mut txs = BTreeMap::new();
        let mut blocks = BTreeMap::new();
        for rolled in rolled {
            let id = |n| RecordId { corpus: kind, n };
            for record in &rolled.records {
                let (hash, fields) =
                    ledger_tx(&record.tx.0).unwrap_or_else(|e| panic!("TX n={} {e}", record.n));
                txs.insert(hash.to_vec(), (id(record.n), fields));
            }
            let hash = rolled.point.hash().expect("a rolled block has a hash");
            let fields = ledger_block(&rolled.block)
                .unwrap_or_else(|e| panic!("BLOCK slot={} {e}", rolled.point.slot()));
            let last = rolled.records.last().map(|r| Subject::Record(id(r.n)));
            blocks.insert(hash.to_vec(), (last.unwrap_or(Subject::Selection), fields));
        }
        Ledger { txs, blocks }
    }

    /// Adds the verdicts on a served transaction at `entry`.
    fn judge_tx(&self, entry: EntryPoint, tx: &cardano::Tx, out: &mut judgement::Tally) {
        let fails = |detail: String| {
            Verdict::selection_fails(
                Judge::DijkstraField,
                entry,
                format!("DIJKSTRA-FIELD {detail}"),
            )
        };
        let Some((hash, served)) = TxFields::served(tx) else {
            return out.add(fails("common=none".to_string()));
        };
        let Some((id, ledger)) = self.txs.get(&hash.0) else {
            return out.add(fails(format!("tx={hash:?} ledger=none")));
        };
        add(
            out,
            entry,
            Subject::Record(*id),
            TxFields::compared(ledger, &served),
        );
    }

    /// Adds the verdicts on a served block and each transaction it holds at
    /// `entry`.
    fn judge_block(&self, entry: EntryPoint, block: &cardano::Block, out: &mut judgement::Tally) {
        let fails = |detail: String| {
            Verdict::selection_fails(
                Judge::DijkstraField,
                entry,
                format!("DIJKSTRA-FIELD {detail}"),
            )
        };
        let (hash, served, txs) = match served_block(block) {
            Ok(served) => served,
            Err(lacks) => return out.add(fails(lacks.to_string())),
        };
        match self.blocks.get(&hash.0) {
            Some((subject, ledger)) => {
                add(out, entry, *subject, BlockFields::compared(ledger, &served))
            }
            None => out.add(fails(format!("block={hash:?} ledger=none"))),
        }
        for tx in txs {
            self.judge_tx(entry, tx, out);
        }
    }
}

/// Adds the verdicts on the Dijkstra fields of every transaction and block
/// each route serves against the rolled blocks and the records they hold.
pub(super) fn judge(
    kind: CorpusKind,
    rolled: &[&Rolled<'_>],
    routes: &[RouteServed],
    out: &mut judgement::Tally,
) {
    let ledger = Ledger::of(kind, rolled);
    for route in routes {
        let entry = route.route.at(route.version);
        for tx in &route.messages.txs {
            ledger.judge_tx(entry, tx, out);
        }
        for block in &route.messages.blocks {
            ledger.judge_block(entry, block, out);
        }
    }
}

/// Each field path the judge compares, the block fields excluded since the
/// oracle builds every block with none of them.
pub(super) fn members() -> Vec<Member> {
    let at = |prefix: &'static str, names: &'static [&'static str]| {
        names.iter().map(move |name| format!("{prefix}.{name}"))
    };
    let counted = at("Tx", Own::NAMES)
        .chain(at("Tx.common", Common::NAMES))
        .chain(at("Tx.sub_transactions", Common::NAMES))
        .map(|path| Member::counted(Axis::Field, path));
    let reason = "the oracle builds every block with no announcement and no certificate";
    let command = "rg -n 'leios_cert|eb_announcement|peras_certificate' \
                   src/serve/grpc/v1/ledger_oracle_tests/validate.rs";
    let excluded = at("Block.header", Header::NAMES)
        .chain(at("Block.body", Body::NAMES))
        .map(|path| Member::excluded(Axis::Field, path, reason, command));
    counted.chain(excluded).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use ledger_oracle_judge::{
        compare::OutputRoute, corpus, coverage::Reach, record::LedgerOutcome,
    };
    use pallas::ledger::traverse::{Era, MultiEraBlock, MultiEraTx};

    use super::*;

    /// The transaction v1 maps from a record's bytes.
    fn mapped(bytes: &[u8]) -> cardano::Tx {
        let tx = MultiEraTx::decode_for_era(Era::Dijkstra, bytes).unwrap();
        pallas::interop::utxorpc::v1::Mapper::new(output::Unresolved).map_tx(&tx)
    }

    /// The bytes and the transaction v1 maps of every accepted record of both
    /// corpora, by record.
    fn records() -> Vec<(RecordId, Vec<u8>, cardano::Tx)> {
        let mut records = Vec::new();
        for kind in [CorpusKind::Ic, CorpusKind::Supp] {
            let corpus = corpus::load(kind).unwrap_or_else(|e| panic!("{e}"));
            for run in corpus.kept() {
                for record in run.records() {
                    if let LedgerOutcome::Accepted { .. } = &record.outcome {
                        let id = RecordId {
                            corpus: kind,
                            n: record.n,
                        };
                        records.push((id, record.tx.0.clone(), mapped(&record.tx.0)));
                    }
                }
            }
        }
        records
    }

    /// The compared fields of a served transaction against a record's bytes.
    fn compared(bytes: &[u8], served: &cardano::Tx) -> Vec<Compared> {
        let (hash, ledger) = ledger_tx(bytes).unwrap();
        let (served_hash, served) = TxFields::served(served).unwrap();
        assert_eq!(served_hash.0, hash);
        TxFields::compared(&ledger, &served)
    }

    /// The path of each compared field that differs.
    fn differing(compared: &[Compared]) -> Vec<&str> {
        let differ = compared.iter().filter(|c| c.values != Values::Equal);
        differ.map(|c| c.path.as_str()).collect()
    }

    #[test]
    fn every_field_v1_maps_from_an_accepted_record_agrees_with_its_bytes() {
        let records = records();
        assert!(records.len() > 100, "{}", records.len());
        let mut disagree = Vec::new();
        for (id, bytes, served) in &records {
            for c in compared(bytes, served) {
                if c.values != Values::Equal {
                    disagree.push(format!("{id} {c:?}"));
                }
            }
        }
        assert_eq!(disagree, Vec::<String>::new());
    }

    /// Unsets one field of a served transaction.
    type Unset = Box<dyn Fn(&mut cardano::Tx)>;

    /// A field of a served body by name with the change that unsets it.
    type UnsetBody = (&'static str, fn(&mut cardano::TxCommon));

    /// A field a served transaction states beside its body's by path with the
    /// change that unsets it.
    type UnsetOwn = (&'static str, fn(&mut cardano::Tx));

    /// The path of each field the judge compares with the change that unsets
    /// it in a served transaction, a body field in the transaction's body and
    /// in every sub transaction's.
    fn unsets() -> Vec<(String, Unset)> {
        let body: [UnsetBody; 9] = [
            ("guards", |c| c.guards.clear()),
            ("required_top_level_guards", |c| {
                c.required_top_level_guards.clear()
            }),
            ("direct_deposits", |c| c.direct_deposits.clear()),
            ("account_balance_intervals", |c| {
                c.account_balance_intervals.clear()
            }),
            ("auxiliary_data_hash", |c| c.auxiliary_data_hash.clear()),
            ("script_data_hash", |c| c.script_data_hash.clear()),
            ("network_id", |c| c.network_id = None),
            ("treasury_value", |c| c.treasury_value = None),
            ("treasury_donation", |c| c.treasury_donation = None),
        ];
        let own: [UnsetOwn; 2] = [
            ("Tx.sub_transactions", |t| t.sub_transactions.clear()),
            ("Tx.starting_account_balance_intervals", |t| {
                t.starting_account_balance_intervals.clear()
            }),
        ];
        let mut unsets: Vec<(String, Unset)> = Vec::new();
        for (name, unset) in body {
            let top = move |t: &mut cardano::Tx| unset(t.common.as_mut().unwrap());
            unsets.push((format!("Tx.common.{name}"), Box::new(top)));
            let subs = move |t: &mut cardano::Tx| t.sub_transactions.iter_mut().for_each(unset);
            unsets.push((format!("Tx.sub_transactions.{name}"), Box::new(subs)));
        }
        for (path, unset) in own {
            unsets.push((path.to_string(), Box::new(unset)));
        }
        unsets
    }

    #[test]
    fn unsetting_a_field_some_record_holds_disagrees_at_that_field_alone() {
        let records = records();
        let unsets = unsets();
        let counted = members().into_iter().filter(|m| m.reach == Reach::Counted);
        let counted: BTreeSet<String> = counted.map(|m| m.cell.member).collect();
        let paths: BTreeSet<String> = unsets.iter().map(|(path, _)| path.clone()).collect();
        assert_eq!((paths.len(), paths), (unsets.len(), counted));
        let mut unheld = BTreeSet::new();
        for (path, unset) in &unsets {
            let held = records.iter().find(|(_, bytes, served)| {
                let compared = compared(bytes, served);
                compared.iter().any(|c| &c.path == path && c.held)
            });
            let Some((id, bytes, served)) = held else {
                unheld.insert(path.as_str());
                continue;
            };
            let mut changed = served.clone();
            unset(&mut changed);
            let compared = compared(bytes, &changed);
            let differ: BTreeSet<&str> = differing(&compared).into_iter().collect();
            assert_eq!(differ, BTreeSet::from([path.as_str()]), "{id}");
        }
        let expected = BTreeSet::from(["Tx.sub_transactions.auxiliary_data_hash"]);
        assert_eq!(unheld, expected);
    }

    #[test]
    fn a_built_block_agrees_and_one_stating_a_certificate_disagrees_at_that_field_alone() {
        let (_, raw) = super::super::validate::block(10, None, &[]).unwrap();
        let block = MultiEraBlock::decode(&raw).unwrap();
        let mapped =
            pallas::interop::utxorpc::v1::Mapper::new(output::Unresolved).map_block(&block);
        let ledger = ledger_block(&raw).unwrap();
        let (_, served, _) = served_block(&mapped).unwrap();
        let compared = BlockFields::compared(&ledger, &served);
        assert_eq!(differing(&compared), Vec::<&str>::new());
        assert!(compared.iter().all(|c| !c.held), "{compared:?}");
        let mut changed = mapped.clone();
        changed
            .header
            .as_mut()
            .unwrap()
            .body_contains_leios_certificate = true;
        let (_, served, _) = served_block(&changed).unwrap();
        let compared = BlockFields::compared(&ledger, &served);
        let path = "Block.header.body_contains_leios_certificate";
        assert_eq!(differing(&compared), vec![path]);
    }

    #[test]
    fn a_served_transaction_dropping_a_held_deposit_fails_with_its_record_and_path() {
        let records = records();
        let (id, bytes, served) = records
            .iter()
            .find(|(_, _, t)| !t.common.as_ref().unwrap().direct_deposits.is_empty())
            .expect("a record holds a direct deposit");
        let (hash, fields) = ledger_tx(bytes).unwrap();
        let ledger = Ledger {
            txs: BTreeMap::from([(hash.to_vec(), (*id, fields))]),
            blocks: BTreeMap::new(),
        };
        let entry = OutputRoute::ReadTx.at(judgement::Version::V1);
        let mut kept = judgement::Tally::default();
        ledger.judge_tx(entry, served, &mut kept);
        assert!(kept.failing.is_empty(), "{kept}");
        let mut changed = served.clone();
        changed.common.as_mut().unwrap().direct_deposits.clear();
        let mut dropped = judgement::Tally::default();
        ledger.judge_tx(entry, &changed, &mut dropped);
        let failing: Vec<String> = dropped.failing.iter().map(Verdict::to_string).collect();
        assert_eq!(failing.len(), 1, "{failing:?}");
        let head = format!(
            "VERDICT judge=dijkstra-field entry={entry} record={id} \
             cells=field:Tx.common.direct_deposits outcome=disagree DIJKSTRA-FIELD \
             path=Tx.common.direct_deposits ledger=["
        );
        assert!(failing[0].starts_with(&head), "{failing:?}");
        assert!(failing[0].ends_with(" dolos=[]"), "{failing:?}");
    }
}
