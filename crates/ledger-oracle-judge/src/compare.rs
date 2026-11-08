use std::{collections::BTreeMap, fmt};

use crate::{
    eval::Layer,
    judgement::{self, Axis, CellKey, EntryPoint, Finding, Judge, Verdict},
    manifest::{KnownList, Locus},
    record::{CorpusKind, RecordId},
    row::Row,
};

/// The result of comparing the ledger's bytes for one item with the node's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ByteVerdict {
    Equal,
    Differ { at: usize },
    LedgerOnly(Vec<u8>),
    NodeOnly(Vec<u8>),
}

impl ByteVerdict {
    /// Compares the ledger's bytes with the node's, either side possibly absent.
    pub fn of(ledger: Option<&[u8]>, node: Option<&[u8]>) -> Option<Self> {
        match (ledger, node) {
            (None, None) => None,
            (Some(l), node) => Some(ByteVerdict::against(l, node)),
            (None, Some(n)) => Some(ByteVerdict::NodeOnly(n.to_vec())),
        }
    }

    /// Compares the ledger's bytes with the node's, the node's possibly absent.
    pub fn against(ledger: &[u8], node: Option<&[u8]>) -> Self {
        match node {
            None => ByteVerdict::LedgerOnly(ledger.to_vec()),
            Some(n) => match first_difference(ledger, n) {
                None => ByteVerdict::Equal,
                Some(at) => ByteVerdict::Differ { at },
            },
        }
    }

    /// Whether both sides hold the same bytes.
    pub fn agrees(&self) -> bool {
        matches!(self, ByteVerdict::Equal)
    }

    /// The cell the comparison touches, which side holds bytes and whether they
    /// are equal.
    pub fn cell(&self) -> CellKey {
        let member = match self {
            ByteVerdict::Equal => "equal",
            ByteVerdict::Differ { .. } => "differ",
            ByteVerdict::LedgerOnly(_) => "ledger_only",
            ByteVerdict::NodeOnly(_) => "dolos_only",
        };
        CellKey::new(Axis::Bytes, member)
    }
}

impl fmt::Display for ByteVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ByteVerdict::Equal => write!(f, "equal"),
            ByteVerdict::Differ { at } => write!(f, "differ at byte {at}"),
            ByteVerdict::LedgerOnly(l) => write!(f, "ledger={} dolos=none", hex::encode(l)),
            ByteVerdict::NodeOnly(n) => write!(f, "ledger=none dolos={}", hex::encode(n)),
        }
    }
}

/// The index of the first byte where two byte strings differ.
pub fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    match a.iter().zip(b).position(|(x, y)| x != y) {
        Some(at) => Some(at),
        None if a.len() == b.len() => None,
        None => Some(a.len().min(b.len())),
    }
}

/// The number of items one comparison checked and the finding of each that
/// disagreed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tally<F = Finding> {
    pub compared: usize,
    pub disagree: Vec<F>,
}

impl<F> Default for Tally<F> {
    fn default() -> Self {
        Tally {
            compared: 0,
            disagree: Vec::new(),
        }
    }
}

impl<F> Tally<F> {
    /// Counts one compared item, keeping its finding when it disagrees.
    pub fn add(&mut self, agrees: bool, describe: impl FnOnce() -> F) {
        self.compared += 1;
        if !agrees {
            self.disagree.push(describe());
        }
    }

    /// Counts one compared item that agrees.
    pub fn agree(&mut self) {
        self.compared += 1;
    }

    /// Counts the items and keeps the disagreements of another tally.
    pub fn merge(&mut self, other: Tally<F>) {
        self.compared += other.compared;
        self.disagree.extend(other.disagree);
    }
}

impl Tally {
    /// Adds the verdicts of the tally as the judgement of `judge` at `entry`.
    pub fn report(&self, judge: Judge, entry: EntryPoint, out: &mut judgement::Tally) {
        let passed = judgement::Passed {
            judge,
            entry,
            cells: Vec::new(),
            pass: judgement::Pass::Agree,
        };
        out.pass(passed, self.compared - self.disagree.len());
        for finding in &self.disagree {
            out.add(finding.clone().verdict(judge, entry));
        }
    }
}

/// A slot group whose block the node did not apply, with why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollFail {
    pub first: RecordId,
    pub last: RecordId,
    pub records: usize,
    pub slot: u64,
    pub cause: String,
}

impl fmt::Display for RollFail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let RollFail {
            first,
            last,
            records,
            slot,
            cause,
        } = self;
        write!(
            f,
            "group={first}..{last} records={records} slot={slot} {cause}"
        )
    }
}

/// The state comparisons of one arm, one tally each.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StateTallies {
    pub roll_fail: Vec<RollFail>,
    pub utxo: Tally,
    pub as_output: Tally,
    pub sub_only: usize,
    pub seeded_unjudged: usize,
    pub accounts: Tally,
    pub undo: Tally,
    pub revalidate: Tally,
    pub dependents: usize,
    pub params: Tally,
    pub readtx: Tally,
    pub script: Tally,
    pub datum: Tally,
    pub plutus_data: Tally,
    pub served_tx: Tally,
    pub known: Vec<String>,
    pub known_fired: KnownList,
    pub known_stale: Vec<Verdict>,
}

impl StateTallies {
    /// Each tally a passing corpus compares at least once, by its judge and the
    /// entry point it drives.
    fn named(&self) -> [(Judge, EntryPoint, &Tally); 10] {
        let StateTallies {
            roll_fail: _,
            utxo,
            as_output,
            sub_only: _,
            seeded_unjudged: _,
            accounts,
            undo,
            revalidate: _,
            dependents: _,
            params,
            readtx,
            script,
            datum,
            plutus_data,
            served_tx,
            known: _,
            known_fired: _,
            known_stale: _,
        } = self;
        [
            (Judge::Utxo, EntryPoint::ReadUtxos, utxo),
            (Judge::AsOutput, EntryPoint::FetchBlock, as_output),
            (Judge::Accounts, EntryPoint::ReadAccounts, accounts),
            (Judge::Undo, EntryPoint::ApplyBlock, undo),
            (Judge::Params, EntryPoint::ReadParams, params),
            (Judge::ReadTx, EntryPoint::ReadTx(Layer::V1), readtx),
            (Judge::Script, EntryPoint::ReadData, script),
            (Judge::Datum, EntryPoint::ReadData, datum),
            (Judge::PlutusData, EntryPoint::ReadData, plutus_data),
            (Judge::ServedTx, EntryPoint::FetchBlock, served_tx),
        ]
    }

    /// Adds the verdicts of every tally, each block that did not roll forward
    /// and each stale known entry, and requires each named tally to have
    /// compared an item.
    pub fn report(&self, out: &mut judgement::Tally) {
        let revalidate = (Judge::Revalidate, EntryPoint::Revalidate, &self.revalidate);
        let mut required = Vec::new();
        for (judge, entry, tally) in self.named().into_iter().chain([revalidate]) {
            tally.report(judge, entry, out);
            if judge != Judge::Revalidate {
                required.push((judge, entry));
            }
        }
        for ((locus, id), row) in &self.known_fired {
            let (judge, entry) = locus.judge();
            out.add(Verdict {
                judge,
                entry,
                subject: judgement::Subject::Record(*id),
                cells: vec![CellKey::new(Axis::Row, row)],
                outcome: judgement::Outcome::Known(format!("row={row}")),
            });
        }
        for fail in &self.roll_fail {
            out.add(Verdict::disagree(
                Judge::ApplyBlock,
                EntryPoint::ApplyBlock,
                fail.first,
                vec![CellKey::new(Axis::Answer, "refused")],
                fail.to_string(),
            ));
        }
        for stale in &self.known_stale {
            out.add(stale.clone());
        }
        out.require(&required);
    }

    /// Adds the comparisons of another arm part after this one's.
    pub fn merge(&mut self, other: StateTallies) {
        let StateTallies {
            roll_fail,
            utxo,
            as_output,
            sub_only,
            seeded_unjudged,
            accounts,
            undo,
            revalidate,
            dependents,
            params,
            readtx,
            script,
            datum,
            plutus_data,
            served_tx,
            known,
            known_fired,
            known_stale,
        } = other;
        self.roll_fail.extend(roll_fail);
        self.utxo.merge(utxo);
        self.as_output.merge(as_output);
        self.sub_only += sub_only;
        self.seeded_unjudged += seeded_unjudged;
        self.accounts.merge(accounts);
        self.undo.merge(undo);
        self.revalidate.merge(revalidate);
        self.dependents += dependents;
        self.params.merge(params);
        self.readtx.merge(readtx);
        self.script.merge(script);
        self.datum.merge(datum);
        self.plutus_data.merge(plutus_data);
        self.served_tx.merge(served_tx);
        self.known.extend(known);
        self.known_fired.extend(known_fired);
        self.known_stale.extend(known_stale);
    }

    /// The count of records in slot groups whose block the node did not apply.
    pub fn rolled_failed(&self) -> usize {
        self.roll_fail.iter().map(|r| r.records).sum()
    }

    /// Adds the served output comparisons of each record, passing a known record while it still disagrees.
    pub fn settle_as_output(
        &mut self,
        kind: CorpusKind,
        checks: BTreeMap<RecordId, Tally>,
        known: &KnownList,
    ) {
        self.settle(Locus::AsOutput, |s| &mut s.as_output, kind, checks, known);
    }

    /// Adds the account comparisons of each record, passing a known record while it still disagrees.
    pub fn settle_accounts(
        &mut self,
        kind: CorpusKind,
        checks: BTreeMap<RecordId, Tally>,
        known: &KnownList,
    ) {
        self.settle(Locus::Accounts, |s| &mut s.accounts, kind, checks, known);
    }

    /// Adds the comparisons of each record to the tally of `locus`, passing a known record while it still disagrees.
    fn settle(
        &mut self,
        locus: Locus,
        tally_of: fn(&mut StateTallies) -> &mut Tally,
        kind: CorpusKind,
        checks: BTreeMap<RecordId, Tally>,
        known: &KnownList,
    ) {
        let (judge, entry) = locus.judge();
        let stale = |id: RecordId, row: &Row, detail: String| {
            let cells = vec![
                CellKey::new(Axis::Failure, "known_stale"),
                CellKey::new(Axis::Row, row),
            ];
            Verdict::disagree(judge, entry, id, cells, detail)
        };
        for (id, tally) in &checks {
            tally_of(self).compared += tally.compared;
            match known.get(&(locus, *id)) {
                None => {
                    tally_of(self)
                        .disagree
                        .extend(tally.disagree.iter().cloned());
                }
                Some(row) if tally.disagree.is_empty() => {
                    self.known_stale
                        .push(stale(*id, row, format!("row={row} locus={locus}")));
                }
                Some(row) => {
                    let lines = tally
                        .disagree
                        .iter()
                        .map(|d| format!("row={row} ids={id} {locus} {}", d.detail));
                    self.known.extend(lines);
                    self.known_fired.insert((locus, *id), *row);
                }
            }
        }
        for ((entry, id), row) in known {
            if *entry == locus && id.corpus == kind && !checks.contains_key(id) {
                let detail = format!("row={row} locus={locus} not_compared");
                self.known_stale.push(stale(*id, row, detail));
            }
        }
    }
}

impl fmt::Display for StateTallies {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let StateTallies {
            roll_fail: _,
            utxo,
            as_output,
            sub_only,
            seeded_unjudged,
            accounts,
            undo,
            revalidate,
            dependents,
            params,
            readtx,
            script,
            datum,
            plutus_data,
            served_tx,
            known,
            known_fired: _,
            known_stale: _,
        } = self;
        write!(
            f,
            "STATE compared utxo={} as_output={} of sub_only={sub_only} \
             seeded_unjudged={seeded_unjudged} accounts={} undo={} revalidate={} \
             of dependents={dependents} params={} readtx={} script={} datum={} \
             plutus_data={} served_tx={} rolled_failed={}",
            utxo.compared,
            as_output.compared,
            accounts.compared,
            undo.compared,
            revalidate.compared,
            params.compared,
            readtx.compared,
            script.compared,
            datum.compared,
            plutus_data.compared,
            served_tx.compared,
            self.rolled_failed()
        )?;
        for line in known {
            write!(f, "\nKNOWN {line}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::row::Row;

    #[test]
    fn first_difference_names_the_byte_or_the_shorter_length() {
        assert_eq!(first_difference(b"abc", b"abc"), None);
        assert_eq!(first_difference(b"abc", b"abd"), Some(2));
        assert_eq!(first_difference(b"ab", b"abc"), Some(2));
        assert_eq!(first_difference(b"", b""), None);
    }

    fn supp(n: u32) -> RecordId {
        RecordId {
            corpus: CorpusKind::Supp,
            n,
        }
    }

    fn checks(entries: &[(u32, &[&str])]) -> BTreeMap<RecordId, Tally> {
        let mut checks = BTreeMap::new();
        for (n, disagree) in entries {
            let tally: &mut Tally = checks.entry(supp(*n)).or_default();
            tally.agree();
            for d in *disagree {
                tally.add(false, || Finding::new(supp(*n), Vec::new(), *d));
            }
        }
        checks
    }

    fn compared() -> StateTallies {
        let mut state = StateTallies::default();
        state.utxo.agree();
        state.accounts.agree();
        state.undo.agree();
        state.revalidate.agree();
        state.params.agree();
        state.readtx.agree();
        state.script.agree();
        state.datum.agree();
        state.plutus_data.agree();
        state.served_tx.agree();
        state
    }

    /// The verdicts the state reports.
    fn reported(state: &StateTallies) -> judgement::Tally {
        let mut out = judgement::Tally::default();
        state.report(&mut out);
        out
    }

    /// Whether the verdicts the state reports pass a run.
    fn passes(state: &StateTallies) -> bool {
        matches!(
            reported(state).verdict(),
            judgement::RunVerdict::Pass { .. }
        )
    }

    fn tally(lines: &[&str]) -> Tally {
        Tally {
            compared: 2 * lines.len(),
            disagree: lines
                .iter()
                .map(|l| Finding::new(supp(0), Vec::new(), *l))
                .collect(),
        }
    }

    fn stale(line: &str) -> Verdict {
        Verdict::disagree(
            Judge::Accounts,
            EntryPoint::ReadAccounts,
            supp(0),
            Vec::new(),
            line,
        )
    }

    fn roll_fail(n: u32) -> RollFail {
        RollFail {
            first: supp(n),
            last: supp(n),
            records: 1,
            slot: n.into(),
            cause: format!("cause{n}"),
        }
    }

    fn part(n: u32, row: Row) -> StateTallies {
        let line = format!("line{n}");
        let lines = [line.as_str()];
        StateTallies {
            roll_fail: vec![roll_fail(n)],
            utxo: tally(&lines),
            as_output: tally(&lines),
            sub_only: 1,
            seeded_unjudged: 2,
            accounts: tally(&lines),
            undo: tally(&lines),
            revalidate: tally(&lines),
            dependents: 3,
            params: tally(&lines),
            readtx: tally(&lines),
            script: tally(&lines),
            datum: tally(&lines),
            plutus_data: tally(&lines),
            served_tx: tally(&lines),
            known: vec![line.clone()],
            known_fired: BTreeMap::from([((Locus::Accounts, supp(n)), row)]),
            known_stale: vec![stale(&line)],
        }
    }

    #[test]
    fn a_merged_part_adds_every_count_and_keeps_every_line_after_the_first_part() {
        let mut merged = part(0, Row::S1);
        merged.merge(part(1, Row::S2));
        let lines = ["line0", "line1"];
        let both = StateTallies {
            roll_fail: vec![roll_fail(0), roll_fail(1)],
            utxo: tally(&lines),
            as_output: tally(&lines),
            sub_only: 2,
            seeded_unjudged: 4,
            accounts: tally(&lines),
            undo: tally(&lines),
            revalidate: tally(&lines),
            dependents: 6,
            params: tally(&lines),
            readtx: tally(&lines),
            script: tally(&lines),
            datum: tally(&lines),
            plutus_data: tally(&lines),
            served_tx: tally(&lines),
            known: vec!["line0".to_string(), "line1".to_string()],
            known_fired: BTreeMap::from([
                ((Locus::Accounts, supp(0)), Row::S1),
                ((Locus::Accounts, supp(1)), Row::S2),
            ]),
            known_stale: vec![stale("line0"), stale("line1")],
        };
        assert_eq!(merged, both);
    }

    #[test]
    fn merging_an_empty_part_changes_nothing() {
        let mut merged = part(0, Row::S1);
        merged.merge(StateTallies::default());
        assert_eq!(merged, part(0, Row::S1));
    }

    #[test]
    fn a_comparison_that_looked_at_nothing_fails() {
        let mut state = compared();
        state.as_output.agree();
        assert!(passes(&state), "{state}");
        let empties: [fn(&mut StateTallies) -> &mut Tally; 10] = [
            |s| &mut s.utxo,
            |s| &mut s.as_output,
            |s| &mut s.accounts,
            |s| &mut s.undo,
            |s| &mut s.params,
            |s| &mut s.readtx,
            |s| &mut s.script,
            |s| &mut s.datum,
            |s| &mut s.plutus_data,
            |s| &mut s.served_tx,
        ];
        for empty in empties {
            let mut state = state.clone();
            *empty(&mut state) = Tally::default();
            let out = reported(&state);
            assert_eq!(out.failing.len(), 1, "{out}");
            assert!(out.failing[0].to_string().ends_with("judged=0"), "{out}");
        }
        assert!(state.to_string().contains(
            " seeded_unjudged=0 accounts=1 undo=1 revalidate=1 of dependents=0 params=1 \
                 readtx=1 script=1 datum=1 plutus_data=1 served_tx=1"
        ));
    }

    #[test]
    fn a_corpus_with_no_dependent_passes_and_a_kept_dependent_fails() {
        let mut state = compared();
        state.as_output.agree();
        state.revalidate = Tally::default();
        assert!(passes(&state), "{state}");
        state.revalidate.agree();
        assert!(passes(&state), "{state}");
        let cells = vec![CellKey::new(Axis::Rollback, "kept")];
        let line = "kept_after_rollback_to=Slot(40)";
        state
            .revalidate
            .add(false, || Finding::new(supp(2), cells, line));
        assert!(!passes(&state), "{state}");
        let text = reported(&state).to_string();
        assert!(
            text.contains(&format!(
                "VERDICT judge=revalidate entry=revalidate record=supp#2 \
                 cells=rollback:kept outcome=disagree {line}\n"
            )),
            "{text}"
        );
    }

    #[test]
    fn a_group_that_did_not_roll_prints_one_line_and_fails() {
        let mut state = compared();
        state.as_output.agree();
        assert!(passes(&state), "{state}");
        assert!(state.to_string().contains(" served_tx=1 rolled_failed=0"));
        assert!(reported(&state).failing.is_empty());
        state.roll_fail.push(RollFail {
            first: supp(3),
            last: supp(5),
            records: 3,
            slot: 40,
            cause: "panic at=a.rs:1:2 explicit panic".to_string(),
        });
        assert!(!passes(&state), "{state}");
        assert!(state.to_string().ends_with(" served_tx=1 rolled_failed=3"));
        let text = reported(&state).to_string();
        assert!(
            text.contains(
                "VERDICT judge=apply-block entry=apply_block record=supp#3 cells=answer:refused \
                 outcome=disagree group=supp#3..supp#5 records=3 slot=40 panic at=a.rs:1:2 explicit panic\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn agreeing_records_off_the_known_list_print_nothing_and_pass() {
        let mut state = compared();
        state.settle_as_output(
            CorpusKind::Supp,
            checks(&[(1, &[]), (2, &[])]),
            &KnownList::new(),
        );
        assert_eq!(state.as_output.compared, 2);
        assert!(state.known.is_empty() && state.known_stale.is_empty());
        assert!(passes(&state), "{state}");
    }

    #[test]
    fn a_known_record_passes_while_it_disagrees() {
        let mut state = compared();
        let known = KnownList::from([((Locus::AsOutput, supp(9)), Row::S3)]);
        let diff = "ledger=ab dolos=none input=00#1 services=FetchBlock";
        state.settle_as_output(CorpusKind::Supp, checks(&[(9, &[diff])]), &known);
        assert_eq!(
            state.known,
            vec![format!("row=S3 ids=supp#9 as_output {diff}")]
        );
        assert_eq!(state.known_fired, known);
        assert!(state.as_output.disagree.is_empty());
        assert!(passes(&state), "{state}");
    }

    #[test]
    fn a_known_record_that_agrees_or_was_not_compared_is_stale() {
        let mut state = StateTallies::default();
        let known = KnownList::from([
            ((Locus::AsOutput, supp(9)), Row::S3),
            ((Locus::AsOutput, supp(10)), Row::S3),
            (
                (
                    Locus::AsOutput,
                    RecordId {
                        corpus: CorpusKind::Ic,
                        n: 9,
                    },
                ),
                Row::S3,
            ),
            ((Locus::Validation, supp(11)), Row::V1),
        ]);
        state.settle_as_output(CorpusKind::Supp, checks(&[(9, &[])]), &known);
        let lines: Vec<String> = state.known_stale.iter().map(Verdict::to_string).collect();
        assert_eq!(
            lines,
            vec![
                "VERDICT judge=as-output entry=fetch_block record=supp#9 \
                 cells=failure:known_stale,row:S3 outcome=disagree row=S3 locus=as_output"
                    .to_string(),
                "VERDICT judge=as-output entry=fetch_block record=supp#10 \
                 cells=failure:known_stale,row:S3 outcome=disagree row=S3 locus=as_output not_compared"
                    .to_string()
            ]
        );
        assert!(state.known_fired.is_empty());
        assert!(!passes(&state));
    }

    #[test]
    fn an_account_disagreement_off_the_known_list_fails_and_a_known_one_passes() {
        let diff = "account=keyHash-00 ledger=balance=0,pool=none,drep=none dolos=balance=0";
        let mut state = compared();
        state.as_output.agree();
        let as_output = KnownList::from([((Locus::AsOutput, supp(9)), Row::S3)]);
        state.settle_accounts(CorpusKind::Supp, checks(&[(9, &[diff])]), &as_output);
        assert_eq!(
            state.accounts.disagree,
            vec![Finding::new(supp(9), Vec::new(), diff)]
        );
        assert!(state.known.is_empty());
        assert!(!passes(&state), "{state}");

        let mut state = compared();
        state.as_output.agree();
        let known = KnownList::from([((Locus::Accounts, supp(9)), Row::S18)]);
        state.settle_accounts(CorpusKind::Supp, checks(&[(9, &[diff])]), &known);
        assert_eq!(
            state.known,
            vec![format!("row=S18 ids=supp#9 accounts {diff}")]
        );
        assert_eq!(state.known_fired, known);
        assert!(state.accounts.disagree.is_empty());
        assert!(passes(&state), "{state}");
    }

    #[test]
    fn a_disagreement_off_the_known_list_fails_with_its_record() {
        let mut state = compared();
        let diff = "ledger=ab dolos=none input=00#1 services=FetchBlock";
        state.settle_as_output(CorpusKind::Supp, checks(&[(9, &[diff])]), &KnownList::new());
        assert_eq!(
            state.as_output.disagree,
            vec![Finding::new(supp(9), Vec::new(), diff)]
        );
        let out = reported(&state);
        assert_eq!(out.failing.len(), 1, "{out}");
        assert_eq!(
            out.failing[0].to_string(),
            format!("VERDICT judge=as-output entry=fetch_block record=supp#9 cells= outcome=disagree {diff}")
        );
    }

    #[test]
    fn byte_verdict_prints_each_side() {
        assert_eq!(
            ByteVerdict::of(Some(b"\x01"), Some(b"\x01")),
            Some(ByteVerdict::Equal)
        );
        assert_eq!(
            ByteVerdict::of(Some(b"\x01\x02"), Some(b"\x01\x03"))
                .unwrap()
                .to_string(),
            "differ at byte 1"
        );
        assert_eq!(
            ByteVerdict::of(Some(b"\xab"), None).unwrap().to_string(),
            "ledger=ab dolos=none"
        );
        assert_eq!(ByteVerdict::of(None, None), None);
    }
}
