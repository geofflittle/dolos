use std::collections::BTreeSet;

use thiserror::Error;

/// A transaction output reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TxIn {
    pub tx: [u8; 32],
    pub ix: u64,
}

/// A redeemer's purpose, its tag and its index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Purpose {
    pub tag: u64,
    pub index: u64,
}

impl std::fmt::Display for Purpose {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}#{}", self.tag, self.index)
    }
}

/// A CBOR item that is not the shape a reader asked for.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("cbor at byte {at}: {what}")]
pub struct CborError {
    pub at: usize,
    pub what: &'static str,
}

type Result<T> = std::result::Result<T, CborError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Len {
    Def(u64),
    Indef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Head {
    Uint(u64),
    Nint(u64),
    Bytes(Len),
    Text(Len),
    Array(Len),
    Map(Len),
    Tag(u64),
    Simple,
    Break,
}

struct Reader<'b> {
    buf: &'b [u8],
    pos: usize,
}

impl<'b> Reader<'b> {
    fn new(buf: &'b [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    fn fail<T>(&self, what: &'static str) -> Result<T> {
        Err(CborError { at: self.pos, what })
    }

    fn take(&mut self, n: u64) -> Result<&'b [u8]> {
        let end = usize::try_from(n)
            .ok()
            .and_then(|n| self.pos.checked_add(n))
            .filter(|end| *end <= self.buf.len());
        match end {
            Some(end) => {
                let out = &self.buf[self.pos..end];
                self.pos = end;
                Ok(out)
            }
            None => self.fail("item runs past the end"),
        }
    }

    fn head(&mut self) -> Result<Head> {
        let initial = self.take(1)?[0];
        let (major, info) = (initial >> 5, initial & 0x1f);
        if major == 7 {
            return match info {
                0..=23 => Ok(Head::Simple),
                24 => self.take(1).map(|_| Head::Simple),
                25 => self.take(2).map(|_| Head::Simple),
                26 => self.take(4).map(|_| Head::Simple),
                27 => self.take(8).map(|_| Head::Simple),
                31 => Ok(Head::Break),
                _ => self.fail("reserved simple value"),
            };
        }
        let len = match info {
            0..=23 => Len::Def(u64::from(info)),
            24..=27 => {
                let width = 1u64 << (info - 24);
                let raw = self.take(width)?;
                Len::Def(raw.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b)))
            }
            31 if (2..=5).contains(&major) => Len::Indef,
            _ => return self.fail("reserved additional information"),
        };
        let def = |len: Len| match len {
            Len::Def(n) => Some(n),
            Len::Indef => None,
        };
        Ok(match major {
            0 => Head::Uint(def(len).unwrap_or_default()),
            1 => Head::Nint(def(len).unwrap_or_default()),
            2 => Head::Bytes(len),
            3 => Head::Text(len),
            4 => Head::Array(len),
            5 => Head::Map(len),
            _ => Head::Tag(def(len).unwrap_or_default()),
        })
    }

    fn untagged_head(&mut self) -> Result<Head> {
        loop {
            match self.head()? {
                Head::Tag(_) => continue,
                other => return Ok(other),
            }
        }
    }

    fn at_break(&mut self) -> Result<bool> {
        match self.buf.get(self.pos) {
            Some(0xff) => {
                self.pos += 1;
                Ok(true)
            }
            Some(_) => Ok(false),
            None => self.fail("indefinite item runs past the end"),
        }
    }

    fn each(&mut self, len: Len, mut f: impl FnMut(&mut Self) -> Result<()>) -> Result<()> {
        match len {
            Len::Def(n) => (0..n).try_for_each(|_| f(self)),
            Len::Indef => {
                while !self.at_break()? {
                    f(self)?;
                }
                Ok(())
            }
        }
    }

    fn skip(&mut self) -> Result<()> {
        match self.head()? {
            Head::Uint(_) | Head::Nint(_) | Head::Simple => Ok(()),
            Head::Bytes(Len::Def(n)) | Head::Text(Len::Def(n)) => self.take(n).map(|_| ()),
            Head::Bytes(len) | Head::Text(len) | Head::Array(len) => self.each(len, Self::skip),
            Head::Map(len) => self.each(len, |r| {
                r.skip()?;
                r.skip()
            }),
            Head::Tag(_) => self.skip(),
            Head::Break => self.fail("unexpected break"),
        }
    }

    fn array(&mut self) -> Result<Len> {
        match self.untagged_head()? {
            Head::Array(len) => Ok(len),
            _ => self.fail("expected an array"),
        }
    }

    fn map(&mut self) -> Result<Len> {
        match self.untagged_head()? {
            Head::Map(len) => Ok(len),
            _ => self.fail("expected a map"),
        }
    }

    fn uint(&mut self) -> Result<u64> {
        match self.untagged_head()? {
            Head::Uint(n) => Ok(n),
            _ => self.fail("expected an unsigned integer"),
        }
    }

    fn txin(&mut self) -> Result<TxIn> {
        if self.array()? != Len::Def(2) {
            return self.fail("expected a two element input");
        }
        let tx = match self.untagged_head()? {
            Head::Bytes(Len::Def(32)) => {
                let mut tx = [0u8; 32];
                tx.copy_from_slice(self.take(32)?);
                tx
            }
            _ => return self.fail("expected a 32 byte transaction id"),
        };
        Ok(TxIn {
            tx,
            ix: self.uint()?,
        })
    }

    fn purpose(&mut self) -> Result<Purpose> {
        if self.array()? != Len::Def(2) {
            return self.fail("expected a two element purpose");
        }
        Ok(Purpose {
            tag: self.uint()?,
            index: self.uint()?,
        })
    }

    fn txins(&mut self) -> Result<Vec<TxIn>> {
        let len = self.array()?;
        let mut out = Vec::new();
        self.each(len, |r| r.txin().map(|i| out.push(i)))?;
        Ok(out)
    }

    fn bytes(&mut self, len: Len) -> Result<Vec<u8>> {
        match len {
            Len::Def(n) => self.take(n).map(<[u8]>::to_vec),
            Len::Indef => {
                let mut out = Vec::new();
                while !self.at_break()? {
                    match self.head()? {
                        Head::Bytes(Len::Def(n)) => out.extend_from_slice(self.take(n)?),
                        _ => return self.fail("expected a definite byte string chunk"),
                    }
                }
                Ok(out)
            }
        }
    }

    fn data_items(&mut self, len: Len) -> Result<Vec<Data>> {
        let mut out = Vec::new();
        self.each(len, |r| r.data().map(|d| out.push(d)))?;
        Ok(out)
    }

    fn fields(&mut self) -> Result<Vec<Data>> {
        match self.head()? {
            Head::Array(len) => self.data_items(len),
            _ => self.fail("expected constructor fields"),
        }
    }

    fn general_constr(&mut self) -> Result<Data> {
        let len = match self.head()? {
            Head::Array(len @ (Len::Def(2) | Len::Indef)) => len,
            _ => return self.fail("expected a constructor tag and its fields"),
        };
        let tag = match self.head()? {
            Head::Uint(tag) => tag,
            _ => return self.fail("expected a constructor tag"),
        };
        let fields = self.fields()?;
        if len == Len::Indef && !self.at_break()? {
            return self.fail("expected a constructor tag and its fields");
        }
        Ok(Data::Constr(tag, fields))
    }

    fn data(&mut self) -> Result<Data> {
        Ok(match self.head()? {
            Head::Uint(n) => Data::Int(Int::new(false, &n.to_be_bytes())),
            Head::Nint(n) => Data::Int(Int::new(true, &n.to_be_bytes())),
            Head::Bytes(len) => Data::Bytes(self.bytes(len)?),
            Head::Array(len) => Data::List(self.data_items(len)?),
            Head::Map(len) => {
                let mut out = Vec::new();
                self.each(len, |r| {
                    let key = r.data()?;
                    out.push((key, r.data()?));
                    Ok(())
                })?;
                Data::Map(out)
            }
            Head::Tag(tag @ 121..=127) => Data::Constr(tag - 121, self.fields()?),
            Head::Tag(tag @ 1280..=1400) => Data::Constr(tag - 1280 + 7, self.fields()?),
            Head::Tag(102) => self.general_constr()?,
            Head::Tag(tag @ (2 | 3)) => match self.head()? {
                Head::Bytes(len) => Data::Int(Int::new(tag == 3, &self.bytes(len)?)),
                _ => return self.fail("expected the bytes of a big integer"),
            },
            Head::Tag(_) => return self.fail("a tag no data value holds"),
            Head::Text(_) | Head::Simple => return self.fail("an item no data value holds"),
            Head::Break => return self.fail("unexpected break"),
        })
    }
}

/// A Plutus data value, without the encoding choices of the bytes it was read
/// from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Data {
    Constr(u64, Vec<Data>),
    Map(Vec<(Data, Data)>),
    List(Vec<Data>),
    Int(Int),
    Bytes(Vec<u8>),
}

/// An integer as a sign and a big endian magnitude with no leading zero byte, a
/// negative one being minus one minus the magnitude.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Int {
    pub negative: bool,
    pub magnitude: Vec<u8>,
}

impl Int {
    fn new(negative: bool, big_endian: &[u8]) -> Self {
        let start = big_endian.iter().take_while(|b| **b == 0).count();
        Int {
            negative,
            magnitude: big_endian[start..].to_vec(),
        }
    }
}

impl std::fmt::Display for Int {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let small = (self.magnitude.len() <= 16).then(|| {
            self.magnitude
                .iter()
                .fold(0u128, |acc, b| (acc << 8) | u128::from(*b))
        });
        match (self.negative, small) {
            (false, Some(n)) => write!(f, "{n}"),
            (true, Some(n)) if n < u128::MAX => write!(f, "-{}", n + 1),
            (false, _) => write!(f, "0x{}", hex::encode(&self.magnitude)),
            (true, _) => write!(f, "-1-0x{}", hex::encode(&self.magnitude)),
        }
    }
}

impl std::fmt::Display for Data {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn joined(f: &mut std::fmt::Formatter<'_>, items: &[Data]) -> std::fmt::Result {
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    f.write_str(",")?;
                }
                write!(f, "{item}")?;
            }
            Ok(())
        }
        match self {
            Data::Constr(tag, fields) => {
                write!(f, "C{tag}[")?;
                joined(f, fields)?;
                f.write_str("]")
            }
            Data::List(items) => {
                f.write_str("[")?;
                joined(f, items)?;
                f.write_str("]")
            }
            Data::Map(entries) => {
                f.write_str("{")?;
                for (i, (key, value)) in entries.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{key}:{value}")?;
                }
                f.write_str("}")
            }
            Data::Int(n) => write!(f, "{n}"),
            Data::Bytes(b) => write!(f, "h'{}'", hex::encode(b)),
        }
    }
}

/// Decodes one Plutus data value from its CBOR bytes.
pub fn data(raw: &[u8]) -> Result<Data> {
    Reader::new(raw).data()
}

/// The slot and epoch at the start or end of an era.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EraBound {
    pub slot: u64,
    pub epoch: u64,
}

/// One era of a node's era history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeEra {
    pub start: EraBound,
    pub end: EraBound,
    pub epoch_size: u64,
    pub slot_length_ms: u64,
}

impl Reader<'_> {
    /// Reads an array, calling `f` with each item's index, and returns the item
    /// count.
    fn items(&mut self, mut f: impl FnMut(&mut Self, u64) -> Result<()>) -> Result<u64> {
        let len = self.array()?;
        let mut n = 0;
        self.each(len, |r| {
            f(r, n)?;
            n += 1;
            Ok(())
        })?;
        Ok(n)
    }

    fn era_bound(&mut self) -> Result<EraBound> {
        let (mut slot, mut epoch) = (None, None);
        self.items(|r, i| match i {
            1 => r.uint().map(|n| slot = Some(n)),
            2 => r.uint().map(|n| epoch = Some(n)),
            _ => r.skip(),
        })?;
        match (slot, epoch) {
            (Some(slot), Some(epoch)) => Ok(EraBound { slot, epoch }),
            _ => self.fail("expected an era bound"),
        }
    }

    fn node_era(&mut self) -> Result<NodeEra> {
        let (mut start, mut end, mut epoch_size, mut slot_length_ms) = (None, None, None, None);
        self.items(|r, i| match i {
            0 => r.era_bound().map(|b| start = Some(b)),
            1 => r.era_bound().map(|b| end = Some(b)),
            2 => r
                .items(|r, i| match i {
                    0 => r.uint().map(|n| epoch_size = Some(n)),
                    1 => r.uint().map(|n| slot_length_ms = Some(n)),
                    _ => r.skip(),
                })
                .map(|_| ()),
            _ => r.skip(),
        })?;
        match (start, end, epoch_size, slot_length_ms) {
            (Some(start), Some(end), Some(epoch_size), Some(slot_length_ms)) => Ok(NodeEra {
                start,
                end,
                epoch_size,
                slot_length_ms,
            }),
            _ => self.fail("expected an era summary"),
        }
    }
}

/// Decodes the eras of a node's era history query answer.
pub fn era_history(raw: &[u8]) -> Result<Vec<NodeEra>> {
    let mut r = Reader::new(raw);
    let mut eras = Vec::new();
    r.items(|r, _| r.node_era().map(|e| eras.push(e)))?;
    Ok(eras)
}

/// Decodes one output reference from its CBOR bytes.
pub fn txin(raw: &[u8]) -> Result<TxIn> {
    Reader::new(raw).txin()
}

/// Decodes one redeemer purpose from its CBOR bytes.
pub fn purpose(raw: &[u8]) -> Result<Purpose> {
    Reader::new(raw).purpose()
}

/// The inputs one sub transaction body names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubInputs {
    pub spend: Vec<TxIn>,
    pub reference: Vec<TxIn>,
}

/// The inputs one top level transaction and its sub transactions name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TxInputs {
    pub spend: Vec<TxIn>,
    pub collateral: Vec<TxIn>,
    pub reference: Vec<TxIn>,
    pub subs: Vec<SubInputs>,
}

impl TxInputs {
    /// The sub transaction spend inputs that the top level body does not also
    /// spend.
    pub fn child_only(&self) -> impl Iterator<Item = &TxIn> {
        self.subs
            .iter()
            .flat_map(|s| &s.spend)
            .filter(|i| !self.spend.contains(i))
    }

    /// The inputs the sub transactions name that the top level body names in
    /// none of its input sets.
    pub fn sub_only(&self) -> BTreeSet<&TxIn> {
        let top: BTreeSet<&TxIn> = self
            .spend
            .iter()
            .chain(&self.collateral)
            .chain(&self.reference)
            .collect();
        self.subs
            .iter()
            .flat_map(|s| s.spend.iter().chain(&s.reference))
            .filter(|i| !top.contains(i))
            .collect()
    }
}

fn body(r: &mut Reader, mut on: impl FnMut(&mut Reader, u64) -> Result<bool>) -> Result<()> {
    let len = r.map()?;
    r.each(len, |r| {
        let key = r.uint()?;
        if !on(r, key)? {
            r.skip()?;
        }
        Ok(())
    })
}

fn sub_tx(r: &mut Reader) -> Result<SubInputs> {
    let len = r.array()?;
    let mut out = SubInputs::default();
    let mut first = true;
    r.each(len, |r| {
        if !std::mem::take(&mut first) {
            return r.skip();
        }
        body(r, |r, key| {
            match key {
                0 => out.spend = r.txins()?,
                18 => out.reference = r.txins()?,
                _ => return Ok(false),
            }
            Ok(true)
        })
    })?;
    Ok(out)
}

/// The top level transaction or one of its sub transactions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Top,
    Sub,
}

/// A part of a transaction level, a body or witness set entry by its key or the
/// auxiliary data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Part {
    Body(u64),
    Witness(u64),
    Aux,
}

/// A part of a transaction at one level, written as `top.k7`, `sub.w5` or
/// `top.aux`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Field {
    pub level: Level,
    pub part: Part,
}

impl std::fmt::Display for Field {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let level = match self.level {
            Level::Top => "top",
            Level::Sub => "sub",
        };
        match self.part {
            Part::Body(key) => write!(f, "{level}.k{key}"),
            Part::Witness(key) => write!(f, "{level}.w{key}"),
            Part::Aux => write!(f, "{level}.aux"),
        }
    }
}

impl std::str::FromStr for Field {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let bad = || format!("bad field {s}");
        let (level, part) = s.split_once('.').ok_or_else(bad)?;
        let level = match level {
            "top" => Level::Top,
            "sub" => Level::Sub,
            _ => return Err(bad()),
        };
        let key = |k: &str| k.parse::<u64>().map_err(|_| bad());
        let part = match part {
            "aux" => Part::Aux,
            _ => match part.split_at_checked(1) {
                Some(("k", k)) => Part::Body(key(k)?),
                Some(("w", k)) => Part::Witness(key(k)?),
                _ => return Err(bad()),
            },
        };
        Ok(Field { level, part })
    }
}

/// Reads the parts the top level transaction and its sub transactions hold.
pub fn tx_fields(raw: &[u8]) -> Result<BTreeSet<Field>> {
    let mut r = Reader::new(raw);
    let mut out = BTreeSet::new();
    level_fields(&mut r, Level::Top, &mut out)?;
    Ok(out)
}

/// Reads one level's body, witness set and its last item, the auxiliary data,
/// recording each part it holds.
fn level_fields(r: &mut Reader, level: Level, out: &mut BTreeSet<Field>) -> Result<()> {
    let mut add = |part| {
        out.insert(Field { level, part });
    };
    let mut subs = Vec::new();
    let mut last_holds = false;
    let read = r.items(|r, i| match i {
        0 => body(r, |r, key| {
            add(Part::Body(key));
            if level == Level::Top && key == 23 {
                let len = r.array()?;
                r.each(len, |r| {
                    let mut sub = BTreeSet::new();
                    level_fields(r, Level::Sub, &mut sub)?;
                    subs.push(sub);
                    Ok(())
                })?;
                return Ok(true);
            }
            Ok(false)
        }),
        1 => body(r, |_, key| {
            add(Part::Witness(key));
            Ok(false)
        }),
        _ => {
            last_holds = r.buf.get(r.pos) != Some(&0xf6);
            r.skip()
        }
    })?;
    if read < 3 {
        return r.fail("expected a transaction");
    }
    if last_holds {
        add(Part::Aux);
    }
    out.extend(subs.into_iter().flatten());
    Ok(())
}

/// Reads the inputs a top level transaction and its sub transactions name.
pub fn tx_inputs(raw: &[u8]) -> Result<TxInputs> {
    let mut r = Reader::new(raw);
    let len = r.array()?;
    let mut out = TxInputs::default();
    let mut first = true;
    r.each(len, |r| {
        if !std::mem::take(&mut first) {
            return r.skip();
        }
        body(r, |r, key| {
            match key {
                0 => out.spend = r.txins()?,
                13 => out.collateral = r.txins()?,
                18 => out.reference = r.txins()?,
                23 => {
                    let len = r.array()?;
                    let mut subs = Vec::new();
                    r.each(len, |r| sub_tx(r).map(|s| subs.push(s)))?;
                    out.subs = subs;
                }
                _ => return Ok(false),
            }
            Ok(true)
        })
    })?;
    match first {
        true => r.fail("expected a transaction body"),
        false => Ok(out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(tag: bool, byte: u8, ix: u8) -> Vec<u8> {
        let mut out = Vec::new();
        if tag {
            out.extend([0xd9, 0x01, 0x02]);
        }
        out.extend([0x82, 0x58, 0x20]);
        out.extend([byte; 32]);
        out.push(ix);
        out
    }

    #[test]
    fn txin_reads_a_tagged_input_and_refuses_a_short_id() {
        assert_eq!(txin(&input(true, 7, 3)), Ok(TxIn { tx: [7; 32], ix: 3 }));
        let short = [0x82, 0x41, 0x00, 0x00];
        assert_eq!(
            txin(&short).unwrap_err().what,
            "expected a 32 byte transaction id"
        );
    }

    #[test]
    fn tx_inputs_reads_tagged_sets_and_sub_bodies() {
        let shared = input(false, 1, 0);
        let own = input(false, 2, 1);
        let mut tx = vec![0x84, 0xa3, 0x00, 0xd9, 0x01, 0x02, 0x81];
        tx.extend(&shared);
        tx.extend([0x02, 0x00, 0x17, 0x81, 0x83, 0xa2, 0x00, 0x82]);
        tx.extend(&shared);
        tx.extend(&own);
        tx.extend([0x12, 0x80, 0xa0, 0xf6, 0xa0, 0xf5, 0xf6]);
        let read = tx_inputs(&tx).unwrap();
        assert_eq!(read.spend, vec![TxIn { tx: [1; 32], ix: 0 }]);
        assert_eq!(read.subs.len(), 1);
        assert_eq!(read.subs[0].spend.len(), 2);
        assert_eq!(
            read.child_only().collect::<Vec<_>>(),
            vec![&TxIn { tx: [2; 32], ix: 1 }]
        );
    }

    fn fields(names: &[&str]) -> BTreeSet<Field> {
        names.iter().map(|n| n.parse().unwrap()).collect()
    }

    #[test]
    fn tx_fields_names_no_auxiliary_data_where_it_is_null() {
        // [{0: [], 2: 0}, {}, true, null]
        let tx = [0x84, 0xa2, 0x00, 0x80, 0x02, 0x00, 0xa0, 0xf5, 0xf6];
        assert_eq!(tx_fields(&tx), Ok(fields(&["top.k0", "top.k2"])));
    }

    #[test]
    fn tx_fields_reads_the_auxiliary_data_of_a_transaction_without_its_validity_flag() {
        // [{0: []}, {}, {}] and [{0: []}, {}, null]
        let held = [0x83, 0xa1, 0x00, 0x80, 0xa0, 0xa0];
        assert_eq!(tx_fields(&held), Ok(fields(&["top.k0", "top.aux"])));
        let null = [0x83, 0xa1, 0x00, 0x80, 0xa0, 0xf6];
        assert_eq!(tx_fields(&null), Ok(fields(&["top.k0"])));
    }

    #[test]
    fn tx_fields_reads_both_levels_and_the_auxiliary_data_of_each() {
        // [{0: [], 23: [[{16: 0}, {0: []}, {}]]}, {5: []}, true, {}]
        let tx = [
            0x84, 0xa2, 0x00, 0x80, 0x17, 0x81, 0x83, 0xa1, 0x10, 0x00, 0xa1, 0x00, 0x80, 0xa0,
            0xa1, 0x05, 0x80, 0xf5, 0xa0,
        ];
        let expected = fields(&[
            "top.k0", "top.k23", "top.w5", "top.aux", "sub.k16", "sub.w0", "sub.aux",
        ]);
        assert_eq!(tx_fields(&tx), Ok(expected));
    }

    #[test]
    fn a_field_reads_back_as_it_is_written() {
        for name in ["top.k7", "sub.w5", "top.aux", "sub.aux"] {
            assert_eq!(name.parse::<Field>().unwrap().to_string(), name);
        }
        for bad in ["top", "mid.k7", "top.x7", "top.k", "top.kx"] {
            assert!(bad.parse::<Field>().is_err(), "{bad}");
        }
    }

    #[test]
    fn sub_only_leaves_out_every_input_the_top_level_names() {
        let i = |n: u8| TxIn { tx: [n; 32], ix: 0 };
        let inputs = TxInputs {
            spend: vec![i(1)],
            collateral: vec![i(2)],
            reference: vec![i(3)],
            subs: vec![
                SubInputs {
                    spend: vec![i(1), i(2), i(4)],
                    reference: vec![i(3)],
                },
                SubInputs {
                    spend: vec![],
                    reference: vec![i(5), i(4)],
                },
            ],
        };
        assert_eq!(inputs.sub_only(), BTreeSet::from([&i(4), &i(5)]));
    }

    const ERA_HISTORY: &str = concat!(
        "9f838400000000840000000084190438194e20830018d8810018d88384000000008400000000841954601903e8830019",
        "195081001919508384000000008400000000841954601903e88300191950810019195083840000000084000000008419",
        "54601903e8830019195081001919508384000000008400000000841954601903e8830019195081001919508384000000",
        "008400000000841954601903e8830019195081001919508384000000008400000000841954601903e883001919508100",
        "191950838400000000841b12e28863e25a00001a0014c3a0183f193b10851954601903e883001919508100191950185a",
        "ff",
    );

    #[test]
    fn era_history_reads_each_era_bound_and_its_epoch_and_slot_lengths() {
        let raw = hex::decode(ERA_HISTORY).unwrap();
        let eras = era_history(&raw).unwrap();
        let zero = EraBound { slot: 0, epoch: 0 };
        assert_eq!(eras.len(), 8);
        assert_eq!(
            eras[0],
            NodeEra {
                start: zero,
                end: zero,
                epoch_size: 1080,
                slot_length_ms: 20000,
            }
        );
        assert_eq!(
            eras[7],
            NodeEra {
                start: zero,
                end: EraBound {
                    slot: 1360800,
                    epoch: 63,
                },
                epoch_size: 21600,
                slot_length_ms: 1000,
            }
        );
        let short = &raw[..raw.len() - 2];
        assert!(era_history(short).is_err());
    }

    fn decoded(text: &str) -> Data {
        data(&hex::decode(text).unwrap()).unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    #[test]
    fn data_reads_each_encoding_of_a_value_alike() {
        let same = [
            ("d8799f0102ff", "d87982 0102"),
            ("d905009f40ff", "d9050081 40"),
            ("d866820282 0203", "d87b82 0203"),
            ("c24101", "01"),
            ("c3420001", "21"),
            ("1b0000000000000005", "05"),
            ("5f4201024103ff", "43010203"),
            ("bf0102ff", "a1 0102"),
        ];
        for (one, other) in same {
            let (one, other) = (one.replace(' ', ""), other.replace(' ', ""));
            assert_eq!(decoded(&one), decoded(&other), "{one} {other}");
        }
    }

    #[test]
    fn data_tells_different_values_apart() {
        let differ = [
            ("d87980", "d87a80"),
            ("80", "a0"),
            ("01", "20"),
            ("4101", "4102"),
            ("820102", "820201"),
            ("c249010000000000000000", "c349010000000000000000"),
        ];
        for (one, other) in differ {
            assert_ne!(decoded(one), decoded(other), "{one} {other}");
        }
        assert_eq!(
            decoded("d87c9f2043ab01ffc249010000000000000000ff").to_string(),
            "C3[-1,h'ab01ff',18446744073709551616]"
        );
    }

    #[test]
    fn data_refuses_an_item_no_data_value_holds() {
        for raw in ["6161", "f5", "d81801", "d86682014101", "c201"] {
            assert!(data(&hex::decode(raw).unwrap()).is_err(), "{raw}");
        }
    }
}
