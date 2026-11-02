use std::{collections::BTreeMap, fmt};

use crate::{
    manifest::{KnownList, Locus},
    record::{CorpusKind, RecordId},
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

/// The number of items one comparison checked and a description of each that disagreed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tally {
    pub compared: usize,
    pub disagree: Vec<String>,
}

impl Tally {
    /// Counts one compared item, keeping its description when it disagrees.
    pub fn add(&mut self, agrees: bool, describe: impl FnOnce() -> String) {
        self.compared += 1;
        if !agrees {
            self.disagree.push(describe());
        }
    }

    /// Counts the items and keeps the disagreements of another tally.
    pub fn merge(&mut self, other: Tally) {
        self.compared += other.compared;
        self.disagree.extend(other.disagree);
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
    pub known: Vec<String>,
    pub known_fired: KnownList,
    pub known_stale: Vec<String>,
}

impl StateTallies {
    fn named(&self) -> [(&'static str, &Tally); 8] {
        [
            ("utxo", &self.utxo),
            ("as_output", &self.as_output),
            ("accounts", &self.accounts),
            ("undo", &self.undo),
            ("params", &self.params),
            ("readtx", &self.readtx),
            ("script", &self.script),
            ("datum", &self.datum),
        ]
    }

    /// Whether every block rolled forward, every named tally compared at least one item, no tally holds a disagreement, and no known entry is stale.
    pub fn pass(&self) -> bool {
        let tallies = self.named();
        let agree = tallies
            .iter()
            .all(|(_, t)| t.compared > 0 && t.disagree.is_empty());
        let revalidated = self.revalidate.disagree.is_empty();
        agree && revalidated && self.known_stale.is_empty() && self.roll_fail.is_empty()
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
        for (id, tally) in &checks {
            tally_of(self).compared += tally.compared;
            match known.get(&(locus, *id)) {
                None => {
                    let lines = tally.disagree.iter().map(|d| format!("{id} {d}"));
                    tally_of(self).disagree.extend(lines);
                }
                Some(row) if tally.disagree.is_empty() => {
                    self.known_stale
                        .push(format!("{id} row={row} locus={locus}"));
                }
                Some(row) => {
                    let lines = tally
                        .disagree
                        .iter()
                        .map(|d| format!("row={row} ids={id} {locus} {d}"));
                    self.known.extend(lines);
                    self.known_fired.insert((locus, *id), *row);
                }
            }
        }
        for ((entry, id), row) in known {
            if *entry == locus && id.corpus == kind && !checks.contains_key(id) {
                self.known_stale
                    .push(format!("{id} row={row} locus={locus} not_compared"));
            }
        }
    }
}

impl fmt::Display for StateTallies {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
            known,
            known_fired: _,
            known_stale,
        } = self;
        write!(
            f,
            "STATE compared utxo={} as_output={} of sub_only={sub_only} \
             seeded_unjudged={seeded_unjudged} accounts={} undo={} revalidate={} \
             of dependents={dependents} params={} readtx={} script={} datum={} rolled_failed={}",
            utxo.compared,
            as_output.compared,
            accounts.compared,
            undo.compared,
            revalidate.compared,
            params.compared,
            readtx.compared,
            script.compared,
            datum.compared,
            self.rolled_failed()
        )?;
        for line in roll_fail {
            write!(f, "\nROLL_FAIL {line}")?;
        }
        for line in known {
            write!(f, "\nKNOWN {line}")?;
        }
        for line in known_stale {
            write!(f, "\nKNOWN_STALE {line}")?;
        }
        let revalidate = [("revalidate", &self.revalidate)];
        for (name, t) in self.named().into_iter().chain(revalidate) {
            if !t.disagree.is_empty() {
                write!(f, "\nSTATE {name} disagree={}", t.disagree.len())?;
                if name == "as_output" {
                    write!(f, " of sub_only={sub_only}")?;
                }
                for d in t.disagree.iter().take(20) {
                    write!(f, "\nDIFF {name} {d}")?;
                }
            }
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
            tally.add(true, String::new);
            for d in *disagree {
                tally.add(false, || d.to_string());
            }
        }
        checks
    }

    fn compared() -> StateTallies {
        let mut state = StateTallies::default();
        state.utxo.add(true, String::new);
        state.accounts.add(true, String::new);
        state.undo.add(true, String::new);
        state.revalidate.add(true, String::new);
        state.params.add(true, String::new);
        state.readtx.add(true, String::new);
        state.script.add(true, String::new);
        state.datum.add(true, String::new);
        state
    }

    fn tally(lines: &[&str]) -> Tally {
        Tally {
            compared: 2 * lines.len(),
            disagree: lines.iter().map(|l| l.to_string()).collect(),
        }
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
            known: vec![line.clone()],
            known_fired: BTreeMap::from([((Locus::Accounts, supp(n)), row)]),
            known_stale: vec![line],
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
            known: vec!["line0".to_string(), "line1".to_string()],
            known_fired: BTreeMap::from([
                ((Locus::Accounts, supp(0)), Row::S1),
                ((Locus::Accounts, supp(1)), Row::S2),
            ]),
            known_stale: vec!["line0".to_string(), "line1".to_string()],
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
        state.as_output.add(true, String::new);
        assert!(state.pass(), "{state}");
        let empties: [fn(&mut StateTallies) -> &mut Tally; 8] = [
            |s| &mut s.utxo,
            |s| &mut s.as_output,
            |s| &mut s.accounts,
            |s| &mut s.undo,
            |s| &mut s.params,
            |s| &mut s.readtx,
            |s| &mut s.script,
            |s| &mut s.datum,
        ];
        for empty in empties {
            let mut state = state.clone();
            *empty(&mut state) = Tally::default();
            assert!(!state.pass(), "{state}");
        }
        assert!(state.to_string().contains(
            " seeded_unjudged=0 accounts=1 undo=1 revalidate=1 of dependents=0 params=1 \
                 readtx=1 script=1 datum=1"
        ));
    }

    #[test]
    fn a_corpus_with_no_dependent_passes_and_a_kept_dependent_fails() {
        let mut state = compared();
        state.as_output.add(true, String::new);
        state.revalidate = Tally::default();
        assert!(state.pass(), "{state}");
        state.revalidate.add(true, String::new);
        assert!(state.pass(), "{state}");
        let line = "supp#2 kept_after_rollback_to=Slot(40)";
        state.revalidate.add(false, || line.to_string());
        assert!(!state.pass(), "{state}");
        let text = state.to_string();
        assert!(text.contains("\nSTATE revalidate disagree=1\n"), "{text}");
        assert!(
            text.contains(&format!("\nDIFF revalidate {line}")),
            "{text}"
        );
    }

    #[test]
    fn a_group_that_did_not_roll_prints_one_line_and_fails() {
        let mut state = compared();
        state.as_output.add(true, String::new);
        assert!(state.pass(), "{state}");
        assert!(state.to_string().contains(" datum=1 rolled_failed=0"));
        assert!(!state.to_string().contains("ROLL_FAIL"));
        state.roll_fail.push(RollFail {
            first: supp(3),
            last: supp(5),
            records: 3,
            slot: 40,
            cause: "panic at=a.rs:1:2 explicit panic".to_string(),
        });
        assert!(!state.pass(), "{state}");
        let text = state.to_string();
        assert!(text.contains(" datum=1 rolled_failed=3\n"), "{text}");
        assert!(
            text.contains(
                "\nROLL_FAIL group=supp#3..supp#5 records=3 slot=40 panic at=a.rs:1:2 explicit panic"
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
        assert!(state.pass(), "{state}");
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
        assert!(state.pass(), "{state}");
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
        assert_eq!(
            state.known_stale,
            vec![
                "supp#9 row=S3 locus=as_output".to_string(),
                "supp#10 row=S3 locus=as_output not_compared".to_string()
            ]
        );
        assert!(state.known_fired.is_empty());
        assert!(!state.pass());
    }

    #[test]
    fn an_account_disagreement_off_the_known_list_fails_and_a_known_one_passes() {
        let diff = "account=keyHash-00 ledger=balance=0,pool=none,drep=none dolos=balance=0";
        let mut state = compared();
        state.as_output.add(true, String::new);
        let as_output = KnownList::from([((Locus::AsOutput, supp(9)), Row::S3)]);
        state.settle_accounts(CorpusKind::Supp, checks(&[(9, &[diff])]), &as_output);
        assert_eq!(state.accounts.disagree, vec![format!("supp#9 {diff}")]);
        assert!(state.known.is_empty());
        assert!(!state.pass(), "{state}");

        let mut state = compared();
        state.as_output.add(true, String::new);
        let known = KnownList::from([((Locus::Accounts, supp(9)), Row::S18)]);
        state.settle_accounts(CorpusKind::Supp, checks(&[(9, &[diff])]), &known);
        assert_eq!(
            state.known,
            vec![format!("row=S18 ids=supp#9 accounts {diff}")]
        );
        assert_eq!(state.known_fired, known);
        assert!(state.accounts.disagree.is_empty());
        assert!(state.pass(), "{state}");
    }

    #[test]
    fn a_disagreement_off_the_known_list_fails_with_its_record() {
        let mut state = compared();
        let diff = "ledger=ab dolos=none input=00#1 services=FetchBlock";
        state.settle_as_output(CorpusKind::Supp, checks(&[(9, &[diff])]), &KnownList::new());
        assert_eq!(state.as_output.disagree, vec![format!("supp#9 {diff}")]);
        assert!(!state.pass());
        assert!(state
            .to_string()
            .contains("\nSTATE as_output disagree=1 of sub_only=0\n"));
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
