use std::{collections::BTreeMap, fmt, str::FromStr};

use thiserror::Error;

use crate::{
    record::{CorpusKind, RecordId},
    row::{coverage, Check, Row},
};

const EXPECTED: &str = include_str!("../../../test_data/ledger-oracle/expected-unsupported.txt");
const KNOWN: &str = include_str!("../../../test_data/ledger-oracle/known.txt");

/// An unsupported feature label as Dolos's validator names it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Feature(pub String);

impl fmt::Display for Feature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The comparison a known disagreement shows in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Locus {
    Validation,
    AsOutput,
    Accounts,
}

impl Locus {
    /// The comparison the locus names.
    pub fn check(self) -> Check {
        match self {
            Locus::Validation => Check::Validation,
            Locus::AsOutput => Check::AsOutput,
            Locus::Accounts => Check::Accounts,
        }
    }
}

impl fmt::Display for Locus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Locus::Validation => "validation",
            Locus::AsOutput => "as_output",
            Locus::Accounts => "accounts",
        })
    }
}

impl FromStr for Locus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "validation" => Ok(Locus::Validation),
            "as_output" => Ok(Locus::AsOutput),
            "accounts" => Ok(Locus::Accounts),
            other => Err(format!("unknown locus {other}")),
        }
    }
}

macro_rules! live_loci {
    ($($locus:ident = $name:literal judged by $check:ident),* $(,)?) => {
        /// The comparison against a live node a known disagreement shows in.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum LiveLocus {
            $($locus),*
        }

        impl LiveLocus {
            /// Every live locus.
            pub const ALL: &'static [LiveLocus] = &[$(LiveLocus::$locus),*];

            /// The comparison the locus names.
            pub fn check(self) -> Check {
                match self {
                    $(LiveLocus::$locus => Check::$check),*
                }
            }

            /// The name the known list writes the locus as.
            pub fn name(self) -> &'static str {
                match self {
                    $(LiveLocus::$locus => $name),*
                }
            }
        }
    };
}

live_loci! {
    EraSummary = "era_summary" judged by EraSummary,
    Params = "live_params" judged by LiveParams,
    ChildAsOutput = "live_as_output" judged by LiveAsOutput,
    Block = "live_block" judged by LiveBlock,
    Utxo = "live_utxo" judged by LiveUtxo,
}

impl fmt::Display for LiveLocus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for LiveLocus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        LiveLocus::ALL
            .iter()
            .copied()
            .find(|locus| locus.name() == s)
            .ok_or(format!("unknown locus {s}"))
    }
}

/// A manifest line that does not parse.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("MANIFEST {file} line={line} {reason}")]
pub struct ManifestError {
    pub file: &'static str,
    pub line: usize,
    pub reason: String,
}

impl FromStr for RecordId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (corpus, n) = s.split_once('#').ok_or(format!("bad record id {s}"))?;
        let corpus = match corpus {
            "ic" => CorpusKind::Ic,
            "supp" => CorpusKind::Supp,
            other => return Err(format!("unknown corpus {other}")),
        };
        let n = n.parse().map_err(|_| format!("bad record number {n}"))?;
        Ok(RecordId { corpus, n })
    }
}

/// The feature each record is declared to be refused as unsupported for.
pub type Declared = BTreeMap<RecordId, Feature>;

/// The catalog row each known disagreement is filed under, by locus and record.
pub type KnownList = BTreeMap<(Locus, RecordId), Row>;

/// The catalog row each known disagreement with a live node is filed under, by locus.
pub type KnownLive = BTreeMap<LiveLocus, Row>;

/// The committed known disagreements, against the corpora and against a live node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Known {
    pub records: KnownList,
    pub live: KnownLive,
}

fn lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.trim()))
        .filter(|(_, l)| !l.is_empty())
}

/// Reads declared support, one `<record> <label>` per line.
pub fn parse_declared(file: &'static str, text: &str) -> Result<Declared, ManifestError> {
    let mut declared = Declared::new();
    for (line, l) in lines(text) {
        let error = |reason: String| ManifestError { file, line, reason };
        let (id, label) = l.split_once(' ').ok_or(error("no label".into()))?;
        let id: RecordId = id.parse().map_err(error)?;
        if declared.insert(id, Feature(label.to_string())).is_some() {
            return Err(error(format!("{id} declared twice")));
        }
    }
    Ok(declared)
}

/// Reads the known list, one `<row> <locus> <record>...` or `<row> <live locus>` per line.
pub fn parse_known(file: &'static str, text: &str) -> Result<Known, ManifestError> {
    let mut known = Known::default();
    for (line, l) in lines(text) {
        let error = |reason: String| ManifestError { file, line, reason };
        let mut words = l.split(' ');
        let row: Row = words.next().unwrap_or_default().parse().map_err(error)?;
        let locus = words.next().ok_or(error("no locus".into()))?;
        let judged = |check: Check, locus: &dyn fmt::Display| {
            if coverage(row).checks().contains(&check) {
                Ok(())
            } else {
                Err(error(format!("row {row} is not judged by {locus}")))
            }
        };
        if let Ok(live) = locus.parse::<LiveLocus>() {
            judged(live.check(), &live)?;
            if words.next().is_some() {
                return Err(error(format!("{live} takes no record")));
            }
            if known.live.insert(live, row).is_some() {
                return Err(error(format!("{live} known twice")));
            }
            continue;
        }
        let locus: Locus = locus.parse().map_err(error)?;
        judged(locus.check(), &locus)?;
        let mut ids = 0;
        for id in words {
            let id: RecordId = id.parse().map_err(error)?;
            if known.records.insert((locus, id), row).is_some() {
                return Err(error(format!("{id} known twice")));
            }
            ids += 1;
        }
        if ids == 0 {
            return Err(error("no record".into()));
        }
    }
    Ok(known)
}

/// Writes declared support as `parse_declared` reads it, one record per line in record order.
pub fn render_declared(declared: &Declared) -> String {
    let lines = declared.iter();
    lines
        .map(|(id, feature)| format!("{id} {feature}\n"))
        .collect()
}

/// Writes a known list as `parse_known` reads it, record entries by row and then live entries by row.
pub fn render_known(known: &Known) -> String {
    let records = known.records.iter();
    let mut records: Vec<_> = records
        .map(|(&(locus, id), &row)| (row, locus, id))
        .collect();
    records.sort();
    let mut live: Vec<_> = known
        .live
        .iter()
        .map(|(&locus, &row)| (row, locus))
        .collect();
    live.sort();
    let records = records
        .into_iter()
        .map(|(row, locus, id)| format!("{row} {locus} {id}\n"));
    let live = live
        .into_iter()
        .map(|(row, locus)| format!("{row} {locus}\n"));
    records.chain(live).collect()
}

/// The declared support and known entries one judgement's answers would commit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Candidates {
    pub declared: Declared,
    pub known: Known,
}

impl Candidates {
    /// Each candidate line after `CANDIDATE` and the name of the file it belongs in.
    pub fn lines(&self) -> Vec<String> {
        let files = [
            ("expected-unsupported.txt", render_declared(&self.declared)),
            ("known.txt", render_known(&self.known)),
        ];
        let lines = files.iter().flat_map(|(file, text)| {
            text.lines()
                .map(move |line| format!("CANDIDATE {file} {line}"))
        });
        lines.collect()
    }
}

/// The committed declared support.
pub fn declared() -> Result<Declared, ManifestError> {
    parse_declared("expected-unsupported.txt", EXPECTED)
}

/// The committed known list.
pub fn known() -> Result<Known, ManifestError> {
    parse_known("known.txt", KNOWN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_manifests_parse() {
        assert!(!declared().unwrap().is_empty());
        assert!(!known().unwrap().records.is_empty());
    }

    #[test]
    fn a_live_locus_names_its_row_and_no_record() {
        let known = parse_known("k", "S16 era_summary").unwrap();
        assert_eq!(
            known.live,
            KnownLive::from([(LiveLocus::EraSummary, Row::S16)])
        );
        let reason = |text| parse_known("k", text).unwrap_err().reason;
        assert_eq!(
            reason("S16 era_summary supp#5"),
            "era_summary takes no record"
        );
        assert_eq!(
            reason("S13 era_summary"),
            "row S13 is not judged by era_summary"
        );
        assert_eq!(
            reason("S16 era_summary\nS16 era_summary"),
            "era_summary known twice"
        );
    }

    #[test]
    fn a_record_on_two_known_rows_fails_the_load() {
        assert_eq!(
            parse_known("k", "V1 validation supp#5\nV2 validation supp#5").unwrap_err(),
            ManifestError {
                file: "k",
                line: 2,
                reason: "supp#5 known twice".into()
            }
        );
    }

    #[test]
    fn a_known_row_names_a_catalog_row_its_locus_judges() {
        let reason = |text| parse_known("k", text).unwrap_err().reason;
        assert_eq!(reason("V99 validation supp#5"), "unknown row V99");
        assert_eq!(
            reason("S13 validation supp#5"),
            "row S13 is not judged by validation"
        );
        assert_eq!(
            reason("S13 accounts ic#551"),
            "row S13 is not judged by accounts"
        );
    }

    #[test]
    fn an_accounts_row_names_its_record() {
        let known = parse_known("k", "S18 accounts ic#551").unwrap();
        assert_eq!(known.records.len(), 1);
        let ((locus, id), row) = known.records.iter().next().unwrap();
        assert_eq!(
            (locus.check(), id.to_string(), *row),
            (Check::Accounts, "ic#551".to_string(), Row::S18)
        );
        assert_eq!(render_known(&known), "S18 accounts ic#551\n");
    }

    #[test]
    fn the_committed_manifests_render_back_byte_for_byte() {
        assert_eq!(render_declared(&declared().unwrap()), EXPECTED);
        assert_eq!(render_known(&known().unwrap()), KNOWN);
    }

    #[test]
    fn known_entries_render_by_row_with_live_entries_last() {
        let text =
            "S16 era_summary\nV2 validation supp#2\nV1 validation supp#5\nS3 as_output supp#9";
        let known = parse_known("k", text).unwrap();
        assert_eq!(
            render_known(&known),
            "V1 validation supp#5\nV2 validation supp#2\nS3 as_output supp#9\nS16 era_summary\n"
        );
        assert_eq!(render_known(&Known::default()), "");
        assert_eq!(render_declared(&Declared::new()), "");
    }

    #[test]
    fn candidate_lines_name_the_file_they_belong_in() {
        let candidates = Candidates {
            declared: parse_declared("e", "supp#2 non-vkey witnesses").unwrap(),
            known: parse_known("k", "S16 era_summary").unwrap(),
        };
        assert_eq!(
            candidates.lines(),
            vec![
                "CANDIDATE expected-unsupported.txt supp#2 non-vkey witnesses",
                "CANDIDATE known.txt S16 era_summary",
            ]
        );
        assert!(Candidates::default().lines().is_empty());
    }

    #[test]
    fn a_label_keeps_its_spaces() {
        let declared = parse_declared("e", "ic#851 non-vkey witnesses").unwrap();
        assert_eq!(
            declared[&"ic#851".parse().unwrap()],
            Feature("non-vkey witnesses".into())
        );
    }
}
