//! Judges what Dolos validates and serves against ledger corpora.

mod construct;
mod live;
mod output;
mod params;
mod seed;
mod serve;
mod validate;

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::atomic::{AtomicBool, Ordering},
};

use dolos_cardano::{
    indexes::{AsyncCardanoQueryExt as _, ScriptLanguage},
    model::{AccountState, DRepDelegation},
    FixedNamespace as _,
};
use dolos_core::{
    AsyncQueryFacade, BlockHash, ChainError, ChainPoint, Domain as _, EraCbor, MempoolStore as _,
    MempoolTx, RawBlock, StateStore as _, SubmitExt as _, SyncExt as _, TxHash, TxoRef,
};
use itertools::Itertools as _;
use ledger_oracle_judge::{
    accounts::{self, Account, Credential, LedgerAccounts},
    cbor::{tx_inputs, Purpose, TxIn},
    compare::{ByteVerdict, RollFail, StateTallies, Tally},
    corpus::{self, Corpus},
    cost::{self, CostSkip, CostTallies, NodeCosts},
    manifest::{self, Candidates, Declared, Feature, KnownList, Locus},
    record::{
        CorpusKind, Evaluation, LedgerOutcome, LedgerRedeemer, PParams, Phase2, Record, RecordId,
        Units, UtxoEntry,
    },
    row::Row,
    run::{InputOrigin, KeptRun},
    verdict::{
        declares, maker, one_answer, outcome, with_failing_makers, with_known, DolosVerdict, Fail,
        FailingMaker, Maker, Outcome, Pass, ValidationTally,
    },
};
use output::OutputField;
use pallas::{
    codec::{
        minicbor,
        utils::{KeepRaw, Nullable},
    },
    crypto::hash::Hasher,
    ledger::{
        primitives::{
            conway::{DatumOption, RedeemerTag},
            dijkstra,
        },
        traverse::{ComputeHash as _, Era, MultiEraOutput, MultiEraTx, OriginalHash as _},
        validate::phase2::{audited_builtins, tx::TxEvalResult, Builtin},
    },
};
use seed::SeedPoint;
use serve::{Event, Served, Service};
use tonic::{Request, Status};
use validate::Relay;

use super::{
    query::QueryServiceImpl,
    spec::{
        cardano,
        query::{self as q, query_service_server::QueryService as _},
    },
};
use crate::adapters::DomainAdapter;

fn load(kind: CorpusKind) -> Corpus {
    corpus::load(kind).unwrap_or_else(|e| panic!("{e}"))
}

fn epoch_size(corpus: &Corpus) -> u64 {
    corpus
        .globals
        .as_ref()
        .unwrap_or_else(|| panic!("CORPUS {} has no globals", corpus.kind.name()))
        .epoch_size
}

/// The datum bytes Dolos serves through ReadUtxos for each input it holds.
async fn read_datums<'a>(
    domain: &DomainAdapter,
    inputs: impl IntoIterator<Item = &'a TxIn>,
) -> BTreeMap<TxIn, Option<Vec<u8>>> {
    let items = read_utxo_data(domain, inputs).await.into_iter();
    items
        .map(|(input, item)| {
            let datum = match item.parsed_state {
                Some(q::any_utxo_data::ParsedState::Cardano(output)) => output.datum,
                None => None,
            };
            let bytes = datum.and_then(|d| d.original_cbor).map(|b| b.to_vec());
            (input, bytes)
        })
        .collect()
}

/// What Dolos serves through ReadUtxos for each input it holds.
async fn read_utxo_data<'a>(
    domain: &DomainAdapter,
    inputs: impl IntoIterator<Item = &'a TxIn>,
) -> BTreeMap<TxIn, q::AnyUtxoData> {
    let keys = inputs
        .into_iter()
        .map(|i| q::TxoRef {
            hash: i.tx.to_vec().into(),
            index: seed::txo_ref(i).1,
        })
        .collect();
    let items = QueryServiceImpl::new(domain.clone())
        .read_utxos(Request::new(q::ReadUtxosRequest {
            keys,
            field_mask: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .items;
    items
        .into_iter()
        .map(|item| {
            let r = item
                .txo_ref
                .as_ref()
                .expect("ReadUtxos names each output it serves");
            let input = TxIn {
                tx: r
                    .hash
                    .as_ref()
                    .try_into()
                    .expect("a served hash is 32 bytes"),
                ix: r.index.into(),
            };
            (input, item)
        })
        .collect()
}

fn parsed_differing(
    id: RecordId,
    ledger: &[u8],
    parsed: Option<&cardano::TxOutput>,
) -> Vec<OutputField> {
    match parsed {
        Some(parsed) => {
            output::differing(ledger, parsed).unwrap_or_else(|e| panic!("OUTPUT {id} {e}"))
        }
        None => vec![OutputField::ParsedState],
    }
}

/// Counts each input and ledger entry where Dolos serves other parsed fields than the ledger, or other bytes than `written` holds there or else the ledger.
async fn compare_utxos<'a>(
    domain: &DomainAdapter,
    id: RecordId,
    inputs: impl IntoIterator<Item = &'a TxIn>,
    expected: impl IntoIterator<Item = &'a UtxoEntry>,
    written: &BTreeMap<TxIn, Vec<u8>>,
    tally: &mut Tally,
) {
    let expected: BTreeMap<&TxIn, &[u8]> = expected
        .into_iter()
        .map(|e| (&e.input, e.output.0.as_slice()))
        .collect();
    let mut keys: BTreeSet<&TxIn> = inputs.into_iter().collect();
    keys.extend(expected.keys());
    let served = read_utxo_data(domain, keys.iter().copied()).await;
    for input in keys {
        let ledger = expected.get(input).copied();
        let bytes = ledger.map(|l| written.get(input).map_or(l, Vec::as_slice));
        let item = served.get(input);
        let Some(verdict) = ByteVerdict::of(bytes, item.map(|i| i.native_bytes.as_ref())) else {
            tally.add(true, String::new);
            continue;
        };
        let fields = match (ledger, item) {
            (Some(ledger), Some(item)) => {
                let parsed = item.parsed_state.as_ref();
                let parsed = parsed.map(|q::any_utxo_data::ParsedState::Cardano(output)| output);
                parsed_differing(id, ledger, parsed)
            }
            (None, _) | (_, None) => Vec::new(),
        };
        tally.add(verdict.agrees() && fields.is_empty(), || {
            let shown = show_input(input);
            match fields.is_empty() {
                true => format!("{id} input={shown} {verdict}"),
                false => format!(
                    "{id} input={shown} {verdict} fields={}",
                    fields.iter().join(",")
                ),
            }
        });
    }
}

fn show_input(input: &TxIn) -> String {
    format!("{}#{}", hex::encode(input.tx), input.ix)
}

/// The Cardano parameters Dolos serves through ReadParams.
async fn read_params(domain: &DomainAdapter) -> cardano::PParams {
    let values = QueryServiceImpl::new(domain.clone())
        .read_params(Request::new(q::ReadParamsRequest { field_mask: None }))
        .await
        .unwrap()
        .into_inner()
        .values
        .expect("ReadParams answers with values");
    match values.params {
        Some(q::any_chain_params::Params::Cardano(p)) => p,
        None => panic!("ReadParams answers with Cardano parameters"),
    }
}

/// The validation outcomes, state comparisons, cost comparisons, builtin and construct coverage, refusal inventory and manifest candidates of one corpus.
#[derive(Debug, Default)]
struct Judged {
    validation: ValidationTally,
    state: StateTallies,
    cost: CostTallies,
    coverage: Coverage,
    constructs: construct::Coverage,
    inventory: Inventory,
    candidates: Candidates,
}

/// The number of records each ledger failure leaf refuses that Dolos refused for the same class.
#[derive(Debug, Default)]
struct Inventory(BTreeMap<String, usize>);

impl Inventory {
    fn add(&mut self, record: &Record) {
        if let LedgerOutcome::Refused { failure_names, .. } = &record.outcome {
            let leaves: BTreeSet<&str> = failure_names.iter().map(|f| f.leaf()).collect();
            for leaf in leaves {
                *self.0.entry(leaf.to_string()).or_default() += 1;
            }
        }
    }

    fn extend(&mut self, other: Inventory) {
        for (leaf, n) in other.0 {
            *self.0.entry(leaf).or_default() += n;
        }
    }

    /// The line of each leaf and a count line.
    fn lines(&self) -> Vec<String> {
        let records: usize = self.0.values().sum();
        let mut lines: Vec<String> = self
            .0
            .iter()
            .map(|(leaf, n)| format!("INVENTORY {leaf} records={n}"))
            .collect();
        lines.push(format!(
            "INVENTORY leaves={} records={records}",
            self.0.len()
        ));
        lines
    }
}

/// The first record whose accepted, phase two valid evaluation called each builtin in the ledger's units.
#[derive(Debug, Default)]
struct Coverage(Vec<(Builtin, RecordId)>);

impl Coverage {
    fn add(&mut self, id: RecordId, calls: &[Builtin]) {
        for call in calls {
            if self.record(*call).is_none() {
                self.0.push((*call, id));
            }
        }
    }

    fn record(&self, builtin: Builtin) -> Option<RecordId> {
        self.0
            .iter()
            .find(|(b, _)| *b == builtin)
            .map(|(_, id)| *id)
    }

    fn extend(&mut self, other: Coverage) {
        for (builtin, id) in other.0 {
            self.add(id, &[builtin]);
        }
    }
}

/// The audited builtins whose coverage decides the run, the rest of the audited set being reported only.
const ADDED_BUILTINS: [Builtin; 7] = [
    Builtin::QuotientInteger,
    Builtin::SliceByteString,
    Builtin::IndexByteString,
    Builtin::ChooseUnit,
    Builtin::Trace,
    Builtin::NullList,
    Builtin::DropList,
];

/// The coverage line of each audited builtin, a count line, and whether every added builtin has a covering record.
fn builtin_bar(audited: &[Builtin], added: &[Builtin], coverage: &Coverage) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    let mut counts = [[0usize; 2]; 2];
    for builtin in audited {
        let in_base = !added.contains(builtin);
        let record = coverage.record(*builtin);
        lines.push(match (record, in_base) {
            (Some(id), _) => format!("BUILTIN {builtin:?} record={id}"),
            (None, true) => format!("BUILTIN {builtin:?} uncovered base"),
            (None, false) => format!("BUILTIN_UNCOVERED {builtin:?}"),
        });
        let tally = &mut counts[usize::from(in_base)];
        tally[0] += 1;
        tally[1] += usize::from(record.is_some());
    }
    let [[added, added_covered], [base, base_covered]] = counts;
    let pass = added_covered == added;
    lines.push(format!(
        "BUILTIN added covered={added_covered}/{added} base covered={base_covered}/{base} pass={pass}"
    ));
    (lines, pass)
}

impl Judged {
    fn pass(&self) -> bool {
        self.validation.pass() && self.state.pass()
    }
}

impl fmt::Display for Judged {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\n{}\n{}", self.validation, self.state, self.cost)
    }
}

/// The redeemer tag number the ledger writes in a purpose.
fn tag_number(tag: &RedeemerTag) -> u64 {
    match tag {
        RedeemerTag::Spend => 0,
        RedeemerTag::Mint => 1,
        RedeemerTag::Cert => 2,
        RedeemerTag::Reward => 3,
        RedeemerTag::Vote => 4,
        RedeemerTag::Propose => 5,
    }
}

/// The purpose of one evaluated redeemer in the ledger's terms.
fn purpose(r: &TxEvalResult) -> Purpose {
    Purpose {
        tag: tag_number(&r.tag),
        index: r.index.into(),
    }
}

/// The evaluations Dolos estimates for one transaction, each in the ledger's terms.
fn node_costs(estimate: &Result<Vec<(TxEvalResult, Vec<Builtin>)>, ChainError>) -> NodeCosts {
    let results = match estimate {
        Ok(results) => results,
        Err(e) => return NodeCosts::Unevaluated(e.to_string()),
    };
    let evaluated = results.iter().map(|(r, _)| LedgerRedeemer {
        purpose: purpose(r),
        evaluation: match r.success {
            true => Evaluation::Ran(Units {
                mem: r.units.mem,
                steps: r.units.steps,
            }),
            false => Evaluation::Failed(r.failure_message.clone().unwrap_or_default()),
        },
    });
    NodeCosts::Evaluated(evaluated.collect())
}

/// Counts the minimum fee Dolos computes for one record against the ledger's, and the redeemer evaluations of each level and the builtins each redeemer both ran alike in a phase two valid record, when the ledger accepted the record and no row declares it, each only when Dolos took every maker of the record.
fn compare_cost(
    group: &validate::Group,
    id: RecordId,
    record: &Record,
    declared: Option<&Feature>,
    cascade: bool,
    cost: &mut CostTallies,
    coverage: &mut Coverage,
) {
    if cascade {
        cost.skip(CostSkip::Cascade);
        return;
    }
    let fee = match group.min_fee(&record.tx.0) {
        Ok(Some(fee)) => Ok(fee),
        Ok(None) => Err("outside Dijkstra".to_string()),
        Err(e) => Err(e.to_string()),
    };
    cost.add_fee(id, record.min_fee, fee, &record.outcome);
    let skip = match (&record.outcome, declared) {
        (LedgerOutcome::Refused { .. }, _) => Some(CostSkip::Refused),
        (LedgerOutcome::Accepted { .. }, Some(_)) => Some(CostSkip::Declared),
        (LedgerOutcome::Accepted { .. }, None) => None,
    };
    if let Some(why) = skip {
        cost.skip(why);
        return;
    }
    let sub_units = record.sub_ex_units.iter().any(|sub| !sub.is_empty());
    if record.ex_units.is_empty() && !sub_units {
        return;
    }
    let estimate = group.estimate(&record.tx.0);
    let costs = cost::pair(&record.ex_units, &node_costs(&estimate));
    cost.add_units(id, &costs);
    if let (Ok(results), Phase2::Valid) = (&estimate, record.phase2_valid) {
        for alike in costs.iter().filter(|c| c.ran_alike().is_some()) {
            let calls = results.iter().filter(|(r, _)| purpose(r) == alike.purpose);
            for (_, calls) in calls {
                coverage.add(id, calls);
            }
        }
    }
    let unestimated = NodeCosts::Unevaluated(match &estimate {
        Ok(_) => "sub transaction redeemers are not estimated".to_string(),
        Err(e) => e.to_string(),
    });
    for (k, sub) in record.sub_ex_units.iter().enumerate() {
        cost.add_units(format!("{id} sub#{k}"), &cost::pair(sub, &unestimated));
    }
}

/// The declared support and the known list a judgement reads.
#[derive(Debug, Clone)]
struct Manifests {
    declared: Declared,
    known: KnownList,
}

impl Manifests {
    fn committed() -> Self {
        Manifests {
            declared: manifest::declared().unwrap_or_else(|e| panic!("{e}")),
            known: manifest::known().unwrap_or_else(|e| panic!("{e}")).records,
        }
    }

    /// Fails each declared or validation known entry for the corpus that names a record the judgement did not reach.
    fn unjudged(&self, kind: CorpusKind, judged: &BTreeSet<RecordId>, tally: &mut ValidationTally) {
        let missed = |id: &RecordId| id.corpus == kind && !judged.contains(id);
        for (id, feature) in self.declared.iter().filter(|(id, _)| missed(id)) {
            let fail = Fail::StaleManifest {
                declared: feature.to_string(),
                dolos: "not_judged".to_string(),
            };
            tally.add(*id, Outcome::Fail(fail));
        }
        let validation = |locus: &Locus| *locus == Locus::Validation;
        let entries = self.known.iter();
        for ((_, id), row) in entries.filter(|((locus, id), _)| validation(locus) && missed(id)) {
            let fail = Fail::KnownStale { row: *row };
            tally.add(*id, Outcome::Fail(fail));
        }
    }
}

/// Every input a record names, at the top level and in its sub transactions.
fn named_inputs(id: RecordId, record: &Record) -> BTreeSet<TxIn> {
    let inputs = tx_inputs(&record.tx.0).unwrap_or_else(|e| panic!("TX {id} {e}"));
    let subs = inputs
        .subs
        .iter()
        .flat_map(|s| s.spend.iter().chain(&s.reference));
    inputs
        .spend
        .iter()
        .chain(&inputs.collateral)
        .chain(&inputs.reference)
        .chain(subs)
        .copied()
        .collect()
}

/// Every output reference a record's transaction and sub transactions could produce, each output index and the index after them that a collateral return takes.
fn producible(id: RecordId, record: &Record) -> Vec<TxIn> {
    let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &record.tx.0)
        .unwrap_or_else(|e| panic!("TX {id} {e}"));
    let subs = tx.sub_transactions();
    let txs = std::iter::once(&tx).chain(&subs);
    let refs = txs.flat_map(|t| {
        let hash = *t.hash();
        (0..=t.outputs().len() as u64).map(move |ix| TxIn { tx: hash, ix })
    });
    refs.collect()
}

/// The bytes of each output a record's transaction and sub transactions write, its collateral return at the index after its outputs, none when the transaction does not decode.
fn written(record: &Record) -> BTreeMap<TxIn, Vec<u8>> {
    let Ok(tx) = MultiEraTx::decode_for_era(Era::Dijkstra, &record.tx.0) else {
        return BTreeMap::new();
    };
    let subs = tx.sub_transactions();
    let txs = std::iter::once(&tx).chain(&subs);
    let outputs = txs.flat_map(|t| {
        let hash = *t.hash();
        let outputs = t.outputs().into_iter().chain(t.collateral_return());
        let outputs = outputs.enumerate();
        outputs.map(move |(ix, output)| {
            (
                TxIn {
                    tx: hash,
                    ix: ix as u64,
                },
                output.encode(),
            )
        })
    });
    outputs.collect()
}

/// The inputs of a record that Dolos misses because a maker in its group was not taken, by what that maker gives them.
struct Missing<'r> {
    seeded: Vec<&'r UtxoEntry>,
    seeded_by: BTreeSet<RecordId>,
    failed_by: BTreeSet<FailingMaker>,
}

/// The inputs of a record that Dolos misses because a maker in its group was not taken.
fn missing_from_makers<'r>(
    group: &validate::Group,
    run: &'r KeptRun,
    record: &Record,
    inputs: &BTreeSet<TxIn>,
    makers: &BTreeMap<TxIn, RecordId>,
    records: &[Record],
    given: &BTreeMap<RecordId, Maker>,
) -> Missing<'r> {
    let mut missing = Missing {
        seeded: Vec::new(),
        seeded_by: BTreeSet::new(),
        failed_by: BTreeSet::new(),
    };
    for input in inputs {
        let maker = match run.origin(input, record, makers, records) {
            InputOrigin::InGroup { made_by } => made_by,
            InputOrigin::Seeded | InputOrigin::InRun { .. } | InputOrigin::AbsentFromPre => {
                continue
            }
        };
        if group.sees(seed::txo_ref(input)) {
            continue;
        }
        match given.get(&maker) {
            Some(Maker::Seeds(_)) => {
                let entry = run.pre_entry(record, input);
                missing
                    .seeded
                    .push(entry.expect("an in group input is in the pre state"));
                missing.seeded_by.insert(maker);
            }
            Some(Maker::Fails(failing)) => {
                missing.failed_by.insert(*failing);
            }
            Some(Maker::Taken) | None => {}
        }
    }
    missing
}

/// A block a run rolled, with the records it holds, their slot group, and its first record when Dolos accepted that record on submission.
struct Rolled<'r> {
    point: ChainPoint,
    group: &'r [Record],
    records: Vec<&'r Record>,
    requeued: Option<&'r Record>,
}

/// What became of the block of one slot group, unmade when the group holds no accepted record.
enum GroupBlock<'r> {
    Unmade(&'r [Record]),
    Rolled(Rolled<'r>),
    Failed,
}

impl<'r> GroupBlock<'r> {
    fn rolled(&self) -> Option<&Rolled<'r>> {
        match self {
            GroupBlock::Rolled(rolled) => Some(rolled),
            GroupBlock::Unmade(_) | GroupBlock::Failed => None,
        }
    }
}

/// Submits each slot group of a run in order to a fresh mempool at the group slot, once holding accepted records pending and once marking them inflight, then rolls the block of its accepted records, compares the outputs and returns what became of each group's block with the account comparisons of each group's last accepted record.
async fn validate_run<'r>(
    base: &DomainAdapter,
    kind: CorpusKind,
    run: &'r KeptRun,
    manifests: &Manifests,
    edits: &Edits<'_>,
    judged: &mut BTreeSet<RecordId>,
    tallies: &mut Judged,
) -> (Vec<GroupBlock<'r>>, BTreeMap<RecordId, Tally>) {
    let Judged {
        validation: tally,
        state,
        cost,
        coverage,
        constructs,
        inventory,
        candidates,
    } = tallies;
    let id_of = |record: &Record| RecordId {
        corpus: kind,
        n: record.n,
    };
    let makers = run.makers(kind);
    let by_n: BTreeMap<u32, &Record> = run.records().iter().map(|r| (r.n, r)).collect();
    let mut tip = None;
    let mut blocks = Vec::new();
    let mut account_checks = BTreeMap::new();
    for records in run.groups() {
        let group = validate::group(base, records[0].slot, Relay::Held);
        let relayed = validate::group(base, records[0].slot, Relay::Inflight);
        let mut given = BTreeMap::new();
        let mut dolos_accepted = BTreeSet::new();
        for record in records {
            let id = id_of(record);
            let inputs = named_inputs(id, record);
            let missing =
                missing_from_makers(&group, run, record, &inputs, &makers, records, &given);
            if !missing.seeded.is_empty() {
                group.seed(&missing.seeded);
                tally.dependent.push((id, missing.seeded_by));
            }
            let declared = manifests.declared.get(&id);
            let cascade = !missing.failed_by.is_empty();
            compare_cost(&group, id, record, declared, cascade, cost, coverage);
            let dolos = validate::verdict(&group.receive(&record.tx.0));
            let inflight = validate::verdict(&relayed.receive(&record.tx.0));
            if matches!(dolos, DolosVerdict::Accepted) {
                dolos_accepted.insert(record.n);
            }
            if let Some(feature) = declares(&record.outcome, &dolos) {
                candidates.declared.insert(id, feature);
            }
            let judgement = match one_answer(&dolos, &inflight) {
                Ok(dolos) => outcome(&record.outcome, record.phase2_valid, dolos, declared),
                Err(fail) => Outcome::Fail(fail),
            };
            let judgement = with_failing_makers(judgement, &dolos, missing.failed_by);
            let known = manifests.known.get(&(Locus::Validation, id));
            let judgement = with_known(judgement, known);
            let answer = match &judgement {
                Outcome::Pass(Pass::Agree) => Some(construct::Answer::Agree),
                Outcome::Pass(Pass::UnsupportedExpected { label }) => {
                    let label = label.clone();
                    Some(construct::Answer::Declared { label })
                }
                _ => None,
            };
            match (&record.outcome, answer) {
                (LedgerOutcome::Accepted { .. }, Some(answer)) => {
                    let held =
                        construct::of_tx(&record.tx.0).unwrap_or_else(|e| panic!("TX {id} {e}"));
                    constructs.add(id, answer, held);
                }
                (LedgerOutcome::Refused { .. }, Some(construct::Answer::Agree)) => {
                    inventory.add(record)
                }
                _ => {}
            }
            if let Outcome::Pass(Pass::Known { row, .. }) = &judgement {
                let entry = (Locus::Validation, id);
                candidates.known.records.insert(entry, *row);
            }
            given.insert(id, maker(id, &dolos, &judgement));
            tally.add(id, judgement);
            judged.insert(id);
        }
        let accepted = records.iter().filter_map(|r| match &r.outcome {
            LedgerOutcome::Accepted { accounts_post, .. } => Some((r, accounts_post)),
            LedgerOutcome::Refused { .. } => None,
        });
        let Some((last, accounts_post)) = accepted.clone().next_back() else {
            blocks.push(GroupBlock::Unmade(records));
            continue;
        };
        let included: Vec<(&Record, Phase2)> = accepted
            .filter_map(|(r, ..)| Some((r, (edits.block_tx)(id_of(r), r.phase2_valid)?)))
            .collect();
        let txs: Vec<_> = included
            .iter()
            .map(|(r, phase2)| (r.tx.0.as_slice(), *phase2))
            .collect();
        let rolled = match validate::block(last.slot, tip, &txs) {
            Ok((next, block)) => group
                .roll((edits.rolled_block)(id_of(last), block))
                .map(|_| next)
                .map_err(|e| e.to_string()),
            Err(e) => Err(format!("decode {e}")),
        };
        let next = match rolled {
            Ok(next) => next,
            Err(cause) => {
                state.roll_fail.push(RollFail {
                    first: id_of(&records[0]),
                    last: id_of(&records[records.len() - 1]),
                    records: records.len(),
                    slot: last.slot,
                    cause,
                });
                blocks.push(GroupBlock::Failed);
                // A failed block leaves the tip where it was, so the next block names that parent.
                continue;
            }
        };
        tip = Some(next);
        let requeued = included
            .first()
            .map(|(r, _)| *r)
            .filter(|r| dolos_accepted.contains(&r.n));
        blocks.push(GroupBlock::Rolled(Rolled {
            point: ChainPoint::Specific(last.slot, next.hash()),
            group: records,
            records: included.into_iter().map(|(r, _)| r).collect(),
            requeued,
        }));
        let producible: Vec<TxIn> = records
            .iter()
            .flat_map(|r| producible(id_of(r), r))
            .collect();
        let inputs = run.held_in(records).chain(&producible);
        let wrote = records.iter().flat_map(written).collect();
        compare_utxos(
            group.domain(),
            id_of(last),
            inputs,
            run.post(last),
            &wrote,
            &mut state.utxo,
        )
        .await;
        let made = run
            .post(last)
            .filter_map(|e| Some((e, by_n[&makers.get(&e.input)?.n])));
        let made: Vec<(&UtxoEntry, &Record)> = made.collect();
        compare_datums(group.domain(), id_of(last), &made, edits, &mut state.datum).await;
        let served = dolos_accounts(group.domain());
        let checks = account_checks.entry(id_of(last)).or_default();
        accounts::compare(accounts_post, &served, checks);
    }
    (blocks, account_checks)
}

/// The datum ReadUtxos serves for an output with a datum hash, by whether the transaction that made the output witnesses that datum.
enum ExpectedDatum {
    Witnessed(Vec<u8>),
    Unwitnessed,
}

/// Counts the datum ReadUtxos serves for each ledger output with a datum hash against the datum its making transaction witnesses, or against the hash when it witnesses none.
async fn compare_datums(
    domain: &DomainAdapter,
    id: RecordId,
    made: &[(&UtxoEntry, &Record)],
    edits: &Edits<'_>,
    tally: &mut Tally,
) {
    let mut expected = BTreeMap::new();
    for (entry, maker) in made {
        let maker_id = RecordId {
            corpus: id.corpus,
            n: maker.n,
        };
        let output = MultiEraOutput::decode(Era::Dijkstra, &entry.output.0)
            .unwrap_or_else(|e| panic!("OUTPUT {maker_id} {e}"));
        let Some(DatumOption::Hash(hash)) = output.datum() else {
            continue;
        };
        let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &maker.tx.0)
            .unwrap_or_else(|e| panic!("TX {maker_id} {e}"));
        let subs = tx.sub_transactions();
        let making = std::iter::once(&tx)
            .chain(&subs)
            .find(|t| *t.hash() == entry.input.tx);
        let datum = making
            .into_iter()
            .flat_map(|t| t.plutus_data())
            .find(|d| d.original_hash() == hash);
        let ledger = match datum {
            Some(d) => {
                ExpectedDatum::Witnessed((edits.expected_datum)(maker_id, d.raw_cbor().to_vec()))
            }
            None => ExpectedDatum::Unwitnessed,
        };
        expected.insert(entry.input, (maker_id, *hash, ledger));
    }
    let served = read_datums(domain, expected.keys()).await;
    for (input, (maker, hash, ledger)) in &expected {
        let served = served.get(input).cloned().flatten();
        let (agrees, dolos) = match ledger {
            ExpectedDatum::Witnessed(bytes) => {
                let verdict = ByteVerdict::against(bytes, served.as_deref());
                (verdict.agrees(), verdict.to_string())
            }
            ExpectedDatum::Unwitnessed => match served {
                None => (true, "unwitnessed dolos=none".to_string()),
                Some(bytes) => {
                    let served_hash = *Hasher::<256>::hash(&bytes);
                    let dolos = format!("unwitnessed dolos_hash={}", hex::encode(served_hash));
                    (served_hash == *hash, dolos)
                }
            },
        };
        tally.add(agrees, || {
            format!(
                "{id} input={} made_by={maker} datum={} {dolos}",
                show_input(input),
                hex::encode(hash)
            )
        });
    }
}

/// The registered accounts Dolos holds, in the fields the ledger also holds.
fn dolos_accounts(domain: &DomainAdapter) -> BTreeMap<Credential, Account> {
    let entities = domain
        .state()
        .iter_entities_typed::<AccountState>(AccountState::NS, None)
        .unwrap();
    entities
        .map(|entity| entity.unwrap().1)
        .filter(AccountState::is_registered)
        .map(|account| {
            let drep = match account.drep.live() {
                Some(DRepDelegation::Delegated(drep)) => Some(seed::drep(drep)),
                Some(DRepDelegation::NotDelegated) | None => None,
            };
            let served = Account {
                balance: account.stake.live().map_or(0, |s| s.withdrawable()),
                pool: account.delegated_pool_live().map(|pool| **pool),
                drep,
            };
            (seed::credential(&account.credential), served)
        })
        .collect()
}

/// The block and transaction bytes Dolos serves through ReadTx for a transaction hash.
struct ServedTx {
    block: BlockAt,
    bytes: Vec<u8>,
}

/// The slot and hash of a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockAt {
    slot: u64,
    hash: [u8; 32],
}

impl fmt::Display for BlockAt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "slot={},block={}",
            self.slot,
            hex::encode(&self.hash[..4])
        )
    }
}

/// What Dolos serves through ReadTx for a transaction hash, nothing when it answers not found, or the status it answers with otherwise.
async fn read_tx(domain: &DomainAdapter, hash: &[u8; 32]) -> Result<Option<ServedTx>, Status> {
    let request = q::ReadTxRequest {
        hash: hash.to_vec().into(),
        ..Default::default()
    };
    let answer = QueryServiceImpl::new(domain.clone())
        .read_tx(Request::new(request))
        .await;
    let tx = match answer {
        Ok(answer) => answer
            .into_inner()
            .tx
            .expect("ReadTx answers with a transaction"),
        Err(status) if status.code() == tonic::Code::NotFound => return Ok(None),
        Err(status) => return Err(status),
    };
    let block = tx.block_ref.expect("ReadTx names the block");
    let hash = block
        .hash
        .as_ref()
        .try_into()
        .expect("a served block hash is 32 bytes");
    Ok(Some(ServedTx {
        block: BlockAt {
            slot: block.slot,
            hash,
        },
        bytes: tx.native_bytes.to_vec(),
    }))
}

/// The raw body, witness set and auxiliary data of a Dijkstra transaction, with whether the block producer marks it successful.
#[derive(Debug, PartialEq, Eq)]
struct TxParts {
    body: Vec<u8>,
    witness: Vec<u8>,
    auxiliary: Option<Vec<u8>>,
    success: bool,
}

fn raw_auxiliary(aux: &Nullable<KeepRaw<'_, dijkstra::AuxiliaryData>>) -> Option<Vec<u8>> {
    match aux {
        Nullable::Some(aux) => Some(aux.raw_cbor().to_vec()),
        Nullable::Null | Nullable::Undefined => None,
    }
}

impl TxParts {
    /// The parts of a ledger transaction, marked successful when the ledger ran its scripts to success.
    fn ledger(bytes: &[u8], phase2: Phase2) -> Result<Self, String> {
        let tx: dijkstra::MempoolTransaction =
            minicbor::decode(bytes).map_err(|e| e.to_string())?;
        Ok(TxParts {
            body: tx.transaction_body.raw_cbor().to_vec(),
            witness: tx.transaction_witness_set.raw_cbor().to_vec(),
            auxiliary: raw_auxiliary(&tx.auxiliary_data),
            success: phase2 == Phase2::Valid,
        })
    }

    /// The parts of a transaction in the four element form of a block.
    fn block(bytes: &[u8]) -> Result<Self, String> {
        let tx: dijkstra::BlockTransaction = minicbor::decode(bytes).map_err(|e| e.to_string())?;
        Ok(TxParts {
            body: tx.transaction_body.raw_cbor().to_vec(),
            witness: tx.transaction_witness_set.raw_cbor().to_vec(),
            auxiliary: raw_auxiliary(&tx.auxiliary_data),
            success: tx.success,
        })
    }

    /// The parts that differ between two transactions.
    fn differing(&self, other: &TxParts) -> Vec<TxPart> {
        let parts = [
            (TxPart::Body, self.body == other.body),
            (TxPart::Witness, self.witness == other.witness),
            (TxPart::Auxiliary, self.auxiliary == other.auxiliary),
            (TxPart::Success, self.success == other.success),
        ];
        parts
            .into_iter()
            .filter(|(_, same)| !same)
            .map(|(name, _)| name)
            .collect()
    }
}

/// A part of a transaction compared between the ledger's records and what ReadTx serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TxPart {
    Body,
    Witness,
    Auxiliary,
    Success,
}

impl fmt::Display for TxPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TxPart::Body => "body",
            TxPart::Witness => "witness",
            TxPart::Auxiliary => "auxiliary",
            TxPart::Success => "success",
        })
    }
}

/// The block and transaction the ledger's records say ReadTx answers a hash with.
#[derive(Clone, Copy)]
struct ExpectedTx<'r> {
    slot: u64,
    block: Option<[u8; 32]>,
    tx: &'r [u8],
    phase2: Phase2,
}

/// A transaction hash the ledger's records name, top level or sub transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum TxName {
    Top([u8; 32]),
    Sub([u8; 32]),
}

impl fmt::Display for TxName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TxName::Top(h) => write!(f, "tx={}", hex::encode(h)),
            TxName::Sub(h) => write!(f, "sub_tx={}", hex::encode(h)),
        }
    }
}

/// Counts each transaction hash of the groups whose block rolled or was never made against what ReadTx serves for it, the block and transaction of the accepted record that names it or nothing when only refused records do.
async fn compare_read_tx(
    domain: &DomainAdapter,
    kind: CorpusKind,
    blocks: &[GroupBlock<'_>],
    tally: &mut Tally,
) {
    let mut expected: BTreeMap<TxName, (RecordId, Option<ExpectedTx>)> = BTreeMap::new();
    let groups = blocks.iter().filter_map(|block| match block {
        GroupBlock::Unmade(records) => Some((*records, None)),
        GroupBlock::Rolled(rolled) => Some((rolled.group, rolled.point.hash().map(|h| *h))),
        GroupBlock::Failed => None,
    });
    let records = groups.flat_map(|(records, block)| records.iter().map(move |r| (r, block)));
    for (record, block) in records {
        let id = RecordId {
            corpus: kind,
            n: record.n,
        };
        let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &record.tx.0)
            .unwrap_or_else(|e| panic!("TX {id} {e}"));
        let mut names = vec![TxName::Top(*tx.hash())];
        names.extend(
            tx.sub_transactions()
                .iter()
                .map(|sub| TxName::Sub(*sub.hash())),
        );
        let ledger = match &record.outcome {
            LedgerOutcome::Accepted { .. } => Some(ExpectedTx {
                slot: record.slot,
                block,
                tx: &record.tx.0,
                phase2: record.phase2_valid,
            }),
            LedgerOutcome::Refused { .. } => None,
        };
        for name in names {
            let entry = expected.entry(name).or_insert((id, None));
            if let Some(ledger) = ledger {
                *entry = (id, Some(ledger));
            }
        }
    }
    for (name, (id, ledger)) in expected {
        let hash = match name {
            TxName::Top(h) | TxName::Sub(h) => h,
        };
        let shown = match &ledger {
            Some(e) => match e.block {
                Some(hash) => BlockAt { slot: e.slot, hash }.to_string(),
                None => format!("slot={},block=none", e.slot),
            },
            None => "none".to_string(),
        };
        let (agrees, dolos) = match (&ledger, read_tx(domain, &hash).await) {
            (_, Err(status)) => (false, format!("status={:?}", status.code())),
            (None, Ok(None)) => (true, "none".to_string()),
            (Some(_), Ok(None)) => (false, "none".to_string()),
            (None, Ok(Some(served))) => (false, served.block.to_string()),
            (Some(e), Ok(Some(served))) => {
                let ledger =
                    TxParts::ledger(e.tx, e.phase2).unwrap_or_else(|err| panic!("TX {id} {err}"));
                let differing = match TxParts::block(&served.bytes) {
                    Ok(parts) => ledger.differing(&parts).iter().join(","),
                    Err(err) => format!("undecoded {err}"),
                };
                let block = served.block;
                let same_block = block.slot == e.slot && e.block == Some(block.hash);
                let dolos = match differing.is_empty() {
                    true => block.to_string(),
                    false => format!("{block} differing={differing}"),
                };
                (same_block && differing.is_empty(), dolos)
            }
        };
        tally.add(agrees, || {
            format!("{id} {name} ledger={shown} dolos={dolos}")
        });
    }
}

/// Where a script appears, in the witnesses of a transaction or on an output.
#[derive(Debug, Clone, Copy)]
enum ScriptAt {
    Witness(TxName),
    Output(TxIn),
}

impl fmt::Display for ScriptAt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScriptAt::Witness(name) => write!(f, "witness,{name}"),
            ScriptAt::Output(input) => write!(f, "output={}", show_input(input)),
        }
    }
}

/// The hash and language of each script in the witnesses of a transaction.
fn witness_scripts(tx: &MultiEraTx<'_>) -> Vec<([u8; 28], ScriptLanguage)> {
    let native = tx.multi_era_native_scripts();
    let native = native.iter().map(|s| (*s.hash(), ScriptLanguage::Native));
    let v1 = tx.plutus_v1_scripts().iter();
    let v1 = v1.map(|s| (*s.compute_hash(), ScriptLanguage::PlutusV1));
    let v2 = tx.plutus_v2_scripts().iter();
    let v2 = v2.map(|s| (*s.compute_hash(), ScriptLanguage::PlutusV2));
    let v3 = tx.plutus_v3_scripts().iter();
    let v3 = v3.map(|s| (*s.compute_hash(), ScriptLanguage::PlutusV3));
    let v4 = tx.plutus_v4_scripts().iter();
    let v4 = v4.map(|s| (*s.compute_hash(), ScriptLanguage::PlutusV4));
    native.chain(v1).chain(v2).chain(v3).chain(v4).collect()
}

/// The hash and language of the reference script of an output.
fn output_script(output: &MultiEraOutput<'_>) -> Option<([u8; 28], ScriptLanguage)> {
    let script = output.multi_era_script_ref()?;
    Some((*script.hash(), script.language()))
}

/// The language and place of a script in an accepted record, none for a script the ledger does not hold.
type LedgerScript = Option<(ScriptLanguage, ScriptAt)>;

/// Counts each script hash of the groups whose block rolled or was never made against what the script lookup serves for it, the language from the witnesses or produced outputs of an accepted record, or nothing otherwise.
async fn compare_scripts(
    domain: &DomainAdapter,
    kind: CorpusKind,
    blocks: &[GroupBlock<'_>],
    tally: &mut Tally,
) {
    let mut expected: BTreeMap<[u8; 28], (RecordId, LedgerScript)> = BTreeMap::new();
    let groups = blocks.iter().filter_map(|block| match block {
        GroupBlock::Unmade(records) => Some(*records),
        GroupBlock::Rolled(rolled) => Some(rolled.group),
        GroupBlock::Failed => None,
    });
    for record in groups.flatten() {
        let id = RecordId {
            corpus: kind,
            n: record.n,
        };
        let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &record.tx.0)
            .unwrap_or_else(|e| panic!("TX {id} {e}"));
        let subs = tx.sub_transactions();
        let txs = std::iter::once((TxName::Top(*tx.hash()), &tx));
        let txs = txs.chain(subs.iter().map(|sub| (TxName::Sub(*sub.hash()), sub)));
        let mut carried = Vec::new();
        let mut written = Vec::new();
        for (name, tx) in txs {
            let witnessed = witness_scripts(tx).into_iter();
            carried.extend(witnessed.map(|(h, l)| (h, l, ScriptAt::Witness(name))));
            written.extend(tx.outputs().iter().filter_map(output_script));
        }
        for (hash, _) in &written {
            expected.entry(*hash).or_insert((id, None));
        }
        let LedgerOutcome::Accepted { produced, .. } = &record.outcome else {
            for (hash, _, _) in carried {
                expected.entry(hash).or_insert((id, None));
            }
            continue;
        };
        for entry in produced {
            let output = MultiEraOutput::decode(Era::Dijkstra, &entry.output.0)
                .unwrap_or_else(|e| panic!("OUTPUT {id} {e}"));
            if let Some((hash, language)) = output_script(&output) {
                carried.push((hash, language, ScriptAt::Output(entry.input)));
            }
        }
        for (hash, language, at) in carried {
            let entry = expected.entry(hash).or_insert((id, None));
            if entry.1.is_none() {
                *entry = (id, Some((language, at)));
            }
        }
    }
    let query = AsyncQueryFacade::new(domain.clone());
    for (hash, (id, ledger)) in expected {
        let shown = match &ledger {
            Some((language, at)) => format!("{language:?},{at}"),
            None => "none".to_string(),
        };
        let (agrees, dolos) = match (&ledger, query.script_by_hash(&hash.into()).await) {
            (_, Err(e)) => (false, format!("error={e:?}")),
            (None, Ok(None)) => (true, "none".to_string()),
            (Some(_), Ok(None)) => (false, "none".to_string()),
            (None, Ok(Some(served))) => (false, format!("{:?}", served.language)),
            (Some((language, _)), Ok(Some(served))) => {
                let rehashed = *Hasher::<224>::hash_tagged(&served.script, served.language.tag());
                let dolos = match rehashed == hash {
                    true => format!("{:?}", served.language),
                    false => format!("{:?},hash={}", served.language, hex::encode(rehashed)),
                };
                (served.language == *language && rehashed == hash, dolos)
            }
        };
        tally.add(agrees, || {
            format!(
                "{id} script={} ledger={shown} dolos={dolos}",
                hex::encode(hash)
            )
        });
    }
}

/// The output bytes each service serves for the inputs of the blocks a run rolled.
async fn serve_rolled(
    domain: &DomainAdapter,
    seed: &SeedPoint,
    rolled: &[&Rolled<'_>],
) -> [(Service, Served); 4] {
    let hashes: Vec<BlockHash> = rolled
        .iter()
        .map(|r| r.point.hash().expect("a rolled block has a hash"))
        .collect();
    let txs = rolled.iter().map(|r| r.records.len()).sum();
    let streams = serve::Streams::open(domain, seed.point()).await;
    let [follow, watch] = streams.read(Event::Apply, hashes.len(), txs).await;
    let fetch = serve::fetch_block(domain, &hashes).await;
    let tx_hashes: Vec<[u8; 32]> = rolled
        .iter()
        .flat_map(|r| &r.records)
        .map(|record| {
            let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &record.tx.0)
                .unwrap_or_else(|e| panic!("TX n={} {e}", record.n));
            *tx.hash()
        })
        .collect();
    let read = serve::read_tx(domain, &tx_hashes).await;
    [
        (Service::FetchBlock, fetch),
        follow,
        watch,
        (Service::ReadTx, read),
    ]
}

/// The verdict on one served output against the ledger's output bytes, none when its bytes and parsed fields agree.
fn served_verdict(
    id: RecordId,
    ledger: &[u8],
    served: Option<&cardano::TxOutput>,
) -> Option<String> {
    let Some(served) = served else {
        return Some(ByteVerdict::against(ledger, None).to_string());
    };
    let bytes = served.original_cbor.as_ref();
    let bytes = bytes.expect("a served output carries its bytes");
    let verdict = ByteVerdict::against(ledger, Some(bytes));
    let fields = parsed_differing(id, ledger, Some(served));
    match (verdict.agrees(), fields.is_empty()) {
        (true, true) => None,
        (_, true) => Some(verdict.to_string()),
        (_, false) => Some(format!("{verdict} fields={}", fields.iter().join(","))),
    }
}

/// What each service serves for one input against the ledger's output bytes, grouping the services by each disagreeing verdict.
fn served_verdicts(
    id: RecordId,
    tx: &[u8; 32],
    input: &TxIn,
    ledger: &[u8],
    services: &[(Service, Served)],
) -> BTreeMap<String, BTreeSet<Service>> {
    let mut failures: BTreeMap<String, BTreeSet<Service>> = BTreeMap::new();
    for (service, served) in services {
        let verdicts: Vec<String> = match served.get(tx) {
            None => vec!["not_served".to_string()],
            Some(named) => {
                let occurrences: Vec<_> = named.iter().filter(|(i, _)| i == input).collect();
                if occurrences.is_empty() {
                    vec!["not_named".to_string()]
                } else {
                    occurrences
                        .into_iter()
                        .filter_map(|(_, output)| served_verdict(id, ledger, output.as_ref()))
                        .collect()
                }
            }
        };
        for verdict in verdicts {
            failures.entry(verdict).or_default().insert(*service);
        }
    }
    failures
}

/// The served output comparisons of the records of one block, with the inputs left uncompared because the run's seed made them and the compared inputs only sub transactions name.
#[derive(Debug, Default)]
struct ServedChecks {
    records: BTreeMap<RecordId, Tally>,
    seeded: usize,
    sub_only: usize,
}

/// Counts each input a record of the block names that an earlier record made, against what every service serves for it.
fn compare_served(
    kind: CorpusKind,
    run: &KeptRun,
    makers: &BTreeMap<TxIn, RecordId>,
    block: &Rolled<'_>,
    services: &[(Service, Served)],
) -> ServedChecks {
    let mut checks = ServedChecks::default();
    for record in &block.records {
        let id = RecordId {
            corpus: kind,
            n: record.n,
        };
        let parsed = tx_inputs(&record.tx.0).unwrap_or_else(|e| panic!("TX {id} {e}"));
        let sub_only = parsed.sub_only();
        let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &record.tx.0)
            .unwrap_or_else(|e| panic!("TX {id} {e}"));
        let hash: [u8; 32] = *tx.hash();
        let tally = checks.records.entry(id).or_default();
        for input in named_inputs(id, record) {
            let shown = show_input(&input);
            let made = match run.origin(&input, record, makers, block.group) {
                InputOrigin::Seeded => {
                    checks.seeded += 1;
                    continue;
                }
                InputOrigin::AbsentFromPre => {
                    tally.add(false, || format!("absent_from_pre input={shown}"));
                    continue;
                }
                InputOrigin::InRun { made_by } => format!("in_run made_by={made_by}"),
                InputOrigin::InGroup { made_by } => format!("in_group made_by={made_by}"),
            };
            if sub_only.contains(&input) {
                checks.sub_only += 1;
            }
            let ledger = &run
                .pre_entry(record, &input)
                .expect("an input made in the run is in the pre state")
                .output;
            let failures = served_verdicts(id, &hash, &input, &ledger.0, services);
            tally.add(failures.is_empty(), || {
                let lines = failures.iter().map(|(verdict, services)| {
                    let mut names = services.iter().map(Service::to_string);
                    let names = names.join(",");
                    format!("{verdict} input={shown} {made} services={names}")
                });
                lines.collect::<Vec<_>>().join(" and ")
            });
        }
    }
    checks
}

/// Counts the served outputs of every rolled block into the per record as_output checks.
fn compare_as_output(
    kind: CorpusKind,
    run: &KeptRun,
    rolled: &[&Rolled<'_>],
    services: &[(Service, Served)],
    state: &mut StateTallies,
    checks: &mut BTreeMap<RecordId, Tally>,
) {
    let makers = run.makers(kind);
    for block in rolled {
        let served = compare_served(kind, run, &makers, block, services);
        state.seeded_unjudged += served.seeded;
        state.sub_only += served.sub_only;
        for (id, tally) in served.records {
            checks.entry(id).or_default().merge(tally);
        }
    }
}

/// Receives as pending each transaction of a block after block `i` that names an output block `i` made, returning those the mempool took.
fn hold_dependents(
    domain: &DomainAdapter,
    kind: CorpusKind,
    run: &KeptRun,
    makers: &BTreeMap<TxIn, RecordId>,
    rolled: &[&Rolled<'_>],
    i: usize,
    state: &mut StateTallies,
) -> Vec<(RecordId, TxHash)> {
    let made: BTreeSet<u32> = rolled[i].records.iter().map(|r| r.n).collect();
    let mut held = vec![];
    let later = rolled[i + 1..]
        .iter()
        .flat_map(|block| block.records.iter().map(move |r| (*r, block.group)));
    for (record, group) in later {
        let rid = RecordId {
            corpus: kind,
            n: record.n,
        };
        let depends = named_inputs(rid, record).iter().any(|input| {
            let from = run.origin(input, record, makers, group);
            matches!(from, InputOrigin::InRun { made_by } if made.contains(&made_by.n))
        });
        if !depends {
            continue;
        }
        state.dependents += 1;
        let received = domain.receive_tx("ledger-oracle", &domain.read_chain(), &record.tx.0);
        if let Ok(hash) = received {
            held.push((rid, hash));
        }
    }
    held
}

/// Queues the block's requeued record unvalidated, behind the held dependents, since the ledger accepted it in the state the rollback of that block restores.
fn hold_undone(
    domain: &DomainAdapter,
    kind: CorpusKind,
    block: &Rolled<'_>,
) -> Option<(RecordId, TxHash)> {
    let record = block.requeued?;
    let rid = RecordId {
        corpus: kind,
        n: record.n,
    };
    let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &record.tx.0)
        .unwrap_or_else(|e| panic!("RUN {rid} decode {e}"));
    let hash = tx.hash();
    let payload = EraCbor(u16::from(Era::Dijkstra), record.tx.0.clone());
    domain
        .mempool()
        .receive(MempoolTx::new(hash, payload, vec![]))
        .unwrap_or_else(|e| panic!("RUN {rid} queue {e}"));
    Some((rid, hash))
}

/// Rolls a run back one block at a time to its seed point, counting what FollowTip and WatchTx serve for each undone input, what ReadUtxos serves for the inputs earlier blocks made, whether a pending transaction spending the undone block is dropped and the undone block's first transaction kept, and the seed outputs and accounts Dolos holds at the end.
async fn sweep(
    domain: &DomainAdapter,
    seed: &SeedPoint,
    kind: CorpusKind,
    run: &KeptRun,
    rolled: &[&Rolled<'_>],
    state: &mut StateTallies,
) {
    let makers = run.makers(kind);
    let first = &run.records()[0];
    let id = RecordId {
        corpus: kind,
        n: first.n,
    };
    for (i, block) in rolled.iter().enumerate().rev() {
        let to = match i.checked_sub(1) {
            Some(previous) => &rolled[previous].point,
            None => seed.point(),
        };
        let held = hold_dependents(domain, kind, run, &makers, rolled, i, state);
        let undone = hold_undone(domain, kind, block);
        let streams = serve::Streams::open(domain, &block.point).await;
        let rolled_back = domain.rollback(to);
        let pending: BTreeSet<TxHash> = domain
            .mempool()
            .peek_pending()
            .iter()
            .map(|tx| tx.hash)
            .collect();
        let kept: Vec<TxHash> = held
            .iter()
            .chain(&undone)
            .map(|(_, hash)| *hash)
            .filter(|hash| pending.contains(hash))
            .collect();
        domain
            .mempool()
            .drop_pending(&kept)
            .unwrap_or_else(|e| panic!("RUN {id} drop pending {e}"));
        if let Err(e) = rolled_back {
            let line = || format!("{id} rollback to={to} {e}");
            state.undo.add(false, line);
            return;
        }
        for (rid, hash) in &held {
            let dropped = !pending.contains(hash);
            state
                .revalidate
                .add(dropped, || format!("{rid} kept_after_rollback_to={to}"));
        }
        if let Some((rid, hash)) = &undone {
            let kept = pending.contains(hash);
            state
                .revalidate
                .add(kept, || format!("{rid} dropped_after_rollback_to={to}"));
        }
        let tally = &mut state.undo;
        let services = streams.read(Event::Undo, 1, block.records.len()).await;
        for (record, checked) in compare_served(kind, run, &makers, block, &services).records {
            tally.compared += checked.compared;
            let lines = checked.disagree.into_iter();
            tally
                .disagree
                .extend(lines.map(|d| format!("{record} {d}")));
        }
        let wrote: BTreeMap<TxIn, Vec<u8>> = run.records().iter().flat_map(written).collect();
        for record in &block.records {
            let rid = RecordId {
                corpus: kind,
                n: record.n,
            };
            let restored: Vec<&UtxoEntry> = named_inputs(rid, record)
                .iter()
                .filter(|input| {
                    let from = run.origin(input, record, &makers, block.group);
                    matches!(from, InputOrigin::InRun { .. })
                })
                .filter_map(|input| run.pre_entry(record, input))
                .collect();
            compare_utxos(domain, rid, [], restored, &wrote, tally).await;
        }
    }
    let tally = &mut state.undo;
    compare_utxos(domain, id, [], run.seed(), &BTreeMap::new(), tally).await;
    let mut restored = Tally::default();
    accounts::compare(&first.accounts_pre, &dolos_accounts(domain), &mut restored);
    let lines = restored.disagree.iter().map(|d| format!("{id} {d}"));
    tally.compared += restored.compared;
    tally.disagree.extend(lines);
}

/// The pre state a planted check seeds for a record in place of its own.
type PreEdit<'a> = Box<dyn Fn(RecordId, &[UtxoEntry]) -> Vec<UtxoEntry> + 'a>;

/// What a planted check changes in the seed, the expected parameters and the blocks of one judgement.
struct Edits<'a> {
    pre: PreEdit<'a>,
    accounts_pre: Box<dyn Fn(&LedgerAccounts) -> LedgerAccounts + 'a>,
    expected_params: Box<dyn Fn(&PParams) -> PParams + 'a>,
    block_tx: Box<dyn Fn(RecordId, Phase2) -> Option<Phase2> + 'a>,
    rolled_block: Box<dyn Fn(RecordId, RawBlock) -> RawBlock + 'a>,
    expected_datum: Box<dyn Fn(RecordId, Vec<u8>) -> Vec<u8> + 'a>,
}

impl Default for Edits<'_> {
    fn default() -> Self {
        Edits {
            pre: Box::new(|_, pre| pre.to_vec()),
            accounts_pre: Box::new(LedgerAccounts::clone),
            expected_params: Box::new(PParams::clone),
            block_tx: Box::new(|_, phase2| Some(phase2)),
            rolled_block: Box::new(|_, block| block),
            expected_datum: Box::new(|_, datum| datum),
        }
    }
}

/// Judges every kept run of a corpus, each seeded from its first pre state.
async fn judge(corpus: &Corpus, manifests: &Manifests, edits: &Edits<'_>) -> Judged {
    let genesis = seed::genesis(epoch_size(corpus));
    let mut judged = Judged::default();
    let mut judged_ids = BTreeSet::new();
    let mut as_output = BTreeMap::new();
    let mut account_checks: BTreeMap<RecordId, Tally> = BTreeMap::new();
    for run in corpus.kept() {
        let first = &run.records()[0];
        let id = RecordId {
            corpus: corpus.kind,
            n: first.n,
        };
        let epoch = run
            .epoch()
            .unwrap_or_else(|| panic!("RUN {id} has no epoch"));
        let pparams = params::seed_values(run.pparams()).unwrap_or_else(|e| panic!("{e}"));
        let seeded = seed::seed(
            &genesis,
            epoch,
            &pparams,
            &(edits.pre)(id, run.seed()),
            &(edits.accounts_pre)(&first.accounts_pre),
        );
        let seed = run.seed();
        compare_utxos(
            &seeded.domain,
            id,
            [],
            seed,
            &BTreeMap::new(),
            &mut judged.state.utxo,
        )
        .await;
        let served = dolos_accounts(&seeded.domain);
        let checks = account_checks.entry(id).or_default();
        accounts::compare(&first.accounts_pre, &served, checks);
        let served = read_params(&seeded.domain).await;
        let expected = (edits.expected_params)(run.pparams());
        params::compare(id, &expected, &served, &mut judged.state.params);
        let (blocks, run_accounts) = validate_run(
            &seeded.domain,
            corpus.kind,
            run,
            manifests,
            edits,
            &mut judged_ids,
            &mut judged,
        )
        .await;
        for (id, tally) in run_accounts {
            account_checks.entry(id).or_default().merge(tally);
        }
        let readtx = &mut judged.state.readtx;
        compare_read_tx(&seeded.domain, corpus.kind, &blocks, readtx).await;
        let script = &mut judged.state.script;
        compare_scripts(&seeded.domain, corpus.kind, &blocks, script).await;
        let rolled: Vec<&Rolled> = blocks.iter().filter_map(GroupBlock::rolled).collect();
        let services = serve_rolled(&seeded.domain, &seeded.seed, &rolled).await;
        compare_as_output(
            corpus.kind,
            run,
            &rolled,
            &services,
            &mut judged.state,
            &mut as_output,
        );
        sweep(
            &seeded.domain,
            &seeded.seed,
            corpus.kind,
            run,
            &rolled,
            &mut judged.state,
        )
        .await;
    }
    manifests.unjudged(corpus.kind, &judged_ids, &mut judged.validation);
    judged
        .state
        .settle_as_output(corpus.kind, as_output, &manifests.known);
    judged
        .state
        .settle_accounts(corpus.kind, account_checks, &manifests.known);
    let fired = judged.state.known_fired.clone();
    judged.candidates.known.records.extend(fired);
    judged
}

/// Judges the supplement corpus under edited manifests.
async fn judge_supp(edit: impl FnOnce(&mut Manifests)) -> Judged {
    let mut manifests = Manifests::committed();
    edit(&mut manifests);
    judge(&load(CorpusKind::Supp), &manifests, &Edits::default()).await
}

fn supp(n: u32) -> RecordId {
    RecordId {
        corpus: CorpusKind::Supp,
        n,
    }
}

#[test]
fn an_uncovered_base_builtin_passes_the_bar_and_names_no_record() {
    let mut coverage = Coverage::default();
    coverage.add(supp(41), &[Builtin::ChooseUnit, Builtin::IfThenElse]);
    let audited = [
        Builtin::ChooseUnit,
        Builtin::IfThenElse,
        Builtin::AddInteger,
    ];
    let (lines, pass) = builtin_bar(&audited, &[Builtin::ChooseUnit], &coverage);
    assert!(pass, "{lines:?}");
    assert_eq!(
        lines,
        [
            "BUILTIN ChooseUnit record=supp#41",
            "BUILTIN IfThenElse record=supp#41",
            "BUILTIN AddInteger uncovered base",
            "BUILTIN added covered=1/1 base covered=1/2 pass=true",
        ]
    );
}

#[test]
fn an_uncovered_builtin_outside_the_base_fails_the_bar() {
    let mut coverage = Coverage::default();
    coverage.add(supp(41), &[Builtin::ChooseUnit]);
    let audited = [Builtin::ChooseUnit, Builtin::NullList];
    let (lines, pass) = builtin_bar(&audited, &audited, &coverage);
    assert!(!pass);
    assert_eq!(lines[1], "BUILTIN_UNCOVERED NullList");
    assert_eq!(
        lines[2],
        "BUILTIN added covered=1/2 base covered=0/0 pass=false"
    );
}

#[test]
fn every_added_builtin_is_audited() {
    let audited = audited_builtins();
    let outside: Vec<_> = ADDED_BUILTINS
        .iter()
        .filter(|b| !audited.contains(b))
        .collect();
    assert_eq!(outside, Vec::<&Builtin>::new());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn ledger_oracle() {
    let mut pass = true;
    let manifests = Manifests::committed();
    let mut candidates = Candidates::default();
    let mut costs = Vec::new();
    let mut coverage = Coverage::default();
    let mut constructs = construct::Coverage::default();
    let mut inventory = Inventory::default();
    let mut revalidated = 0;
    for kind in [CorpusKind::Ic, CorpusKind::Supp] {
        let corpus = load(kind);
        println!("{corpus}");
        let tallies = judge(&corpus, &manifests, &Edits::default()).await;
        println!("{tallies}");
        pass &= tallies.pass();
        revalidated += tallies.state.revalidate.compared;
        candidates.declared.extend(tallies.candidates.declared);
        let known = tallies.candidates.known.records;
        candidates.known.records.extend(known);
        costs.push(tallies.cost);
        coverage.extend(tallies.coverage);
        constructs.extend(tallies.constructs);
        inventory.extend(tallies.inventory);
    }
    let ran_alike: usize = costs.iter().map(|c| c.ran).sum();
    let cost_pass = cost::pass_all(&costs);
    println!("COST all ran_alike={ran_alike} pass={cost_pass}");
    pass &= cost_pass;
    println!(
        "REVALIDATE all compared={revalidated} pass={}",
        revalidated > 0
    );
    pass &= revalidated > 0;
    let (lines, bar_pass) = builtin_bar(&audited_builtins(), &ADDED_BUILTINS, &coverage);
    for line in lines {
        println!("{line}");
    }
    pass &= bar_pass;
    let (lines, construct_pass) = constructs.bar();
    for line in lines.into_iter().chain(inventory.lines()) {
        println!("{line}");
    }
    pass &= construct_pass;
    for line in candidates.lines() {
        println!("{line}");
    }
    let result = if pass { "PASS" } else { "FAIL" };
    println!("RESULT {result}");
    assert!(pass);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn dropping_one_seed_pair_prints_utxo_disagree() {
    let corpus = load(CorpusKind::Supp);
    let manifests = Manifests::committed();
    let intact = judge(&corpus, &manifests, &Edits::default()).await;
    assert!(intact.state.utxo.compared > 0);
    assert!(intact.state.utxo.disagree.is_empty(), "{intact}");
    let dropped = AtomicBool::new(false);
    let edits = Edits {
        pre: Box::new(|_, pre| {
            let skip = !dropped.swap(true, Ordering::Relaxed);
            pre.iter().skip(usize::from(skip)).cloned().collect()
        }),
        ..Edits::default()
    };
    let planted = judge(&corpus, &manifests, &edits).await;
    let run = corpus.kept().next().unwrap();
    let first = &run.records()[0];
    let pair = &run.seed()[0];
    let line = format!(
        "{} input={} ledger={} dolos=none",
        supp(first.n),
        show_input(&pair.input),
        hex::encode(&pair.output.0)
    );
    assert!(planted.state.utxo.disagree.contains(&line), "{planted}");
    assert!(planted.to_string().contains("\nSTATE utxo disagree="));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn one_changed_cost_model_entry_prints_params_disagree() {
    let corpus = load(CorpusKind::Supp);
    let manifests = Manifests::committed();
    let intact = judge(&corpus, &manifests, &Edits::default()).await;
    assert!(intact.state.params.compared > 0);
    assert!(intact.state.params.disagree.is_empty(), "{intact}");
    let changed = AtomicBool::new(false);
    let edits = Edits {
        expected_params: Box::new(|pparams| {
            let mut pparams = pparams.clone();
            if !changed.swap(true, Ordering::Relaxed) {
                let entry = &mut pparams.0["costModels"]["PlutusV1"][0];
                *entry = (entry.as_i64().unwrap() + 1).into();
            }
            pparams
        }),
        ..Edits::default()
    };
    let planted = judge(&corpus, &manifests, &edits).await;
    let text = planted.to_string();
    assert!(text.contains("STATE params disagree=1\n"), "{text}");
    assert!(text.contains("costModels.PlutusV1[0] ledger="), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn the_v2_record_is_known_and_its_maker_agrees() {
    let text = judge_supp(|_| ()).await.validation.to_string();
    assert!(!text.contains(" supp#1 "), "{text}");
    assert!(!text.contains("\nDISAGREE supp#2 "), "{text}");
    assert!(
        text.contains("\nKNOWN row=V2 ids=supp#2 ledger=accepted dolos=refused[InputNotInUTxO]"),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn deleting_the_v2_known_entry_prints_its_disagreement() {
    let text = judge_supp(|m| {
        m.known.remove(&(Locus::Validation, supp(2))).unwrap();
    })
    .await
    .to_string();
    assert!(!text.contains("\nKNOWN row=V2 "), "{text}");
    assert!(
        text.contains("\nDISAGREE supp#2 ledger=accepted dolos=refused[InputNotInUTxO]"),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn deleting_one_declared_entry_prints_unsupported_unexpected() {
    let label = "stateful or scripted subtransaction";
    let intact = judge_supp(|_| ()).await.to_string();
    assert!(
        !intact.contains("UNSUPPORTED_UNEXPECTED supp#14 "),
        "{intact}"
    );
    let deleted = judge_supp(|m| {
        assert_eq!(m.declared.remove(&supp(14)), Some(Feature(label.into())));
        m.declared.insert(supp(999), Feature(label.into()));
    })
    .await
    .to_string();
    assert!(
        deleted.contains(&format!("\nUNSUPPORTED_UNEXPECTED supp#14 label={label}")),
        "{deleted}"
    );
    assert!(
        deleted.contains(&format!(
            "\nSTALE_MANIFEST supp#999 declared={label} dolos=not_judged"
        )),
        "{deleted}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn a_known_entry_naming_an_agreeing_or_unjudged_record_prints_known_stale() {
    let intact = judge_supp(|_| ()).await.validation.to_string();
    assert!(!intact.contains("supp#0 "), "{intact}");
    let named = judge_supp(|m| {
        m.known.insert((Locus::Validation, supp(0)), Row::V9);
        m.known.insert((Locus::Validation, supp(999)), Row::V9);
    })
    .await
    .to_string();
    assert!(named.contains("\nKNOWN_STALE supp#0 row=V9"), "{named}");
    assert!(named.contains("\nKNOWN_STALE supp#999 row=V9"), "{named}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn a_maker_that_fails_leaves_its_dependents_disagreeing() {
    let corpus = load(CorpusKind::Ic);
    let committed = Manifests::committed();
    let intact = judge(&corpus, &committed, &Edits::default()).await;
    let maker = intact
        .validation
        .dependent
        .iter()
        .flat_map(|(_, makers)| makers)
        .find(|m| committed.declared.contains_key(m))
        .copied()
        .expect("a declared unsupported maker has a seeded dependent");
    let dependents: BTreeSet<RecordId> = intact
        .validation
        .dependent
        .iter()
        .filter(|(_, makers)| makers.contains(&maker))
        .map(|(id, _)| *id)
        .collect();
    let disagrees = |judged: &Judged| -> BTreeSet<RecordId> {
        let ids = judged.validation.disagree.iter().map(|(id, _)| *id);
        ids.filter(|id| dependents.contains(id)).collect()
    };
    assert!(disagrees(&intact).is_empty(), "{intact}");
    let mut failing = committed.clone();
    failing.declared.remove(&maker).unwrap();
    let patched = judge(&corpus, &failing, &Edits::default()).await;
    assert!(
        patched
            .validation
            .dependent
            .iter()
            .all(|(_, makers)| !makers.contains(&maker)),
        "{patched}"
    );
    assert!(
        !disagrees(&patched).is_empty(),
        "maker={maker} dependents={dependents:?} {patched}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn dropping_the_seeded_input_under_a_metadata_refusal_prints_reason_mismatch() {
    let corpus = load(CorpusKind::Ic);
    let manifests = Manifests::committed();
    let run = corpus
        .kept()
        .find(|r| r.records().iter().any(|x| x.n == 1311))
        .unwrap();
    let first = &run.records()[0];
    assert_eq!(run.seed().len(), 1, "the run is seeded with one output");
    let first = RecordId {
        corpus: CorpusKind::Ic,
        n: first.n,
    };
    let line = "\nREASON_MISMATCH ic#1311 ";
    let intact = judge(&corpus, &manifests, &Edits::default()).await;
    assert!(!intact.to_string().contains(line), "{intact}");
    let edits = Edits {
        pre: Box::new(|id, pre| if id == first { vec![] } else { pre.to_vec() }),
        ..Edits::default()
    };
    let planted = judge(&corpus, &manifests, &edits).await.to_string();
    assert!(
        planted.contains(&format!(
            "{line}ledger=refused[SubMissingTxMetadata] ledger_class=metadata \
             dolos=refused[InputNotInUTxO] dolos_class=inputs"
        )),
        "{planted}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn deleting_the_v1_known_entry_prints_its_reason_mismatch() {
    let intact = judge_supp(|_| ()).await.to_string();
    assert!(intact.contains("\nKNOWN row=V1 ids=supp#5 "), "{intact}");
    let deleted = judge_supp(|m| {
        m.known.remove(&(Locus::Validation, supp(5))).unwrap();
    })
    .await
    .to_string();
    assert!(
        deleted.contains(
            "\nREASON_MISMATCH supp#5 ledger=refused[BadInputsUTxO,BadInputsUTxO] \
             ledger_class=inputs dolos=refused[PreservationOfValue] dolos_class=value"
        ),
        "{deleted}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn leaving_an_accepted_tx_out_of_its_block_prints_utxo_disagree() {
    let corpus = load(CorpusKind::Supp);
    let manifests = Manifests::committed();
    let left_out = corpus
        .kept()
        .flat_map(|run| run.groups().filter_map(<[Record]>::last))
        .find(|r| matches!(r.outcome, LedgerOutcome::Accepted { .. }))
        .map(|r| supp(r.n))
        .expect("a group ends in an accepted record");
    let missing = |judged: &Judged| {
        let prefix = format!("{left_out} input=");
        let lines = judged.state.utxo.disagree.iter();
        lines
            .filter(|d| d.starts_with(&prefix) && d.ends_with(" dolos=none"))
            .count()
    };
    let intact = judge(&corpus, &manifests, &Edits::default()).await;
    assert_eq!(missing(&intact), 0, "{intact}");
    let edits = Edits {
        block_tx: Box::new(|id, phase2| (id != left_out).then_some(phase2)),
        ..Edits::default()
    };
    let planted = judge(&corpus, &manifests, &edits).await;
    assert!(missing(&planted) > 0, "{left_out} {planted}");
    assert!(planted.to_string().contains("\nSTATE utxo disagree="));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn a_block_that_does_not_roll_prints_one_roll_fail_and_no_state_line_for_its_group() {
    let corpus = load(CorpusKind::Supp);
    let manifests = Manifests::committed();
    let intact = judge(&corpus, &manifests, &Edits::default()).await;
    let text = intact.to_string();
    assert!(intact.state.roll_fail.is_empty(), "{text}");
    assert!(text.contains(" rolled_failed=0\n"), "{text}");
    let group: Vec<RecordId> = corpus
        .kept()
        .flat_map(|run| run.groups())
        .find(|g| {
            g.iter()
                .any(|r| matches!(r.outcome, LedgerOutcome::Accepted { .. }))
        })
        .expect("a group holds an accepted record")
        .iter()
        .map(|r| supp(r.n))
        .collect();
    let edits = Edits {
        rolled_block: Box::new(|id, block| match group.contains(&id) {
            true => std::sync::Arc::new(vec![0x80]),
            false => block,
        }),
        ..Edits::default()
    };
    let planted = judge(&corpus, &manifests, &edits).await;
    let text = planted.to_string();
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("ROLL_FAIL "))
        .collect();
    let first = group[0];
    let last = group[group.len() - 1];
    let head = format!("ROLL_FAIL group={first}..{last} records={} ", group.len());
    assert_eq!(lines.len(), 1, "{text}");
    assert!(lines[0].starts_with(&head), "{text}");
    assert!(
        text.contains(&format!(" rolled_failed={}\n", group.len())),
        "{text}"
    );
    assert!(!planted.pass());
    let state = &planted.state;
    let loci = [&state.utxo, &state.readtx, &state.script];
    let named = loci
        .iter()
        .flat_map(|t| &t.disagree)
        .filter(|d| group.iter().any(|id| d.starts_with(&format!("{id} "))))
        .join("\n");
    assert!(named.is_empty(), "{named}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn supp8_written_as_phase2_valid_leaves_the_input_of_supp9_missing() {
    let corpus = load(CorpusKind::Supp);
    let supp8 = corpus
        .kept()
        .flat_map(KeptRun::records)
        .find(|r| r.n == 8)
        .expect("supp#8 is in a kept run");
    assert_eq!(supp8.phase2_valid, Phase2::Invalid);
    assert!(matches!(supp8.outcome, LedgerOutcome::Accepted { .. }));
    let manifests = Manifests::committed();
    let line = "\nDISAGREE supp#9 ";
    let intact = judge(&corpus, &manifests, &Edits::default()).await;
    assert!(!intact.to_string().contains(line), "{intact}");
    let edits = Edits {
        block_tx: Box::new(|id, phase2| match id == supp(8) {
            true => Some(Phase2::Valid),
            false => Some(phase2),
        }),
        ..Edits::default()
    };
    let planted = judge(&corpus, &manifests, &edits).await.to_string();
    assert!(
        planted.contains(&format!(
            "{line}ledger=accepted dolos=refused[InputNotInUTxO]"
        )),
        "{planted}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn an_output_dolos_holds_that_the_ledger_never_made_prints_utxo_disagree() {
    let corpus = load(CorpusKind::Supp);
    let manifests = Manifests::committed();
    let intact = judge(&corpus, &manifests, &Edits::default()).await;
    let unmade = |judged: &Judged, tx: Option<[u8; 32]>| -> Vec<String> {
        let lines = judged.state.utxo.disagree.iter();
        let lines = lines.filter(|d| d.contains(" ledger=none dolos="));
        let named =
            |d: &&String| tx.is_none_or(|tx| d.contains(&format!(" input={}#", hex::encode(tx))));
        lines.filter(named).cloned().collect()
    };
    assert!(unmade(&intact, None).is_empty(), "{intact}");
    let supp8 = corpus
        .kept()
        .flat_map(KeptRun::records)
        .find(|r| r.n == 8)
        .expect("supp#8 is in a kept run");
    let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &supp8.tx.0).unwrap();
    assert!(!tx.outputs().is_empty());
    let edits = Edits {
        block_tx: Box::new(|id, phase2| match id == supp(8) {
            true => Some(Phase2::Valid),
            false => Some(phase2),
        }),
        ..Edits::default()
    };
    let planted = judge(&corpus, &manifests, &edits).await;
    let lines = unmade(&planted, Some(*tx.hash()));
    assert_eq!(lines.len(), tx.outputs().len(), "{planted}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn a_tx_received_in_one_group_is_absent_from_the_next() {
    let corpus = load(CorpusKind::Supp);
    let genesis = seed::genesis(epoch_size(&corpus));
    let mut checked = 0;
    for run in corpus.kept() {
        let groups: Vec<_> = run.groups().collect();
        let [first, second, ..] = groups.as_slice() else {
            continue;
        };
        let pparams = params::seed_values(run.pparams()).unwrap();
        let seeded = seed::seed(
            &genesis,
            run.epoch().unwrap(),
            &pparams,
            run.seed(),
            &run.records()[0].accounts_pre,
        );
        let one = validate::group(&seeded.domain, first[0].slot, Relay::Held);
        let Some(hash) = first
            .iter()
            .filter(|r| matches!(r.outcome, LedgerOutcome::Accepted { .. }))
            .find_map(|r| one.receive(&r.tx.0).ok())
        else {
            continue;
        };
        let made = TxoRef(hash, 0);
        assert!(one.sees(made.clone()));
        let two = validate::group(&seeded.domain, second[0].slot, Relay::Held);
        assert!(!two.sees(made));
        checked += 1;
    }
    assert!(checked > 0);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn the_collateral_return_supp9_spends_is_served() {
    let intact = judge_supp(|_| ()).await;
    let text = intact.to_string();
    assert!(!text.contains("\nKNOWN row=S3 "), "{text}");
    assert!(!text.contains("\nDIFF as_output supp#9 "), "{text}");
    assert!(intact.state.as_output.disagree.is_empty(), "{text}");
    let listed = judge_supp(|m| {
        m.known.insert((Locus::AsOutput, supp(9)), Row::S3);
    })
    .await
    .to_string();
    assert!(
        listed.contains("\nKNOWN_STALE supp#9 row=S3 locus=as_output"),
        "{listed}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn the_vote_delegation_ic551_keeps_is_known() {
    let corpus = load(CorpusKind::Ic);
    let manifests = Manifests::committed();
    let intact = judge(&corpus, &manifests, &Edits::default()).await;
    let text = intact.state.to_string();
    let line = "\nKNOWN row=S18 ids=ic#551 accounts account=";
    assert!(text.contains(line), "{text}");
    assert!(intact.state.accounts.disagree.is_empty(), "{text}");
    assert!(intact.state.known_stale.is_empty(), "{text}");
    let mut deleted = manifests;
    let id = RecordId {
        corpus: CorpusKind::Ic,
        n: 551,
    };
    deleted.known.remove(&(Locus::Accounts, id)).unwrap();
    let deleted = judge(&corpus, &deleted, &Edits::default()).await;
    let text = deleted.state.to_string();
    assert!(!text.contains("\nKNOWN row=S18 "), "{text}");
    assert!(text.contains("\nSTATE accounts disagree=1\n"), "{text}");
    assert!(text.contains("\nDIFF accounts ic#551 account="), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn rolling_back_each_block_serves_every_input_the_run_made() {
    let corpus = load(CorpusKind::Supp);
    let seeds: usize = corpus.kept().map(|r| r.seed().len()).sum();
    let judged = judge(&corpus, &Manifests::committed(), &Edits::default()).await;
    let text = judged.to_string();
    assert!(judged.state.undo.compared > seeds, "seeds={seeds} {text}");
    let undo = judged.state.undo.disagree.iter().join("\n");
    assert!(undo.is_empty(), "{text}\nUNDO\n{undo}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn an_accepted_tx_left_out_of_its_block_is_not_found_by_read_tx() {
    let corpus = load(CorpusKind::Supp);
    let manifests = Manifests::committed();
    let intact = judge(&corpus, &manifests, &Edits::default()).await;
    assert!(intact.state.readtx.compared > 0, "{intact}");
    assert!(intact.state.readtx.disagree.is_empty(), "{intact}");
    let left_out = corpus
        .kept()
        .flat_map(|run| run.groups().filter_map(<[Record]>::last))
        .find(|r| matches!(r.outcome, LedgerOutcome::Accepted { .. }))
        .map(|r| supp(r.n))
        .expect("a group ends in an accepted record");
    let edits = Edits {
        block_tx: Box::new(|id, phase2| (id != left_out).then_some(phase2)),
        ..Edits::default()
    };
    let planted = judge(&corpus, &manifests, &edits).await;
    let prefix = format!("{left_out} tx=");
    let lines: Vec<&String> = planted
        .state
        .readtx
        .disagree
        .iter()
        .filter(|d| d.starts_with(&prefix))
        .collect();
    assert_eq!(lines.len(), 1, "{planted}");
    assert!(lines[0].ends_with(" dolos=none"), "{planted}");
}

/// The hash of the first reference script on an output of a sub transaction of the record.
fn sub_output_script(record: &Record) -> Option<[u8; 28]> {
    let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &record.tx.0).ok()?;
    for sub in tx.sub_transactions() {
        for output in sub.outputs() {
            if let Some(script) = output.multi_era_script_ref() {
                return Some(*script.hash());
            }
        }
    }
    None
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn a_reference_script_on_a_sub_transaction_output_is_served() {
    let corpus = load(CorpusKind::Supp);
    let manifests = Manifests::committed();
    let (id, hash) = corpus
        .kept()
        .flat_map(KeptRun::records)
        .filter(|r| matches!(r.outcome, LedgerOutcome::Accepted { .. }))
        .find_map(|r| sub_output_script(r).map(|hash| (supp(r.n), hash)))
        .expect("a sub transaction output carries a reference script");
    let prefix = format!("{id} script={} ", hex::encode(hash));
    let lines = |judged: &Judged| -> Vec<String> {
        judged
            .state
            .script
            .disagree
            .iter()
            .filter(|d| d.starts_with(&prefix))
            .cloned()
            .collect()
    };

    let intact = judge(&corpus, &manifests, &Edits::default()).await;
    assert!(intact.state.script.compared > 0, "{intact}");
    assert_eq!(lines(&intact), Vec::<String>::new(), "{intact}");

    let edits = Edits {
        block_tx: Box::new(|record, phase2| (record != id).then_some(phase2)),
        ..Edits::default()
    };
    let planted = judge(&corpus, &manifests, &edits).await;
    let planted_lines = lines(&planted);
    assert_eq!(planted_lines.len(), 1, "{planted}");
    assert!(
        planted_lines[0].contains(" ledger=Native,output="),
        "{planted}"
    );
    assert!(planted_lines[0].ends_with(" dolos=none"), "{planted}");
}

/// The first output of a sub transaction of the record with a datum hash its sub transaction witnesses, and the length of that datum.
fn sub_witnessed_datum(record: &Record) -> Option<(TxIn, usize)> {
    let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &record.tx.0).ok()?;
    for sub in tx.sub_transactions() {
        for (ix, output) in sub.outputs().iter().enumerate() {
            let Some(DatumOption::Hash(hash)) = output.datum() else {
                continue;
            };
            let witnessed = sub.plutus_data().iter().find(|d| d.original_hash() == hash);
            if let Some(datum) = witnessed {
                let input = TxIn {
                    tx: *sub.hash(),
                    ix: ix as u64,
                };
                return Some((input, datum.raw_cbor().len()));
            }
        }
    }
    None
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn a_witnessed_datum_changed_in_its_last_byte_prints_datum_differ() {
    let corpus = load(CorpusKind::Supp);
    let manifests = Manifests::committed();
    let intact = judge(&corpus, &manifests, &Edits::default()).await;
    assert!(intact.state.datum.compared > 0, "{intact}");
    let (maker, input, len) = corpus
        .kept()
        .flat_map(KeptRun::records)
        .filter(|r| matches!(r.outcome, LedgerOutcome::Accepted { .. }))
        .find_map(|r| sub_witnessed_datum(r).map(|(input, len)| (supp(r.n), input, len)))
        .expect("a sub transaction output holds a datum hash its sub transaction witnesses");
    let named = format!(" input={} made_by={maker} ", show_input(&input));
    let text = intact.to_string();
    assert!(
        intact
            .state
            .datum
            .disagree
            .iter()
            .all(|d| !d.contains(&named)),
        "{text}"
    );
    let edits = Edits {
        expected_datum: Box::new(|id, mut datum| {
            if id == maker {
                if let Some(last) = datum.last_mut() {
                    *last ^= 1;
                }
            }
            datum
        }),
        ..Edits::default()
    };
    let planted = judge(&corpus, &manifests, &edits).await;
    let lines: Vec<&String> = planted
        .state
        .datum
        .disagree
        .iter()
        .filter(|d| d.contains(&named))
        .collect();
    println!(
        "PLANTED datum disagree={} named={}",
        planted.state.datum.disagree.len(),
        lines.len()
    );
    assert!(!lines.is_empty(), "{planted}");
    let differ = format!(" differ at byte {}", len - 1);
    assert!(lines.iter().all(|d| d.ends_with(&differ)), "{planted}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn a_seeded_reward_changed_by_one_prints_accounts_disagree() {
    let corpus = load(CorpusKind::Ic);
    let manifests = Manifests::committed();
    let intact = judge(&corpus, &manifests, &Edits::default()).await;
    assert!(intact.state.accounts.compared > 0, "{intact}");
    let before: BTreeSet<&String> = intact.state.accounts.disagree.iter().collect();
    let bumped = std::sync::Mutex::new(None);
    let edits = Edits {
        accounts_pre: Box::new(|accounts| {
            let mut accounts = accounts.clone();
            let mut bumped = bumped.lock().unwrap();
            if bumped.is_none() {
                if let Some((credential, account)) = accounts.iter_mut().next() {
                    account.balance += 1;
                    *bumped = Some((*credential, account.balance));
                }
            }
            accounts
        }),
        ..Edits::default()
    };
    let planted = judge(&corpus, &manifests, &edits).await;
    drop(edits);
    let (credential, balance) = bumped
        .into_inner()
        .unwrap()
        .expect("a kept run seeds an account");
    let text = planted.to_string();
    let lines: Vec<&String> = planted
        .state
        .accounts
        .disagree
        .iter()
        .filter(|d| !before.contains(d))
        .collect();
    println!(
        "PLANTED accounts disagree={} new={}",
        planted.state.accounts.disagree.len(),
        lines.len()
    );
    let seeded = format!(" account={credential} ledger=balance={},", balance - 1);
    assert!(lines.first().is_some_and(|d| d.contains(&seeded)), "{text}");
    assert!(
        lines[0].contains(&format!(" dolos=balance={balance},")),
        "{text}"
    );
    let named = format!(" account={credential} ");
    assert!(lines.iter().all(|d| d.contains(&named)), "{text}");
}

#[test]
fn a_dolos_refusal_is_named_by_its_kind_and_classed_by_its_variant() {
    use dolos_core::{ChainError as C, DomainError};
    use ledger_oracle_judge::refusal::{DolosClass, RefusalClass};
    use pallas::ledger::validate::utils::{PostAlonzoError as P, ValidationError as V};
    let refused = |e: C| validate::verdict(&Err(DomainError::ChainError(e)));
    let expected = |kind: &str, class| DolosVerdict::Refused {
        kind: kind.to_string(),
        class,
    };
    let inputs = DolosClass::Rule(RefusalClass::Inputs);
    let cases = [
        (
            C::Phase1ValidationRejected(V::PostAlonzo(P::InputNotInUTxO)),
            expected("InputNotInUTxO", inputs),
        ),
        (
            C::Phase1ValidationRejected(V::PostAlonzo(P::UnsupportedPlutusLanguage)),
            expected("UnsupportedPlutusLanguage", DolosClass::NoRule),
        ),
        (
            C::Phase1ValidationRejected(V::DijkstraInputAlreadySpent),
            expected("DijkstraInputAlreadySpent", inputs),
        ),
        (
            C::Phase1ValidationRejected(V::DijkstraMissingParameters("cost models")),
            expected("DijkstraMissingParameters", DolosClass::NoRule),
        ),
        (
            C::NoActiveEpoch,
            expected("NoActiveEpoch", DolosClass::NoRule),
        ),
    ];
    for (error, verdict) in cases {
        assert_eq!(refused(error), verdict);
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn a_seed_output_compared_as_bytes_names_the_first_differing_byte() {
    let corpus = load(CorpusKind::Supp);
    let run = corpus.kept().next().unwrap();
    let first = &run.records()[0];
    let genesis = seed::genesis(epoch_size(&corpus));
    let seeded = seed::seed(
        &genesis,
        run.epoch().unwrap(),
        &[],
        run.seed(),
        &first.accounts_pre,
    );
    let id = RecordId {
        corpus: corpus.kind,
        n: first.n,
    };
    let none = BTreeMap::new();
    let seed = &run.seed()[..1];
    let mut equal = Tally::default();
    compare_utxos(&seeded.domain, id, [], seed, &none, &mut equal).await;
    assert_eq!((equal.compared, equal.disagree.len()), (1, 0));
    let mut flipped = seed.to_vec();
    flipped[0].output.0[3] ^= 1;
    let mut differ = Tally::default();
    compare_utxos(&seeded.domain, id, [], &flipped, &none, &mut differ).await;
    assert_eq!(differ.disagree.len(), 1);
    assert!(
        differ.disagree[0].ends_with("differ at byte 3 fields=address"),
        "{}",
        differ.disagree[0]
    );
    let input = seed[0].input;
    let wrote_flipped = BTreeMap::from([(input, flipped[0].output.0.clone())]);
    let mut differ = Tally::default();
    compare_utxos(&seeded.domain, id, [], seed, &wrote_flipped, &mut differ).await;
    assert_eq!(differ.disagree.len(), 1);
    assert!(
        differ.disagree[0].ends_with("differ at byte 3"),
        "{}",
        differ.disagree[0]
    );
    let mut indefinite = seed.to_vec();
    let bytes = &mut indefinite[0].output.0;
    bytes[0] = match bytes[0] {
        0x80..=0x97 => 0x9f,
        0xa0..=0xb7 => 0xbf,
        other => panic!("seed output starts with {other:#04x}"),
    };
    bytes.push(0xff);
    let wrote_seed = BTreeMap::from([(input, seed[0].output.0.clone())]);
    let mut equal = Tally::default();
    compare_utxos(&seeded.domain, id, [], &indefinite, &wrote_seed, &mut equal).await;
    assert_eq!(
        (equal.compared, equal.disagree.len()),
        (1, 0),
        "{:?}",
        equal.disagree
    );
    let mut differ = Tally::default();
    compare_utxos(&seeded.domain, id, [], &indefinite, &none, &mut differ).await;
    assert_eq!(differ.disagree.len(), 1);
    assert!(
        differ.disagree[0].ends_with("differ at byte 0"),
        "{}",
        differ.disagree[0]
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn the_seed_point_is_a_rollback_target() {
    let corpus = load(CorpusKind::Supp);
    let run = corpus.kept().next().unwrap();
    let genesis = seed::genesis(epoch_size(&corpus));
    let seeded = seed::seed(
        &genesis,
        run.epoch().unwrap(),
        &[],
        run.seed(),
        &run.records()[0].accounts_pre,
    );
    let rolled = seeded.domain.rollback(seeded.seed.point());
    assert!(rolled.is_ok(), "{rolled:?}");
}
