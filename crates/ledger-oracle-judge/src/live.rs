use std::{collections::BTreeMap, fmt, ops::Range, str::FromStr};

use serde::Deserialize;

use crate::{
    cbor::{EraBound, NodeEra},
    compare,
    manifest::{KnownLive, LiveLocus},
    row::{judged_by, Row},
};

/// The comparisons of one live locus, each disagreement shown as text since a
/// live chain has no corpus record.
pub type Tally = compare::Tally<String>;

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
    /// The slots of the tip's epoch, none when the tip's counts do not fit its
    /// slot.
    pub fn epoch_slots(&self) -> Option<Range<u64>> {
        let start = self.slot.checked_sub(self.slot_in_epoch)?;
        let end = self.slot.checked_add(self.slots_to_epoch_end)?;
        Some(start..end)
    }
}

/// Reads taken while the node stayed in the epoch that holds the served ledger
/// slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aligned<T> {
    epoch: u64,
    value: T,
}

impl<T> Aligned<T> {
    /// Pairs the reads with the epoch that contains both node tips and the
    /// served slot, none otherwise.
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

/// The slot a node translates a POSIX second to, or past its horizon when it
/// translates none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeSlot {
    Slot(u64),
    PastHorizon,
}

impl FromStr for NodeSlot {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "past-horizon" => Ok(NodeSlot::PastHorizon),
            n => n
                .parse()
                .map(NodeSlot::Slot)
                .map_err(|_| format!("bad node slot {n}")),
        }
    }
}

impl fmt::Display for NodeSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeSlot::Slot(n) => write!(f, "{n}"),
            NodeSlot::PastHorizon => f.write_str("past-horizon"),
        }
    }
}

/// One era Dolos serves, with the POSIX milliseconds it serves for its start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedEra {
    pub name: String,
    pub start: EraBound,
    pub start_ms: u64,
    pub end: Option<EraBound>,
}

/// A slot Dolos serves with the POSIX milliseconds it serves for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServedTime {
    pub slot: u64,
    pub ms: u64,
}

/// The node's era, era history and translations of the seconds the served times
/// name.
#[derive(Debug, Clone, Copy)]
pub struct NodeTime<'a> {
    pub era: &'a str,
    pub history: &'a [NodeEra],
    pub slots: &'a BTreeMap<u64, NodeSlot>,
}

fn show_bound(bound: Option<EraBound>) -> String {
    bound.map_or("none".to_string(), |b| format!("{}/{}", b.slot, b.epoch))
}

/// Counts the served eras against the node's eras that hold a slot, the start
/// and tip times against the node's translation, and the slot length between
/// them against the node's.
pub fn compare_era_summary(
    served: &[ServedEra],
    tip: ServedTime,
    node: &NodeTime,
    tally: &mut Tally,
) {
    let names: Vec<String> = served.iter().map(|e| e.name.clone()).collect();
    let verdict = era_name(&names, node.era);
    tally.add(verdict.agrees(), || verdict.to_string());
    let eras: Vec<&NodeEra> = node
        .history
        .iter()
        .filter(|e| e.end.slot > e.start.slot)
        .collect();
    tally.add(served.len() == eras.len(), || {
        format!("eras served={} node={}", served.len(), eras.len())
    });
    for (i, (s, n)) in served.iter().zip(&eras).enumerate() {
        tally.add(s.start == n.start, || {
            let (served, node) = (show_bound(Some(s.start)), show_bound(Some(n.start)));
            format!("era={} start served={served} node={node}", s.name)
        });
        // The node's end of its current era is the forecast horizon, which is
        // not an era end.
        let end = (i + 1 < eras.len()).then_some(n.end);
        tally.add(s.end == end, || {
            let (served, node) = (show_bound(s.end), show_bound(end));
            format!("era={} end served={served} node={node}", s.name)
        });
    }
    let (Some(era), Some(node_era)) = (served.last(), eras.last()) else {
        return;
    };
    let show = |slot: Option<&NodeSlot>| slot.map_or("not_queried".to_string(), |s| s.to_string());
    let mut translated = |label: String, time: ServedTime| {
        let second = time.ms / 1000;
        let at = node.slots.get(&second);
        let before = second.checked_sub(1).and_then(|b| node.slots.get(&b));
        let slot = NodeSlot::Slot(time.slot);
        let agrees =
            time.ms.is_multiple_of(1000) && at == Some(&slot) && before.is_some_and(|b| *b != slot);
        tally.add(agrees, || {
            format!(
                "{label} slot={} time={} node_slot={} node_slot_before={}",
                time.slot,
                time.ms,
                show(at),
                show(before)
            )
        });
    };
    let start = ServedTime {
        slot: era.start.slot,
        ms: era.start_ms,
    };
    translated(format!("era={} start_time", era.name), start);
    translated("tip".to_string(), tip);
    let length = tip
        .slot
        .checked_sub(start.slot)
        .filter(|slots| *slots > 0)
        .and_then(|slots| {
            let ms = tip.ms.checked_sub(start.ms)?;
            ms.is_multiple_of(slots).then_some(ms / slots)
        });
    tally.add(length == Some(node_era.slot_length_ms), || {
        let served = length.map_or("underived".to_string(), |n| n.to_string());
        let node = node_era.slot_length_ms;
        format!("era={} slot_length served={served} node={node}", era.name)
    });
}

/// The lines and verdict of the live comparisons, judged against the known
/// list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settled {
    pub lines: Vec<String>,
    pub known_fired: KnownLive,
    pub pass: bool,
}

/// Judges each live locus's tally, failing one that compared nothing, disagrees
/// off the list, or agrees on it.
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

/// The bytes of one block the node's immutable database holds, with where its
/// header is.
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

/// How the bytes of a block Dolos serves compare with the node's block at its
/// slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockBytes {
    Same,
    /// A block whose body holds a certificate, whose header bytes are the
    /// node's and whose body carries this many transactions, the node's
    /// endorser block's in order.
    SameHeader(usize),
    NodeAbsent,
    HeaderDiffers,
    BytesDiffer,
    EndorsedDiffer,
}

/// How the transactions a certifying block serves compare with its endorser
/// block's on the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endorsed {
    /// Both lists hold this many transactions, equal in id, bytes and order.
    Agree(usize),
    Differ,
}

impl BlockBytes {
    /// Whether the served bytes agree with the node's.
    pub fn agrees(self) -> bool {
        matches!(self, BlockBytes::Same | BlockBytes::SameHeader(_))
    }

    /// The name printed in judgement lines.
    pub fn name(self) -> &'static str {
        match self {
            BlockBytes::Same => "same",
            BlockBytes::SameHeader(_) => "same_header",
            BlockBytes::NodeAbsent => "node_absent",
            BlockBytes::HeaderDiffers => "header_differs",
            BlockBytes::BytesDiffer => "bytes_differ",
            BlockBytes::EndorsedDiffer => "endorsed_differ",
        }
    }
}

/// Compares the bytes Dolos serves for a block with the node's, the header and
/// the endorsed transactions for a block whose body holds a certificate.
pub fn block_bytes(
    node: Option<&NodeBlock>,
    served: &[u8],
    endorsed: Option<Endorsed>,
) -> BlockBytes {
    let Some(node) = node else {
        return BlockBytes::NodeAbsent;
    };
    if served.get(node.header.clone()) != node.bytes.get(node.header.clone()) {
        return BlockBytes::HeaderDiffers;
    }
    match (served == node.bytes.as_slice(), endorsed) {
        (_, Some(Endorsed::Differ)) => BlockBytes::EndorsedDiffer,
        (true, _) => BlockBytes::Same,
        (false, Some(Endorsed::Agree(n))) => BlockBytes::SameHeader(n),
        (false, None) => BlockBytes::BytesDiffer,
    }
}

/// How one output reference compares between Dolos and the node's UTxO query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UtxoPresence {
    Both { differing: Vec<String> },
    NodeOnly,
    DolosOnly,
    Neither,
}

/// Compares an output Dolos serves with the node's, by the fields `differing`
/// names when both hold it.
pub fn utxo_presence<N, D>(
    node: Option<&N>,
    dolos: Option<&D>,
    differing: impl Fn(&N, &D) -> Vec<String>,
) -> UtxoPresence {
    match (node, dolos) {
        (Some(n), Some(d)) => UtxoPresence::Both {
            differing: differing(n, d),
        },
        (Some(_), None) => UtxoPresence::NodeOnly,
        (None, Some(_)) => UtxoPresence::DolosOnly,
        (None, None) => UtxoPresence::Neither,
    }
}

/// A chain point as its slot and block hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Point {
    pub slot: u64,
    pub hash: Vec<u8>,
}

/// The points the node and Dolos held around one UTxO read on each, in the
/// order they were read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UtxoBracket {
    pub node_before: Point,
    pub dolos_before: Point,
    pub dolos_after: Point,
    pub node_after: Point,
}

impl fmt::Display for UtxoBracket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "node_before={} dolos_before={} dolos_after={} node_after={}",
            self.node_before.slot,
            self.dolos_before.slot,
            self.dolos_after.slot,
            self.node_after.slot
        )
    }
}

/// Reads the node and Dolos both took at one chain point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtPoint<T> {
    point: Point,
    value: T,
}

impl<T> AtPoint<T> {
    /// Pairs the reads with the point all four bracket points name, none when
    /// two of them differ.
    pub fn new(bracket: &UtxoBracket, value: T) -> Option<Self> {
        let point = &bracket.node_before;
        let one = [
            &bracket.dolos_before,
            &bracket.dolos_after,
            &bracket.node_after,
        ]
        .iter()
        .all(|other| *other == point);
        one.then(|| AtPoint {
            point: point.clone(),
            value,
        })
    }

    /// The point the reads were taken at.
    pub fn point(&self) -> &Point {
        &self.point
    }
}

/// The outputs one UTxO read asked for, with the node's outputs and the outputs
/// Dolos served, `None` where Dolos served one unreadably.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UtxoReads<N, D> {
    pub refs: Vec<String>,
    pub node: BTreeMap<String, N>,
    pub dolos: BTreeMap<String, Option<D>>,
}

/// Counts each output asked for by the fields `differing` names between Dolos
/// and the node, and returns how many neither holds.
pub fn compare_utxos<N, D>(
    reads: &AtPoint<UtxoReads<N, D>>,
    tally: &mut Tally,
    differing: impl Fn(&N, &D) -> Vec<String>,
) -> usize {
    let slot = reads.point.slot;
    let UtxoReads { refs, node, dolos } = &reads.value;
    let mut neither = 0;
    for key in refs {
        let show = |presence: &str| format!("utxo={key} {presence} point={slot}");
        let served = match dolos.get(key) {
            None => None,
            Some(Some(output)) => Some(output),
            Some(None) => {
                tally.add(false, || show("dolos=unreadable"));
                continue;
            }
        };
        match utxo_presence(node.get(key), served, &differing) {
            UtxoPresence::Neither => neither += 1,
            UtxoPresence::Both { differing } => tally.add(differing.is_empty(), || {
                show(&format!("differing={}", differing.join(",")))
            }),
            UtxoPresence::NodeOnly => tally.add(false, || show("dolos=absent")),
            UtxoPresence::DolosOnly => tally.add(false, || show("node=absent")),
        }
    }
    neither
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
    fn served_block_bytes_agree_only_with_the_node_bytes_or_its_header_and_endorser_block() {
        let node = NodeBlock {
            slot: 7,
            header: 1..3,
            bytes: b"aHHbb".to_vec(),
        };
        let agree = Some(Endorsed::Agree(2));
        assert_eq!(block_bytes(Some(&node), b"aHHbb", None), BlockBytes::Same);
        assert_eq!(block_bytes(Some(&node), b"aHHbb", agree), BlockBytes::Same);
        assert_eq!(
            block_bytes(Some(&node), b"aHHxxxx", agree),
            BlockBytes::SameHeader(2)
        );
        assert!(BlockBytes::SameHeader(2).agrees());
        assert_eq!(
            block_bytes(Some(&node), b"aHHxxxx", Some(Endorsed::Differ)),
            BlockBytes::EndorsedDiffer
        );
        assert_eq!(
            block_bytes(Some(&node), b"aHHbb", Some(Endorsed::Differ)),
            BlockBytes::EndorsedDiffer
        );
        assert!(!BlockBytes::EndorsedDiffer.agrees());
        assert_eq!(
            block_bytes(Some(&node), b"aHHxxxx", None),
            BlockBytes::BytesDiffer
        );
        assert_eq!(
            block_bytes(Some(&node), b"aHXbb", agree),
            BlockBytes::HeaderDiffers
        );
        assert_eq!(
            block_bytes(Some(&node), b"aH", agree),
            BlockBytes::HeaderDiffers
        );
        assert_eq!(block_bytes(None, b"aHHbb", None), BlockBytes::NodeAbsent);
        assert!(!BlockBytes::NodeAbsent.agrees());
    }

    /// Names `coin` when two coins differ.
    fn coin(node: &u64, dolos: &u64) -> Vec<String> {
        if node == dolos {
            Vec::new()
        } else {
            vec!["coin".to_string()]
        }
    }

    #[test]
    fn an_output_both_hold_names_the_fields_that_differ() {
        assert_eq!(
            utxo_presence(Some(&5), Some(&5), coin),
            UtxoPresence::Both { differing: vec![] }
        );
        assert_eq!(
            utxo_presence(Some(&5), Some(&6), coin),
            UtxoPresence::Both {
                differing: vec!["coin".to_string()]
            }
        );
        assert_eq!(utxo_presence(Some(&5), None, coin), UtxoPresence::NodeOnly);
        assert_eq!(utxo_presence(None, Some(&5), coin), UtxoPresence::DolosOnly);
        assert_eq!(utxo_presence(None, None, coin), UtxoPresence::Neither);
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

    const START_MS: u64 = 1_792_281_600_000;
    const TIP: u64 = 1_354_846;

    fn node_slots(served: &[ServedTime]) -> BTreeMap<u64, NodeSlot> {
        let start = START_MS / 1000;
        let translate = |second: u64| match second.checked_sub(start) {
            Some(slot) => NodeSlot::Slot(slot),
            None => NodeSlot::PastHorizon,
        };
        let seconds = served.iter().flat_map(|t| [t.ms / 1000 - 1, t.ms / 1000]);
        seconds.map(|s| (s, translate(s))).collect()
    }

    fn history() -> Vec<NodeEra> {
        let zero = EraBound { slot: 0, epoch: 0 };
        let empty = NodeEra {
            start: zero,
            end: zero,
            epoch_size: 21600,
            slot_length_ms: 1000,
        };
        let current = NodeEra {
            end: EraBound {
                slot: 1382400,
                epoch: 64,
            },
            ..empty
        };
        vec![empty, empty, current]
    }

    fn served(start_ms: u64) -> Vec<ServedEra> {
        vec![ServedEra {
            name: "dijkstra".to_string(),
            start: EraBound { slot: 0, epoch: 0 },
            start_ms,
            end: None,
        }]
    }

    fn era_disagreements(served: &[ServedEra], tip: ServedTime) -> (usize, Vec<String>) {
        let start = ServedTime {
            slot: 0,
            ms: served[0].start_ms,
        };
        let slots = node_slots(&[start, tip]);
        let history = history();
        let node = NodeTime {
            era: "Dijkstra",
            history: &history,
            slots: &slots,
        };
        let mut tally = Tally::default();
        compare_era_summary(served, tip, &node, &mut tally);
        (tally.compared, tally.disagree)
    }

    fn tip_at(slot_ms: u64, shift_ms: u64) -> ServedTime {
        ServedTime {
            slot: TIP,
            ms: START_MS + shift_ms + TIP * slot_ms,
        }
    }

    #[test]
    fn served_eras_and_times_the_node_translates_alike_agree() {
        let (compared, disagree) = era_disagreements(&served(START_MS), tip_at(1000, 0));
        assert_eq!((compared, disagree), (7, Vec::<String>::new()));
    }

    #[test]
    fn a_served_time_one_second_late_disagrees_at_the_start_and_the_tip() {
        let (_, disagree) = era_disagreements(&served(START_MS + 1000), tip_at(1000, 1000));
        let start = START_MS + 1000;
        let tip = START_MS + 1000 + TIP * 1000;
        assert_eq!(
            disagree,
            [
                format!(
                    "era=dijkstra start_time slot=0 time={start} node_slot=1 node_slot_before=0"
                ),
                format!(
                    "tip slot={TIP} time={tip} node_slot={} node_slot_before={TIP}",
                    TIP + 1
                ),
            ]
        );
    }

    #[test]
    fn a_served_slot_length_other_than_the_node_s_disagrees() {
        let (_, disagree) = era_disagreements(&served(START_MS), tip_at(2000, 0));
        let tip = START_MS + TIP * 2000;
        assert_eq!(
            disagree,
            [
                format!(
                    "tip slot={TIP} time={tip} node_slot={} node_slot_before={}",
                    2 * TIP,
                    2 * TIP - 1
                ),
                "era=dijkstra slot_length served=2000 node=1000".to_string(),
            ]
        );
    }

    #[test]
    fn a_served_end_or_start_other_than_the_node_s_disagrees() {
        let mut moved = served(START_MS);
        moved[0].end = Some(EraBound {
            slot: 1382400,
            epoch: 64,
        });
        moved[0].start.epoch = 1;
        let (compared, disagree) = era_disagreements(&moved, tip_at(1000, 0));
        assert_eq!(compared, 7);
        assert_eq!(
            disagree,
            [
                "era=dijkstra start served=0/1 node=0/0",
                "era=dijkstra end served=1382400/64 node=none",
            ]
        );
        let two = [served(START_MS), served(START_MS)].concat();
        let (_, disagree) = era_disagreements(&two, tip_at(1000, 0));
        assert_eq!(disagree, ["eras served=2 node=1"]);
    }

    #[test]
    fn a_served_time_the_node_was_not_asked_to_translate_disagrees() {
        let history = history();
        let slots = BTreeMap::new();
        let node = NodeTime {
            era: "Dijkstra",
            history: &history,
            slots: &slots,
        };
        let mut tally = Tally::default();
        compare_era_summary(&served(START_MS), tip_at(1000, 0), &node, &mut tally);
        assert_eq!(tally.disagree.len(), 2);
        assert!(tally.disagree[0].ends_with("node_slot=not_queried node_slot_before=not_queried"));
    }

    #[test]
    fn a_node_slot_reads_a_slot_or_past_horizon() {
        assert_eq!("17\n".parse::<NodeSlot>(), Ok(NodeSlot::Slot(17)));
        assert_eq!(
            "past-horizon\n".parse::<NodeSlot>(),
            Ok(NodeSlot::PastHorizon)
        );
        assert!("Error".parse::<NodeSlot>().is_err());
    }

    fn point(slot: u64) -> Point {
        Point {
            slot,
            hash: vec![slot as u8; 32],
        }
    }

    fn bracket(slots: [u64; 4]) -> UtxoBracket {
        let [node_before, dolos_before, dolos_after, node_after] = slots.map(point);
        UtxoBracket {
            node_before,
            dolos_before,
            dolos_after,
            node_after,
        }
    }

    #[test]
    fn utxo_reads_bracketed_by_one_point_are_paired_with_it() {
        let reads = AtPoint::new(&bracket([7, 7, 7, 7]), ()).unwrap();
        assert_eq!(reads.point(), &point(7));
    }

    #[test]
    fn utxo_reads_bracketed_by_two_points_are_not_paired() {
        for slots in [[7, 7, 7, 8], [7, 7, 8, 8], [7, 8, 8, 8], [6, 7, 7, 7]] {
            assert_eq!(AtPoint::new(&bracket(slots), ()), None, "{slots:?}");
        }
        let mut fork = bracket([7, 7, 7, 7]);
        fork.dolos_after.hash = vec![0; 32];
        assert_eq!(AtPoint::new(&fork, ()), None);
        assert_eq!(
            bracket([1213053, 1213053, 1213099, 1213099]).to_string(),
            "node_before=1213053 dolos_before=1213053 dolos_after=1213099 node_after=1213099"
        );
    }

    fn reads(node: &[(&str, u64)], dolos: &[(&str, Option<u64>)]) -> AtPoint<UtxoReads<u64, u64>> {
        let refs = ["a#0", "a#1", "b#0"].map(String::from).to_vec();
        let node = node.iter().map(|(r, n)| (r.to_string(), *n));
        let dolos = dolos.iter().map(|(r, n)| (r.to_string(), *n));
        let reads = UtxoReads {
            refs,
            node: node.collect(),
            dolos: dolos.collect(),
        };
        AtPoint::new(&bracket([9, 9, 9, 9]), reads).unwrap()
    }

    #[test]
    fn outputs_held_at_one_value_on_both_sides_agree() {
        let mut t = Tally::default();
        let both = reads(
            &[("a#0", 5), ("b#0", 2)],
            &[("a#0", Some(5)), ("b#0", Some(2))],
        );
        assert_eq!(compare_utxos(&both, &mut t, coin), 1);
        assert_eq!(t, tally(2, &[]));
    }

    #[test]
    fn an_output_one_side_lacks_or_states_differently_disagrees() {
        let mut t = Tally::default();
        let read = reads(
            &[("a#0", 5), ("a#1", 3)],
            &[("a#0", Some(6)), ("b#0", Some(2)), ("a#1", None)],
        );
        assert_eq!(compare_utxos(&read, &mut t, coin), 0);
        assert_eq!(
            t,
            tally(
                3,
                &[
                    "utxo=a#0 differing=coin point=9",
                    "utxo=a#1 dolos=unreadable point=9",
                    "utxo=b#0 node=absent point=9",
                ]
            )
        );
        let mut t = Tally::default();
        compare_utxos(&reads(&[("b#0", 2)], &[]), &mut t, coin);
        assert_eq!(t, tally(1, &["utxo=b#0 dolos=absent point=9"]));
    }
}
