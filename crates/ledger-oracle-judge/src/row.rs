use std::{fmt, str::FromStr};

/// A comparison the oracle makes between the ledger and Dolos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Check {
    Validation,
    Utxo,
    AsOutput,
    Accounts,
    Undo,
    Params,
    ReadTx,
    Script,
    Datum,
    EraSummary,
    LiveParams,
    LiveAsOutput,
    LiveBlock,
    LiveUtxo,
}

/// How the oracle's comparisons judge one catalog row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    Covered(Check, &'static [Check]),
    Partial(Check, &'static [Check], &'static str),
    Uncovered(&'static str),
}

impl Coverage {
    /// The comparisons that judge the row.
    pub fn checks(self) -> Vec<Check> {
        match self {
            Coverage::Covered(first, rest) | Coverage::Partial(first, rest, _) => {
                std::iter::once(first).chain(rest.iter().copied()).collect()
            }
            Coverage::Uncovered(_) => Vec::new(),
        }
    }
}

macro_rules! rows {
    ($($row:ident),* $(,)?) => {
        /// A catalog row, one variant per catalog id.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Row {
            $($row),*
        }

        impl Row {
            /// Every row in catalog order.
            pub const ALL: &'static [Row] = &[$(Row::$row),*];

            /// The catalog id of the row.
            pub fn id(self) -> &'static str {
                match self {
                    $(Row::$row => stringify!($row)),*
                }
            }
        }
    };
}

rows! {
    V1, V2, V3, V4, V5, V6, V7, V8, V9, V10, V11, V12, V13, V14, V15, V16, V17, V18, V19, V20,
    V21, V22, V23, V24, V25, V26, V27, V28, S1, S2, S3, S4, S5, S6, S7, S8, S9, S10, S11, S12,
    S13, S14, S15, S16, S17, S18, S19, S20, S21, S22, S23, S24, S25,
}

impl fmt::Display for Row {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

impl FromStr for Row {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Row::ALL
            .iter()
            .copied()
            .find(|row| row.id() == s)
            .ok_or(format!("unknown row {s}"))
    }
}

/// The rows a comparison judges, in catalog order.
pub fn judged_by(check: Check) -> Vec<Row> {
    let judges = |row: &Row| coverage(*row).checks().contains(&check);
    Row::ALL.iter().copied().filter(judges).collect()
}

/// How the oracle's comparisons judge the row.
pub fn coverage(row: Row) -> Coverage {
    use Check::*;
    use Coverage::*;
    match row {
        Row::V1 | Row::V2 | Row::V5 | Row::V6 | Row::V11 | Row::V12 => Covered(Validation, &[]),
        Row::V16 | Row::V17 | Row::V19 | Row::V20 | Row::V21 | Row::V22 | Row::V23 | Row::V24 => {
            Covered(Validation, &[])
        }
        Row::V26 | Row::V27 => Covered(Validation, &[]),
        Row::V3 => {
            Uncovered("each group is judged at the ledger's slot, so both sides read one slot")
        }
        Row::V4 => Partial(
            Validation,
            &[Accounts],
            "seeded balances are the ledger's, so only deposits inside a run test Dolos's own",
        ),
        Row::V7 => {
            Uncovered("runs end at an epoch change, so no withdrawal follows an epoch start")
        }
        Row::V8 => Partial(
            Validation,
            &[Params],
            "pool and DRep certificates are unsupported, so the parameters they read are unjudged",
        ),
        Row::V9 => Partial(
            Validation,
            &[],
            "the corpus holds protocol version 12.0 only",
        ),
        Row::V10 => Uncovered("the corpus holds ledger encodings only, never a malformed envelope"),
        Row::V13 => Partial(
            Validation,
            &[],
            "each child input at a pointer address meets an earlier unsupported gate",
        ),
        Row::V14 => Partial(
            Validation,
            &[],
            "Plutus witnesses in children are unsupported",
        ),
        Row::V15 => Partial(
            Validation,
            &[],
            "the ledger's typed builder writes no non minimal duplicate guard",
        ),
        Row::V18 => {
            Uncovered("child bodies are canonical, and an edited body breaks its signatures")
        }
        Row::V25 => Partial(Validation, &[ReadTx], "Plutus in a batch is unsupported"),
        Row::V28 => Uncovered("the ledger gives no execution unit estimate to judge EvalTx"),
        Row::S1 => Covered(AsOutput, &[LiveAsOutput, LiveBlock]),
        Row::S2 | Row::S3 | Row::S4 | Row::S7 => Covered(AsOutput, &[]),
        Row::S5 => Covered(Undo, &[]),
        Row::S6 => {
            Uncovered("the memory archive never ends at a slot without a write ahead log point")
        }
        Row::S8 => Uncovered("the WatchTx consumed set is not compared"),
        Row::S9 => Partial(
            Utxo,
            &[LiveUtxo],
            "ReadUtxos by reference only, and valid blocks never undo a recreated output",
        ),
        Row::S10 => Partial(
            Validation,
            &[],
            "TRP is not called, the pending overlay it reads is judged through SubmitTx",
        ),
        Row::S11 => Uncovered("the run never applies a block between two reads of one call"),
        Row::S12 => Partial(
            Datum,
            &[],
            "two outputs in the corpora hold a hash datum their producer witnesses",
        ),
        Row::S13 => Covered(Script, &[]),
        Row::S14 => Uncovered("ReadData panics on any call and serves nothing from chain state"),
        Row::S15 => Partial(
            Params,
            &[LiveParams],
            "the live node is read in one epoch, so no update across an epoch boundary is judged",
        ),
        Row::S16 => Partial(
            EraSummary,
            &[],
            "the name of the last era only, starts and slot lengths are not compared",
        ),
        Row::S17 => Uncovered("stake distribution changes only across an epoch change"),
        Row::S18 => Partial(
            Accounts,
            &[],
            "account entities in state only, minibf answers and epoch rewards are not compared",
        ),
        Row::S19 => Covered(ReadTx, &[]),
        Row::S20 => Uncovered("no served call is known to read the spent index"),
        Row::S21 => Partial(
            ReadTx,
            &[],
            "phase two invalid transactions only, endorser transactions need a Leios chain",
        ),
        Row::S22 => Uncovered("mempool stage and finality are judged only against a node"),
        Row::S23 => Uncovered("ReadMempool is unimplemented"),
        Row::S24 => Uncovered("the tip is judged only against a node"),
        Row::S25 => Uncovered("o7s answers are judged only against a node client"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CATALOG: &str = include_str!("../../../test_data/ledger-oracle/catalog-rows.txt");

    #[test]
    fn every_catalog_id_has_a_row_and_every_row_an_id() {
        let ids: Vec<&str> = CATALOG
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let missing: Vec<&str> = ids
            .iter()
            .copied()
            .filter(|id| id.parse::<Row>().is_err())
            .collect();
        assert!(missing.is_empty(), "catalog ids with no row: {missing:?}");
        let rows: Vec<&str> = Row::ALL.iter().map(|row| row.id()).collect();
        assert_eq!(rows, ids);
    }

    #[test]
    fn a_covered_row_names_its_checks_and_an_uncovered_row_none() {
        assert_eq!(coverage(Row::S13).checks(), vec![Check::Script]);
        assert_eq!(
            coverage(Row::V4).checks(),
            vec![Check::Validation, Check::Accounts]
        );
        assert!(coverage(Row::V3).checks().is_empty());
    }
}
