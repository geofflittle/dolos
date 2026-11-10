use std::collections::{BTreeMap, BTreeSet};

use crate::{
    accounts::LedgerAccounts,
    cbor::TxIn,
    record::{CorpusKind, EraTime, LedgerOutcome, PParams, Pre, Record, RecordId, UtxoEntry},
};

/// The point in a run where an output enters the UTxO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Made {
    Seed,
    Record(u32),
}

/// One output the UTxO of a run holds after some record, with the records that
/// made and spent it.
#[derive(Debug, Clone)]
struct Life {
    entry: UtxoEntry,
    made: Made,
    spent: Option<u32>,
}

impl Life {
    /// Whether the UTxO holds the output at the start of some record numbered
    /// `first` to `last`.
    fn held(&self, first: u32, last: u32) -> bool {
        let made = match self.made {
            Made::Seed => true,
            Made::Record(m) => m < last,
        };
        made && !matches!(self.spent, Some(s) if s < first)
    }
}

/// Every output the run's UTxO holds at some record, from its seed and the
/// consumed and produced entries of each accepted record.
fn lives(seed: &[UtxoEntry], records: &[Record]) -> BTreeMap<TxIn, Life> {
    let mut lives: BTreeMap<TxIn, Life> = seed
        .iter()
        .map(|entry| {
            let life = Life {
                entry: entry.clone(),
                made: Made::Seed,
                spent: None,
            };
            (entry.input, life)
        })
        .collect();
    for record in records {
        let LedgerOutcome::Accepted {
            consumed, produced, ..
        } = &record.outcome
        else {
            continue;
        };
        for input in consumed {
            match lives.get_mut(input) {
                Some(life) if life.held(record.n, record.n) => life.spent = Some(record.n),
                _ => panic!(
                    "RECORD {} consumes an output its pre state does not hold",
                    record.n
                ),
            }
        }
        for entry in produced {
            let life = Life {
                entry: entry.clone(),
                made: Made::Record(record.n),
                spent: None,
            };
            if lives.insert(entry.input, life).is_some() {
                panic!(
                    "RECORD {} produces an output the run already held",
                    record.n
                );
            }
        }
    }
    lives
}

/// A run of records in one epoch under one parameter set and one era time map, each starting from the state the previous left.
#[derive(Debug, Clone)]
pub struct KeptRun {
    epoch: Option<u64>,
    pparams: PParams,
    era: EraTime,
    seed: Vec<UtxoEntry>,
    records: Vec<Record>,
    lives: BTreeMap<TxIn, Life>,
}

impl KeptRun {
    /// The UTxO the run starts from.
    pub fn seed(&self) -> &[UtxoEntry] {
        &self.seed
    }

    /// The entry the UTxO holds at an input when a record of the run starts.
    pub fn pre_entry(&self, record: &Record, input: &TxIn) -> Option<&UtxoEntry> {
        let life = self.lives.get(input)?;
        life.held(record.n, record.n).then_some(&life.entry)
    }

    /// Every input the UTxO holds when some record of a group of the run starts.
    pub fn held_in(&self, group: &[Record]) -> impl Iterator<Item = &TxIn> {
        let (first, last) = (group[0].n, group[group.len() - 1].n);
        let lives = self.lives.values();
        lives
            .filter(move |l| l.held(first, last))
            .map(|l| &l.entry.input)
    }

    /// The UTxO once a record of the run has run.
    pub fn post(&self, record: &Record) -> impl Iterator<Item = &UtxoEntry> {
        let next = record.n + 1;
        let lives = self.lives.values();
        lives.filter(move |l| l.held(next, next)).map(|l| &l.entry)
    }
    /// The epoch every record of the run is in.
    pub fn epoch(&self) -> Option<u64> {
        self.epoch
    }

    /// The protocol parameters every record of the run ran under.
    pub fn pparams(&self) -> &PParams {
        &self.pparams
    }

    /// The era time map every record of the run ran under.
    pub fn era(&self) -> &EraTime {
        &self.era
    }

    /// The records in ledger order, at least one.
    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// The run from the same seed without the records whose numbers `left_out` holds, none when it holds them all.
    pub fn without(&self, left_out: &BTreeSet<u32>) -> Option<KeptRun> {
        let records: Vec<Record> = self
            .records
            .iter()
            .filter(|r| !left_out.contains(&r.n))
            .cloned()
            .collect();
        if records.is_empty() {
            return None;
        }
        Some(KeptRun {
            epoch: self.epoch,
            pparams: self.pparams.clone(),
            era: self.era.clone(),
            lives: lives(&self.seed, &records),
            seed: self.seed.clone(),
            records,
        })
    }

    /// The records split into maximal sequences that share one slot.
    pub fn groups(&self) -> impl Iterator<Item = &[Record]> {
        self.records.chunk_by(|a, b| a.slot == b.slot)
    }

    /// The record whose ledger run made each output that a later record of the run can spend.
    pub fn makers(&self, kind: CorpusKind) -> BTreeMap<TxIn, RecordId> {
        let made = self
            .lives
            .iter()
            .filter_map(|(input, life)| match life.made {
                Made::Seed => None,
                Made::Record(n) => Some((*input, RecordId { corpus: kind, n })),
            });
        made.collect()
    }

    /// The origin of one input of a record, given the run's makers and the
    /// records of its slot group.
    pub fn origin(
        &self,
        input: &TxIn,
        record: &Record,
        makers: &BTreeMap<TxIn, RecordId>,
        group: &[Record],
    ) -> InputOrigin {
        if self.pre_entry(record, input).is_none() {
            return InputOrigin::AbsentFromPre;
        }
        match makers.get(input) {
            None => InputOrigin::Seeded,
            Some(made_by) if group.iter().any(|r| r.n == made_by.n) => {
                InputOrigin::InGroup { made_by: *made_by }
            }
            Some(made_by) => InputOrigin::InRun { made_by: *made_by },
        }
    }
}

/// Where an input a record names comes from, as the ledger saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputOrigin {
    Seeded,
    InRun { made_by: RecordId },
    InGroup { made_by: RecordId },
    AbsentFromPre,
}

/// The accounts a record leaves for the next record.
fn accounts_left(record: &Record) -> &LedgerAccounts {
    match &record.outcome {
        LedgerOutcome::Accepted { accounts_post, .. } => accounts_post,
        LedgerOutcome::Refused { .. } => &record.accounts_pre,
    }
}

/// Whether a record starts from the epoch, parameters, era time map and accounts the previous record left.
fn continues(prev: &Record, next: &Record) -> bool {
    next.epoch == prev.epoch
        && next.pparams == prev.pparams
        && next.accounts_pre == *accounts_left(prev)
        && next.time.era == prev.time.era
}

/// Cuts records that share one UTxO history into runs at each record that does not continue the previous, each run seeded with the UTxO its first record starts from.
fn cut(records: Vec<Record>) -> Vec<KeptRun> {
    let first = &records[0];
    let Pre::Fresh(seed) = &first.pre else {
        panic!("RECORD {} starts a corpus from a previous record", first.n);
    };
    let history = lives(seed, &records);
    let mut parts: Vec<Vec<Record>> = Vec::new();
    for record in records {
        match parts.last_mut() {
            Some(part) if continues(&part[part.len() - 1], &record) => part.push(record),
            _ => parts.push(vec![record]),
        }
    }
    parts
        .into_iter()
        .map(|records| {
            let n = records[0].n;
            let held = history.values().filter(|l| l.held(n, n));
            let seed: Vec<UtxoEntry> = held.map(|l| l.entry.clone()).collect();
            KeptRun {
                epoch: records[0].epoch,
                pparams: records[0].pparams.clone(),
                era: records[0].time.era.clone(),
                lives: lives(&seed, &records),
                seed,
                records,
            }
        })
        .collect()
}

/// Splits records into runs, each in one epoch under one parameter set and each record starting from the state the previous left.
pub fn split(records: Vec<Record>) -> Vec<KeptRun> {
    let mut histories: Vec<Vec<Record>> = Vec::new();
    for record in records {
        match histories.last_mut() {
            Some(history) if record.pre == Pre::FollowsPrev => history.push(record),
            _ => histories.push(vec![record]),
        }
    }
    histories.into_iter().flat_map(cut).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::{cbor::tx_inputs, corpus};

    fn supp(n: u32) -> RecordId {
        RecordId {
            corpus: CorpusKind::Supp,
            n,
        }
    }

    /// The origin of every spend input of one supplement record.
    fn spend_origins(n: u32) -> Vec<InputOrigin> {
        let corpus = corpus::load(CorpusKind::Supp).unwrap();
        let run = corpus
            .kept()
            .find(|r| r.records().iter().any(|x| x.n == n))
            .unwrap();
        let makers = run.makers(CorpusKind::Supp);
        let group = run.groups().find(|g| g.iter().any(|x| x.n == n)).unwrap();
        let record = group.iter().find(|x| x.n == n).unwrap();
        let inputs = tx_inputs(&record.tx.0).unwrap();
        let spend = inputs
            .spend
            .iter()
            .chain(inputs.subs.iter().flat_map(|s| &s.spend));
        spend
            .map(|i| run.origin(i, record, &makers, group))
            .collect()
    }

    #[test]
    fn the_first_record_of_a_run_spends_only_seeded_inputs() {
        let origins = spend_origins(6);
        assert!(!origins.is_empty());
        assert!(
            origins.iter().all(|o| *o == InputOrigin::Seeded),
            "{origins:?}"
        );
    }

    #[test]
    fn an_output_made_earlier_in_the_slot_group_is_in_group() {
        assert!(spend_origins(2).contains(&InputOrigin::InGroup { made_by: supp(1) }));
    }

    #[test]
    fn an_output_made_in_an_earlier_slot_group_is_in_run() {
        assert!(spend_origins(9).contains(&InputOrigin::InRun { made_by: supp(8) }));
    }

    #[test]
    fn an_input_the_ledger_did_not_hold_is_absent_from_pre() {
        assert!(spend_origins(5).contains(&InputOrigin::AbsentFromPre));
    }

    /// The records of a committed corpus as the extractor wrote them.
    fn written(kind: CorpusKind) -> Vec<Record> {
        let bytes = corpus::gunzip(kind.gz()).unwrap();
        corpus::records(corpus::verify(kind, &bytes, kind.sha256()).unwrap()).unwrap()
    }

    #[test]
    fn records_in_one_epoch_under_one_parameter_set_stay_in_one_run() {
        let records = written(CorpusKind::Supp);
        let fresh = records.iter().filter(|r| r.pre != Pre::FollowsPrev).count();
        let repriced: Vec<u32> = records
            .windows(2)
            .filter(|w| w[1].pre == Pre::FollowsPrev && w[1].pparams != w[0].pparams)
            .map(|w| w[1].n)
            .collect();
        assert_eq!((fresh, &repriced[..]), (723, &[374][..]));
        assert_eq!(split(records).len(), fresh + repriced.len());
    }

    #[test]
    fn every_record_of_the_corpus_is_in_a_run() {
        let records = written(CorpusKind::Ic);
        let total = records.len();
        let runs = split(records);
        let held: usize = runs.iter().map(|r| r.records().len()).sum();
        assert_eq!((held, total), (9853, 9853));
    }

    #[test]
    fn a_run_cut_at_an_epoch_change_starts_from_the_utxo_the_previous_record_left() {
        let runs = split(written(CorpusKind::Ic));
        let (before, after) = runs
            .windows(2)
            .find(|w| w[1].records()[0].pre == Pre::FollowsPrev && w[0].epoch() != w[1].epoch())
            .map(|w| (&w[0], &w[1]))
            .unwrap();
        let last = &before.records()[before.records().len() - 1];
        let left: BTreeSet<TxIn> = before.post(last).map(|e| e.input).collect();
        let seed: BTreeSet<TxIn> = after.seed().iter().map(|e| e.input).collect();
        assert!(!seed.is_empty());
        assert_eq!(seed, left);
    }

    #[test]
    fn a_record_starting_from_other_accounts_than_the_previous_left_does_not_continue() {
        let records = written(CorpusKind::Supp);
        let (prev, next) = records
            .windows(2)
            .find(|w| w[1].pre == Pre::FollowsPrev && !w[1].accounts_pre.is_empty())
            .map(|w| (&w[0], &w[1]))
            .unwrap();
        assert!(continues(prev, next));
        let mut moved = next.clone();
        moved.accounts_pre.clear();
        assert!(!continues(prev, &moved));
    }

    #[test]
    fn a_record_under_another_era_time_map_does_not_continue_and_another_horizon_does() {
        let records = written(CorpusKind::Supp);
        let (prev, next) = records
            .windows(2)
            .find(|w| continues(&w[0], &w[1]))
            .map(|w| (&w[0], &w[1]))
            .unwrap();
        let mut bounded = next.clone();
        bounded.time.horizon_slot = Some(next.slot + 1);
        assert!(continues(prev, &bounded));
        let mut shifted = next.clone();
        shifted.time.era.system_start_ms += 1000;
        assert!(!continues(prev, &shifted));
    }
}
