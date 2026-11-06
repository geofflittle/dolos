use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::Read,
};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    record::{CorpusKind, Record, RecordId},
    run::{self, KeptRun},
};

/// The sha256 of the decompressed Imp spec corpus.
pub const IC_SHA256: &str = "38eb868092373a3d10f9f0da913216115b5ce00459993b435768b2e6274575d6";

/// The sha256 of the decompressed supplement corpus.
pub const SUPP_SHA256: &str = "0b13f2138fb9da18f9d1c8e32166e712c534745e5cee56151a28155d40ae65ba";

/// The sha256 of the decompressed phase one corpus.
pub const P1_SHA256: &str = "861611cc08a7ff09d45e1bdfda556ab8c8a0bed534df8a16ed228a74481539a2";

const IC_GZ: &[u8] = include_bytes!("../../../test_data/ledger-oracle/ic.jsonl.gz");
const SUPP_GZ: &[u8] = include_bytes!("../../../test_data/ledger-oracle/supp.jsonl.gz");
const P1_GZ: &[u8] = include_bytes!("../../../test_data/ledger-oracle/p1.jsonl.gz");

impl CorpusKind {
    /// The committed sha256 of this corpus.
    pub fn sha256(self) -> &'static str {
        match self {
            CorpusKind::Ic => IC_SHA256,
            CorpusKind::Supp => SUPP_SHA256,
            CorpusKind::P1 => P1_SHA256,
        }
    }

    pub(crate) fn gz(self) -> &'static [u8] {
        match self {
            CorpusKind::Ic => IC_GZ,
            CorpusKind::Supp => SUPP_GZ,
            CorpusKind::P1 => P1_GZ,
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
    pub runs: Vec<KeptRun>,
}

impl Corpus {
    /// Splits loaded records into runs.
    pub fn new(v: Verified, records: Vec<Record>) -> Self {
        Corpus {
            kind: v.kind,
            sha256: hex::encode(Sha256::digest(v.bytes)),
            records: records.len(),
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
    use crate::record::{LedgerRedeemers, Redeemers};

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
            "CORPUS ic sha=38eb868092373a3d records=9853 runs=892 kept_records=9853"
        );
        assert_eq!(
            load(CorpusKind::Supp).unwrap().to_string(),
            "CORPUS supp sha=0b13f2138fb9da18 records=190 runs=58 kept_records=190"
        );
        assert_eq!(
            load(CorpusKind::P1).unwrap().to_string(),
            "CORPUS p1 sha=861611cc08a7ff09 records=65 runs=22 kept_records=65"
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
            .starts_with("CORPUS ic sha mismatch expected=38eb8680"));
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
    fn committed_corpora_carry_the_arguments_of_each_context_the_ledger_built() {
        let built = |kind| {
            let records =
                records(verify(kind, &gunzip(kind.gz()).unwrap(), kind.sha256()).unwrap());
            let redeemers = records
                .unwrap()
                .into_iter()
                .flat_map(|r| match r.redeemers {
                    LedgerRedeemers::Read(Redeemers { top, subs }) => {
                        top.into_iter().chain(subs.into_iter().flatten()).collect()
                    }
                    LedgerRedeemers::Undecoded => Vec::new(),
                })
                .collect::<Vec<_>>();
            let built = redeemers.iter().filter(|r| r.args.is_some()).count();
            (redeemers.len(), built)
        };
        assert_eq!(built(CorpusKind::Ic), (472, 402));
        assert_eq!(built(CorpusKind::Supp), (30, 27));
    }

    #[test]
    fn committed_corpora_list_no_redeemer_for_bytes_the_decoder_refused() {
        let undecoded = |kind| {
            let records =
                records(verify(kind, &gunzip(kind.gz()).unwrap(), kind.sha256()).unwrap());
            let undecoded = records.unwrap().into_iter();
            undecoded
                .filter(|r| r.redeemers == LedgerRedeemers::Undecoded)
                .count()
        };
        assert_eq!(undecoded(CorpusKind::Ic), 1);
        assert_eq!(undecoded(CorpusKind::Supp), 33);
    }

    #[test]
    fn a_record_the_decoder_refused_listing_redeemers_fails_the_load() {
        let decoder_refused = |v: &serde_json::Value| {
            v["outcome"]["refused"]["failure_names"][0][0]
                .as_str()
                .is_some_and(|name| name.starts_with("DecoderError"))
        };
        let lines: Vec<serde_json::Value> = ic_bytes()
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_slice(l).unwrap())
            .collect();
        let refused = lines.iter().position(decoder_refused).unwrap();
        let redeemer = lines
            .iter()
            .find_map(|v| v["ex_units"].as_array()?.first().cloned())
            .unwrap();
        let unchanged = edit_line(refused, |_| {});
        assert_eq!(reload(CorpusKind::Ic, &unchanged).unwrap().len(), 9853);
        let listed = edit_line(refused, |v| {
            v["ex_units"] = serde_json::Value::Array(vec![redeemer.clone()]);
        });
        let e = reload(CorpusKind::Ic, &listed).unwrap_err();
        assert_eq!(
            e.to_string(),
            format!(
                "CORPUS ic load error records=1 first_line={} reason=the ledger's decoder refused the bytes and 1 redeemers are listed",
                refused + 1
            )
        );
    }

    #[test]
    fn a_redeemer_without_its_arguments_fails_the_load() {
        let line = ic_bytes()
            .split(|b| *b == b'\n')
            .position(|l| {
                let v: serde_json::Value = serde_json::from_slice(l).unwrap_or_default();
                v["ex_units"].as_array().is_some_and(|r| !r.is_empty())
            })
            .unwrap();
        let missing = edit_line(line, |v| {
            let redeemers = v["ex_units"].as_array_mut().unwrap();
            redeemers[0].as_object_mut().unwrap().remove("args");
        });
        let e = reload(CorpusKind::Ic, &missing).unwrap_err();
        assert_eq!(
            e.to_string(),
            format!(
                "CORPUS ic load error records=1 first_line={} reason=missing field `args`",
                line + 1
            )
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
        assert_eq!(input_totals(CorpusKind::Supp), [235, 31, 6, 16, 19, 0, 19]);
    }
}
