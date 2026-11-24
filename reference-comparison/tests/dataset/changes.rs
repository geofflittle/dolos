//! The changes that turn one normal form encoding into another, read item by
//! item.

use std::collections::BTreeSet;

use ledger_oracle_judge::normalize::normalize;
use pallas_codec::minicbor::Decoder;
use pallas_codec::minicbor::data::Type;

/// One CBOR item of a normal form encoding.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Item {
    /// An item with no items inside, as its bytes.
    Leaf(Vec<u8>),
    Array(Vec<Item>),
    Map(Vec<(Item, Item)>),
    Tag(u64, Box<Item>),
}

/// The tag of a CBOR set, which the Conway CDDL makes optional.
const SET: u64 = 258;

/// A change from the first encoding to the second that keeps every other
/// item in place.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    /// A set tag is written where none was.
    SetTagAdded,
    /// A set tag is left out where one was.
    SetTagDropped,
    /// A tag is written with another number.
    TagNumber,
    /// A fraction is written under its tag where another tag was.
    FractionTag,
    /// An embedded encoding is written under its tag where another tag was.
    EmbeddedTag,
    /// Items at the end of an array are left out.
    ItemsDropped,
    /// Entries of a map with the same key are written as one.
    DuplicateKeysMerged,
    /// Entries of a map are left out.
    EntriesDropped,
    /// The entries of a map are in another order.
    EntriesReordered,
    /// The items of an array are in another order.
    ItemsReordered,
    /// Bytes after the first item of an embedded encoding are left out.
    EmbeddedBytesDropped,
    /// Nulls are written at the end of an array.
    NullsAdded,
    /// An output written as a legacy array is written as a map.
    OutputAsMap,
    /// A transaction is written with the valid flag `true` as its third item.
    ValidFlagAdded,
    /// A pool registration is written with a null BLS key as its fourth item.
    NullBlsKeyAdded,
    /// Key credential guards are written as their key hashes.
    GuardKeyHashes,
    /// A coin is written as a value of that coin and no assets.
    EmptyAssetsAdded,
    /// A Byron address is written with another encoding of its payload.
    ByronPayloadForm,
    /// An embedded encoding is written as another form of the same item.
    EmbeddedForm {
        place: Place,
        from: Vec<u8>,
        to: Vec<u8>,
    },
    /// An embedded encoding is written in the form the ledger writes for the
    /// bytes read.
    EmbeddedLedgerForm,
}

/// A difference from the first encoding to the second that no listed change
/// makes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Unexplained {
    /// The item in this place is written as another item.
    Differs { place: Place, from: Item, to: Item },
    /// The map in this place is written with an entry of a key it lacks.
    KeyAdded { place: Place, key: Item },
}

/// The listed changes from one encoding to another and every difference no
/// listed change makes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comparison {
    pub changes: BTreeSet<Change>,
    pub unexplained: BTreeSet<Unexplained>,
}

impl Comparison {
    /// The changes, when every difference is a listed change.
    pub fn explained(self) -> Result<BTreeSet<Change>, BTreeSet<Unexplained>> {
        match self.unexplained.is_empty() {
            true => Ok(self.changes),
            false => Err(self.unexplained),
        }
    }

    fn add(&mut self, other: Comparison) {
        self.changes.extend(other.changes);
        self.unexplained.extend(other.unexplained);
    }
}

/// The comparison of `from` with `to` in this place, kept apart from any
/// other.
fn trial(from: &Item, to: &Item, place: &Place) -> Comparison {
    let mut found = Comparison::default();
    compare(from, to, place, &mut found);
    found
}

/// The position of an item in an encoding: the key of the nearest map entry
/// holding it, and its positions in the arrays inside that entry's value.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Place {
    key: Option<Item>,
    positions: Vec<usize>,
}

impl Place {
    /// The place of the value of a map entry with this key.
    pub fn under(key: &Item) -> Place {
        Place {
            key: Some(key.clone()),
            positions: Vec::new(),
        }
    }

    /// The place of the item at this position of an array in this place.
    pub fn at(&self, position: usize) -> Place {
        let mut place = self.clone();
        place.positions.push(position);
        place
    }
}

/// The heads that one encoding of an item writes another way than another
/// encoding of it, each by its path in the item with its first byte in each.
pub type Reform = BTreeSet<(Vec<usize>, u8, u8)>;

/// The heads written another way, when both encodings are one whole item
/// with items in the same paths and some head differs.
pub fn reform(from: &[u8], to: &[u8]) -> Option<Reform> {
    let (from, to) = (heads(from)?, heads(to)?);
    let same_paths = from.len() == to.len() && from.iter().zip(&to).all(|(f, t)| f.0 == t.0);
    let reform: Reform = from
        .into_iter()
        .zip(to)
        .filter(|(f, t)| f.1 != t.1)
        .map(|((path, f), (_, t))| (path, f, t))
        .collect();
    (same_paths && !reform.is_empty()).then_some(reform)
}

/// The first byte of each item's head in one whole encoding, by the item's
/// path.
fn heads(bytes: &[u8]) -> Option<Vec<(Vec<usize>, u8)>> {
    let mut d = Decoder::new(bytes);
    let mut found = Vec::new();
    head(&mut d, &mut Vec::new(), &mut found)?;
    (d.position() == bytes.len()).then_some(found)
}

fn head(
    d: &mut Decoder<'_>,
    path: &mut Vec<usize>,
    found: &mut Vec<(Vec<usize>, u8)>,
) -> Option<()> {
    found.push((path.clone(), *d.input().get(d.position())?));
    let len = match d.datatype().ok()? {
        Type::Array | Type::ArrayIndef => d.array().ok()?,
        Type::Map | Type::MapIndef => d.map().ok()?.map(|n| n.saturating_mul(2)),
        Type::Tag => d.tag().ok().map(|_| Some(1))?,
        _ => return d.skip().ok(),
    };
    let mut i = 0;
    loop {
        match len {
            Some(n) if i == n => return Some(()),
            None if d.datatype().ok()? == Type::Break => {
                d.set_position(d.position() + 1);
                return Some(());
            }
            _ => {}
        }
        path.push(usize::try_from(i).ok()?);
        head(d, path, found)?;
        path.pop();
        i += 1;
    }
}

/// The encoding of a null.
const NULL: u8 = 0xf6;

/// The tag of a fraction, which the normal form writes reduced.
const FRACTION: u64 = 30;

/// The tag of an embedded encoding.
const EMBEDDED: u64 = 24;

/// The change of a tag written as `written` where another number was.
fn retagged(written: u64) -> Change {
    match written {
        FRACTION => Change::FractionTag,
        EMBEDDED => Change::EmbeddedTag,
        _ => Change::TagNumber,
    }
}

/// The items of a normal form encoding.
pub fn items(cbor: &[u8]) -> Result<Item, String> {
    let mut d = Decoder::new(cbor);
    let item = item(&mut d)?;
    match d.position() == cbor.len() {
        true => Ok(item),
        false => Err(format!("bytes after the item at {}", d.position())),
    }
}

fn item(d: &mut Decoder<'_>) -> Result<Item, String> {
    let at = d.position();
    match d.datatype().map_err(|e| e.to_string())? {
        Type::Array => {
            let len = d.array().map_err(|e| e.to_string())?;
            let len = len.ok_or_else(|| format!("an indefinite array at {at}"))?;
            (0..len)
                .map(|_| item(d))
                .collect::<Result<_, _>>()
                .map(Item::Array)
        }
        Type::Map => {
            let len = d.map().map_err(|e| e.to_string())?;
            let len = len.ok_or_else(|| format!("an indefinite map at {at}"))?;
            (0..len)
                .map(|_| Ok((item(d)?, item(d)?)))
                .collect::<Result<_, String>>()
                .map(Item::Map)
        }
        Type::Tag => {
            let tag = d.tag().map_err(|e| e.to_string())?;
            Ok(Item::Tag(tag.as_u64(), Box::new(item(d)?)))
        }
        _ => {
            d.skip().map_err(|e| e.to_string())?;
            Ok(Item::Leaf(d.input()[at..d.position()].to_vec()))
        }
    }
}

/// The changes that turn `from` into `to` and every difference no listed
/// change makes.
pub fn changes(from: &Item, to: &Item) -> Comparison {
    trial(from, to, &Place::default())
}

fn compare(from: &Item, to: &Item, place: &Place, found: &mut Comparison) {
    match (from, to) {
        _ if from == to => {}
        (Item::Tag(EMBEDDED, f), Item::Tag(EMBEDDED, t)) if embedded_prefix(f, t) => {
            found.changes.insert(Change::EmbeddedBytesDropped);
        }
        (Item::Tag(EMBEDDED, f), Item::Tag(EMBEDDED, t)) if same_item(f, t) => {
            found.changes.insert(Change::EmbeddedForm {
                place: place.clone(),
                from: byte_content(f).unwrap_or_default().to_vec(),
                to: byte_content(t).unwrap_or_default().to_vec(),
            });
        }
        (Item::Tag(a, f), Item::Tag(b, t)) if a == b => compare(f, t, place, found),
        (Item::Tag(a, f), Item::Tag(b, t))
            if (*a == FRACTION || *b == FRACTION) && same_ratio(f, t) =>
        {
            found.changes.insert(retagged(*b));
        }
        (Item::Tag(SET, f), _) => {
            found.changes.insert(Change::SetTagDropped);
            compare(f, to, place, found);
        }
        (_, Item::Tag(SET, t)) => {
            found.changes.insert(Change::SetTagAdded);
            compare(from, t, place, found);
        }
        (Item::Tag(_, f), Item::Tag(b, _)) => {
            found.changes.insert(retagged(*b));
            compare(&Item::Tag(*b, f.clone()), to, place, found);
        }
        (Item::Array(f), Item::Array(t)) if *place == Place::default() && valid_flag(f, t) => {
            found.changes.insert(Change::ValidFlagAdded);
            let rest = [t[0].clone(), t[1].clone(), t[3].clone()];
            compare(from, &Item::Array(rest.into()), place, found);
        }
        (Item::Array(f), Item::Array(t)) if null_bls_key(f, t) => {
            found.changes.insert(Change::NullBlsKeyAdded);
            let rest = t[..3].iter().chain(&t[4..]).cloned().collect();
            compare(from, &Item::Array(rest), place, found);
        }
        (Item::Array(f), Item::Array(t)) if guards(place) && key_hashes(f, t) => {
            found.changes.insert(Change::GuardKeyHashes);
        }
        (Item::Leaf(f), Item::Array(t)) if no_assets(f, t) => {
            found.changes.insert(Change::EmptyAssetsAdded);
        }
        (Item::Leaf(_), Item::Leaf(_)) if same_byron_address(from, to) => {
            found.changes.insert(Change::ByronPayloadForm);
        }
        (Item::Array(f), Item::Array(t)) if t.len() <= f.len() => {
            if t.len() < f.len() {
                found.changes.insert(Change::ItemsDropped);
            }
            let mut in_place = Comparison::default();
            for (i, (a, b)) in f.iter().zip(t).enumerate() {
                compare(a, b, &place.at(i), &mut in_place);
            }
            match in_place.unexplained.is_empty() {
                false if f.len() == t.len() && sorted(f) == sorted(t) => {
                    found.changes.insert(Change::ItemsReordered);
                }
                _ => found.add(in_place),
            }
        }
        (Item::Array(f), Item::Array(t))
            if t[f.len().min(t.len())..]
                .iter()
                .all(|i| *i == Item::Leaf(vec![NULL])) =>
        {
            found.changes.insert(Change::NullsAdded);
            compare(from, &Item::Array(t[..f.len()].to_vec()), place, found);
        }
        (Item::Map(f), Item::Map(t)) => compare_maps(f, t, place, found),
        (Item::Array(f), Item::Map(t)) if output_fields(f, t).is_some() => {
            found.changes.insert(Change::OutputAsMap);
            for (a, b, at) in output_fields(f, t).unwrap_or_default() {
                compare(a, b, &at, found);
            }
        }
        _ => {
            found.unexplained.insert(Unexplained::Differs {
                place: place.clone(),
                from: from.clone(),
                to: to.clone(),
            });
        }
    }
}

/// The encoding of `true`.
const TRUE: u8 = 0xf5;

/// The tag of a pool registration certificate.
const POOL_REGISTRATION: u8 = 3;

/// The key of the guards of a transaction body.
const GUARDS: u8 = 14;

/// Whether a transaction of a body, a witness set and auxiliary data is
/// written with the valid flag `true` before its auxiliary data, which the
/// Dijkstra ledger reads as the same transaction.
fn valid_flag(from: &[Item], to: &[Item]) -> bool {
    from.len() == 3
        && to.len() == 4
        && to[2] == Item::Leaf(vec![TRUE])
        && matches!((&from[0], &from[1]), (Item::Map(_), Item::Map(_)))
}

/// Whether a pool registration of ten items is written with a null BLS key
/// as its fourth item, which the Dijkstra ledger reads as no BLS key.
fn null_bls_key(from: &[Item], to: &[Item]) -> bool {
    let registration = Item::Leaf(vec![POOL_REGISTRATION]);
    from.len() == 10
        && to.len() == 11
        && from[0] == registration
        && to[0] == registration
        && to[3] == Item::Leaf(vec![NULL])
}

/// Whether an item in this place is a set of guards: the whole value read,
/// or the value of a transaction body's guards key.
fn guards(place: &Place) -> bool {
    place.positions.is_empty()
        && place
            .key
            .as_ref()
            .is_none_or(|key| *key == Item::Leaf(vec![GUARDS]))
}

/// Whether a coin is written as an array of that coin and an empty map,
/// which the Conway ledger reads as a value of that coin and no assets.
fn no_assets(coin: &[u8], value: &[Item]) -> bool {
    let unsigned = coin.first().is_some_and(|head| head >> 5 == 0);
    unsigned && *value == [Item::Leaf(coin.to_vec()), Item::Map(Vec::new())]
}

/// Whether two byte strings each hold a Byron address, an embedded payload
/// and its checksum, whose payloads have the same normal form, which the
/// ledger reads as the same address.
fn same_byron_address(from: &Item, to: &Item) -> bool {
    let payload = |item: &Item| match items(byte_content(item)?).ok()? {
        Item::Array(parts) => match &parts[..] {
            [payload @ Item::Tag(EMBEDDED, _), Item::Leaf(_)] => Some(payload.clone()),
            _ => None,
        },
        _ => None,
    };
    match (payload(from), payload(to)) {
        (Some(Item::Tag(_, f)), Some(Item::Tag(_, t))) => same_item(&f, &t),
        _ => false,
    }
}

/// Whether every item is a key hash credential written as its key hash,
/// which the Dijkstra ledger reads as the same guard.
fn key_hashes(from: &[Item], to: &[Item]) -> bool {
    let key_hash = |credential: &Item, hash: &Item| match credential {
        Item::Array(c) => c.len() == 2 && c[0] == Item::Leaf(vec![0]) && c[1] == *hash,
        _ => false,
    };
    !from.is_empty() && from.len() == to.len() && from.iter().zip(to).all(|(c, h)| key_hash(c, h))
}

/// The content of a byte string item.
fn byte_content(item: &Item) -> Option<&[u8]> {
    match item {
        Item::Leaf(bytes) => Decoder::new(bytes).bytes().ok(),
        _ => None,
    }
}

/// Whether the second embedded encoding is the first with bytes left out
/// at its end.
fn embedded_prefix(from: &Item, to: &Item) -> bool {
    match (byte_content(from), byte_content(to)) {
        (Some(f), Some(t)) => t.len() < f.len() && f.starts_with(t),
        _ => false,
    }
}

/// Whether two embedded encodings are each one whole item with the same
/// normal form.
fn same_item(from: &Item, to: &Item) -> bool {
    match (byte_content(from), byte_content(to)) {
        (Some(f), Some(t)) => normalize(f).is_ok_and(|f| normalize(t).is_ok_and(|t| f == t)),
        _ => false,
    }
}

/// The numerator and denominator of a two number array.
fn ratio(item: &Item) -> Option<(u128, u128)> {
    let number = |item: &Item| match item {
        Item::Leaf(bytes) => Decoder::new(bytes).u64().ok().map(u128::from),
        _ => None,
    };
    match item {
        Item::Array(pair) if pair.len() == 2 => Some((number(&pair[0])?, number(&pair[1])?)),
        _ => None,
    }
}

/// Whether two two number arrays are the same fraction.
fn same_ratio(from: &Item, to: &Item) -> bool {
    match (ratio(from), ratio(to)) {
        (Some((a, b)), Some((c, d))) => a * d == c * b,
        _ => false,
    }
}

/// Each item of a legacy output array beside the field of an output map
/// that holds it and that field's place, when the map holds the array's
/// address, value and datum hash and nothing else.
fn output_fields<'a>(
    array: &'a [Item],
    map: &'a [(Item, Item)],
) -> Option<Vec<(&'a Item, &'a Item, Place)>> {
    let field = |n: u8| {
        map.iter()
            .find(|(k, _)| *k == Item::Leaf(vec![n]))
            .map(|(k, v)| (v, Place::under(k)))
    };
    if !(2..=3).contains(&array.len()) || map.len() != array.len() {
        return None;
    }
    let (address, at_address) = field(0)?;
    let (value, at_value) = field(1)?;
    let mut fields = vec![
        (&array[0], address, at_address),
        (&array[1], value, at_value),
    ];
    if let Some(hash) = array.get(2) {
        match field(2)? {
            (Item::Array(datum), at) if datum.len() == 2 && datum[0] == Item::Leaf(vec![0]) => {
                fields.push((hash, &datum[1], at.at(1)));
            }
            _ => return None,
        }
    }
    Some(fields)
}

fn sorted(items: &[Item]) -> Vec<String> {
    let mut texts: Vec<String> = items.iter().map(|i| format!("{i:?}")).collect();
    texts.sort();
    texts
}

fn compare_maps(from: &[(Item, Item)], to: &[(Item, Item)], place: &Place, found: &mut Comparison) {
    let mut keys: Vec<&Item> = Vec::new();
    for (key, _) in from {
        match keys.contains(&key) {
            true => {
                found.changes.insert(Change::DuplicateKeysMerged);
            }
            false => keys.push(key),
        }
    }
    let mut read_keys: Vec<(&Item, &Item, &Item)> = Vec::new();
    for (key, value) in to {
        let read = match keys.iter().find(|k| **k == key) {
            Some(read) => *read,
            None => {
                let matched = keys
                    .iter()
                    .filter(|k| !to.iter().any(|(t, _)| t == **k))
                    .map(|k| (*k, trial(k, key, place)))
                    .find(|(_, inner)| inner.unexplained.is_empty());
                let Some((read, inner)) = matched else {
                    found.unexplained.insert(Unexplained::KeyAdded {
                        place: place.clone(),
                        key: key.clone(),
                    });
                    continue;
                };
                found.add(inner);
                read
            }
        };
        read_keys.push((read, key, value));
    }
    if read_keys.len() < keys.len() {
        found.changes.insert(Change::EntriesDropped);
    }
    let read: Vec<&Item> = read_keys.iter().map(|(read, _, _)| *read).collect();
    let kept: Vec<&Item> = keys.iter().copied().filter(|k| read.contains(k)).collect();
    if kept != read {
        found.changes.insert(Change::EntriesReordered);
    }
    for (read_key, key, value) in read_keys {
        let mut tried = Comparison::default();
        for (_, candidate) in from.iter().filter(|(k, _)| k == read_key) {
            tried = trial(candidate, value, &Place::under(key));
            if tried.unexplained.is_empty() {
                break;
            }
        }
        found.add(tried);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Item {
        items(&hex::decode(text).expect("hex")).expect("items")
    }

    fn changed(from: &str, to: &str) -> Result<BTreeSet<Change>, BTreeSet<Unexplained>> {
        changes(&read(from), &read(to)).explained()
    }

    fn hash(byte: &str) -> String {
        format!("581c{}", byte.repeat(28))
    }

    #[test]
    fn the_same_items_have_no_change() {
        assert_eq!(changed("a201020304", "a201020304"), Ok(BTreeSet::new()));
    }

    #[test]
    fn a_changed_leaf_is_no_listed_change() {
        assert!(changed("820102", "820103").is_err());
    }

    #[test]
    fn a_merged_duplicate_key_is_told() {
        assert_eq!(
            changed("a3010201030304", "a201030304"),
            Ok([Change::DuplicateKeysMerged].into())
        );
    }

    #[test]
    fn a_set_tag_and_a_tag_number_are_told() {
        assert_eq!(
            changed("828101c4820101", "82d901028101d81e820101"),
            Ok([Change::SetTagAdded, Change::FractionTag].into())
        );
    }

    #[test]
    fn reordered_entries_and_items_are_told() {
        assert_eq!(
            changed("a203040102", "a201020304"),
            Ok([Change::EntriesReordered].into())
        );
        assert_eq!(
            changed("820201", "820102"),
            Ok([Change::ItemsReordered].into())
        );
    }

    #[test]
    fn a_fraction_under_another_tag_is_told_by_its_ratio() {
        assert_eq!(
            changed("c4820204", "d81e820102"),
            Ok([Change::FractionTag].into())
        );
        assert!(changed("c4820204", "d81e820103").is_err());
    }

    #[test]
    fn an_embedded_encoding_under_another_tag_is_told_apart_from_a_fraction() {
        assert_ne!(
            changed("c54101", "d8184101"),
            changed("c4820204", "d81e820102")
        );
        assert_ne!(changed("c54101", "d8184101"), changed("c54101", "c64101"));
    }

    #[test]
    fn bytes_left_out_of_an_embedded_encoding_are_told() {
        assert_eq!(
            changed("d8184200ff", "d8184100"),
            Ok([Change::EmbeddedBytesDropped].into())
        );
        assert!(changed("d8184100", "d8184200ff").is_err());
    }

    #[test]
    fn another_item_in_an_embedded_encoding_is_no_listed_change() {
        assert!(changed("d8184100", "d8184101").is_err());
        assert!(changed("d818449f0102ff", "d81843820103").is_err());
        assert!(changed("d81843820102", "d8184201ff").is_err());
    }

    #[test]
    fn another_form_of_an_embedded_item_is_told_with_both_forms() {
        assert_eq!(
            changed("d818449f0102ff", "d81843820102"),
            Ok([Change::EmbeddedForm {
                place: Place::default(),
                from: vec![0x9f, 0x01, 0x02, 0xff],
                to: vec![0x82, 0x01, 0x02],
            }]
            .into())
        );
    }

    #[test]
    fn another_form_of_an_embedded_item_is_told_with_its_place() {
        assert_eq!(
            changed("a1028201d818449f0102ff", "a1028201d81843820102"),
            Ok([Change::EmbeddedForm {
                place: Place::under(&Item::Leaf(vec![0x02])).at(1),
                from: vec![0x9f, 0x01, 0x02, 0xff],
                to: vec![0x82, 0x01, 0x02],
            }]
            .into())
        );
    }

    #[test]
    fn another_form_of_an_item_embedded_under_another_tag_is_told_with_both_changes() {
        assert_eq!(
            changed("c5449f0102ff", "d81843820102"),
            Ok([
                Change::EmbeddedTag,
                Change::EmbeddedForm {
                    place: Place::default(),
                    from: vec![0x9f, 0x01, 0x02, 0xff],
                    to: vec![0x82, 0x01, 0x02],
                }
            ]
            .into())
        );
        assert!(changed("c5449f0102ff", "d81843820103").is_err());
    }

    #[test]
    fn encodings_with_items_in_other_paths_or_the_same_heads_have_no_reform() {
        let bytes = |h: &str| hex::decode(h).expect("hex");
        assert_eq!(reform(&bytes("820102"), &bytes("820102")), None);
        assert_eq!(reform(&bytes("5f420102ff"), &bytes("5f41014102ff")), None);
        assert_eq!(reform(&bytes("9f0102ff"), &bytes("83010203")), None);
        assert_eq!(reform(&bytes("9f0102ff"), &bytes("820102ff")), None);
    }

    #[test]
    fn a_head_written_another_way_is_a_reform_at_its_path() {
        let bytes = |h: &str| hex::decode(h).expect("hex");
        assert_eq!(
            reform(&bytes("9f0102ff"), &bytes("820102")),
            Some([(vec![], 0x9f, 0x82)].into())
        );
        assert_eq!(
            reform(&bytes("82019f02ff"), &bytes("9802018102")),
            Some([(vec![], 0x82, 0x98), (vec![1], 0x9f, 0x81)].into())
        );
    }

    #[test]
    fn nulls_written_at_the_end_of_an_array_are_told() {
        assert_eq!(changed("8101", "8201f6"), Ok([Change::NullsAdded].into()));
        assert!(changed("8101", "820102").is_err());
    }

    #[test]
    fn a_key_written_with_fewer_items_is_told() {
        assert_eq!(
            changed("a18300010205", "a182000105"),
            Ok([Change::ItemsDropped].into())
        );
        assert!(changed("a182000105", "a182000205").is_err());
    }

    #[test]
    fn a_legacy_output_written_as_a_map_is_told() {
        assert_eq!(
            changed("834161014102", "a300416101010282004102"),
            Ok([Change::OutputAsMap].into())
        );
        assert!(changed("834161014102", "a300416101010282014102").is_err());
        assert!(changed("82416101", "a30041610101030a").is_err());
    }

    #[test]
    fn dropped_array_items_are_told() {
        assert_eq!(
            changed("83010203", "820102"),
            Ok([Change::ItemsDropped].into())
        );
        assert!(changed("820102", "83010203").is_err());
    }

    #[test]
    fn two_differing_leaves_give_two_unexplained_differences() {
        let differs = |at: usize, from: u8, to: u8| Unexplained::Differs {
            place: Place::default().at(at),
            from: Item::Leaf(vec![from]),
            to: Item::Leaf(vec![to]),
        };
        assert_eq!(
            changed("820102", "820304"),
            Err([differs(0, 1, 3), differs(1, 2, 4)].into())
        );
    }

    #[test]
    fn an_added_key_and_a_differing_value_are_both_unexplained() {
        assert_eq!(
            changed("a10102", "a201030405"),
            Err([
                Unexplained::Differs {
                    place: Place::under(&Item::Leaf(vec![1])),
                    from: Item::Leaf(vec![2]),
                    to: Item::Leaf(vec![3]),
                },
                Unexplained::KeyAdded {
                    place: Place::default(),
                    key: Item::Leaf(vec![4]),
                },
            ]
            .into())
        );
    }

    #[test]
    fn the_valid_flag_of_a_transaction_is_named() {
        assert_eq!(
            changed("83a0a0f6", "84a0a0f5f6"),
            Ok([Change::ValidFlagAdded].into())
        );
        assert!(changed("83a0a0f6", "84a0a0f4f6").is_err());
        assert!(changed("a10083a0a0f6", "a10084a0a0f5f6").is_err());
    }

    #[test]
    fn a_null_bls_key_of_a_pool_registration_is_named() {
        assert_eq!(
            changed("8a03010203040506070809", "8b030102f603040506070809"),
            Ok([Change::NullBlsKeyAdded].into())
        );
        assert!(changed("8a03010203040506070809", "8b030102f603040506070808").is_err());
        assert!(changed("8a04010203040506070809", "8b040102f603040506070809").is_err());
        assert!(changed("8a03010203040506070809", "8b030102050304050607080a").is_err());
    }

    #[test]
    fn a_coin_written_with_no_assets_is_named() {
        assert_eq!(
            changed("05", "8205a0"),
            Ok([Change::EmptyAssetsAdded].into())
        );
        assert!(changed("05", "8206a0").is_err());
        assert!(changed("05", "8205a10102").is_err());
        assert!(changed("24", "8224a0").is_err());
    }

    #[test]
    fn a_byron_address_written_with_another_payload_form_is_named() {
        let address = |head: &str, root: &str, checksum: &str| {
            let payload = format!("{head}581c{}a000", root.repeat(28));
            let embedded = format!("d818{:02x}{:02x}{payload}", 0x58, payload.len() / 2);
            let address = format!("82{embedded}1a{checksum}");
            format!("58{:02x}{address}", address.len() / 2)
        };
        let canonical = address("83", "11", "4e47c474");
        assert_eq!(
            canonical,
            "582b82d818582183581c11111111111111111111111111111111111111111111111111111111a0001a4e47c474"
        );
        assert_eq!(
            changed(&canonical, &address("9803", "11", "21b3cd0d")),
            Ok([Change::ByronPayloadForm].into())
        );
        assert!(changed(&canonical, &address("9803", "12", "21b3cd0d")).is_err());
    }

    #[test]
    fn guards_written_as_key_hashes_are_named() {
        let (a, b) = (hash("aa"), hash("bb"));
        assert_eq!(
            changed(&format!("d90102828200{a}8200{b}"), &format!("82{a}{b}")),
            Ok([Change::SetTagDropped, Change::GuardKeyHashes].into())
        );
        assert_eq!(
            changed(&format!("a10e818200{a}"), &format!("a10e81{a}")),
            Ok([Change::GuardKeyHashes].into())
        );
        assert!(changed(&format!("828200{a}8201{b}"), &format!("82{a}{b}")).is_err());
        assert!(changed(&format!("a105818200{a}"), &format!("a10581{a}")).is_err());
        assert!(changed(&format!("8200{a}"), &a).is_err());
    }
}
