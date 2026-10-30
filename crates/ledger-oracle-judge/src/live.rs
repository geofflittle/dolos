use std::{collections::BTreeMap, fmt, ops::Range};

use serde::Deserialize;

use crate::{
    compare::Tally,
    manifest::{KnownLive, LiveLocus},
    row::{judged_by, Row},
};

/// The node's tip as its tip query prints it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeTip {
    pub slot: u64,
    pub epoch: u64,
    pub era: String,
    pub slot_in_epoch: u64,
    pub slots_to_epoch_end: u64,
}

impl NodeTip {
    /// The slots of the tip's epoch, none when the tip's counts do not fit its slot.
    pub fn epoch_slots(&self) -> Option<Range<u64>> {
        let start = self.slot.checked_sub(self.slot_in_epoch)?;
        let end = self.slot.checked_add(self.slots_to_epoch_end)?;
        Some(start..end)
    }
}

/// Reads taken while the node stayed in the epoch that holds the served ledger slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aligned<T> {
    epoch: u64,
    value: T,
}

impl<T> Aligned<T> {
    /// Pairs the reads with the epoch that contains both node tips and the served slot, none otherwise.
    pub fn new(before: &NodeTip, after: &NodeTip, served_slot: u64, value: T) -> Option<Self> {
        let slots = before.epoch_slots()?;
        let aligned = before.epoch == after.epoch && slots.contains(&served_slot);
        aligned.then_some(Aligned {
            epoch: before.epoch,
            value,
        })
    }

    /// The epoch that contains the reads.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The aligned reads.
    pub fn value(&self) -> &T {
        &self.value
    }
}

/// The result of comparing the last era name Dolos serves with the node's era.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EraName {
    Agree(String),
    Disagree {
        served: Option<String>,
        node: String,
    },
}

impl EraName {
    /// Whether the served name is the node's era.
    pub fn agrees(&self) -> bool {
        matches!(self, EraName::Agree(_))
    }
}

impl fmt::Display for EraName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EraName::Agree(name) => write!(f, "name={name}"),
            EraName::Disagree { served, node } => {
                let served = served.as_deref().unwrap_or("none");
                write!(f, "name={served} node={node}")
            }
        }
    }
}

/// Compares the name of the last served era with the node's era, ignoring case.
pub fn era_name(served: &[String], node: &str) -> EraName {
    match served.last() {
        Some(name) if name.eq_ignore_ascii_case(node) => EraName::Agree(name.clone()),
        last => EraName::Disagree {
            served: last.cloned(),
            node: node.to_string(),
        },
    }
}

/// The lines and verdict of the live comparisons, judged against the known list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settled {
    pub lines: Vec<String>,
    pub known_fired: KnownLive,
    pub pass: bool,
}

/// Judges each live locus's tally, failing one that compared nothing, disagrees off the list, or agrees on it.
pub fn settle(tallies: &BTreeMap<LiveLocus, Tally>, known: &KnownLive) -> Settled {
    let mut settled = Settled {
        lines: Vec::new(),
        known_fired: KnownLive::new(),
        pass: true,
    };
    for &locus in LiveLocus::ALL {
        let none = Tally::default();
        let tally = tallies.get(&locus).unwrap_or(&none);
        let mut fail = |line: String| {
            settled.lines.push(line);
            settled.pass = false;
        };
        if tally.compared == 0 {
            fail(format!("LIVE-GAP locus={locus} compared=0"));
            continue;
        }
        match known.get(&locus) {
            Some(row) if tally.disagree.is_empty() => {
                fail(format!("KNOWN_STALE row={row} locus={locus}"));
            }
            Some(row) => {
                let lines = tally.disagree.iter();
                let lines = lines.map(|d| format!("KNOWN row={row} locus={locus} {d}"));
                settled.lines.extend(lines);
                settled.known_fired.insert(locus, *row);
            }
            None => {
                let rows: Vec<String> = judged_by(locus.check())
                    .iter()
                    .map(Row::to_string)
                    .collect();
                let rows = rows.join(",");
                for d in &tally.disagree {
                    fail(format!("LIVE-DIFF row={rows} locus={locus} {d}"));
                }
            }
        }
    }
    settled
}

/// The bytes of one block the node's immutable database holds, with where its header is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeBlock {
    pub slot: u64,
    pub header: Range<usize>,
    pub bytes: Vec<u8>,
}

/// The size of one entry of an immutable chunk's secondary index.
const SECONDARY_ENTRY: usize = 56;

/// The blocks of one immutable chunk, by the entries of its secondary index.
pub fn chunk_blocks(secondary: &[u8], chunk: &[u8]) -> Result<Vec<NodeBlock>, String> {
    let entries = secondary.chunks(SECONDARY_ENTRY);
    let entries: Vec<(usize, Range<usize>, u64)> = entries
        .map(|e| {
            let e: &[u8; SECONDARY_ENTRY] = e
                .try_into()
                .map_err(|_| format!("secondary entry of {} bytes", e.len()))?;
            let offset = u64::from_be_bytes(e[0..8].try_into().unwrap());
            let header_offset = u16::from_be_bytes(e[8..10].try_into().unwrap());
            let header_size = u16::from_be_bytes(e[10..12].try_into().unwrap());
            let slot = u64::from_be_bytes(e[48..56].try_into().unwrap());
            let start = usize::from(header_offset);
            Ok((
                offset as usize,
                start..start + usize::from(header_size),
                slot,
            ))
        })
        .collect::<Result<_, String>>()?;
    let ends = entries
        .iter()
        .skip(1)
        .map(|(offset, ..)| *offset)
        .chain([chunk.len()]);
    entries
        .iter()
        .zip(ends)
        .map(|((offset, header, slot), end)| {
            let bytes = chunk
                .get(*offset..end)
                .ok_or(format!("slot {slot} bytes {offset}..{end} past the chunk"))?;
            Ok(NodeBlock {
                slot: *slot,
                header: header.clone(),
                bytes: bytes.to_vec(),
            })
        })
        .collect()
}

/// How the bytes of a block Dolos serves compare with the node's block at its slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockBytes {
    Same,
    /// A block whose body holds a certificate, whose header bytes are the node's and whose body carries the endorsed transactions in place of the node's certificate body.
    SameHeader,
    NodeAbsent,
    HeaderDiffers,
    BytesDiffer,
}

impl BlockBytes {
    /// Whether the served bytes agree with the node's.
    pub fn agrees(self) -> bool {
        matches!(self, BlockBytes::Same | BlockBytes::SameHeader)
    }

    /// The name printed in judgement lines.
    pub fn name(self) -> &'static str {
        match self {
            BlockBytes::Same => "same",
            BlockBytes::SameHeader => "same_header",
            BlockBytes::NodeAbsent => "node_absent",
            BlockBytes::HeaderDiffers => "header_differs",
            BlockBytes::BytesDiffer => "bytes_differ",
        }
    }
}

/// Compares the bytes Dolos serves for a block with the node's, the header alone for a block whose body holds a certificate.
pub fn block_bytes(node: Option<&NodeBlock>, served: &[u8], certifying: bool) -> BlockBytes {
    let Some(node) = node else {
        return BlockBytes::NodeAbsent;
    };
    if served.get(node.header.clone()) != node.bytes.get(node.header.clone()) {
        return BlockBytes::HeaderDiffers;
    }
    match (served == node.bytes.as_slice(), certifying) {
        (true, _) => BlockBytes::Same,
        (false, true) => BlockBytes::SameHeader,
        (false, false) => BlockBytes::BytesDiffer,
    }
}

/// An output's value as named amounts, lovelace under `lovelace` and each asset under its policy and name in hex.
pub type Amounts = BTreeMap<String, u128>;

/// The value of each output in the node's UTxO query answer, keyed by its reference as the query prints it.
pub fn node_amounts(answer: &serde_json::Value) -> Result<BTreeMap<String, Amounts>, String> {
    let outputs = answer
        .as_object()
        .ok_or("the UTxO answer is not an object")?;
    outputs
        .iter()
        .map(|(reference, output)| {
            let value = output["value"]
                .as_object()
                .ok_or(format!("{reference} has no value"))?;
            let mut amounts = Amounts::new();
            for (key, amount) in value {
                match amount {
                    serde_json::Value::Number(n) => {
                        let n = n.as_u64().ok_or(format!("{reference} {key}={n}"))?;
                        amounts.insert(key.clone(), u128::from(n));
                    }
                    serde_json::Value::Object(assets) => {
                        for (name, n) in assets {
                            let n = n.as_u64().ok_or(format!("{reference} {key}.{name}={n}"))?;
                            amounts.insert(format!("{key}.{name}"), u128::from(n));
                        }
                    }
                    other => return Err(format!("{reference} {key}={other}")),
                }
            }
            Ok((reference.clone(), amounts))
        })
        .collect()
}

/// How one output reference compares between Dolos and the node's UTxO query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UtxoPresence {
    Both { same: bool },
    NodeOnly,
    DolosOnly,
    Neither,
}

/// Compares an output Dolos serves with the node's, by value when both hold it.
pub fn utxo_presence(node: Option<&Amounts>, dolos: Option<&Amounts>) -> UtxoPresence {
    match (node, dolos) {
        (Some(n), Some(d)) => UtxoPresence::Both { same: n == d },
        (Some(_), None) => UtxoPresence::NodeOnly,
        (None, Some(_)) => UtxoPresence::DolosOnly,
        (None, None) => UtxoPresence::Neither,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(offset: u64, header: (u16, u16), slot: u64) -> Vec<u8> {
        let mut e = offset.to_be_bytes().to_vec();
        e.extend(header.0.to_be_bytes());
        e.extend(header.1.to_be_bytes());
        e.extend([0u8; 4 + 32]);
        e.extend(slot.to_be_bytes());
        e
    }

    #[test]
    fn a_chunk_splits_into_its_blocks_by_the_secondary_index() {
        let secondary = [entry(0, (1, 2), 16), entry(5, (1, 3), 20)].concat();
        let chunk = b"aHHbbcHHHd";
        let blocks = chunk_blocks(&secondary, chunk).unwrap();
        assert_eq!(
            blocks,
            vec![
                NodeBlock {
                    slot: 16,
                    header: 1..3,
                    bytes: b"aHHbb".to_vec()
                },
                NodeBlock {
                    slot: 20,
                    header: 1..4,
                    bytes: b"cHHHd".to_vec()
                },
            ]
        );
        assert!(chunk_blocks(&secondary[..55], chunk).is_err());
        assert!(chunk_blocks(&entry(11, (0, 0), 1), chunk).is_err());
    }

    #[test]
    fn served_block_bytes_agree_only_with_the_node_bytes_or_its_header_when_certifying() {
        let node = NodeBlock {
            slot: 7,
            header: 1..3,
            bytes: b"aHHbb".to_vec(),
        };
        assert_eq!(block_bytes(Some(&node), b"aHHbb", false), BlockBytes::Same);
        assert_eq!(block_bytes(Some(&node), b"aHHbb", true), BlockBytes::Same);
        assert_eq!(
            block_bytes(Some(&node), b"aHHxxxx", true),
            BlockBytes::SameHeader
        );
        assert_eq!(
            block_bytes(Some(&node), b"aHHxxxx", false),
            BlockBytes::BytesDiffer
        );
        assert_eq!(
            block_bytes(Some(&node), b"aHXbb", true),
            BlockBytes::HeaderDiffers
        );
        assert_eq!(
            block_bytes(Some(&node), b"aH", true),
            BlockBytes::HeaderDiffers
        );
        assert_eq!(block_bytes(None, b"aHHbb", false), BlockBytes::NodeAbsent);
        assert!(!BlockBytes::NodeAbsent.agrees());
    }

    #[test]
    fn node_utxo_values_read_as_named_amounts() {
        let answer = serde_json::json!({
            "ab#0": {"address": "addr", "value": {"lovelace": 5, "cd": {"ef": 2, "": 1}}},
            "ab#1": {"value": {"lovelace": 7}},
        });
        let amounts = node_amounts(&answer).unwrap();
        assert_eq!(
            amounts["ab#0"],
            Amounts::from([
                ("lovelace".into(), 5),
                ("cd.ef".into(), 2),
                ("cd.".into(), 1)
            ])
        );
        assert_eq!(amounts["ab#1"], Amounts::from([("lovelace".into(), 7)]));
        assert!(node_amounts(&serde_json::json!({"ab#0": {}})).is_err());
        assert!(node_amounts(&serde_json::json!({"ab#0": {"value": {"lovelace": "x"}}})).is_err());
    }

    #[test]
    fn an_output_agrees_only_when_both_hold_the_same_value() {
        let five = Amounts::from([("lovelace".into(), 5)]);
        let six = Amounts::from([("lovelace".into(), 6)]);
        assert_eq!(
            utxo_presence(Some(&five), Some(&five)),
            UtxoPresence::Both { same: true }
        );
        assert_eq!(
            utxo_presence(Some(&five), Some(&six)),
            UtxoPresence::Both { same: false }
        );
        assert_eq!(utxo_presence(Some(&five), None), UtxoPresence::NodeOnly);
        assert_eq!(utxo_presence(None, Some(&five)), UtxoPresence::DolosOnly);
        assert_eq!(utxo_presence(None, None), UtxoPresence::Neither);
    }

    fn tip() -> NodeTip {
        NodeTip {
            slot: 829955,
            epoch: 38,
            era: "Dijkstra".into(),
            slot_in_epoch: 9155,
            slots_to_epoch_end: 12445,
        }
    }

    #[test]
    fn reads_inside_one_epoch_are_aligned() {
        let later = NodeTip {
            slot: 829990,
            slot_in_epoch: 9190,
            slots_to_epoch_end: 12410,
            ..tip()
        };
        let aligned = Aligned::new(&tip(), &later, 820800, ()).unwrap();
        assert_eq!(aligned.epoch(), 38);
        assert!(Aligned::new(&tip(), &later, 842399, ()).is_some());
    }

    #[test]
    fn reads_across_an_epoch_or_outside_it_are_not_aligned() {
        let next = NodeTip {
            slot: 842400,
            epoch: 39,
            slot_in_epoch: 0,
            slots_to_epoch_end: 21600,
            ..tip()
        };
        assert_eq!(Aligned::new(&tip(), &next, 829955, ()), None);
        assert_eq!(Aligned::new(&tip(), &tip(), 842400, ()), None);
        assert_eq!(Aligned::new(&tip(), &tip(), 820799, ()), None);
    }

    fn tally(compared: usize, disagree: &[&str]) -> Tally {
        Tally {
            compared,
            disagree: disagree.iter().map(|d| d.to_string()).collect(),
        }
    }

    fn all(era: Tally, params: Tally, child: Tally) -> BTreeMap<LiveLocus, Tally> {
        BTreeMap::from([
            (LiveLocus::EraSummary, era),
            (LiveLocus::Params, params),
            (LiveLocus::ChildAsOutput, child),
            (LiveLocus::Block, tally(4, &[])),
            (LiveLocus::Utxo, tally(9, &[])),
        ])
    }

    #[test]
    fn agreeing_live_comparisons_pass_with_no_lines() {
        let settled = settle(
            &all(tally(1, &[]), tally(50, &[]), tally(12, &[])),
            &KnownLive::new(),
        );
        assert_eq!(
            settled,
            Settled {
                lines: vec![],
                known_fired: KnownLive::new(),
                pass: true
            }
        );
    }

    #[test]
    fn a_disagreement_off_the_list_fails_and_one_on_it_passes() {
        let tallies = all(
            tally(1, &["name=unknown node=Dijkstra"]),
            tally(50, &["costModels.PlutusV1[0] ledger=1 dolos=2"]),
            tally(12, &[]),
        );
        let known = KnownLive::from([(LiveLocus::EraSummary, Row::S16)]);
        let settled = settle(&tallies, &known);
        assert_eq!(
            settled.lines,
            vec![
                "KNOWN row=S16 locus=era_summary name=unknown node=Dijkstra",
                "LIVE-DIFF row=S15 locus=live_params costModels.PlutusV1[0] ledger=1 dolos=2",
            ]
        );
        assert!(!settled.pass);
        assert_eq!(settled.known_fired, known);
        let tallies = all(
            tally(1, &["name=unknown node=Dijkstra"]),
            tally(50, &[]),
            tally(12, &[]),
        );
        assert!(settle(&tallies, &known).pass);
    }

    #[test]
    fn a_known_locus_that_agrees_or_a_locus_that_compared_nothing_fails() {
        let known = KnownLive::from([(LiveLocus::EraSummary, Row::S16)]);
        let settled = settle(&all(tally(1, &[]), tally(50, &[]), tally(12, &[])), &known);
        assert_eq!(settled.lines, vec!["KNOWN_STALE row=S16 locus=era_summary"]);
        assert!(settled.known_fired.is_empty());
        assert!(!settled.pass);
        let mut tallies = all(tally(1, &[]), tally(50, &[]), tally(0, &[]));
        let settled = settle(&tallies, &KnownLive::new());
        assert_eq!(
            settled.lines,
            vec!["LIVE-GAP locus=live_as_output compared=0"]
        );
        assert!(!settled.pass);
        tallies.remove(&LiveLocus::Params);
        let settled = settle(&tallies, &KnownLive::new());
        assert_eq!(
            settled.lines,
            vec![
                "LIVE-GAP locus=live_params compared=0",
                "LIVE-GAP locus=live_as_output compared=0",
            ]
        );
    }

    #[test]
    fn the_last_served_era_name_is_the_node_era() {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(era_name(&names(&["conway", "dijkstra"]), "Dijkstra").agrees());
        let unknown = era_name(&names(&["unknown"]), "Dijkstra");
        assert_eq!(unknown.to_string(), "name=unknown node=Dijkstra");
        assert_eq!(
            era_name(&names(&[]), "Dijkstra").to_string(),
            "name=none node=Dijkstra"
        );
    }
}
