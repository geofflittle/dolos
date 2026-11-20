//! The state Dolos holds after replaying a stretch of chain, judged against the
//! state the ledger held at the first block of each epoch in it.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use serde::Deserialize;

use crate::{
    accounts::{self, Account, Credential, LedgerAccount},
    compare::Tally,
    judgement::{self, Axis, CellKey, EntryPoint, Instrument, Judge, Outcome, Verdict},
    manifest::ManifestError,
    state::{DRepState, Hex},
};

/// The disagreements the ledger and Dolos are known to have over a replay, each
/// the exact detail of its verdict.
pub fn parse_known(file: &'static str, text: &str) -> Result<BTreeSet<String>, ManifestError> {
    let mut known = BTreeSet::new();
    let lines = text.lines().enumerate().map(|(i, l)| (i + 1, l.trim()));
    for (line, l) in lines.filter(|(_, l)| !l.is_empty()) {
        if !known.insert(l.to_string()) {
            let reason = format!("{l} listed twice");
            return Err(ManifestError { file, line, reason });
        }
    }
    Ok(known)
}

/// The entry point the replay judge drives.
pub const ENTRY: EntryPoint = EntryPoint::Instrument(Instrument::ApplyBlock);

/// The pots both the ledger and Dolos hold, in lovelace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pots {
    pub reserves: u64,
    pub treasury: u64,
    pub deposited: u64,
    pub fees: u64,
}

/// The deposit of a registered pool and the epoch it retires at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pool {
    pub deposit: u64,
    pub retiring: Option<u64>,
}

/// The pots, accounts, pools and DReps of one side, its accounts of type `A`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainState<A> {
    pub pots: Pots,
    pub accounts: BTreeMap<Credential, A>,
    pub pools: BTreeMap<Hex<28>, Pool>,
    pub dreps: BTreeMap<Credential, DRepState>,
}

/// The state the ledger held at the first block of an epoch.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub epoch: u64,
    pub slot: u64,
    pub hash: Hex<32>,
    pub state: ChainState<LedgerAccount>,
}

/// What Dolos held once it applied the block at a checkpoint's slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub epoch: u64,
    pub hash: Hex<32>,
    pub state: ChainState<Account>,
}

/// How a replay stopped before the last block of its fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// Dolos refused a block.
    Refused,
    /// Dolos panicked applying a block.
    Panicked,
    /// Dolos held a value at a checkpoint that the ledger's state cannot hold.
    Unrepresentable,
}

impl fmt::Display for Stopped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Stopped::Refused => "refused",
            Stopped::Panicked => "panicked",
            Stopped::Unrepresentable => "unrepresentable",
        })
    }
}

/// The block a replay stopped at, how, and what Dolos said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stop {
    pub slot: u64,
    pub how: Stopped,
    pub detail: String,
}

/// What a replay of the fixture's blocks reached.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Replay {
    /// What Dolos held at each checkpoint slot it applied a block at.
    pub held: BTreeMap<u64, Held>,
    /// The block Dolos stopped at, none when it applied every block.
    pub stop: Option<Stop>,
}

/// The comparisons of the replay judge by table, each disagreement a failing
/// verdict.
#[derive(Debug, Default)]
pub struct ReplayTally {
    tables: BTreeMap<&'static str, Tally<Verdict>>,
    stopped: Vec<Verdict>,
}

/// A failing verdict of the replay judge over the whole replay.
fn fails(cells: Vec<CellKey>, detail: String) -> Verdict {
    Verdict {
        cells,
        ..Verdict::selection_fails(Judge::Replay, ENTRY, detail)
    }
}

/// The cells of a disagreement over one item of `table`: which side holds it,
/// and when both do, each field they differ at.
fn item_cells(table: &str, ledger: bool, dolos: bool, fields: &[(&str, bool)]) -> Vec<CellKey> {
    let answer = match (ledger, dolos) {
        (true, false) => "ledger_only",
        (false, true) => "dolos_only",
        _ => "differ",
    };
    let differing = fields.iter().filter(|(_, differs)| *differs);
    [
        CellKey::new(Axis::Table, table),
        CellKey::new(Axis::Answer, answer),
    ]
    .into_iter()
    .chain(differing.map(|(field, _)| CellKey::new(Axis::Field, field)))
    .collect()
}

/// Counts each item of a keyed table either side holds against the other
/// side's item, naming the fields `fields` finds differing.
fn compare_keyed<K: Ord + fmt::Display, V: PartialEq + fmt::Debug>(
    at: &str,
    table: &'static str,
    ledger: &BTreeMap<K, V>,
    dolos: &BTreeMap<K, V>,
    fields: impl Fn(&V, &V) -> Vec<(&'static str, bool)>,
    tally: &mut Tally<Verdict>,
) {
    let mut keys: Vec<&K> = ledger.keys().chain(dolos.keys()).collect();
    keys.sort();
    keys.dedup();
    for key in keys {
        let (l, d) = (ledger.get(key), dolos.get(key));
        tally.add(l == d, || {
            let differing = match (l, d) {
                (Some(l), Some(d)) => fields(l, d),
                _ => Vec::new(),
            };
            let cells = item_cells(table, l.is_some(), d.is_some(), &differing);
            let detail =
                format!("REPLAY-DIFF {at} table={table} key={key} ledger={l:?} dolos={d:?}");
            fails(cells, detail)
        });
    }
}

impl ReplayTally {
    fn table(&mut self, table: &'static str) -> &mut Tally<Verdict> {
        self.tables.entry(table).or_default()
    }

    /// Compares what Dolos held at each checkpoint with the ledger's state
    /// there, and fails each checkpoint Dolos did not reach.
    pub fn judge(checkpoints: &[Checkpoint], replay: &Replay) -> Self {
        let mut tally = ReplayTally::default();
        if let Some(Stop { slot, how, detail }) = &replay.stop {
            let cells = vec![
                CellKey::new(Axis::Table, "blocks"),
                CellKey::new(Axis::Answer, how),
            ];
            let detail = format!("REPLAY-STOP slot={slot} {how}={detail}");
            tally.stopped.push(fails(cells, detail));
        }
        for checkpoint in checkpoints {
            match replay.held.get(&checkpoint.slot) {
                Some(held) => tally.compare(checkpoint, held),
                None => {
                    let at = format!("epoch={} slot={}", checkpoint.epoch, checkpoint.slot);
                    let cells = vec![
                        CellKey::new(Axis::Table, "blocks"),
                        CellKey::new(Axis::Answer, "unreached"),
                    ];
                    tally.table("blocks").add(false, || {
                        fails(cells, format!("REPLAY-DIFF {at} table=blocks unreached"))
                    });
                }
            }
        }
        tally
    }

    /// Compares the state Dolos held at one checkpoint with the ledger's.
    fn compare(&mut self, checkpoint: &Checkpoint, held: &Held) {
        let at = format!("epoch={} slot={}", checkpoint.epoch, checkpoint.slot);
        let block = [
            ("epoch", checkpoint.epoch == held.epoch),
            ("hash", checkpoint.hash == held.hash),
        ];
        for (field, agrees) in block {
            self.table("blocks").add(agrees, || {
                let cells = vec![
                    CellKey::new(Axis::Table, "blocks"),
                    CellKey::new(Axis::Answer, "differ"),
                    CellKey::new(Axis::Field, field),
                ];
                let detail = format!(
                    "REPLAY-DIFF {at} table=blocks key={field} ledger={},{} dolos={},{}",
                    checkpoint.epoch, checkpoint.hash, held.epoch, held.hash
                );
                fails(cells, detail)
            });
        }
        let (ledger, dolos) = (&checkpoint.state.pots, &held.state.pots);
        let pots = [
            ("reserves", ledger.reserves, dolos.reserves),
            ("treasury", ledger.treasury, dolos.treasury),
            ("deposited", ledger.deposited, dolos.deposited),
            ("fees", ledger.fees, dolos.fees),
        ];
        for (pot, ledger, dolos) in pots {
            self.table("pots").add(ledger == dolos, || {
                let cells = vec![
                    CellKey::new(Axis::Table, "pots"),
                    CellKey::new(Axis::Answer, "differ"),
                    CellKey::new(Axis::Field, pot),
                ];
                let detail =
                    format!("REPLAY-DIFF {at} table=pots key={pot} ledger={ledger} dolos={dolos}");
                fails(cells, detail)
            });
        }
        let accounts = self.table("accounts");
        accounts::compare_by(
            &checkpoint.state.accounts,
            &held.state.accounts,
            accounts,
            |cells, detail| {
                let table = std::iter::once(CellKey::new(Axis::Table, "accounts"));
                let detail = format!("REPLAY-DIFF {at} table=accounts {detail}");
                fails(table.chain(cells).collect(), detail)
            },
        );
        compare_keyed(
            &at,
            "pools",
            &checkpoint.state.pools,
            &held.state.pools,
            |l, d| {
                vec![
                    ("deposit", l.deposit != d.deposit),
                    ("retiring", l.retiring != d.retiring),
                ]
            },
            self.table("pools"),
        );
        compare_keyed(
            &at,
            "dreps",
            &checkpoint.state.dreps,
            &held.state.dreps,
            |l, d| {
                vec![
                    ("anchor", l.anchor != d.anchor),
                    ("deposit", l.deposit != d.deposit),
                    ("expiry", l.expiry != d.expiry),
                ]
            },
            self.table("dreps"),
        );
    }

    /// The tally line.
    pub fn line(&self) -> String {
        let compared: usize = self.tables.values().map(|t| t.compared).sum();
        let disagree: usize = self.tables.values().map(|t| t.disagree.len()).sum();
        let tables = self
            .tables
            .iter()
            .map(|(table, t)| format!("{table}={}", t.compared));
        format!(
            "REPLAY compared={compared} agree={} disagree={disagree} stopped={} {}",
            compared - disagree,
            self.stopped.len(),
            tables.collect::<Vec<_>>().join(" ")
        )
    }

    /// Adds the passing count of each table and every disagreement, a known
    /// one as known, and fails each known disagreement the replay did not
    /// show.
    pub fn report(&self, known: &BTreeSet<String>, out: &mut judgement::Tally) {
        let mut shown = BTreeSet::new();
        let disagreements = self.tables.values().flat_map(|t| &t.disagree);
        for verdict in disagreements.chain(&self.stopped).cloned() {
            match &verdict.outcome {
                Outcome::Disagree(detail) if known.contains(detail) => {
                    shown.insert(detail.clone());
                    let outcome = Outcome::Known(detail.clone());
                    out.add(Verdict { outcome, ..verdict });
                }
                _ => out.add(verdict),
            }
        }
        for (table, Tally { compared, disagree }) in &self.tables {
            let passed = judgement::Passed {
                judge: Judge::Replay,
                entry: Some(ENTRY),
                cells: vec![CellKey::new(Axis::Table, table)],
                pass: judgement::Pass::Agree,
            };
            out.pass(passed, compared - disagree.len());
        }
        for line in known.difference(&shown) {
            let cells = vec![CellKey::new(Axis::Answer, "known_stale")];
            out.add(fails(cells, format!("REPLAY-KNOWN-STALE {line}")));
        }
        out.require(&[(Judge::Replay, ENTRY)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::judgement::RunVerdict;
    use crate::state::Anchor;

    /// A change to one held field and the cells the replay must fail for it.
    type FieldChange = (&'static str, fn(&mut Held));

    const FIXTURE: &str = r#"[
      {"epoch": 1, "slot": 21602, "hash": "e1cf320b00000000000000000000000000000000000000000000000000000000",
       "state": {
        "pots": {"reserves": 100, "treasury": 7, "deposited": 4, "fees": 3},
        "accounts": {"keyHash-1cca48da7f291b81a09ddc0e132fce0c5a1ef3410e5a70f9fcd5f777":
          {"balance": 5, "deposit": 2, "drep": null, "spool": "5f0b42b4fbdc7e442d5da37744cafdca86e0e7f3bf94223e75e9312b"}},
        "pools": {"5f0b42b4fbdc7e442d5da37744cafdca86e0e7f3bf94223e75e9312b": {"deposit": 2, "retiring": null}},
        "dreps": {"keyHash-da388496cca1dc6418e33fe706639873469d8b1e423d056e47736aca": {"deposit": 2, "expiry": 20}}}},
      {"epoch": 2, "slot": 43208, "hash": "0222a50b00000000000000000000000000000000000000000000000000000000",
       "state": {
        "pots": {"reserves": 90, "treasury": 17, "deposited": 4, "fees": 0},
        "accounts": {}, "pools": {}, "dreps": {}}}
    ]"#;

    fn held(checkpoint: &Checkpoint) -> Held {
        let ChainState {
            pots,
            accounts,
            pools,
            dreps,
        } = &checkpoint.state;
        Held {
            epoch: checkpoint.epoch,
            hash: checkpoint.hash,
            state: ChainState {
                pots: *pots,
                accounts: accounts.iter().map(|(c, a)| (*c, a.into())).collect(),
                pools: pools.clone(),
                dreps: dreps.clone(),
            },
        }
    }

    /// A replay that held each checkpoint's state exactly.
    fn faithful(checkpoints: &[Checkpoint]) -> Replay {
        let held = checkpoints.iter().map(|c| (c.slot, held(c))).collect();
        Replay { held, stop: None }
    }

    fn failing(checkpoints: &[Checkpoint], replay: &Replay) -> Vec<String> {
        let mut out = judgement::Tally::default();
        ReplayTally::judge(checkpoints, replay).report(&BTreeSet::new(), &mut out);
        out.failing.iter().map(Verdict::to_string).collect()
    }

    #[test]
    fn a_replay_holding_the_ledgers_state_passes_every_table() {
        let checkpoints: Vec<Checkpoint> = serde_json::from_str(FIXTURE).unwrap();
        let tally = ReplayTally::judge(&checkpoints, &faithful(&checkpoints));
        assert_eq!(
            tally.line(),
            "REPLAY compared=15 agree=15 disagree=0 stopped=0 \
             accounts=1 blocks=4 dreps=1 pools=1 pots=8"
        );
        let mut out = judgement::Tally::default();
        tally.report(&BTreeSet::new(), &mut out);
        assert!(out.failing.is_empty(), "{:?}", out.failing);
        assert!(matches!(out.verdict(), RunVerdict::Pass { .. }));
    }

    #[test]
    fn a_differing_pot_fails_naming_its_checkpoint() {
        let checkpoints: Vec<Checkpoint> = serde_json::from_str(FIXTURE).unwrap();
        let mut replay = faithful(&checkpoints);
        replay.held.get_mut(&43208).unwrap().state.pots.treasury = 18;
        assert_eq!(
            failing(&checkpoints, &replay),
            ["VERDICT judge=replay entry=apply_block record=selection \
              cells=table:pots,answer:differ,field:treasury outcome=disagree \
              REPLAY-DIFF epoch=2 slot=43208 table=pots key=treasury ledger=17 dolos=18"]
        );
    }

    #[test]
    fn a_differing_account_fails_naming_its_field() {
        let checkpoints: Vec<Checkpoint> = serde_json::from_str(FIXTURE).unwrap();
        let mut replay = faithful(&checkpoints);
        let held = replay.held.get_mut(&21602).unwrap();
        held.state
            .accounts
            .values_mut()
            .for_each(|a| a.deposit = None);
        let lines = failing(&checkpoints, &replay);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].contains("cells=table:accounts,answer:differ,field:deposit ")
                && lines[0].contains("REPLAY-DIFF epoch=1 slot=21602 table=accounts account="),
            "{lines:?}"
        );
    }

    #[test]
    fn a_pool_or_drep_only_one_side_holds_fails() {
        let checkpoints: Vec<Checkpoint> = serde_json::from_str(FIXTURE).unwrap();
        let mut replay = faithful(&checkpoints);
        let held = replay.held.get_mut(&21602).unwrap();
        held.state.pools.clear();
        held.state.dreps.values_mut().for_each(|d| d.expiry = 21);
        assert_eq!(
            failing(&checkpoints, &replay),
            [
                "VERDICT judge=replay entry=apply_block record=selection \
                 cells=table:dreps,answer:differ,field:expiry outcome=disagree \
                 REPLAY-DIFF epoch=1 slot=21602 table=dreps \
                 key=keyHash-da388496cca1dc6418e33fe706639873469d8b1e423d056e47736aca \
                 ledger=Some(DRepState { anchor: None, deposit: 2, expiry: 20 }) \
                 dolos=Some(DRepState { anchor: None, deposit: 2, expiry: 21 })",
                "VERDICT judge=replay entry=apply_block record=selection \
                 cells=table:pools,answer:ledger_only outcome=disagree \
                 REPLAY-DIFF epoch=1 slot=21602 table=pools \
                 key=5f0b42b4fbdc7e442d5da37744cafdca86e0e7f3bf94223e75e9312b \
                 ledger=Some(Pool { deposit: 2, retiring: None }) dolos=None",
            ]
        );
    }

    #[test]
    fn a_different_block_at_a_checkpoint_slot_fails() {
        let checkpoints: Vec<Checkpoint> = serde_json::from_str(FIXTURE).unwrap();
        let mut replay = faithful(&checkpoints);
        replay.held.get_mut(&43208).unwrap().epoch = 1;
        let lines = failing(&checkpoints, &replay);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].contains("cells=table:blocks,answer:differ,field:epoch ")
                && lines[0].contains("epoch=2 slot=43208 table=blocks key=epoch"),
            "{lines:?}"
        );
    }

    #[test]
    fn each_compared_field_fails_alone_naming_its_cells() {
        let checkpoints: Vec<Checkpoint> = serde_json::from_str(FIXTURE).unwrap();
        let changes: [FieldChange; 12] = [
            ("table:blocks,answer:differ,field:epoch", |h| h.epoch = 9),
            ("table:blocks,answer:differ,field:hash", |h| {
                h.hash.0[31] = 1
            }),
            ("table:pots,answer:differ,field:reserves", |h| {
                h.state.pots.reserves += 1
            }),
            ("table:pots,answer:differ,field:treasury", |h| {
                h.state.pots.treasury += 1
            }),
            ("table:pots,answer:differ,field:deposited", |h| {
                h.state.pots.deposited += 1
            }),
            ("table:pots,answer:differ,field:fees", |h| {
                h.state.pots.fees += 1
            }),
            ("table:pools,answer:differ,field:deposit", |h| {
                h.state.pools.values_mut().for_each(|p| p.deposit += 1)
            }),
            ("table:pools,answer:differ,field:retiring", |h| {
                h.state
                    .pools
                    .values_mut()
                    .for_each(|p| p.retiring = Some(3))
            }),
            ("table:dreps,answer:differ,field:anchor", |h| {
                h.state.dreps.values_mut().for_each(|d| {
                    d.anchor = Some(Anchor {
                        data_hash: Hex([0; 32]),
                        url: String::new(),
                    })
                })
            }),
            ("table:dreps,answer:differ,field:deposit", |h| {
                h.state.dreps.values_mut().for_each(|d| d.deposit += 1)
            }),
            ("table:dreps,answer:differ,field:expiry", |h| {
                h.state.dreps.values_mut().for_each(|d| d.expiry += 1)
            }),
            ("table:pools,answer:dolos_only", |h| {
                h.state.pools.insert(
                    Hex([1; 28]),
                    Pool {
                        deposit: 2,
                        retiring: None,
                    },
                );
            }),
        ];
        for (cells, change) in changes {
            let mut replay = faithful(&checkpoints);
            change(replay.held.get_mut(&21602).unwrap());
            let lines = failing(&checkpoints, &replay);
            assert_eq!(lines.len(), 1, "{cells}: {lines:?}");
            assert!(
                lines[0].contains(&format!(
                    " cells={cells} outcome=disagree REPLAY-DIFF epoch=1 slot=21602 "
                )),
                "{cells}: {lines:?}"
            );
        }
    }

    #[test]
    fn a_replay_that_stops_fails_its_stop_and_each_checkpoint_after_it() {
        let checkpoints: Vec<Checkpoint> = serde_json::from_str(FIXTURE).unwrap();
        let mut replay = faithful(&checkpoints);
        replay.held.remove(&43208);
        replay.stop = Some(Stop {
            slot: 30000,
            how: Stopped::Panicked,
            detail: "no epoch".to_string(),
        });
        assert_eq!(
            failing(&checkpoints, &replay),
            [
                "VERDICT judge=replay entry=apply_block record=selection \
                 cells=table:blocks,answer:unreached outcome=disagree \
                 REPLAY-DIFF epoch=2 slot=43208 table=blocks unreached",
                "VERDICT judge=replay entry=apply_block record=selection \
                 cells=table:blocks,answer:panicked outcome=disagree \
                 REPLAY-STOP slot=30000 panicked=no epoch",
            ]
        );
    }

    #[test]
    fn a_replay_that_reached_no_checkpoint_passes_nothing() {
        let checkpoints: Vec<Checkpoint> = serde_json::from_str(FIXTURE).unwrap();
        let mut out = judgement::Tally::default();
        ReplayTally::judge(&checkpoints, &Replay::default()).report(&BTreeSet::new(), &mut out);
        assert!(out.passing.is_empty(), "{:?}", out.passing);
        assert_eq!(out.failing.len(), 2);
        assert!(matches!(out.verdict(), RunVerdict::Fail { failing: 2 }));
    }

    #[test]
    fn a_fixture_without_checkpoints_fails_as_judging_nothing() {
        let mut out = judgement::Tally::default();
        ReplayTally::judge(&[], &Replay::default()).report(&BTreeSet::new(), &mut out);
        let lines: Vec<String> = out.failing.iter().map(Verdict::to_string).collect();
        assert_eq!(
            lines,
            [
                "VERDICT judge=replay entry=apply_block record=selection cells= \
              outcome=disagree judged=0"
            ]
        );
    }

    const TREASURY: &str =
        "REPLAY-DIFF epoch=2 slot=43208 table=pots key=treasury ledger=17 dolos=18";

    #[test]
    fn a_known_disagreement_passes_as_known() {
        let checkpoints: Vec<Checkpoint> = serde_json::from_str(FIXTURE).unwrap();
        let mut replay = faithful(&checkpoints);
        replay.held.get_mut(&43208).unwrap().state.pots.treasury = 18;
        let known = parse_known("replay-known.txt", &format!("{TREASURY}\n")).unwrap();
        let mut out = judgement::Tally::default();
        ReplayTally::judge(&checkpoints, &replay).report(&known, &mut out);
        assert!(out.failing.is_empty(), "{:?}", out.failing);
        let known: Vec<usize> = out
            .passing
            .iter()
            .filter(|(p, _)| p.pass == judgement::Pass::Known)
            .map(|(_, n)| *n)
            .collect();
        assert_eq!(known, [1]);
    }

    #[test]
    fn a_known_disagreement_with_another_value_fails_and_goes_stale() {
        let checkpoints: Vec<Checkpoint> = serde_json::from_str(FIXTURE).unwrap();
        let mut replay = faithful(&checkpoints);
        replay.held.get_mut(&43208).unwrap().state.pots.treasury = 19;
        let known = parse_known("replay-known.txt", TREASURY).unwrap();
        let mut out = judgement::Tally::default();
        ReplayTally::judge(&checkpoints, &replay).report(&known, &mut out);
        let lines: Vec<String> = out.failing.iter().map(Verdict::to_string).collect();
        assert_eq!(
            lines,
            [
                "VERDICT judge=replay entry=apply_block record=selection \
                 cells=table:pots,answer:differ,field:treasury outcome=disagree \
                 REPLAY-DIFF epoch=2 slot=43208 table=pots key=treasury ledger=17 dolos=19",
                "VERDICT judge=replay entry=apply_block record=selection \
                 cells=answer:known_stale outcome=disagree REPLAY-KNOWN-STALE \
                 REPLAY-DIFF epoch=2 slot=43208 table=pots key=treasury ledger=17 dolos=18",
            ]
        );
    }

    #[test]
    fn a_disagreement_listed_twice_is_refused() {
        let text = format!("{TREASURY}\n\n{TREASURY}\n");
        let error = parse_known("replay-known.txt", &text).unwrap_err();
        assert_eq!(
            (error.line, error.reason),
            (3, format!("{TREASURY} listed twice"))
        );
    }
}
