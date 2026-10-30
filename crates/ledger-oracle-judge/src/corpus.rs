use std::{collections::BTreeMap, fmt, io::Read};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    record::{CorpusKind, Globals, Record},
    run::{self, Run},
};

/// The sha256 of the decompressed Imp spec corpus.
pub const IC_SHA256: &str = "e541d1a3746d772279761f083fecd6a8b9e27eeb709709e1d6cd8dfdc46650e4";

/// The sha256 of the decompressed supplement corpus.
pub const SUPP_SHA256: &str = "c27521203db8fe223061a38e543bb646092e276f99404ffcf71ba217c2e31c93";

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
    pub runs: Vec<Run>,
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
    pub fn kept(&self) -> impl Iterator<Item = &run::KeptRun> {
        self.runs.iter().filter_map(|r| match r {
            Run::Kept(k) => Some(k),
            Run::Excluded { .. } => None,
        })
    }
}

impl fmt::Display for Corpus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let excluded = |test: fn(run::ExcludeReason) -> bool| {
            self.runs
                .iter()
                .filter(|r| matches!(r, Run::Excluded { reason, .. } if test(*reason)))
                .count()
        };
        write!(
            f,
            "CORPUS {} sha={} records={} runs={} kept_runs={} kept_records={} excluded_epoch={} excluded_pparams={}",
            self.kind.name(),
            &self.sha256[..16],
            self.records,
            self.runs.len(),
            self.kept().count(),
            self.kept().map(|k| k.records().len()).sum::<usize>(),
            excluded(run::ExcludeReason::epoch),
            excluded(run::ExcludeReason::pparams),
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
            "CORPUS ic sha=e541d1a3746d7722 records=9853 runs=700 kept_runs=574 kept_records=8063 excluded_epoch=112 excluded_pparams=29"
        );
        assert_eq!(
            load(CorpusKind::Supp).unwrap().to_string(),
            "CORPUS supp sha=c27521203db8fe22 records=119 runs=36 kept_runs=36 kept_records=119 excluded_epoch=0 excluded_pparams=0"
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
            .starts_with("CORPUS ic sha mismatch expected=e541d1a3"));
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
        assert_eq!(input_totals(CorpusKind::Supp), [144, 11, 1, 15, 18, 0, 18]);
    }
}
