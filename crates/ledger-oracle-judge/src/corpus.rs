use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::Read,
};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    record::{CorpusKind, Globals, Record, RecordId},
    run::{self, KeptRun},
};

/// The sha256 of the decompressed Imp spec corpus.
pub const IC_SHA256: &str = "015e7b932273eb86c6ccb10c503913c86afabaf5ea7d7a34480ddb7af3176353";

/// The sha256 of the decompressed supplement corpus.
pub const SUPP_SHA256: &str = "7ca1ea0e4444845e84493280faf6d06016432a1472adf8836c50c0e6236d6c71";

const IC_GZ: &[u8] = include_bytes!("../../../test_data/ledger-oracle/ic.jsonl.gz");
const SUPP_GZ: &[u8] = include_bytes!("../../../test_data/ledger-oracle/supp.jsonl.gz");

impl CorpusKind {
    /// The committed sha256 of this corpus.
    pub fn sha256(self) -> &'static str {
        match self {
            CorpusKind::Ic => IC_SHA256,
            CorpusKind::Supp => SUPP_SHA256,
        }
    }

    fn gz(self) -> &'static [u8] {
        match self {
            CorpusKind::Ic => IC_GZ,
            CorpusKind::Supp => SUPP_GZ,
        }
    }
}

/// Corpus bytes whose sha256 equals the one they were checked against.
#[derive(Debug, Clone, Copy)]
pub struct Verified<'a> {
    kind: CorpusKind,
    bytes: &'a [u8],
}

/// Corpus bytes whose sha256 is not the expected one.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("CORPUS {} sha mismatch expected={expected} found={found}", .kind.name())]
pub struct ShaMismatch {
    pub kind: CorpusKind,
    pub expected: String,
    pub found: String,
}

/// Checks corpus bytes against an expected sha256.
pub fn verify<'a>(
    kind: CorpusKind,
    bytes: &'a [u8],
    expected: &str,
) -> Result<Verified<'a>, ShaMismatch> {
    let found = hex::encode(Sha256::digest(bytes));
    match found == expected {
        true => Ok(Verified { kind, bytes }),
        false => Err(ShaMismatch {
            kind,
            expected: expected.to_string(),
            found,
        }),
    }
}

/// The lines of a corpus that failed to load, grouped by reason.
#[derive(Debug, PartialEq, Eq)]
pub struct LoadErrors {
    pub kind: CorpusKind,
    pub reasons: BTreeMap<String, (usize, usize)>,
}

impl fmt::Display for LoadErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, (reason, (records, first_line))) in self.reasons.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(
                f,
                "CORPUS {} load error records={records} first_line={first_line} reason={reason}",
                self.kind.name()
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for LoadErrors {}

/// Parses every line of verified corpus bytes into a record.
pub fn records(v: Verified) -> Result<Vec<Record>, LoadErrors> {
    let mut out = Vec::new();
    let mut reasons: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for (i, line) in v.bytes.split(|b| *b == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice::<Record>(line) {
            Ok(record) => out.push(record),
            Err(e) => {
                let text = e.to_string();
                let reason = text
                    .rsplit_once(" at line ")
                    .map_or(text.as_str(), |(r, _)| r);
                reasons.entry(reason.to_string()).or_insert((0, i + 1)).0 += 1;
            }
        }
    }
    match reasons.is_empty() {
        true => Ok(out),
        false => Err(LoadErrors {
            kind: v.kind,
            reasons,
        }),
    }
}

/// One corpus split into runs.
#[derive(Debug, Clone)]
pub struct Corpus {
    pub kind: CorpusKind,
    pub sha256: String,
    pub records: usize,
    pub globals: Option<Globals>,
    pub runs: Vec<KeptRun>,
}

impl Corpus {
    /// Splits loaded records into runs.
    pub fn new(v: Verified, records: Vec<Record>) -> Self {
        let globals = records.iter().find_map(|r| r.globals.clone());
        Corpus {
            kind: v.kind,
            sha256: hex::encode(Sha256::digest(v.bytes)),
            records: records.len(),
            globals,
            runs: run::split(records),
        }
    }

    /// The runs kept for judgement.
    pub fn kept(&self) -> impl Iterator<Item = &KeptRun> {
        self.runs.iter()
    }

    /// The corpus with only the kept runs that hold a record of this corpus
    /// the set names.
    pub fn select(&self, named: &BTreeSet<RecordId>) -> Result<Corpus, NotKept> {
        let mine: BTreeSet<u32> = named
            .iter()
            .filter(|id| id.corpus == self.kind)
            .map(|id| id.n)
            .collect();
        let kept: Vec<&KeptRun> = self
            .kept()
            .filter(|run| run.records().iter().any(|r| mine.contains(&r.n)))
            .collect();
        let held: BTreeSet<u32> = kept
            .iter()
            .flat_map(|run| run.records().iter().map(|r| r.n))
            .collect();
        if let Some(n) = mine.difference(&held).next() {
            return Err(NotKept(RecordId {
                corpus: self.kind,
                n: *n,
            }));
        }
        Ok(Corpus {
            kind: self.kind,
            sha256: self.sha256.clone(),
            records: held.len(),
            globals: self.globals.clone(),
            runs: kept.into_iter().cloned().collect(),
        })
    }
}

/// A named record that no kept run of its corpus holds.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("SELECTION {0} is in no kept run")]
pub struct NotKept(pub RecordId);

impl fmt::Display for Corpus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CORPUS {} sha={} records={} runs={} kept_records={}",
            self.kind.name(),
            &self.sha256[..16],
            self.records,
            self.runs.len(),
            self.kept().map(|k| k.records().len()).sum::<usize>(),
        )
    }
}

/// A committed corpus that does not load.
#[derive(Debug, Error)]
pub enum LoadError {
    #[error("CORPUS gzip error {0}")]
    Gzip(#[from] std::io::Error),
    #[error(transparent)]
    Sha(#[from] ShaMismatch),
    #[error(transparent)]
    Parse(#[from] LoadErrors),
}

/// Decompresses gzip bytes.
pub fn gunzip(gz: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(gz).read_to_end(&mut out)?;
    Ok(out)
}

/// Loads a committed corpus after checking its sha256.
pub fn load(kind: CorpusKind) -> Result<Corpus, LoadError> {
    let bytes = gunzip(kind.gz())?;
    let v = verify(kind, &bytes, kind.sha256())?;
    let records = records(v)?;
    Ok(Corpus::new(v, records))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cbor;

    fn ic_bytes() -> Vec<u8> {
        gunzip(CorpusKind::Ic.gz()).unwrap()
    }

    fn reload(kind: CorpusKind, bytes: &[u8]) -> Result<Vec<Record>, LoadErrors> {
        let sha = hex::encode(Sha256::digest(bytes));
        records(verify(kind, bytes, &sha).unwrap())
    }

    fn edit_line(n: usize, edit: impl Fn(&mut serde_json::Value)) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, line) in ic_bytes().split(|b| *b == b'\n').enumerate() {
            if line.is_empty() {
                continue;
            }
            let mut v: serde_json::Value = serde_json::from_slice(line).unwrap();
            if i == n {
                edit(&mut v);
            }
            out.extend(serde_json::to_vec(&v).unwrap());
            out.push(b'\n');
        }
        out
    }

    #[test]
    fn committed_corpora_load_into_their_runs() {
        assert_eq!(
            load(CorpusKind::Ic).unwrap().to_string(),
            "CORPUS ic sha=015e7b932273eb86 records=9853 runs=892 kept_records=9853"
        );
        assert_eq!(
            load(CorpusKind::Supp).unwrap().to_string(),
            "CORPUS supp sha=7ca1ea0e4444845e records=125 runs=39 kept_records=125"
        );
    }

    #[test]
    fn one_flipped_byte_is_a_sha_mismatch() {
        let mut bytes = ic_bytes();
        assert!(verify(CorpusKind::Ic, &bytes, IC_SHA256).is_ok());
        bytes[100] ^= 1;
        let e = verify(CorpusKind::Ic, &bytes, IC_SHA256).unwrap_err();
        assert!(e
            .to_string()
            .starts_with("CORPUS ic sha mismatch expected=015e7b93"));
    }

    #[test]
    fn a_record_without_pparams_fails_the_load() {
        let unchanged = edit_line(3, |_| {});
        assert_eq!(reload(CorpusKind::Ic, &unchanged).unwrap().len(), 9853);
        let missing = edit_line(3, |v| {
            v.as_object_mut().unwrap().remove("pparams");
        });
        let e = reload(CorpusKind::Ic, &missing).unwrap_err();
        assert_eq!(
            e.to_string(),
            "CORPUS ic load error records=1 first_line=4 reason=missing field `pparams`"
        );
    }

    #[test]
    fn a_record_without_its_pool_vote_or_governance_state_fails_the_load() {
        for field in ["certs_pre", "gov_pre", "treasury"] {
            let missing = edit_line(3, |v| {
                v.as_object_mut().unwrap().remove(field);
            });
            let e = reload(CorpusKind::Ic, &missing).unwrap_err();
            assert_eq!(
                e.to_string(),
                format!(
                    "CORPUS ic load error records=1 first_line=4 reason=missing field `{field}`"
                )
            );
        }
    }

    #[test]
    fn a_refused_record_without_failure_names_fails_the_load() {
        let refused = ic_bytes()
            .split(|b| *b == b'\n')
            .position(|l| l.windows(9).any(|w| w == b"\"refused\""))
            .unwrap();
        let empty = edit_line(refused, |v| {
            v["outcome"]["refused"]["failure_names"] = serde_json::json!([]);
        });
        let e = reload(CorpusKind::Ic, &empty).unwrap_err();
        assert_eq!(e.reasons.len(), 1);
        let (reason, (records, _)) = e.reasons.iter().next().unwrap();
        assert_eq!(*records, 1);
        assert!(reason.contains("expected at least one element"), "{reason}");
    }

    #[test]
    fn a_leaf_outside_the_class_table_fails_the_load() {
        let refused = ic_bytes()
            .split(|b| *b == b'\n')
            .position(|l| l.windows(9).any(|w| w == b"\"refused\""))
            .unwrap();
        let unclassified = edit_line(refused, |v| {
            v["outcome"]["refused"]["failure_names"][0] = serde_json::json!(["UtxoFailure", "Nil"]);
        });
        let e = reload(CorpusKind::Ic, &unclassified).unwrap_err();
        assert_eq!(e.reasons.len(), 1);
        let (reason, (records, _)) = e.reasons.iter().next().unwrap();
        assert_eq!(*records, 1);
        assert!(reason.contains("unclassified ledger leaf Nil"), "{reason}");
    }

    fn numbers(corpus: &Corpus) -> Vec<u32> {
        let kept = corpus.kept();
        kept.flat_map(|run| run.records().iter().map(|r| r.n))
            .collect()
    }

    #[test]
    fn a_record_in_no_kept_run_is_refused() {
        let corpus = load(CorpusKind::Ic).unwrap();
        let named = BTreeSet::from([RecordId {
            corpus: CorpusKind::Ic,
            n: 9853,
        }]);
        assert_eq!(
            corpus.select(&named).unwrap_err().to_string(),
            "SELECTION ic#9853 is in no kept run"
        );
    }

    #[test]
    fn a_record_of_the_other_corpus_selects_no_run() {
        let corpus = load(CorpusKind::Supp).unwrap();
        let named = BTreeSet::from([RecordId {
            corpus: CorpusKind::Ic,
            n: 9,
        }]);
        let selected = corpus.select(&named).unwrap();
        assert_eq!(selected.runs.len(), 0);
        assert_eq!(selected.records, 0);
    }

    #[test]
    fn a_selection_keeps_every_record_of_each_named_run_and_no_other_run() {
        let corpus = load(CorpusKind::Supp).unwrap();
        let named = BTreeSet::from([RecordId {
            corpus: CorpusKind::Supp,
            n: 9,
        }]);
        let selected = corpus.select(&named).unwrap();
        let run = corpus
            .kept()
            .find(|run| run.records().iter().any(|r| r.n == 9))
            .unwrap();
        let whole: Vec<u32> = run.records().iter().map(|r| r.n).collect();
        assert!(whole.contains(&8), "{whole:?}");
        assert_eq!(numbers(&selected), whole);
        assert_eq!(selected.records, whole.len());
        assert_eq!(selected.sha256, corpus.sha256);
    }

    fn input_totals(kind: CorpusKind) -> [usize; 7] {
        let bytes = gunzip(kind.gz()).unwrap();
        let mut t = [0usize; 7];
        for r in reload(kind, &bytes).unwrap() {
            let i = cbor::tx_inputs(&r.tx.0).unwrap();
            t[0] += i.spend.len();
            t[1] += i.collateral.len();
            t[2] += i.reference.len();
            t[3] += i.subs.len();
            t[4] += i.subs.iter().map(|s| s.spend.len()).sum::<usize>();
            t[5] += i.subs.iter().map(|s| s.reference.len()).sum::<usize>();
            t[6] += i.child_only().count();
        }
        t
    }

    #[test]
    fn cbor_reader_input_totals_equal_an_independent_reader() {
        assert_eq!(
            input_totals(CorpusKind::Ic),
            [10239, 467, 5162, 473, 475, 16, 474]
        );
        assert_eq!(input_totals(CorpusKind::Supp), [151, 12, 1, 16, 19, 0, 19]);
    }
}
