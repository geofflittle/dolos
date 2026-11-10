//! Judges what Dolos validates and serves against ledger corpora.

mod coverage;
mod endorser;
mod eval;
mod guarding;
mod ledger_state;
mod live;
mod missing_script_or_redeemer;
mod output;
mod params;
mod seed;
mod serve;
mod served_tx;
mod submit;
mod timing;
mod validate;

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

use dolos_cardano::{
    indexes::{AsyncCardanoQueryExt as _, ScriptData, ScriptLanguage},
    model::{AccountState, DRepDelegation},
    FixedNamespace as _,
};
use dolos_core::{
    AsyncQueryFacade, BlockHash, ChainPoint, Domain as _, DomainError, EraCbor, MempoolStore as _,
    MempoolTx, RawBlock, StateStore as _, SubmitExt as _, SyncExt as _, TxHash, TxoRef,
};
use itertools::Itertools as _;
use ledger_oracle_judge::{
    accounts::{self, Account, Credential, LedgerAccounts},
    cbor::{tx_inputs, Purpose, TxIn},
    compare::{self, ByEntry, ByteVerdict, RollFail, StateTallies, Tally},
    context::{ContextKnown, ContextTally},
    corpus::{self, Corpus},
    cost::CostTallies,
    eval::{ledger_rows, Answer, EvalKnown, EvalTally, Layer, TxKnown},
    judgement::{self, Axis, CellKey, EntryPoint, Finding, Instrument, Judge, Route, RunVerdict},
    manifest::{self, Candidates, Declared, Feature, Known, KnownList, Locus},
    parallel::in_parallel,
    record::{CorpusKind, LedgerOutcome, PParams, Phase2, Record, RecordId, UtxoEntry},
    row::Row,
    rules::{self, RuleTally, Rules},
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
        primitives::{conway::DatumOption, dijkstra, PlutusData},
        traverse::{
            ComputeHash as _, Era, MultiEraOutput, MultiEraRedeemerTag as RedeemerTag, MultiEraTx,
            OriginalHash as _,
        },
        validate::phase2::{tx::MultiEraTxEvalResult as TxEvalResult, Builtin},
    },
};
use seed::SeedPoint;
use serve::{Event, RouteServed};
use served_tx::EveryVersionMaps;
use timing::{Stopwatch, Timing};
use tokio::{runtime::Handle, task};
use validate::Relay;

use crate::adapters::DomainAdapter;

fn load(kind: CorpusKind) -> Corpus {
    corpus::load(kind).unwrap_or_else(|e| panic!("{e}"))
}

fn parsed_differing(
    id: RecordId,
    ledger: &[u8],
    parsed: Option<&output::ServedOutput>,
) -> Vec<OutputField> {
    match parsed {
        Some(parsed) => {
            output::differing(ledger, parsed).unwrap_or_else(|e| panic!("OUTPUT {id} {e}"))
        }
        None => vec![OutputField::ParsedState],
    }
}

/// The ledger bytes of each expected entry, and every input to read, those named and those expected.
fn utxo_keys<'a>(
    inputs: impl IntoIterator<Item = &'a TxIn>,
    expected: impl IntoIterator<Item = &'a UtxoEntry>,
) -> (BTreeMap<&'a TxIn, &'a [u8]>, Vec<&'a TxIn>) {
    let expected: BTreeMap<&TxIn, &[u8]> = expected
        .into_iter()
        .map(|e| (&e.input, e.output.0.as_slice()))
        .collect();
    let mut keys: BTreeSet<&TxIn> = inputs.into_iter().collect();
    keys.extend(expected.keys());
    (expected, keys.into_iter().collect())
}

/// Counts each input and ledger entry where ReadUtxos at v1 serves other
/// parsed fields than the ledger, or other bytes than `written` holds there
/// or else the ledger.
async fn compare_utxos<'a>(
    domain: &DomainAdapter,
    id: RecordId,
    inputs: impl IntoIterator<Item = &'a TxIn>,
    expected: impl IntoIterator<Item = &'a UtxoEntry>,
    written: &BTreeMap<TxIn, Vec<u8>>,
    tally: &mut Tally,
) {
    let (expected, keys) = utxo_keys(inputs, expected);
    let (_, served) = serve::v1::read_utxos(domain, &keys).await;
    compare_served_utxos(&served, id, &keys, &expected, written, tally);
}

/// Counts, at each version, each input and ledger entry where ReadUtxos
/// serves other parsed fields than the ledger, or other bytes than `written`
/// holds there or else the ledger, and each ledger entry and found output
/// where SearchUtxos by the address of each ledger entry does.
async fn compare_utxos_at_every_version<'a>(
    domain: &DomainAdapter,
    id: RecordId,
    inputs: impl IntoIterator<Item = &'a TxIn>,
    expected: impl IntoIterator<Item = &'a UtxoEntry>,
    written: &BTreeMap<TxIn, Vec<u8>>,
    tallies: &mut ByEntry,
) {
    let (expected, keys) = utxo_keys(inputs, expected);
    for (entry, served) in serve::read_utxos(domain, &keys).await {
        compare_served_utxos(&served, id, &keys, &expected, written, tallies.at(entry));
    }
    let addresses = addresses(id, expected.values().copied());
    let addresses: Vec<&[u8]> = addresses.iter().map(Vec::as_slice).collect();
    for (entry, served) in serve::search_utxos(domain, &addresses).await {
        let found: BTreeSet<&TxIn> = expected.keys().copied().chain(served.keys()).collect();
        let found: Vec<&TxIn> = found.into_iter().collect();
        compare_served_utxos(&served, id, &found, &expected, written, tallies.at(entry));
    }
}

/// The distinct address of each output.
fn addresses<'a>(id: RecordId, outputs: impl IntoIterator<Item = &'a [u8]>) -> BTreeSet<Vec<u8>> {
    let address = |bytes: &[u8]| {
        let output = MultiEraOutput::decode(Era::Dijkstra, bytes)
            .unwrap_or_else(|e| panic!("OUTPUT {id} {e}"));
        let address = output
            .address()
            .unwrap_or_else(|e| panic!("OUTPUT {id} {e}"));
        address.to_vec()
    };
    outputs.into_iter().map(address).collect()
}

/// Counts each of `keys` where `served` holds other parsed fields than the
/// ledger, or other bytes than `written` holds there or else the ledger.
fn compare_served_utxos(
    served: &serve::Utxos,
    id: RecordId,
    keys: &[&TxIn],
    expected: &BTreeMap<&TxIn, &[u8]>,
    written: &BTreeMap<TxIn, Vec<u8>>,
    tally: &mut Tally,
) {
    for &input in keys {
        let ledger = expected.get(input).copied();
        let bytes = ledger.map(|l| written.get(input).map_or(l, Vec::as_slice));
        let item = served.get(input);
        let Some(verdict) = ByteVerdict::of(bytes, item.map(|i| i.bytes.as_slice())) else {
            tally.agree();
            continue;
        };
        let fields = match (ledger, item) {
            (Some(ledger), Some(item)) => parsed_differing(id, ledger, item.parsed.as_ref()),
            (None, _) | (_, None) => Vec::new(),
        };
        tally.add(verdict.agrees() && fields.is_empty(), || {
            let shown = show_input(input);
            let mut cells = vec![verdict.cell()];
            cells.extend(fields.iter().map(|f| CellKey::new(Axis::Field, f)));
            let detail = match fields.is_empty() {
                true => format!("input={shown} {verdict}"),
                false => format!("input={shown} {verdict} fields={}", fields.iter().join(",")),
            };
            Finding::new(id, cells, detail)
        });
    }
}

fn show_input(input: &TxIn) -> String {
    format!("{}#{}", hex::encode(input.tx), input.ix)
}

/// The validation outcomes, phase one rules, state comparisons, fee, redeemer
/// evaluation and script context comparisons, manifest candidates and pass
/// times of one corpus.
#[derive(Debug, Default)]
struct Judged {
    validation: ValidationTally,
    rules: RuleTally,
    state: StateTallies,
    cost: CostTallies,
    eval: EvalTally,
    context: ContextTally,
    candidates: Candidates,
    timing: Timing,
}

impl Judged {
    /// The verdicts of the validation, state, fee, evaluation and script
    /// context judges.
    fn report(&self) -> judgement::Tally {
        let mut out = judgement::Tally::default();
        self.validation.report(&mut out);
        self.state.report(&mut out);
        self.cost.report(&mut out);
        self.eval.report(&mut out);
        self.context.report(&mut out);
        out.require(&[(Judge::Validation, Instrument::ReceiveTx.into())]);
        out
    }

    /// Adds the judgement of another part of the corpus after this one's.
    fn merge(&mut self, other: Judged) {
        let Judged {
            validation,
            rules,
            state,
            cost,
            eval,
            context,
            candidates: Candidates { declared, known },
            timing,
        } = other;
        let Known { records, live } = known;
        self.timing.merge(timing);
        self.validation.merge(validation);
        self.rules.merge(rules);
        self.state.merge(state);
        self.cost.merge(cost);
        self.eval.merge(eval);
        self.context.merge(context);
        self.candidates.declared.extend(declared);
        self.candidates.known.records.extend(records);
        self.candidates.known.live.extend(live);
    }
}

impl fmt::Display for Judged {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\n{}\n{}", self.validation, self.state, self.cost)
    }
}

/// One agreeing record whose corpus compared each state tally but the ones named in `uncompared`.
#[cfg(test)]
fn judged_part(uncompared: &[&str]) -> Judged {
    let mut judged = Judged::default();
    judged.validation.agree.insert(supp(1), Vec::new());
    let state = &mut judged.state;
    let tallies = [
        ("accounts", &mut state.accounts),
        ("undo", &mut state.undo),
        ("script", &mut state.script),
        ("plutus_data", &mut state.plutus_data),
    ];
    for (name, tally) in tallies {
        if !uncompared.contains(&name) {
            tally.agree();
        }
    }
    let by_entry = [
        ("utxo", &mut state.utxo, compare::SERVED_UTXOS),
        ("datum", &mut state.datum, compare::SERVED_UTXOS),
        ("params", &mut state.params, compare::SERVED_PARAMS),
        ("as_output", &mut state.as_output, compare::SERVED_OUTPUTS),
        ("served_tx", &mut state.served_tx, compare::SERVED_TXS),
        ("readtx", &mut state.readtx, compare::SERVED_TXS),
        ("submit", &mut state.submit, compare::SERVED_SUBMIT),
    ];
    for (name, tallies, routes) in by_entry {
        if !uncompared.contains(&name) {
            compare::at_every_version(routes).for_each(|entry| tallies.at(entry).agree());
        }
    }
    judged
}

/// The text of each disagreement.
#[cfg(test)]
fn shown<'a>(findings: impl IntoIterator<Item = &'a Finding>) -> Vec<String> {
    findings.into_iter().map(Finding::to_string).collect()
}

/// The failing verdict lines of `judge` in a corpus judgement.
#[cfg(test)]
fn failing(judged: &Judged, judge: Judge) -> Vec<String> {
    let failing = judged.report().failing.into_iter();
    let of_judge = failing.filter(|v| v.judge == judge);
    of_judge.map(|v| v.to_string()).collect()
}

/// Whether `judged` holds a failing verdict of `judge` on `record` whose line
/// ends with `tail`.
#[cfg(test)]
fn fails_with(judged: &Judged, judge: Judge, record: &str, tail: &str) -> bool {
    let record = format!(" record={record} ");
    let lines = failing(judged, judge);
    lines
        .iter()
        .any(|l| l.contains(&record) && l.ends_with(tail))
}

#[cfg(test)]
fn passes(judged: &Judged) -> bool {
    matches!(judged.report().verdict(), RunVerdict::Pass { .. })
}

#[test]
fn a_selection_passes_when_its_corpus_parts_together_compare_each_state_tally() {
    let mut selection = judged_part(&["accounts", "datum"]);
    assert!(!passes(&selection));
    selection.merge(judged_part(&["script"]));
    assert!(passes(&selection));

    let mut neither = judged_part(&["accounts"]);
    neither.merge(judged_part(&["accounts", "datum"]));
    assert!(!passes(&neither));
}

#[test]
fn a_selection_fails_on_a_fee_disagreement_alone() {
    let mut selection = judged_part(&[]);
    assert!(passes(&selection));
    let finding = Finding::new(
        supp(1),
        vec![CellKey::new(Axis::Answer, "differ")],
        "ledger=7 dolos=8",
    );
    selection.cost.fee.add(false, || finding);
    assert!(!passes(&selection));
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
        RedeemerTag::Guarding => 6,
        other => panic!("unsupported ledger-oracle redeemer purpose: {other:?}"),
    }
}

/// The purpose of one evaluated redeemer in the ledger's terms.
fn purpose(r: &TxEvalResult) -> Purpose {
    Purpose {
        tag: tag_number(&r.tag),
        index: r.index.into(),
    }
}

#[test]
fn guarding_result_uses_ledger_tag_and_original_index() {
    let result = TxEvalResult {
        sub_transaction: None,
        tag: RedeemerTag::Guarding,
        index: 9,
        units: pallas::ledger::primitives::ExUnits { mem: 11, steps: 23 },
        success: true,
        logs: vec![],
        failure_message: None,
    };
    assert_eq!(purpose(&result), Purpose { tag: 6, index: 9 });
}

#[test]
fn each_tag_the_dijkstra_decoder_accepts_has_its_ledger_name_and_number() {
    use pallas::{codec::minicbor, ledger::primitives::dijkstra::RedeemerTag as Tag};
    let mut decoded = 0;
    for n in 0..=u8::MAX {
        let bytes = minicbor::to_vec(n).unwrap();
        match minicbor::decode::<Tag>(&bytes) {
            Ok(tag) => {
                decoded += 1;
                assert_eq!(
                    ledger_oracle_judge::eval::tag_name(n.into()),
                    format!("{tag:?}")
                );
                assert_eq!(tag_number(&tag.into()), u64::from(n), "{tag:?}");
            }
            Err(_) => assert_eq!(ledger_oracle_judge::eval::tag_name(n.into()), n.to_string()),
        }
    }
    assert!(decoded > 0);
}

/// Counts the minimum fee Dolos computes for one record against the ledger's.
fn compare_fee(group: &validate::Group, id: RecordId, record: &Record, cost: &mut CostTallies) {
    let fee = match group.min_fee(&record.tx.0) {
        Ok(Some(fee)) => Ok(fee),
        Ok(None) => Err("outside Dijkstra".to_string()),
        Err(e) => Err(e.to_string()),
    };
    cost.add_fee(id, record.min_fee, fee, &record.outcome);
}

/// The declared support and the known lists a judgement reads.
#[derive(Debug, Clone)]
struct Manifests {
    declared: Declared,
    known: KnownList,
    eval: EvalKnown,
    tx: TxKnown,
    rules: Rules,
    context: ContextKnown,
}

impl Manifests {
    fn committed() -> Self {
        // An arm catch naming no judge or axis fails every run here.
        manifest::catches().unwrap_or_else(|e| panic!("{e}"));
        Manifests {
            declared: manifest::declared().unwrap_or_else(|e| panic!("{e}")),
            known: manifest::known().unwrap_or_else(|e| panic!("{e}")).records,
            eval: ledger_oracle_judge::eval::eval_known().unwrap_or_else(|e| panic!("{e}")),
            tx: ledger_oracle_judge::eval::tx_known().unwrap_or_else(|e| panic!("{e}")),
            rules: rules::committed().unwrap_or_else(|e| panic!("{e}")),
            context: ledger_oracle_judge::context::context_known()
                .unwrap_or_else(|e| panic!("{e}")),
        }
    }

    /// The entries that name one of the records.
    fn within(&self, records: &BTreeSet<RecordId>) -> Self {
        let declared = self.declared.iter().filter(|(id, _)| records.contains(id));
        let known = self
            .known
            .iter()
            .filter(|((_, id), _)| records.contains(id));
        let eval = self.eval.iter().filter(|(id, _)| records.contains(id));
        let tx = self.tx.iter().filter(|(id, _)| records.contains(id));
        let context = self.context.iter().filter(|(id, _)| records.contains(id));
        Manifests {
            declared: declared.map(|(id, f)| (*id, f.clone())).collect(),
            known: known.map(|(entry, row)| (*entry, *row)).collect(),
            eval: eval.map(|(id, rows)| (*id, rows.clone())).collect(),
            tx: tx.map(|(id, answer)| (*id, answer.clone())).collect(),
            rules: self.rules.within(records),
            context: context.map(|(id, rows)| (*id, rows.clone())).collect(),
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
            tally.add(*id, Outcome::Fail(fail), Vec::new());
        }
        let validation = |locus: &Locus| *locus == Locus::Validation;
        let entries = self.known.iter();
        for ((_, id), row) in entries.filter(|((locus, id), _)| validation(locus) && missed(id)) {
            let fail = Fail::KnownStale { row: *row };
            tally.add(*id, Outcome::Fail(fail), Vec::new());
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

/// The coverage cells of a record Dolos agreed on, the transaction cells and the builtins
/// `builtins` its evaluation called when the ledger accepted it, its leaves when the ledger
/// refused it.
fn agreeing_cells(id: RecordId, record: &Record, builtins: &[Builtin]) -> Vec<CellKey> {
    match &record.outcome {
        LedgerOutcome::Accepted { .. } => {
            let cells = coverage::cells_of_tx(&record.tx.0);
            let mut cells = cells.unwrap_or_else(|e| panic!("TX {id} {e}"));
            let builtins = builtins.iter().map(|b| format!("{b:?}"));
            cells.extend(builtins.map(|b| CellKey::new(Axis::Builtin, b)));
            cells.into_iter().collect()
        }
        LedgerOutcome::Refused { failure_names, .. } => {
            let leaves = failure_names
                .iter()
                .map(|f| CellKey::new(Axis::Leaf, f.leaf));
            leaves.collect::<BTreeSet<_>>().into_iter().collect()
        }
    }
}

/// Every output reference a record's transaction and sub transactions could produce, each output index and the index after them that a collateral return takes.
fn producible(id: RecordId, record: &Record) -> Vec<TxIn> {
    let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &record.tx.0)
        .unwrap_or_else(|e| panic!("TX {id} {e}"));
    let refs = named_txs(tx).into_iter().flat_map(|(_, t)| {
        let hash = *t.hash();
        (0..=t.outputs().len() as u64).map(move |ix| TxIn { tx: hash, ix })
    });
    refs.collect()
}

/// The bytes of each output a record's transaction and sub transactions write,
/// its collateral return at the index after its outputs, none when the
/// transaction does not decode.
fn written(record: &Record) -> BTreeMap<TxIn, Vec<u8>> {
    let Ok(tx) = MultiEraTx::decode_for_era(Era::Dijkstra, &record.tx.0) else {
        return BTreeMap::new();
    };
    let txs = named_txs(tx);
    let outputs = txs.iter().flat_map(|(_, t)| {
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

/// A block a run rolled, with the records it holds, their slot group, and its
/// first record when Dolos accepted that record on submission.
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

/// Each record of the groups whose block rolled or was never made, with the hash of its block when one rolled.
fn judged_records<'a, 'r>(
    blocks: &'a [GroupBlock<'r>],
) -> impl Iterator<Item = (&'r Record, Option<[u8; 32]>)> + 'a {
    let groups = blocks.iter().filter_map(|block| match block {
        GroupBlock::Unmade(records) => Some((*records, None)),
        GroupBlock::Rolled(rolled) => Some((rolled.group, rolled.point.hash().map(|h| *h))),
        GroupBlock::Failed => None,
    });
    groups.flat_map(|(records, block)| records.iter().map(move |r| (r, block)))
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
        rules,
        state,
        cost,
        eval: evaluations,
        context,
        candidates,
        timing,
    } = tallies;
    let mut watch = Stopwatch::start();
    let unlisted = BTreeMap::new();
    let context_unlisted = BTreeMap::new();
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
        let routed = validate::group(base, records[0].slot, Relay::Held);
        let mut watching = submit::Watching::open(routed.domain()).await;
        watch.charge(&mut timing.groups);
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
            let mut builtins = Vec::new();
            watch.charge(&mut timing.receive);
            if missing.failed_by.is_empty() {
                compare_fee(&group, id, record, cost);
                let listed = manifests.eval.get(&id).unwrap_or(&unlisted);
                let tx_listed = manifests.tx.get(&id).map(String::as_str);
                let calls =
                    eval::judge_record(&group, id, record, listed, tx_listed, evaluations).await;
                let listed = manifests.context.get(&id).unwrap_or(&context_unlisted);
                eval::judge_context(&group, id, record, listed, context);
                let covering = (record.submitted(), record.flag(), declared);
                if let (LedgerOutcome::Accepted { .. }, Phase2::Valid, None) = covering {
                    builtins = calls;
                }
            } else {
                cost.skipped_cascade += 1;
                evaluations.skipped_cascade += 1;
            }
            watch.charge(&mut timing.evaluate);
            let submit = |group: &validate::Group| match record.flag() {
                Phase2::Valid => group.receive(&record.tx.0),
                // A submitted transaction carries no flag, so one the ledger
                // reads under a false flag is validated and never held.
                Phase2::Invalid => group.check(&record.tx.0),
            };
            let received = submit(&group);
            let dolos = validate::verdict(&received);
            let inflight = validate::verdict(&submit(&relayed));
            if record.flag() == Phase2::Valid {
                let decoded = MultiEraTx::decode_for_era(Era::Dijkstra, &record.tx.0);
                let hash = match &received {
                    Ok(hash) => Some(**hash),
                    Err(_) => decoded.ok().map(|tx| *tx.hash()),
                };
                let (domain, tx) = (routed.domain(), &record.tx.0);
                let tallies = &mut state.submit;
                watch.charge(&mut timing.receive);
                submit::compare(domain, &mut watching, id, tx, hash, &dolos, tallies).await;
                watch.charge(&mut timing.submit_routes);
            }
            if matches!(dolos, DolosVerdict::Accepted) {
                dolos_accepted.insert(record.n);
            }
            if let Some(feature) = declares(&dolos) {
                candidates.declared.insert(id, feature);
            }
            let judgement = match one_answer(&dolos, &inflight) {
                Ok(dolos) => {
                    if missing.failed_by.is_empty() {
                        let ledger = record.submitted();
                        let tx = &record.tx.0;
                        let read = manifests.rules.judge(id, ledger, dolos, tx, rules);
                        read.unwrap_or_else(|e| panic!("TX {id} {e}"));
                    }
                    outcome(record.submitted(), dolos, declared)
                }
                Err(fail) => Outcome::Fail(fail),
            };
            let judgement = with_failing_makers(judgement, &dolos, missing.failed_by);
            let known = manifests.known.get(&(Locus::Validation, id));
            let judgement = with_known(judgement, known);
            let cells = match &judgement {
                Outcome::Pass(Pass::Agree) => agreeing_cells(id, record, &builtins),
                _ => Vec::new(),
            };
            if let Outcome::Pass(Pass::Known { row, .. }) = &judgement {
                let entry = (Locus::Validation, id);
                candidates.known.records.insert(entry, *row);
            }
            given.insert(id, maker(id, &dolos, &judgement));
            tally.add(id, judgement, cells);
            judged.insert(id);
            watch.charge(&mut timing.receive);
        }
        let accepted = records.iter().filter_map(|r| match &r.outcome {
            LedgerOutcome::Accepted { accounts_post, .. } => Some((r, accounts_post)),
            LedgerOutcome::Refused { .. } => None,
        });
        let Some((last, accounts_post)) = accepted.clone().next_back() else {
            blocks.push(GroupBlock::Unmade(records));
            watch.charge(&mut timing.roll);
            continue;
        };
        let included: Vec<(&Record, Phase2)> = accepted
            .filter_map(|(r, ..)| Some((r, (edits.block_tx)(id_of(r), r.flag())?)))
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
                watch.charge(&mut timing.roll);
                // A failed block leaves the tip where it was, so the next
                // block names that parent.
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
        watch.charge(&mut timing.roll);
        compare_utxos_at_every_version(
            group.domain(),
            id_of(last),
            inputs,
            run.post(last),
            &wrote,
            &mut state.utxo,
        )
        .await;
        watch.charge(&mut timing.group_state);
        let made = run
            .post(last)
            .filter_map(|e| Some((e, by_n[&makers.get(&e.input)?.n])));
        let made: Vec<(&UtxoEntry, &Record)> = made.collect();
        compare_datums(group.domain(), id_of(last), &made, edits, &mut state.datum).await;
        watch.charge(&mut timing.datums);
        let served = dolos_accounts(group.domain());
        let checks = account_checks.entry(id_of(last)).or_default();
        accounts::compare(id_of(last), accounts_post, &served, checks);
        watch.charge(&mut timing.group_state);
    }
    (blocks, account_checks)
}

/// The datum ReadUtxos serves for an output with a datum hash, by whether the transaction that made the output witnesses that datum.
enum ExpectedDatum {
    Witnessed(Vec<u8>),
    Unwitnessed,
}

/// Counts the datum ReadUtxos and SearchUtxos serve for each ledger output with a datum hash against the datum its making transaction witnesses, or against the hash when it witnesses none.
async fn compare_datums(
    domain: &DomainAdapter,
    id: RecordId,
    made: &[(&UtxoEntry, &Record)],
    edits: &Edits<'_>,
    tally: &mut ByEntry,
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
        let txs = named_txs(tx);
        let making = txs.iter().find(|(_, t)| *t.hash() == entry.input.tx);
        let datum = making
            .into_iter()
            .flat_map(|(_, t)| t.plutus_data())
            .find(|d| d.original_hash() == hash);
        let ledger = match datum {
            Some(d) => {
                ExpectedDatum::Witnessed((edits.expected_datum)(maker_id, d.raw_cbor().to_vec()))
            }
            None => ExpectedDatum::Unwitnessed,
        };
        expected.insert(entry.input, (maker_id, *hash, ledger));
    }
    let inputs: Vec<&TxIn> = expected.keys().collect();
    let made = made.iter().filter(|(e, _)| expected.contains_key(&e.input));
    let addresses = addresses(id, made.map(|(e, _)| e.output.0.as_slice()));
    let addresses: Vec<&[u8]> = addresses.iter().map(Vec::as_slice).collect();
    let read = serve::read_utxos(domain, &inputs).await;
    let searched = serve::search_utxos(domain, &addresses).await;
    for (entry, served) in read.into_iter().chain(searched) {
        compare_served_datums(&served, id, &expected, tally.at(entry));
    }
}

/// Counts the datum one route serves for each ledger output with a datum hash against the expected datum.
fn compare_served_datums(
    served: &serve::Utxos,
    id: RecordId,
    expected: &BTreeMap<TxIn, (RecordId, [u8; 32], ExpectedDatum)>,
    tally: &mut Tally,
) {
    for (input, (maker, hash, ledger)) in expected {
        let served = served
            .get(input)
            .and_then(|u| u.parsed.as_ref()?.datum_cbor());
        let (agrees, dolos, cell) = match ledger {
            ExpectedDatum::Witnessed(bytes) => {
                let verdict = ByteVerdict::against(bytes, served);
                (verdict.agrees(), verdict.to_string(), verdict.cell())
            }
            ExpectedDatum::Unwitnessed => {
                let cell = CellKey::new(Axis::Datum, "unwitnessed");
                match served {
                    None => (true, "unwitnessed dolos=none".to_string(), cell),
                    Some(bytes) => {
                        let served_hash = *Hasher::<256>::hash(bytes);
                        let dolos = format!("unwitnessed dolos_hash={}", hex::encode(served_hash));
                        (served_hash == *hash, dolos, cell)
                    }
                }
            }
        };
        tally.add(agrees, || {
            let detail = format!(
                "input={} made_by={maker} datum={} {dolos}",
                show_input(input),
                hex::encode(hash)
            );
            Finding::new(id, vec![cell], detail)
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

/// The raw body, witness set and auxiliary data of a Dijkstra transaction, with whether the block producer marks it successful when the transaction is not a sub transaction.
#[derive(Debug, PartialEq, Eq)]
struct TxParts {
    body: Vec<u8>,
    witness: Vec<u8>,
    auxiliary: Option<Vec<u8>>,
    success: Option<bool>,
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
            success: Some(phase2 == Phase2::Valid),
        })
    }

    /// The parts of the sub transaction with the hash that a ledger transaction lists, none when it lists no such sub transaction.
    fn ledger_sub(bytes: &[u8], hash: &[u8; 32]) -> Result<Option<Self>, String> {
        let tx: dijkstra::MempoolTransaction =
            minicbor::decode(bytes).map_err(|e| e.to_string())?;
        let subs = tx.transaction_body.sub_transactions.iter().flatten();
        let mut found =
            subs.filter(|s| *Hasher::<256>::hash(s.sub_transaction_body.raw_cbor()) == *hash);
        Ok(found.next().map(TxParts::of_sub))
    }

    /// The parts of a transaction in the four element form of a block.
    fn block(bytes: &[u8]) -> Result<Self, String> {
        let tx: dijkstra::BlockTransaction = minicbor::decode(bytes).map_err(|e| e.to_string())?;
        Ok(TxParts {
            body: tx.transaction_body.raw_cbor().to_vec(),
            witness: tx.transaction_witness_set.raw_cbor().to_vec(),
            auxiliary: raw_auxiliary(&tx.auxiliary_data),
            success: Some(tx.success),
        })
    }

    /// The parts of a sub transaction in its three element form.
    fn sub(bytes: &[u8]) -> Result<Self, String> {
        let sub: dijkstra::SubTransaction = minicbor::decode(bytes).map_err(|e| e.to_string())?;
        Ok(TxParts::of_sub(&sub))
    }

    /// The parts of a decoded sub transaction.
    fn of_sub(sub: &dijkstra::SubTransaction<'_>) -> Self {
        TxParts {
            body: sub.sub_transaction_body.raw_cbor().to_vec(),
            witness: sub.transaction_witness_set.raw_cbor().to_vec(),
            auxiliary: raw_auxiliary(&sub.auxiliary_data),
            success: None,
        }
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

impl TxName {
    /// Whether the name is of a top level or a sub transaction.
    fn kind(&self) -> &'static str {
        match self {
            TxName::Top(_) => "top",
            TxName::Sub(_) => "sub",
        }
    }
}

/// A transaction and each of its sub transactions, by name.
fn named_txs(tx: MultiEraTx<'_>) -> Vec<(TxName, MultiEraTx<'_>)> {
    let subs = tx.sub_transactions();
    let mut named = vec![(TxName::Top(*tx.hash()), tx)];
    named.extend(subs.into_iter().map(|sub| (TxName::Sub(*sub.hash()), sub)));
    named
}

/// Whether the ledger holds the item a lookup serves, as its cell shows it.
fn held<T>(ledger: &Option<T>) -> &'static str {
    match ledger {
        Some(_) => "held",
        None => "none",
    }
}

impl fmt::Display for TxName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TxName::Top(h) => write!(f, "tx={}", hex::encode(h)),
            TxName::Sub(h) => write!(f, "sub_tx={}", hex::encode(h)),
        }
    }
}

/// Counts what ReadTx serves at every version for each transaction hash of the groups whose block rolled or was never made, against the block and transaction of the accepted record that names it, the sub transaction itself at a version that states no sub transactions, or nothing when only refused records do.
async fn compare_read_tx(
    domain: &DomainAdapter,
    kind: CorpusKind,
    blocks: &[GroupBlock<'_>],
    tallies: &mut ByEntry,
    _: &EveryVersionMaps,
) {
    let mut expected: BTreeMap<TxName, (RecordId, Option<ExpectedTx>)> = BTreeMap::new();
    for (record, block) in judged_records(blocks) {
        let id = RecordId {
            corpus: kind,
            n: record.n,
        };
        let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &record.tx.0)
            .unwrap_or_else(|e| panic!("TX {id} {e}"));
        let ledger = match &record.outcome {
            LedgerOutcome::Accepted { .. } => Some(ExpectedTx {
                slot: record.slot,
                block,
                tx: &record.tx.0,
                phase2: record.flag(),
            }),
            LedgerOutcome::Refused { .. } => None,
        };
        for (name, _) in named_txs(tx) {
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
        for found in serve::read_tx_found(domain, &hash).await {
            let (agrees, dolos) = match (&ledger, found.answer) {
                (_, Err(status)) => (false, format!("status={:?}", status.code())),
                (None, Ok(None)) => (true, "none".to_string()),
                (Some(_), Ok(None)) => (false, "none".to_string()),
                (None, Ok(Some(served))) => (false, served.block.to_string()),
                (Some(e), Ok(Some(served))) => {
                    let (ledger, parts) = match (name, found.subs) {
                        (TxName::Sub(_), false) => {
                            let sub = TxParts::ledger_sub(e.tx, &hash)
                                .unwrap_or_else(|err| panic!("TX {id} {err}"))
                                .unwrap_or_else(|| panic!("TX {id} lists no {name}"));
                            (sub, TxParts::sub(&served.bytes))
                        }
                        _ => {
                            let tx = TxParts::ledger(e.tx, e.phase2)
                                .unwrap_or_else(|err| panic!("TX {id} {err}"));
                            (tx, TxParts::block(&served.bytes))
                        }
                    };
                    let differing = match parts {
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
            tallies.at(found.entry).add(agrees, || {
                let cells = vec![CellKey::new(Axis::Tx, name.kind())];
                Finding::new(id, cells, format!("{name} ledger={shown} dolos={dolos}"))
            });
        }
    }
}

/// Where a script or datum appears, in the witnesses of a transaction or on an output.
#[derive(Debug, Clone, Copy)]
enum CarriedAt {
    Witness(TxName),
    Output(TxIn),
}

impl fmt::Display for CarriedAt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CarriedAt::Witness(name) => write!(f, "witness,{name}"),
            CarriedAt::Output(input) => write!(f, "output={}", show_input(input)),
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

/// A kind of item a transaction witnesses or an output carries, named by its hash and served by a lookup.
trait Carried {
    /// The hash that names an item.
    type Hash: Ord + Copy + AsRef<[u8]>;
    /// What an accepted record holds of an item.
    type Held;
    /// What the lookup serves for an item.
    type Served;
    /// The key of an item's hash on a finding line.
    const KEY: &'static str;
    /// The hash and held part of each item the transaction witnesses.
    fn witnessed(tx: &MultiEraTx<'_>) -> Vec<(Self::Hash, Self::Held)>;
    /// The hash of the item the output names, whether or not the output holds it.
    fn named(output: &MultiEraOutput<'_>) -> Option<Self::Hash>;
    /// The hash and held part of the item the output holds.
    fn held(output: &MultiEraOutput<'_>) -> Option<(Self::Hash, Self::Held)>;
    /// What the lookup serves for the hash.
    async fn served(
        query: &AsyncQueryFacade<DomainAdapter>,
        hash: Self::Hash,
    ) -> Result<Option<Self::Served>, DomainError>;
    /// The ledger side of a finding line for a held item at its place.
    fn shown(held: &Self::Held, at: CarriedAt) -> String;
    /// The Dolos side of a finding line for a served item the ledger does not hold.
    fn unheld(served: &Self::Served) -> String;
    /// Whether the served item is the held one, with the Dolos side of its finding line.
    fn judged(
        id: RecordId,
        hash: Self::Hash,
        held: &Self::Held,
        served: Self::Served,
    ) -> (bool, String);
}

/// The record that first names each item hash, with the held item and its place once a record holds it.
type Expected<C> =
    BTreeMap<<C as Carried>::Hash, (RecordId, Option<(<C as Carried>::Held, CarriedAt)>)>;

/// Counts each item hash of the groups whose block rolled or was never made against what the lookup serves for it, the item in the witnesses or on the produced outputs of an accepted record, or nothing otherwise.
async fn compare_carried<C: Carried>(
    domain: &DomainAdapter,
    kind: CorpusKind,
    blocks: &[GroupBlock<'_>],
    tally: &mut Tally,
) {
    let mut expected: Expected<C> = BTreeMap::new();
    for (record, _) in judged_records(blocks) {
        let id = RecordId {
            corpus: kind,
            n: record.n,
        };
        let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &record.tx.0)
            .unwrap_or_else(|e| panic!("TX {id} {e}"));
        let mut carried = Vec::new();
        for (name, tx) in named_txs(tx) {
            let witnessed = C::witnessed(&tx).into_iter();
            carried.extend(witnessed.map(|(h, held)| (h, held, CarriedAt::Witness(name))));
            for hash in tx.outputs().iter().filter_map(C::named) {
                expected.entry(hash).or_insert((id, None));
            }
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
            if let Some((hash, held)) = C::held(&output) {
                carried.push((hash, held, CarriedAt::Output(entry.input)));
            }
        }
        for (hash, held, at) in carried {
            let entry = expected.entry(hash).or_insert((id, None));
            if entry.1.is_none() {
                *entry = (id, Some((held, at)));
            }
        }
    }
    let query = AsyncQueryFacade::new(domain.clone());
    for (hash, (id, ledger)) in expected {
        let shown = match &ledger {
            Some((held, at)) => C::shown(held, *at),
            None => "none".to_string(),
        };
        let (agrees, dolos) = match (&ledger, C::served(&query, hash).await) {
            (_, Err(e)) => (false, format!("error={e:?}")),
            (None, Ok(None)) => (true, "none".to_string()),
            (Some(_), Ok(None)) => (false, "none".to_string()),
            (None, Ok(Some(served))) => (false, C::unheld(&served)),
            (Some((held, _)), Ok(Some(served))) => C::judged(id, hash, held, served),
        };
        tally.add(agrees, || {
            let detail = format!(
                "{}={} ledger={shown} dolos={dolos}",
                C::KEY,
                hex::encode(hash)
            );
            Finding::new(id, vec![CellKey::new(Axis::Ledger, held(&ledger))], detail)
        });
    }
}

/// Scripts, served by the script lookup and judged by language and hash.
struct Script;

impl Carried for Script {
    type Hash = [u8; 28];
    type Held = ScriptLanguage;
    type Served = ScriptData;
    const KEY: &'static str = "script";

    fn witnessed(tx: &MultiEraTx<'_>) -> Vec<([u8; 28], ScriptLanguage)> {
        witness_scripts(tx)
    }

    fn named(output: &MultiEraOutput<'_>) -> Option<[u8; 28]> {
        output_script(output).map(|(hash, _)| hash)
    }

    fn held(output: &MultiEraOutput<'_>) -> Option<([u8; 28], ScriptLanguage)> {
        output_script(output)
    }

    async fn served(
        query: &AsyncQueryFacade<DomainAdapter>,
        hash: [u8; 28],
    ) -> Result<Option<ScriptData>, DomainError> {
        query.script_by_hash(&hash.into()).await
    }

    fn shown(language: &ScriptLanguage, at: CarriedAt) -> String {
        format!("{language:?},{at}")
    }

    fn unheld(served: &ScriptData) -> String {
        format!("{:?}", served.language)
    }

    fn judged(
        _: RecordId,
        hash: [u8; 28],
        language: &ScriptLanguage,
        served: ScriptData,
    ) -> (bool, String) {
        let rehashed = *Hasher::<224>::hash_tagged(&served.script, served.language.tag());
        let dolos = match rehashed == hash {
            true => format!("{:?}", served.language),
            false => format!("{:?},hash={}", served.language, hex::encode(rehashed)),
        };
        (served.language == *language && rehashed == hash, dolos)
    }
}

/// The hash of the datum an output names, by hash or inline.
fn output_datum_hash(output: &MultiEraOutput<'_>) -> Option<[u8; 32]> {
    match output.datum()? {
        DatumOption::Hash(hash) => Some(*hash),
        DatumOption::Data(data) => Some(*data.original_hash()),
    }
}

/// Datums, served by the datum lookup and judged by their decoded value.
struct Datum;

impl Carried for Datum {
    type Hash = [u8; 32];
    type Held = Vec<u8>;
    type Served = PlutusData;
    const KEY: &'static str = "datum";

    fn witnessed(tx: &MultiEraTx<'_>) -> Vec<([u8; 32], Vec<u8>)> {
        let data = tx.plutus_data();
        let data = data
            .iter()
            .map(|d| (*d.original_hash(), d.raw_cbor().to_vec()));
        data.collect()
    }

    fn named(output: &MultiEraOutput<'_>) -> Option<[u8; 32]> {
        output_datum_hash(output)
    }

    fn held(output: &MultiEraOutput<'_>) -> Option<([u8; 32], Vec<u8>)> {
        match output.datum()? {
            DatumOption::Data(data) => Some((*data.original_hash(), data.raw_cbor().to_vec())),
            DatumOption::Hash(_) => None,
        }
    }

    async fn served(
        query: &AsyncQueryFacade<DomainAdapter>,
        hash: [u8; 32],
    ) -> Result<Option<PlutusData>, DomainError> {
        query.plutus_data(&hash.into()).await
    }

    fn shown(_: &Vec<u8>, at: CarriedAt) -> String {
        at.to_string()
    }

    fn unheld(_: &PlutusData) -> String {
        "datum".to_string()
    }

    fn judged(id: RecordId, _: [u8; 32], bytes: &Vec<u8>, served: PlutusData) -> (bool, String) {
        let datum: PlutusData =
            minicbor::decode(bytes).unwrap_or_else(|e| panic!("DATUM {id} {e}"));
        let same = datum == served;
        (same, if same { "same" } else { "different" }.to_string())
    }
}

/// What every served route at every version serves for the inputs of the blocks a run rolled.
async fn serve_rolled(
    domain: &DomainAdapter,
    seed: &SeedPoint,
    rolled: &[&Rolled<'_>],
    _: &EveryVersionMaps,
) -> Vec<RouteServed> {
    let hashes: Vec<BlockHash> = rolled
        .iter()
        .map(|r| r.point.hash().expect("a rolled block has a hash"))
        .collect();
    let txs = rolled.iter().map(|r| r.records.len()).sum();
    let streams = serve::Streams::open(domain, seed.point()).await;
    let mut served = streams.read(Event::Apply, hashes.len(), txs).await;
    let tx_hashes: Vec<[u8; 32]> = rolled
        .iter()
        .flat_map(|r| &r.records)
        .map(|record| {
            let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &record.tx.0)
                .unwrap_or_else(|e| panic!("TX n={} {e}", record.n));
            *tx.hash()
        })
        .collect();
    served.extend(serve::fetched(domain, &hashes, &tx_hashes).await);
    served
}

/// The verdict on one served output against the ledger's output bytes, none when its bytes and parsed fields agree.
fn served_verdict(
    id: RecordId,
    ledger: &[u8],
    served: Option<&output::ServedOutput>,
) -> Option<String> {
    let Some(served) = served else {
        return Some(ByteVerdict::against(ledger, None).to_string());
    };
    let fields = parsed_differing(id, ledger, Some(served));
    let verdict = match &served.bytes {
        output::ServedBytes::NoField => None,
        output::ServedBytes::Stated(bytes) => Some(ByteVerdict::against(ledger, bytes.as_deref())),
    };
    let agrees = verdict.as_ref().is_none_or(ByteVerdict::agrees);
    let shown = verdict.map_or("fields_only".to_string(), |v| v.to_string());
    match (agrees, fields.is_empty()) {
        (true, true) => None,
        (_, true) => Some(shown),
        (_, false) => Some(format!("{shown} fields={}", fields.iter().join(","))),
    }
}

#[test]
fn a_served_output_stating_no_bytes_disagrees_and_one_stating_the_ledger_bytes_agrees() {
    let address = [[0x58, 0x1d, 0x61].as_slice(), &[0x11; 28]].concat();
    let ledger = [
        vec![0xa2, 0x00],
        address,
        vec![0x01, 0x1a, 0x00, 0x0f, 0x42, 0x40],
    ]
    .concat();
    let id = RecordId {
        corpus: CorpusKind::Supp,
        n: 1,
    };
    let mut served = output::v1::output(&output::mapped(&ledger));
    let stated = served_verdict(id, &ledger, Some(&served));
    served.bytes = output::ServedBytes::Stated(None);
    let unstated = served_verdict(id, &ledger, Some(&served));
    let absent = ByteVerdict::against(&ledger, None).to_string();
    assert_eq!((stated, unstated), (None, Some(absent)));
}

/// The disagreeing verdicts on what each route serves for one input against the ledger's output bytes, by the route's entry point, with the routes left uncompared because their version states no sub transaction.
fn served_verdicts(
    id: RecordId,
    tx: &[u8; 32],
    input: &TxIn,
    sub_only: bool,
    ledger: &[u8],
    routes: &[RouteServed],
) -> (BTreeMap<EntryPoint, Vec<String>>, usize) {
    let mut verdicts = BTreeMap::new();
    let mut unheld = 0;
    for route in routes {
        if sub_only && !route.subs {
            unheld += 1;
            continue;
        }
        let failing: Vec<String> = match route.served.get(tx) {
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
        verdicts.insert(route.entry, failing);
    }
    (verdicts, unheld)
}

/// The served output comparisons of the records of one block, with the inputs left uncompared because the run's seed made them, the compared inputs only sub transactions name, and the route comparisons left out because the route's version states no sub transaction.
#[derive(Debug, Default)]
struct ServedChecks {
    records: BTreeMap<RecordId, ByEntry>,
    seeded: usize,
    sub_only: usize,
    sub_unheld: usize,
}

/// Counts each input a record of the block names that an earlier record made, against what every route serves for it.
fn compare_served(
    kind: CorpusKind,
    run: &KeptRun,
    makers: &BTreeMap<TxIn, RecordId>,
    block: &Rolled<'_>,
    routes: &[RouteServed],
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
        let by_entry = checks.records.entry(id).or_default();
        for input in named_inputs(id, record) {
            let shown = show_input(&input);
            let made = match run.origin(&input, record, makers, block.group) {
                InputOrigin::Seeded => {
                    checks.seeded += 1;
                    continue;
                }
                InputOrigin::AbsentFromPre => {
                    by_entry.at(endorser::FETCH_BLOCK).add(false, || {
                        let cells = vec![CellKey::new(Axis::Input, "absent_from_pre")];
                        Finding::new(id, cells, format!("absent_from_pre input={shown}"))
                    });
                    continue;
                }
                InputOrigin::InRun { made_by } => format!("in_run made_by={made_by}"),
                InputOrigin::InGroup { made_by } => format!("in_group made_by={made_by}"),
            };
            let sub = sub_only.contains(&input);
            if sub {
                checks.sub_only += 1;
            }
            let ledger = &run
                .pre_entry(record, &input)
                .expect("an input made in the run is in the pre state")
                .output;
            let (verdicts, unheld) = served_verdicts(id, &hash, &input, sub, &ledger.0, routes);
            checks.sub_unheld += unheld;
            for (entry, failing) in verdicts {
                by_entry.at(entry).add(failing.is_empty(), || {
                    let cells = vec![CellKey::new(Axis::Entry, entry)];
                    let detail = format!("{} input={shown} {made}", failing.join(","));
                    Finding::new(id, cells, detail)
                });
            }
        }
    }
    checks
}

/// Counts the served outputs of every rolled block into the per record as_output checks.
fn compare_as_output(
    kind: CorpusKind,
    run: &KeptRun,
    rolled: &[&Rolled<'_>],
    routes: &[RouteServed],
    state: &mut StateTallies,
    checks: &mut BTreeMap<RecordId, ByEntry>,
) {
    let makers = run.makers(kind);
    for block in rolled {
        let served = compare_served(kind, run, &makers, block, routes);
        state.seeded_unjudged += served.seeded;
        state.sub_only += served.sub_only;
        state.sub_unheld += served.sub_unheld;
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

/// Queues the block's requeued record unvalidated, behind the held dependents,
/// since the ledger accepted it in the state the rollback of that block
/// restores.
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

/// Rolls a run back to its seed point one block at a time and adds what each
/// step serves to `state`.
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
            .map(|tx| tx.hash())
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
            state.undo.add(false, || {
                let cells = vec![CellKey::new(Axis::Rollback, "refused")];
                Finding::new(id, cells, format!("rollback to={to} {e}"))
            });
            return;
        }
        let rollback = |rid: RecordId, member: &'static str| {
            let cells = vec![CellKey::new(Axis::Rollback, member)];
            Finding::new(rid, cells, format!("{member}_after_rollback_to={to}"))
        };
        for (rid, hash) in &held {
            let dropped = !pending.contains(hash);
            state.revalidate.add(dropped, || rollback(*rid, "kept"));
        }
        if let Some((rid, hash)) = &undone {
            let kept = pending.contains(hash);
            state.revalidate.add(kept, || rollback(*rid, "dropped"));
        }
        let tally = &mut state.undo;
        let routes = streams.read(Event::Undo, 1, block.records.len()).await;
        for (_, checked) in compare_served(kind, run, &makers, block, &routes).records {
            for (_, at_entry) in checked.0 {
                tally.merge(at_entry);
            }
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
    accounts::compare(id, &first.accounts_pre, &dolos_accounts(domain), tally);
}

/// The pre state a planted check seeds for a record in place of its own.
type PreEdit<'a> = Box<dyn Fn(RecordId, &[UtxoEntry]) -> Vec<UtxoEntry> + Sync + 'a>;

/// What a planted check changes in the seed, the expected parameters and the blocks of one judgement.
struct Edits<'a> {
    pre: PreEdit<'a>,
    accounts_pre: Box<dyn Fn(&LedgerAccounts) -> LedgerAccounts + Sync + 'a>,
    expected_params: Box<dyn Fn(&PParams) -> PParams + Sync + 'a>,
    block_tx: Box<dyn Fn(RecordId, Phase2) -> Option<Phase2> + Sync + 'a>,
    rolled_block: Box<dyn Fn(RecordId, RawBlock) -> RawBlock + Sync + 'a>,
    expected_datum: Box<dyn Fn(RecordId, Vec<u8>) -> Vec<u8> + Sync + 'a>,
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

/// The judgement of one kept run, the records it judged and the served output
/// and account comparisons of each of its records.
#[derive(Default)]
struct RunJudged {
    judged: Judged,
    ids: BTreeSet<RecordId>,
    as_output: BTreeMap<RecordId, ByEntry>,
    accounts: BTreeMap<RecordId, Tally>,
}

/// Judges one kept run of a corpus seeded from its first pre state.
async fn judge_run(
    kind: CorpusKind,
    run: &KeptRun,
    manifests: &Manifests,
    edits: &Edits<'_>,
) -> RunJudged {
    let mut part = RunJudged::default();
    let RunJudged {
        judged,
        ids,
        as_output,
        accounts: account_checks,
    } = &mut part;
    let mut watch = Stopwatch::start();
    let first = &run.records()[0];
    let id = RecordId {
        corpus: kind,
        n: first.n,
    };
    let epoch = run
        .epoch()
        .unwrap_or_else(|| panic!("RUN {id} has no epoch"));
    let pparams = params::seed_values(run.pparams()).unwrap_or_else(|e| panic!("{e}"));
    let seeded = seed::seed(
        &seed::genesis(run.era()),
        epoch,
        &pparams,
        &(edits.pre)(id, run.seed()),
        &(edits.accounts_pre)(&first.accounts_pre),
        first,
    );
    let seed = run.seed();
    compare_utxos_at_every_version(
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
    accounts::compare(id, &first.accounts_pre, &served, checks);
    watch.charge(&mut judged.timing.seed);
    let expected = (edits.expected_params)(run.pparams());
    for (version, served) in serve::read_params(&seeded.domain).await {
        let tally = judged
            .state
            .params
            .at(EntryPoint::Route(Route::ReadParams, version));
        params::compare(&expected, &served, version, tally, |key, d| {
            Finding::new(id, vec![CellKey::new(Axis::Param, key)], d)
        });
    }
    watch.charge(&mut judged.timing.read_params);
    let (blocks, run_accounts) =
        validate_run(&seeded.domain, kind, run, manifests, edits, ids, judged).await;
    let mut watch = Stopwatch::start();
    for (id, tally) in run_accounts {
        account_checks.entry(id).or_default().merge(tally);
    }
    let script = &mut judged.state.script;
    compare_carried::<Script>(&seeded.domain, kind, &blocks, script).await;
    watch.charge(&mut judged.timing.scripts);
    let plutus_data = &mut judged.state.plutus_data;
    compare_carried::<Datum>(&seeded.domain, kind, &blocks, plutus_data).await;
    watch.charge(&mut judged.timing.plutus_data);
    let served = &mut judged.state.served_tx;
    let maps = served_tx::compare(&seeded.domain, kind, &blocks, served).await;
    watch.charge(&mut judged.timing.served_tx);
    let Some(maps) = maps else {
        return part;
    };
    let readtx = &mut judged.state.readtx;
    compare_read_tx(&seeded.domain, kind, &blocks, readtx, &maps).await;
    watch.charge(&mut judged.timing.read_tx);
    let rolled: Vec<&Rolled> = blocks.iter().filter_map(GroupBlock::rolled).collect();
    let routes = serve_rolled(&seeded.domain, &seeded.seed, &rolled, &maps).await;
    watch.charge(&mut judged.timing.rolled_routes);
    compare_as_output(kind, run, &rolled, &routes, &mut judged.state, as_output);
    watch.charge(&mut judged.timing.as_output);
    sweep(
        &seeded.domain,
        &seeded.seed,
        kind,
        run,
        &rolled,
        &mut judged.state,
    )
    .await;
    watch.charge(&mut judged.timing.sweep);
    part
}

/// Judges every kept run of a corpus, each seeded from its first pre state, on
/// one thread per core.
async fn judge(corpus: &Corpus, manifests: &Manifests, edits: &Edits<'_>) -> Judged {
    let runs: Vec<&KeptRun> = corpus.kept().collect();
    let threads = thread::available_parallelism().map_or(1, usize::from);
    let handle = Handle::current();
    let parts = task::block_in_place(|| {
        in_parallel(
            &runs,
            threads,
            |run| run.records().len(),
            |run| handle.block_on(judge_run(corpus.kind, run, manifests, edits)),
        )
    });
    let mut judged = Judged::default();
    let mut judged_ids = BTreeSet::new();
    let mut as_output: BTreeMap<RecordId, ByEntry> = BTreeMap::new();
    let mut account_checks: BTreeMap<RecordId, Tally> = BTreeMap::new();
    for part in parts {
        judged.merge(part.judged);
        judged_ids.extend(part.ids);
        for (id, tally) in part.as_output {
            as_output.entry(id).or_default().merge(tally);
        }
        for (id, tally) in part.accounts {
            account_checks.entry(id).or_default().merge(tally);
        }
    }
    manifests.unjudged(corpus.kind, &judged_ids, &mut judged.validation);
    judged.eval.unjudged(&manifests.eval, corpus.kind);
    judged.eval.tx_unjudged(&manifests.tx, corpus.kind);
    judged.context.unjudged(&manifests.context, corpus.kind);
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

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn ledger_oracle() {
    let manifests = Manifests::committed();
    let left_out = left_out();
    let mut verdicts = judgement::Tally::default();
    let mut all = Judged::default();
    for kind in CorpusKind::ALL {
        let corpus = load(kind)
            .without(&left_out)
            .unwrap_or_else(|e| panic!("{e}"));
        println!("{corpus}");
        let judged = judge(&corpus, &manifests, &Edits::default()).await;
        println!("{judged}");
        for line in judged.timing.lines(kind) {
            println!("{line}");
        }
        let mut part = judged.report();
        part.require(&[(Judge::Cost, Instrument::MinFee.into())]);
        verdicts.merge(part);
        all.merge(judged);
    }
    print_views(&all, &manifests, true, &mut verdicts);
    for verdict in all.eval.every_layer() {
        verdicts.add(verdict);
    }
    let revalidated = all.state.revalidate.compared;
    println!("REVALIDATE all compared={revalidated}");
    verdicts.require(&[(Judge::Revalidate, Instrument::Revalidate.into())]);
    let cases = endorser::load(&endorser::fixture_path());
    let endorsed = endorser::judge(&cases).await;
    println!("{}", endorsed.line());
    endorsed.report(cases.len(), &mut verdicts);
    let members = coverage::members(&manifests.rules);
    let committed = ledger_oracle_judge::coverage::committed();
    let committed = committed.unwrap_or_else(|e| panic!("{e}"));
    for line in ledger_oracle_judge::coverage::table(&members, &committed, &mut verdicts) {
        println!("{line}");
    }
    conclude(&verdicts);
}

/// Prints the rule lines, the evaluation and script context views and the
/// manifest candidates, adding the rule verdicts.
fn print_views(all: &Judged, manifests: &Manifests, whole: bool, out: &mut judgement::Tally) {
    for line in manifests.rules.report(&all.rules, whole, out) {
        println!("{line}");
    }
    println!("{}", all.eval);
    println!("{}", all.context);
    for line in all
        .candidates
        .lines()
        .into_iter()
        .chain(all.eval.candidates())
    {
        println!("{line}");
    }
}

/// Prints the failing verdicts, the passing counts and the run's result, and
/// fails the test unless the run passes.
fn conclude(verdicts: &judgement::Tally) {
    println!("{verdicts}");
    let result = verdicts.verdict();
    println!("{result}");
    assert!(matches!(result, RunVerdict::Pass { .. }));
}

/// The records ORACLE_DESELECT names, which the whole run leaves out, none
/// when it is unset.
fn left_out() -> BTreeSet<RecordId> {
    let Ok(list) = std::env::var("ORACLE_DESELECT") else {
        return BTreeSet::new();
    };
    let named = named_records(&list);
    let names: Vec<String> = named.iter().map(RecordId::to_string).collect();
    println!("DESELECT {}", names.join(","));
    named
}

/// The records a comma separated list names.
fn named_records(list: &str) -> BTreeSet<RecordId> {
    let names = list.split(',').map(str::trim).filter(|s| !s.is_empty());
    let named: BTreeSet<RecordId> = names
        .map(|s| s.parse().unwrap_or_else(|e| panic!("{e}")))
        .collect();
    assert!(!named.is_empty(), "the list names no record: {list:?}");
    named
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos on the kept runs that hold a record ORACLE_RECORDS names, run in release"]
async fn ledger_oracle_records() {
    let list = std::env::var("ORACLE_RECORDS").unwrap_or_else(|e| panic!("ORACLE_RECORDS {e}"));
    let named = named_records(&list);
    let committed = Manifests::committed();
    let mut selection = Judged::default();
    for kind in CorpusKind::ALL {
        let corpus = load(kind).select(&named).unwrap_or_else(|e| panic!("{e}"));
        let held = corpus.kept().flat_map(|run| {
            let records = run.records().iter();
            records.map(move |r| RecordId {
                corpus: kind,
                n: r.n,
            })
        });
        let manifests = committed.within(&held.collect());
        println!("{corpus}");
        let tallies = judge(&corpus, &manifests, &Edits::default()).await;
        println!("{tallies}");
        selection.merge(tallies);
    }
    let mut verdicts = selection.report();
    print_views(&selection, &committed, false, &mut verdicts);
    conclude(&verdicts);
}

#[test]
fn manifests_within_records_keep_only_their_entries() {
    let known = manifest::parse_known("known.txt", "V25 validation ic#1971\nS18 accounts ic#551")
        .unwrap_or_else(|e| panic!("{e}"));
    let declared = manifest::parse_declared(
        "expected-unsupported.txt",
        "ic#890 witness fields\nic#1344 guard datum",
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let manifests = Manifests {
        declared,
        known: known.records,
        ..Manifests::committed()
    };
    let (declared, _) = manifests.declared.iter().next().expect("a declared record");
    let ((_, known), _) = manifests.known.iter().next().expect("a known record");
    let named = BTreeSet::from([*declared, *known]);
    let within = manifests.within(&named);
    let kept: Vec<&RecordId> = within.declared.keys().collect();
    let expected: Vec<&RecordId> = manifests
        .declared
        .keys()
        .filter(|id| named.contains(id))
        .collect();
    assert_eq!(kept, expected);
    assert!(within.known.keys().all(|(_, id)| named.contains(id)));
    assert!(within.known.keys().any(|(_, id)| id == known));
    assert!(within.declared.len() < manifests.declared.len());
    assert!(within.known.len() < manifests.known.len());
}

#[test]
fn a_list_names_each_record_once() {
    let named = named_records("supp#55, supp#68,supp#55,");
    assert_eq!(named, BTreeSet::from([supp(55), supp(68)]));
}

#[test]
#[should_panic(expected = "the list names no record")]
fn an_empty_list_is_refused() {
    named_records(" , ");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn dropping_one_seed_pair_prints_utxo_disagree() {
    let corpus = load(CorpusKind::Supp);
    let manifests = Manifests::committed();
    let intact = judge(&corpus, &manifests, &Edits::default()).await;
    assert!(intact.state.utxo.compared() > 0);
    assert!(intact.state.utxo.disagree().next().is_none(), "{intact}");
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
    assert!(
        shown(planted.state.utxo.disagree()).contains(&line),
        "{planted}"
    );
    assert!(!failing(&planted, Judge::Utxo).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn one_changed_cost_model_entry_prints_params_disagree() {
    let corpus = load(CorpusKind::Supp);
    let manifests = Manifests::committed();
    let intact = judge(&corpus, &manifests, &Edits::default()).await;
    assert!(intact.state.params.compared() > 0);
    assert_eq!(intact.state.params.disagree().count(), 0, "{intact}");
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
    let lines = failing(&planted, Judge::Params);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains("costModels.PlutusV1[0] ledger="),
        "{lines:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn the_v2_record_is_known_and_its_maker_agrees() {
    let judged = judge_supp(|_| ()).await;
    let text = judged.validation.to_string();
    assert!(!text.contains(" supp#1 "), "{text}");
    assert!(
        !fails_with(&judged, Judge::Validation, "supp#2", ""),
        "{text}"
    );
    assert!(
        text.contains(
            "\nKNOWN row=V2 ids=supp#2 ledger=accepted dolos=refused[PostAlonzo(InputNotInUTxO)]"
        ),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn every_layer_accepts_the_sha2_256_and_mk_nil_data_records_in_the_ledger_units() {
    let corpus = load(CorpusKind::Supp);
    let records: BTreeMap<u32, &Record> = corpus
        .kept()
        .flat_map(|run| run.records())
        .map(|r| (r.n, r))
        .collect();
    let judged = judge_supp(|_| ()).await;
    for (n, units) in [(211, (1004, 437_340)), (214, (1965, 445_825))] {
        let record = records[&n];
        let ledger: Vec<_> = ledger_rows(record).into_iter().collect();
        let [(key, Answer::Ran(ran))] = &ledger[..] else {
            panic!("supp#{n} ledger rows {ledger:?}");
        };
        assert_eq!((ran.mem, ran.steps), units, "supp#{n}");
        assert!(matches!(record.outcome, LedgerOutcome::Accepted { .. }));
        assert!(
            judged.validation.agree.contains_key(&supp(n)),
            "supp#{n} {}",
            judged.validation
        );
        let every_layer = Layer::ALL.map(|layer| ((layer, *key), *ran));
        assert_eq!(
            judged.eval.ran.get(&supp(n)),
            Some(&BTreeMap::from(every_layer)),
            "supp#{n}"
        );
    }
    assert!(!judged.validation.agree.contains_key(&supp(217)));
    assert_eq!(judged.eval.ran.get(&supp(217)), None);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn deleting_the_v2_known_entry_prints_its_disagreement() {
    let judged = judge_supp(|m| {
        m.known.remove(&(Locus::Validation, supp(2))).unwrap();
    })
    .await;
    let text = judged.to_string();
    assert!(!text.contains("\nKNOWN row=V2 "), "{text}");
    let tail = " ledger=accepted dolos=refused[PostAlonzo(InputNotInUTxO)]";
    assert!(
        fails_with(&judged, Judge::Validation, "supp#2", tail),
        "{:?}",
        failing(&judged, Judge::Validation)
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn deleting_one_declared_entry_prints_unsupported_unexpected() {
    let label = "stateful or scripted subtransaction";
    let unexpected = format!(" label={label}");
    let intact = judge_supp(|_| ()).await;
    assert!(
        !fails_with(&intact, Judge::Validation, "supp#14", &unexpected),
        "{intact}"
    );
    let deleted = judge_supp(|m| {
        assert_eq!(m.declared.remove(&supp(14)), Some(Feature(label.into())));
        m.declared.insert(supp(999), Feature(label.into()));
    })
    .await;
    let lines = failing(&deleted, Judge::Validation);
    assert!(
        fails_with(&deleted, Judge::Validation, "supp#14", &unexpected),
        "{lines:?}"
    );
    let stale = format!(" declared={label} dolos=not_judged");
    assert!(
        fails_with(&deleted, Judge::Validation, "supp#999", &stale),
        "{lines:?}"
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
    .await;
    let lines = failing(&named, Judge::Validation);
    for record in ["supp#0", "supp#999"] {
        let stale = fails_with(&named, Judge::Validation, record, " row=V9");
        assert!(stale, "{record} {lines:?}");
    }
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
        let ids = judged
            .validation
            .failing
            .iter()
            .filter_map(|v| match v.subject {
                judgement::Subject::Record(id) => Some(id),
                judgement::Subject::Selection => None,
            });
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
    let tail = " outcome=disagree ledger=refused[SubMissingTxMetadata] ledger_class=metadata \
                dolos=refused[PostAlonzo(InputNotInUTxO)] dolos_class=inputs";
    let intact = judge(&corpus, &manifests, &Edits::default()).await;
    assert!(
        !fails_with(&intact, Judge::Validation, "ic#1311", ""),
        "{intact}"
    );
    let edits = Edits {
        pre: Box::new(|id, pre| if id == first { vec![] } else { pre.to_vec() }),
        ..Edits::default()
    };
    let planted = judge(&corpus, &manifests, &edits).await;
    assert!(
        fails_with(&planted, Judge::Validation, "ic#1311", tail),
        "{:?}",
        failing(&planted, Judge::Validation)
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
    .await;
    let tail = " outcome=disagree ledger=refused[BadInputsUTxO,BadInputsUTxO] ledger_class=inputs \
                dolos=refused[PostAlonzo(PreservationOfValue)] dolos_class=value";
    assert!(
        fails_with(&deleted, Judge::Validation, "supp#5", tail),
        "{:?}",
        failing(&deleted, Judge::Validation)
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
        let lines = shown(judged.state.utxo.disagree());
        lines
            .iter()
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
    assert!(!failing(&planted, Judge::Utxo).is_empty());
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
    let lines = failing(&planted, Judge::ApplyBlock);
    let first = group[0];
    let last = group[group.len() - 1];
    let head = format!(
        "VERDICT judge=apply_block entry=apply_block record={first} cells=answer:refused \
         outcome=disagree group={first}..{last} records={} ",
        group.len()
    );
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with(&head), "{lines:?}");
    assert!(
        text.contains(&format!(" rolled_failed={}\n", group.len())),
        "{text}"
    );
    assert!(!passes(&planted));
    let state = &planted.state;
    let named = state
        .script
        .disagree
        .iter()
        .chain(state.readtx.disagree())
        .chain(state.utxo.disagree())
        .filter(|d| group.contains(&d.id))
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
    assert_eq!(supp8.flag(), Phase2::Invalid);
    assert!(matches!(supp8.outcome, LedgerOutcome::Accepted { .. }));
    let manifests = Manifests::committed();
    let of_supp9 = |judged: &Judged| -> Vec<String> {
        let prefix = "VERDICT judge=validation entry=receive_tx record=supp#9 ";
        let lines = failing(judged, Judge::Validation).into_iter();
        lines.filter(|l| l.starts_with(prefix)).collect()
    };
    let intact = judge(&corpus, &manifests, &Edits::default()).await;
    assert!(of_supp9(&intact).is_empty(), "{intact}");
    let edits = Edits {
        block_tx: Box::new(|id, phase2| match id == supp(8) {
            true => Some(Phase2::Valid),
            false => Some(phase2),
        }),
        ..Edits::default()
    };
    let planted = of_supp9(&judge(&corpus, &manifests, &edits).await);
    let detail = " outcome=disagree ledger=accepted dolos=refused[PostAlonzo(InputNotInUTxO)]";
    assert!(planted.iter().any(|l| l.ends_with(detail)), "{planted:?}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn an_output_dolos_holds_that_the_ledger_never_made_prints_utxo_disagree() {
    let corpus = load(CorpusKind::Supp);
    let manifests = Manifests::committed();
    let intact = judge(&corpus, &manifests, &Edits::default()).await;
    let unmade = |judged: &Judged, tx: Option<[u8; 32]>| -> Vec<String> {
        let lines = shown(judged.state.utxo.disagree()).into_iter();
        let lines = lines.filter(|d| d.contains(" ledger=none dolos="));
        let named =
            |d: &String| tx.is_none_or(|tx| d.contains(&format!(" input={}#", hex::encode(tx))));
        lines.filter(named).collect()
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
    let mut checked = 0;
    for run in corpus.kept() {
        let groups: Vec<_> = run.groups().collect();
        let [first, second, ..] = groups.as_slice() else {
            continue;
        };
        let pparams = params::seed_values(run.pparams()).unwrap();
        let seeded = seed::seed(
            &seed::genesis(run.era()),
            run.epoch().unwrap(),
            &pparams,
            run.seed(),
            &run.records()[0].accounts_pre,
            &run.records()[0],
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
    assert!(intact.state.as_output.disagree().next().is_none(), "{text}");
    let listed = judge_supp(|m| {
        m.known.insert((Locus::AsOutput, supp(9)), Row::S3);
    })
    .await;
    assert!(
        fails_with(
            &listed,
            Judge::AsOutput,
            "supp#9",
            " row=S3 locus=as_output"
        ),
        "{:?}",
        failing(&listed, Judge::AsOutput)
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
    let lines = failing(&deleted, Judge::Accounts);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let named = " record=ic#551 cells=answer:differ,field:drep outcome=disagree account=";
    assert!(lines[0].contains(named), "{lines:?}");
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
    assert!(intact.state.readtx.compared() > 0, "{intact}");
    let tallies = &intact.state.readtx.0;
    let disagree = tallies
        .iter()
        .flat_map(|(entry, t)| t.disagree.iter().map(move |d| format!("{entry} {d}")))
        .join("\n");
    assert!(disagree.is_empty(), "{disagree}");
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
    let lines: Vec<String> = shown(planted.state.readtx.disagree())
        .into_iter()
        .filter(|d| d.starts_with(&prefix))
        .collect();
    let versions = compare::at_every_version(compare::SERVED_TXS).count();
    assert_eq!(lines.len(), versions, "{planted}");
    assert!(
        lines.iter().all(|l| l.ends_with(" dolos=none")),
        "{planted}"
    );
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
        shown(&judged.state.script.disagree)
            .into_iter()
            .filter(|d| d.starts_with(&prefix))
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
    assert!(intact.state.datum.compared() > 0, "{intact}");
    let (maker, input, len) = corpus
        .kept()
        .flat_map(KeptRun::records)
        .filter(|r| matches!(r.outcome, LedgerOutcome::Accepted { .. }))
        .find_map(|r| sub_witnessed_datum(r).map(|(input, len)| (supp(r.n), input, len)))
        .expect("a sub transaction output holds a datum hash its sub transaction witnesses");
    let named = format!(" input={} made_by={maker} ", show_input(&input));
    let text = intact.to_string();
    assert!(
        shown(intact.state.datum.disagree())
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
    let lines: Vec<String> = shown(planted.state.datum.disagree())
        .into_iter()
        .filter(|d| d.contains(&named))
        .collect();
    println!(
        "PLANTED datum disagree={} named={}",
        planted.state.datum.disagree().count(),
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
    let before: BTreeSet<String> = shown(&intact.state.accounts.disagree).into_iter().collect();
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
    let lines: Vec<String> = shown(&planted.state.accounts.disagree)
        .into_iter()
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
    use pallas::ledger::validate::utils::{
        ParameterFailure, PostAlonzoError as P, ValidationError as V,
    };
    let refused = |e: C| validate::verdict(&Err(DomainError::ChainError(e)));
    let expected = |kind: &str, class| DolosVerdict::Refused {
        kind: kind.to_string(),
        class,
    };
    let inputs = DolosClass::Rule(RefusalClass::Inputs);
    let cases = [
        (
            C::Phase1ValidationRejected(V::PostAlonzo(P::InputNotInUTxO)),
            expected("PostAlonzo(InputNotInUTxO)", inputs),
        ),
        (
            C::Phase1ValidationRejected(V::PostAlonzo(P::CollateralNotInUTxO)),
            expected("PostAlonzo(CollateralNotInUTxO)", inputs),
        ),
        (
            C::Phase1ValidationRejected(V::PostAlonzo(P::CollateralMissing)),
            expected(
                "PostAlonzo(CollateralMissing)",
                DolosClass::Rule(RefusalClass::Collateral),
            ),
        ),
        (
            C::Phase1ValidationRejected(V::PostAlonzo(P::UnsupportedPlutusLanguage)),
            expected("PostAlonzo(UnsupportedPlutusLanguage)", DolosClass::NoRule),
        ),
        (
            C::Phase1ValidationRejected(V::DijkstraInputAlreadySpent),
            expected("DijkstraInputAlreadySpent", inputs),
        ),
        (
            C::Phase1ValidationRejected(V::DijkstraMissingParameters(
                ParameterFailure::ReferenceFeeStride,
            )),
            expected(
                "DijkstraMissingParameters(\"reference fee stride\")",
                DolosClass::NoRule,
            ),
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
    let genesis = seed::genesis(run.era());
    let seeded = seed::seed(
        &genesis,
        run.epoch().unwrap(),
        &[],
        run.seed(),
        &first.accounts_pre,
        first,
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
        differ.disagree[0]
            .detail
            .ends_with("differ at byte 3 fields=address"),
        "{}",
        differ.disagree[0]
    );
    let input = seed[0].input;
    let wrote_flipped = BTreeMap::from([(input, flipped[0].output.0.clone())]);
    let mut differ = Tally::default();
    compare_utxos(&seeded.domain, id, [], seed, &wrote_flipped, &mut differ).await;
    assert_eq!(differ.disagree.len(), 1);
    assert!(
        differ.disagree[0].detail.ends_with("differ at byte 3"),
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
        differ.disagree[0].detail.ends_with("differ at byte 0"),
        "{}",
        differ.disagree[0]
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "judges Dolos against the committed ledger corpora, run in release"]
async fn the_seed_point_is_a_rollback_target() {
    let corpus = load(CorpusKind::Supp);
    let run = corpus.kept().next().unwrap();
    let genesis = seed::genesis(run.era());
    let seeded = seed::seed(
        &genesis,
        run.epoch().unwrap(),
        &[],
        run.seed(),
        &run.records()[0].accounts_pre,
        &run.records()[0],
    );
    let rolled = seeded.domain.rollback(seeded.seed.point());
    assert!(rolled.is_ok(), "{rolled:?}");
}
