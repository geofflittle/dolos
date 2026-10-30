use std::collections::BTreeMap;

use crate::{
    cbor::TxIn,
    record::{CorpusKind, LedgerOutcome, PParams, Pre, Record, RecordId, UtxoEntry},
};

/// Why a run of records is left out of the judgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExcludeReason {
    Epoch,
    PParams,
    EpochAndPParams,
}

impl ExcludeReason {
    /// Whether the run spans more than one epoch.
    pub fn epoch(self) -> bool {
        matches!(self, ExcludeReason::Epoch | ExcludeReason::EpochAndPParams)
    }

    /// Whether the protocol parameters change inside the run.
    pub fn pparams(self) -> bool {
        matches!(
            self,
            ExcludeReason::PParams | ExcludeReason::EpochAndPParams
        )
    }
}

/// The point in a run where an output enters the UTxO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Made {
    Seed,
    Record(u32),
}

/// One output the UTxO of a run holds after some record, with the records that made and spent it.
#[derive(Debug, Clone)]
struct Life {
    entry: UtxoEntry,
    made: Made,
    spent: Option<u32>,
}

impl Life {
    /// Whether the UTxO holds the output at the start of some record numbered `first` to `last`.
    fn held(&self, first: u32, last: u32) -> bool {
        let made = match self.made {
            Made::Seed => true,
            Made::Record(m) => m < last,
        };
        made && !matches!(self.spent, Some(s) if s < first)
    }
}

/// Every output the run's UTxO holds at some record, from its seed and the consumed and produced entries of each accepted record.
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

/// A run of records in one epoch under one parameter set.
#[derive(Debug, Clone)]
pub struct KeptRun {
    epoch: Option<u64>,
    pparams: PParams,
    records: Vec<Record>,
    lives: BTreeMap<TxIn, Life>,
}

impl KeptRun {
    /// The UTxO the run starts from.
    pub fn seed(&self) -> &[UtxoEntry] {
        match &self.records[0].pre {
            Pre::Fresh(seed) => seed,
            Pre::FollowsPrev => unreachable!("a kept run starts at a fresh pre state"),
        }
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

    /// The records in ledger order, at least one.
    pub fn records(&self) -> &[Record] {
        &self.records
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

    /// The origin of one input of a record, given the run's makers and the records of its slot group.
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

/// A maximal sequence of records where each starts from the state the previous left.
#[derive(Debug, Clone)]
pub enum Run {
    Kept(KeptRun),
    Excluded {
        reason: ExcludeReason,
        first: u32,
        records: usize,
    },
}

fn classify(records: Vec<Record>) -> Run {
    let first = &records[0];
    let Pre::Fresh(seed) = &first.pre else {
        panic!("RECORD {} starts a corpus from a previous record", first.n);
    };
    let epoch_changes = records.iter().any(|r| r.epoch != first.epoch);
    let pparams_change = records.iter().any(|r| r.pparams != first.pparams);
    let reason = match (epoch_changes, pparams_change) {
        (false, false) => {
            return Run::Kept(KeptRun {
                epoch: first.epoch,
                pparams: first.pparams.clone(),
                lives: lives(seed, &records),
                records,
            })
        }
        (true, false) => ExcludeReason::Epoch,
        (false, true) => ExcludeReason::PParams,
        (true, true) => ExcludeReason::EpochAndPParams,
    };
    Run::Excluded {
        reason,
        first: first.n,
        records: records.len(),
    }
}

/// Splits records into runs at each record that does not start from the previous state.
pub fn split(records: Vec<Record>) -> Vec<Run> {
    let mut groups: Vec<Vec<Record>> = Vec::new();
    for record in records {
        match groups.last_mut() {
            Some(group) if record.pre == Pre::FollowsPrev => group.push(record),
            _ => groups.push(vec![record]),
        }
    }
    groups.into_iter().map(classify).collect()
}

#[cfg(test)]
mod tests {
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
}
