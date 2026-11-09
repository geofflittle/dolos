//! Judges the fee, witness and auxiliary scripts, committee thresholds and
//! Dijkstra parameter keys each ReadTx version maps for a transaction against
//! the ledger's transaction.

use std::{fmt, panic::AssertUnwindSafe};

use dolos_cardano::indexes::ScriptLanguage;
use futures_util::FutureExt as _;
use itertools::Itertools as _;
use ledger_oracle_judge::{
    compare::ByEntry,
    eval::WireFee,
    judgement::{Axis, CellKey, EntryPoint, Finding, Route, Version},
    record::{CorpusKind, LedgerOutcome, RecordId},
};
use pallas::{
    crypto::hash::Hasher,
    ledger::traverse::{Era, MultiEraGovActionKind, MultiEraParamUpdate, MultiEraTx},
};
use tonic::Request;

use super::{
    eval::wire_fee, named_txs, validate::panic_message, witness_scripts, GroupBlock, TxName,
};
use crate::adapters::DomainAdapter;

/// A witness script by its language, with its hash when the message carries its
/// bytes, none for a script message with no script in it.
type ScriptKey = Option<(ScriptLanguage, Option<[u8; 28]>)>;

/// The threshold a proposal sets when it updates the committee, as one side
/// holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quorum<T> {
    /// A proposal of another kind.
    Other,
    /// A committee update with this threshold.
    Update(T),
}

/// A committee threshold as the ledger holds it, a 64 bit numerator and
/// denominator.
type LedgerQuorum = Quorum<(u64, u64)>;

/// A committee threshold as a u5c message holds it, none when the message
/// carries none.
type MappedQuorum = Quorum<Option<(i32, u32)>>;

impl LedgerQuorum {
    /// Whether a mapped threshold is this one within 2^-30, which a pair of 32
    /// bit fields holds for any threshold the ledger accepts.
    fn holds(self, mapped: MappedQuorum) -> bool {
        match (self, mapped) {
            (Quorum::Other, Quorum::Other) => true,
            (Quorum::Update((n, d)), Quorum::Update(Some((m, e)))) => {
                let cross = i128::from(n) * i128::from(e) - i128::from(m) * i128::from(d);
                cross.unsigned_abs() << 30 <= u128::from(d) * u128::from(e)
            }
            _ => false,
        }
    }
}

impl fmt::Display for LedgerQuorum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Quorum::Other => f.write_str("other"),
            Quorum::Update((n, d)) => write!(f, "{n}/{d}"),
        }
    }
}

impl fmt::Display for MappedQuorum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Quorum::Other => f.write_str("other"),
            Quorum::Update(Some((n, d))) => write!(f, "{n}/{d}"),
            Quorum::Update(None) => f.write_str("none"),
        }
    }
}

/// The parameters new in Dijkstra that a parameter change proposes, none for a
/// proposal of another kind.
type DijkstraKeys = Option<Vec<&'static str>>;

/// The names of the parameters new in Dijkstra that a ledger or message
/// parameter update holds a value for, with every field of the ledger's update
/// named so that a field it gains fails to compile.
macro_rules! dijkstra_keys {
    (@ledger $update:expr; [$($conway:ident),+]; $($name:ident),+) => {{
        let pallas::ledger::primitives::dijkstra::ProtocolParamUpdate {
            $($conway: _,)+
            $($name,)+
        }: &pallas::ledger::primitives::dijkstra::ProtocolParamUpdate = &$update;
        let mut keys = vec![];
        $(if $name.is_some() {
            keys.push(stringify!($name));
        })+
        keys
    }};
    (@message $update:expr; [$($conway:ident),+]; $($name:ident),+) => {{
        let update = $update;
        let mut keys = vec![];
        $(if update.$name.is_some() {
            keys.push(stringify!($name));
        })+
        keys
    }};
    ($side:ident $update:expr) => {
        dijkstra_keys!(
            @$side $update;
            [
                minfee_a,
                minfee_b,
                max_block_body_size,
                max_transaction_size,
                max_block_header_size,
                key_deposit,
                pool_deposit,
                maximum_epoch,
                desired_number_of_stake_pools,
                pool_pledge_influence,
                expansion_rate,
                treasury_growth_rate,
                min_pool_cost,
                ada_per_utxo_byte,
                cost_models_for_script_languages,
                execution_costs,
                max_tx_ex_units,
                max_block_ex_units,
                max_value_size,
                collateral_percentage,
                max_collateral_inputs,
                pool_voting_thresholds,
                drep_voting_thresholds,
                min_committee_size,
                committee_term_limit,
                governance_action_validity_period,
                governance_action_deposit,
                drep_deposit,
                drep_inactivity_period,
                minfee_refscript_cost_per_byte
            ];
            max_ref_script_size_per_block,
            max_ref_script_size_per_tx,
            ref_script_cost_stride,
            ref_script_cost_multiplier,
            max_pledge_leverage,
            min_pool_margin,
            leios_announcement_period_length,
            leios_vote_period_length,
            leios_diffusion_period_length,
            leios_committee_size,
            leios_quorum_stake_threshold,
            max_endorser_block_references_size,
            max_endorser_block_txs_size,
            max_endorser_block_execution_units,
            max_ref_script_size_per_endorser_block,
            peras_min_candidate_block_age,
            peras_healing_factor,
            peras_cert_boost,
            peras_target_committee_size,
            peras_bootstrap_round,
            peras_quorum_threshold_safety_margin,
            ref_inputs_cost_per_multi_asset_policy,
            ref_inputs_cost_per_datum_byte
        )
    };
}

/// The fee, witness and auxiliary scripts, committee thresholds and Dijkstra
/// parameter keys of a transaction as the ledger holds them.
struct LedgerTx {
    fee: Option<u64>,
    scripts: Vec<ScriptKey>,
    aux_scripts: Vec<ScriptKey>,
    quorum: Vec<LedgerQuorum>,
    dijkstra_keys: Vec<DijkstraKeys>,
}

impl LedgerTx {
    /// The fields the ledger holds for a transaction.
    fn of(tx: &MultiEraTx<'_>) -> Self {
        let scripts = witness_scripts(tx).into_iter().map(|(hash, language)| {
            let hash = (language != ScriptLanguage::Native).then_some(hash);
            Some((language, hash))
        });
        let native = tx.multi_era_aux_native_scripts().into_iter();
        let native = native.map(|_| Some((ScriptLanguage::Native, None)));
        let v1 = tx.aux_plutus_v1_scripts().iter();
        let v1 = v1.map(|s| plutus(ScriptLanguage::PlutusV1, &s.0));
        let v2 = tx.aux_plutus_v2_scripts().iter();
        let v2 = v2.map(|s| plutus(ScriptLanguage::PlutusV2, &s.0));
        let v3 = tx.aux_plutus_v3_scripts().iter();
        let v3 = v3.map(|s| plutus(ScriptLanguage::PlutusV3, &s.0));
        let v4 = tx.aux_plutus_v4_scripts().iter();
        let v4 = v4.map(|s| plutus(ScriptLanguage::PlutusV4, &s.0));
        let aux_scripts = native.chain(v1).chain(v2).chain(v3).chain(v4);
        let proposals = tx.gov_proposals();
        let quorum = proposals.iter().map(|p| match p.gov_action().kind() {
            MultiEraGovActionKind::UpdateCommittee(.., threshold) => {
                Quorum::Update((threshold.numerator, threshold.denominator))
            }
            _ => Quorum::Other,
        });
        let keys = proposals.iter().map(|p| match p.gov_action().kind() {
            MultiEraGovActionKind::ParameterChange(_, MultiEraParamUpdate::Dijkstra(x), _) => {
                Some(dijkstra_keys!(ledger x))
            }
            MultiEraGovActionKind::ParameterChange(..) => Some(vec![]),
            _ => None,
        });
        LedgerTx {
            fee: tx.fee(),
            scripts: scripts.sorted().collect(),
            aux_scripts: aux_scripts.sorted().collect(),
            quorum: quorum.collect(),
            dijkstra_keys: keys.collect(),
        }
    }
}

/// The fee, witness and auxiliary scripts, committee thresholds and Dijkstra
/// parameter keys a ReadTx version maps for a transaction, with no fee or keys
/// in a message that has no field for them.
struct MappedTx {
    fee: Option<WireFee>,
    scripts: Vec<ScriptKey>,
    aux_scripts: Vec<ScriptKey>,
    quorum: Vec<MappedQuorum>,
    dijkstra_keys: Option<Vec<DijkstraKeys>>,
}

/// The Dijkstra parameter keys each proposal of a v1 transaction message
/// proposes.
fn v1_dijkstra_keys(
    tx: &pallas::interop::utxorpc::v1::spec::cardano::TxCommon,
) -> Vec<DijkstraKeys> {
    use pallas::interop::utxorpc::v1::spec::cardano::governance_action::GovernanceAction;
    let proposals = tx.proposals.iter();
    let actions = proposals.map(|p| {
        p.gov_action
            .as_ref()
            .and_then(|a| a.governance_action.as_ref())
    });
    let keys = actions.map(|action| match action {
        Some(GovernanceAction::ParameterChangeAction(a)) => Some(
            a.protocol_param_update
                .as_ref()
                .map_or(vec![], |u| dijkstra_keys!(message u)),
        ),
        _ => None,
    });
    keys.collect()
}

/// The language and hash of a Plutus script a message carries the bytes of.
fn plutus(language: ScriptLanguage, bytes: &[u8]) -> ScriptKey {
    let hash = *Hasher::<224>::hash_tagged(bytes, language.tag());
    Some((language, Some(hash)))
}

/// The fields one u5c version maps for a transaction message, with the given
/// fee and Dijkstra parameter keys.
macro_rules! mapped {
    ($version:ident, $tx:expr, $fee:expr, $keys:expr) => {{
        use pallas::interop::utxorpc::$version::spec::cardano::{
            governance_action::GovernanceAction, script::Script,
        };
        let tx = $tx;
        let key = |s: &Option<Script>| match s {
            None => None,
            Some(Script::Native(_)) => Some((ScriptLanguage::Native, None)),
            Some(Script::PlutusV1(b)) => plutus(ScriptLanguage::PlutusV1, b),
            Some(Script::PlutusV2(b)) => plutus(ScriptLanguage::PlutusV2, b),
            Some(Script::PlutusV3(b)) => plutus(ScriptLanguage::PlutusV3, b),
            Some(Script::PlutusV4(b)) => plutus(ScriptLanguage::PlutusV4, b),
        };
        let witnessed = tx.witnesses.iter().flat_map(|w| &w.script);
        let scripts = witnessed.map(|s| key(&s.script));
        let aux_scripts = tx.auxiliary.iter().flat_map(|a| &a.scripts);
        let aux_scripts = aux_scripts.map(|s| key(&s.script));
        let quorum = tx.proposals.iter().map(|p| {
            let action = p.gov_action.as_ref();
            match action.and_then(|a| a.governance_action.as_ref()) {
                Some(GovernanceAction::UpdateCommitteeAction(a)) => {
                    let threshold = a.new_committee_threshold.as_ref();
                    Quorum::Update(threshold.map(|r| (r.numerator, r.denominator)))
                }
                _ => Quorum::Other,
            }
        });
        MappedTx {
            fee: $fee,
            scripts: scripts.sorted().collect(),
            aux_scripts: aux_scripts.sorted().collect(),
            quorum: quorum.collect(),
            dijkstra_keys: $keys,
        }
    }};
}

/// What a ReadTx version answers for a transaction hash.
enum Answer<T> {
    /// The mapped transaction.
    Mapped(T),
    /// An answer that holds no transaction where the ledger names one.
    Absent,
    /// The status the version answers in place of a transaction.
    Status(tonic::Code),
    /// The message of a panic in the version's mapping.
    Panicked(String),
}

impl<T> Answer<T> {
    /// The answer for a part of the mapped transaction, or this answer's
    /// absence, status or panic.
    fn then<U>(&self, part: impl FnOnce(&T) -> Answer<U>) -> Answer<U> {
        match self {
            Answer::Mapped(tx) => part(tx),
            Answer::Absent => Answer::Absent,
            Answer::Status(code) => Answer::Status(*code),
            Answer::Panicked(message) => Answer::Panicked(message.clone()),
        }
    }
}

/// The ReadTx answer of one u5c version for a transaction hash against the
/// domain, with a panic of its mapping caught.
macro_rules! read_tx {
    ($version:ident, $domain:expr, $hash:expr) => {{
        use pallas::interop::utxorpc::$version::spec::query::{
            any_chain_tx::Chain, query_service_server::QueryService as _, ReadTxRequest,
        };
        let request = ReadTxRequest {
            hash: $hash.to_vec().into(),
            ..Default::default()
        };
        let service = crate::serve::grpc::$version::query::QueryServiceImpl::new($domain.clone());
        let answer = AssertUnwindSafe(service.read_tx(Request::new(request)))
            .catch_unwind()
            .await;
        match answer {
            Err(payload) => Answer::Panicked(panic_message(payload).replace('\n', " ")),
            Ok(Err(status)) => Answer::Status(status.code()),
            Ok(Ok(answer)) => match answer.into_inner().tx.and_then(|tx| tx.chain) {
                Some(Chain::Cardano(tx)) => Answer::Mapped(tx),
                None => Answer::Absent,
            },
        }
    }};
}

/// A field the judge compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Fee,
    Scripts,
    AuxScripts,
    Quorum,
    DijkstraKeys,
}

impl fmt::Display for Field {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Field::Fee => "fee",
            Field::Scripts => "scripts",
            Field::AuxScripts => "aux_scripts",
            Field::Quorum => "quorum",
            Field::DijkstraKeys => "dijkstra_keys",
        })
    }
}

/// The Dijkstra parameter keys of each proposal as a line shows them, other for
/// a proposal that changes no parameter.
fn show_keys(keys: &[DijkstraKeys]) -> String {
    let mut shown = keys.iter().map(|k| match k {
        None => "other".to_string(),
        Some(k) if k.is_empty() => "none".to_string(),
        Some(k) => k.join("/"),
    });
    shown.join(",")
}

/// The scripts as a line shows them, each by its language and the first four
/// bytes of its hash.
fn show_scripts(scripts: &[ScriptKey]) -> String {
    let mut shown = scripts.iter().map(|s| match s {
        None => "empty".to_string(),
        Some((language, None)) => format!("{language:?}"),
        Some((language, Some(hash))) => format!("{language:?}:{}", hex::encode(&hash[..4])),
    });
    shown.join(",")
}

/// Each field the mapping holds a different value in than the ledger, with the
/// two values of each.
fn differing(ledger: &LedgerTx, mapped: &MappedTx) -> Vec<(Field, String, String)> {
    let mut differing = vec![];
    if let Some(fee) = &mapped.fee {
        let holds = match (ledger.fee, fee) {
            (None, WireFee::Unset) => true,
            (Some(n), WireFee::Int(m)) => i64::try_from(n) == Ok(*m),
            _ => false,
        };
        if !holds {
            differing.push((Field::Fee, format!("{:?}", ledger.fee), format!("{fee:?}")));
        }
    }
    if ledger.scripts != mapped.scripts {
        let (l, m) = (show_scripts(&ledger.scripts), show_scripts(&mapped.scripts));
        differing.push((Field::Scripts, l, m));
    }
    if ledger.aux_scripts != mapped.aux_scripts {
        let (l, m) = (&ledger.aux_scripts, &mapped.aux_scripts);
        differing.push((Field::AuxScripts, show_scripts(l), show_scripts(m)));
    }
    let (l, m) = (&ledger.quorum, &mapped.quorum);
    if l.len() != m.len() || l.iter().zip(m).any(|(l, m)| !l.holds(*m)) {
        differing.push((Field::Quorum, l.iter().join(","), m.iter().join(",")));
    }
    if let Some(keys) = &mapped.dijkstra_keys {
        if &ledger.dijkstra_keys != keys {
            let (l, m) = (show_keys(&ledger.dijkstra_keys), show_keys(keys));
            differing.push((Field::DijkstraKeys, l, m));
        }
    }
    differing
}

/// Adds the comparison of what one version answers for one transaction the
/// ledger accepted.
fn add(
    tally: &mut ByEntry,
    id: RecordId,
    layer: Version,
    name: TxName,
    ledger: &LedgerTx,
    answer: Answer<MappedTx>,
) {
    let answer_cell = |member: &str| CellKey::new(Axis::Answer, member);
    let (agrees, dolos, mut cells) = match answer {
        Answer::Mapped(mapped) => {
            let differing = differing(ledger, &mapped);
            let fields = differing.iter().map(|(f, _, _)| f).join(",");
            let mut ledger = differing.iter().map(|(f, l, _)| format!("{f}={l}"));
            let ledger = ledger.join(" ");
            let mut dolos = differing.iter().map(|(f, _, d)| format!("{f}={d}"));
            let dolos = dolos.join(" ");
            let shown = format!("differing={fields} ledger=[{ledger}] dolos=[{dolos}]");
            let mut cells = vec![answer_cell("differ")];
            cells.extend(
                differing
                    .iter()
                    .map(|(f, _, _)| CellKey::new(Axis::Field, f)),
            );
            (differing.is_empty(), shown, cells)
        }
        Answer::Absent => (false, "dolos=none".to_string(), vec![answer_cell("none")]),
        Answer::Status(code) => (
            false,
            format!("status={code:?}"),
            vec![answer_cell("status")],
        ),
        Answer::Panicked(message) => (
            false,
            format!("panicked={message}"),
            vec![answer_cell("panicked")],
        ),
    };
    cells.insert(0, CellKey::new(Axis::Layer, layer));
    cells.insert(1, CellKey::new(Axis::Tx, name.kind()));
    let tally = tally.at(EntryPoint::Route(Route::ReadTx, layer));
    tally.add(agrees, || {
        Finding::new(id, cells, format!("{layer} {name} {dolos}"))
    });
}

/// Evidence that ReadTx at every version mapped each accepted transaction of a
/// run without a panic, which the passes that serve the run through every
/// version take.
pub(super) struct EveryVersionMaps(());

/// Counts what each ReadTx version maps for the accepted records of the blocks
/// a run rolled against the ledger's transactions, v1 by each top level hash
/// with its sub transactions and v1beta and v1alpha by each top level and sub
/// transaction hash, with the evidence that every version mapped them when no
/// mapping panicked.
pub(super) async fn compare(
    domain: &DomainAdapter,
    kind: CorpusKind,
    blocks: &[GroupBlock<'_>],
    tally: &mut ByEntry,
) -> Option<EveryVersionMaps> {
    let mut maps = Some(EveryVersionMaps(()));
    let groups = blocks.iter().filter_map(GroupBlock::rolled);
    let accepted = groups
        .flat_map(|rolled| rolled.group)
        .filter(|r| matches!(r.outcome, LedgerOutcome::Accepted { .. }));
    for record in accepted {
        let id = RecordId {
            corpus: kind,
            n: record.n,
        };
        let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &record.tx.0)
            .unwrap_or_else(|e| panic!("TX {id} {e}"));
        let top = *tx.hash();
        let txs = named_txs(tx);
        let named: Vec<(TxName, LedgerTx)> = txs
            .iter()
            .map(|(name, t)| (*name, LedgerTx::of(t)))
            .collect();
        let served = read_tx!(v1, domain, top);
        if let Answer::Panicked(_) = served {
            maps = None;
        }
        for (name, ledger) in &named {
            let common = served.then(|tx| match name {
                TxName::Top(_) => {
                    let fee = wire_fee!(v1, tx.fee.clone());
                    match &tx.common {
                        Some(common) => {
                            let keys = Some(v1_dijkstra_keys(common));
                            Answer::Mapped(mapped!(v1, common, Some(fee), keys))
                        }
                        None => Answer::Absent,
                    }
                }
                TxName::Sub(hash) => {
                    let sub = tx.sub_transactions.iter().find(|c| c.hash == hash[..]);
                    match sub {
                        Some(common) => {
                            let keys = Some(v1_dijkstra_keys(common));
                            Answer::Mapped(mapped!(v1, common, None, keys))
                        }
                        None => Answer::Absent,
                    }
                }
            });
            add(tally, id, Version::V1, *name, ledger, common);
        }
        for &(name, ref ledger) in &named {
            let hash = match name {
                TxName::Top(h) | TxName::Sub(h) => h,
            };
            let served = read_tx!(v1beta, domain, hash);
            if let Answer::Panicked(_) = served {
                maps = None;
            }
            let answer = served.then(|tx| {
                let fee = wire_fee!(v1beta, tx.fee.clone());
                Answer::Mapped(mapped!(v1beta, tx, Some(fee), None))
            });
            add(tally, id, Version::V1beta, name, ledger, answer);
            let served = read_tx!(v1alpha, domain, hash);
            if let Answer::Panicked(_) = served {
                maps = None;
            }
            let answer = served.then(|tx| {
                let fee = wire_fee!(v1alpha, tx.fee.clone());
                Answer::Mapped(mapped!(v1alpha, tx, Some(fee), None))
            });
            add(tally, id, Version::V1alpha, name, ledger, answer);
        }
    }
    maps
}

#[test]
fn a_threshold_the_fields_round_holds_and_a_truncated_one_does_not() {
    let third: LedgerQuorum = Quorum::Update((1, 3));
    assert!(third.holds(Quorum::Update(Some((715_827_882, 2_147_483_647)))));
    assert!(Quorum::Update((5, 0)).holds(Quorum::Update(Some((1, 0)))));
    assert!(LedgerQuorum::Other.holds(Quorum::Other));
    let (n, d): (u64, u64) = (1_435_934_570_883, 2_500_000_000_000);
    let truncated = Quorum::Update(Some((n as i32, d as u32)));
    assert!(!Quorum::Update((n, d)).holds(truncated));
    assert!(!third.holds(Quorum::Update(None)));
    assert!(!third.holds(Quorum::Other));
}

#[test]
fn the_widest_threshold_each_side_holds_compares_without_overflow() {
    let one: LedgerQuorum = Quorum::Update((u64::MAX, u64::MAX));
    assert!(one.holds(Quorum::Update(Some((i32::MAX, i32::MAX as u32)))));
    assert!(!one.holds(Quorum::Update(Some((i32::MAX, u32::MAX)))));
    assert!(!one.holds(Quorum::Update(Some((i32::MIN, u32::MAX)))));
    let most: LedgerQuorum = Quorum::Update((u64::MAX, 1));
    assert!(!most.holds(Quorum::Update(Some((i32::MIN, 1)))));
}

fn ledger(fee: Option<u64>) -> LedgerTx {
    LedgerTx {
        fee,
        scripts: vec![
            Some((ScriptLanguage::Native, None)),
            Some((ScriptLanguage::PlutusV3, Some([3; 28]))),
        ],
        aux_scripts: vec![Some((ScriptLanguage::PlutusV3, Some([4; 28])))],
        quorum: vec![Quorum::Other, Quorum::Other],
        dijkstra_keys: vec![None, Some(vec!["peras_cert_boost"])],
    }
}

fn mapped(fee: Option<WireFee>) -> MappedTx {
    let ledger = ledger(None);
    MappedTx {
        fee,
        scripts: ledger.scripts,
        aux_scripts: ledger.aux_scripts,
        quorum: vec![Quorum::Other, Quorum::Other],
        dijkstra_keys: Some(ledger.dijkstra_keys),
    }
}

fn fields(ledger: &LedgerTx, mapped: &MappedTx) -> Vec<Field> {
    let differing = differing(ledger, mapped);
    differing.into_iter().map(|d| d.0).collect()
}

#[test]
fn a_fee_the_ledger_holds_none_of_differs_when_mapped_as_zero() {
    let none: Vec<Field> = vec![];
    let (unset, zero) = (mapped(Some(WireFee::Unset)), mapped(Some(WireFee::Int(0))));
    assert_eq!(fields(&ledger(None), &unset), none);
    let fee = Some(WireFee::Int(170_000));
    assert_eq!(fields(&ledger(Some(170_000)), &mapped(fee)), none);
    assert_eq!(fields(&ledger(Some(7)), &mapped(None)), none);
    assert_eq!(fields(&ledger(None), &zero), [Field::Fee]);
    assert_eq!(fields(&ledger(Some(7)), &unset), [Field::Fee]);
}

#[test]
fn a_dropped_witness_script_or_proposal_differs() {
    let ledger = ledger(None);
    let mut dropped = mapped(None);
    dropped.scripts.pop();
    assert_eq!(fields(&ledger, &dropped), [Field::Scripts]);
    let mut dropped = mapped(None);
    dropped.quorum.clear();
    assert_eq!(fields(&ledger, &dropped), [Field::Quorum]);
}

#[test]
fn a_dropped_auxiliary_script_or_dijkstra_key_differs() {
    let ledger = ledger(None);
    let mut dropped = mapped(None);
    dropped.aux_scripts.clear();
    assert_eq!(fields(&ledger, &dropped), [Field::AuxScripts]);
    let mut dropped = mapped(None);
    dropped.dijkstra_keys = Some(vec![None, Some(vec![])]);
    assert_eq!(fields(&ledger, &dropped), [Field::DijkstraKeys]);
    let mut unmapped = mapped(None);
    unmapped.dijkstra_keys = None;
    assert_eq!(fields(&ledger, &unmapped), Vec::<Field>::new());
}
