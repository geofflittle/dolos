//! The changes that turn one normal form encoding into another, read item by
//! item.

use std::collections::BTreeSet;

use pallas_codec::minicbor::Decoder;
use pallas_codec::minicbor::data::Type;

/// One CBOR item of a normal form encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    /// A set tag is written where none was.
    SetTagAdded,
    /// A set tag is left out where one was.
    SetTagDropped,
    /// A tag is written with another number.
    TagNumber,
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
}

/// The encoding of a null.
const NULL: u8 = 0xf6;

/// The tag of a fraction, which the normal form writes reduced.
const FRACTION: u64 = 30;

/// The tag of an embedded encoding.
const EMBEDDED: u64 = 24;

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

/// The changes that turn `from` into `to`, or the first difference no
/// listed change makes.
pub fn changes(from: &Item, to: &Item) -> Result<BTreeSet<Change>, String> {
    let mut found = BTreeSet::new();
    compare(from, to, &mut found)?;
    Ok(found)
}

fn compare(from: &Item, to: &Item, found: &mut BTreeSet<Change>) -> Result<(), String> {
    match (from, to) {
        _ if from == to => Ok(()),
        (Item::Tag(EMBEDDED, f), Item::Tag(EMBEDDED, t)) if embedded_prefix(f, t) => {
            found.insert(Change::EmbeddedBytesDropped);
            Ok(())
        }
        (Item::Tag(a, f), Item::Tag(b, t)) if a == b => compare(f, t, found),
        (Item::Tag(a, f), Item::Tag(b, t))
            if (*a == FRACTION || *b == FRACTION) && same_ratio(f, t) =>
        {
            found.insert(Change::TagNumber);
            Ok(())
        }
        (Item::Tag(SET, f), _) => {
            found.insert(Change::SetTagDropped);
            compare(f, to, found)
        }
        (_, Item::Tag(SET, t)) => {
            found.insert(Change::SetTagAdded);
            compare(from, t, found)
        }
        (Item::Tag(_, f), Item::Tag(_, t)) => {
            found.insert(Change::TagNumber);
            compare(f, t, found)
        }
        (Item::Array(f), Item::Array(t)) if t.len() <= f.len() => {
            if t.len() < f.len() {
                found.insert(Change::ItemsDropped);
            }
            let mut inner = BTreeSet::new();
            let in_place = f
                .iter()
                .zip(t)
                .try_for_each(|(a, b)| compare(a, b, &mut inner));
            match in_place {
                Ok(()) => {
                    found.extend(inner);
                    Ok(())
                }
                Err(_) if f.len() == t.len() && sorted(f) == sorted(t) => {
                    found.insert(Change::ItemsReordered);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        (Item::Array(f), Item::Array(t))
            if t[f.len().min(t.len())..]
                .iter()
                .all(|i| *i == Item::Leaf(vec![NULL])) =>
        {
            found.insert(Change::NullsAdded);
            compare(
                &Item::Array(f.clone()),
                &Item::Array(t[..f.len()].to_vec()),
                found,
            )
        }
        (Item::Map(f), Item::Map(t)) => compare_maps(f, t, found),
        (Item::Array(f), Item::Map(t)) if output_fields(f, t).is_some() => {
            found.insert(Change::OutputAsMap);
            output_fields(f, t)
                .unwrap_or_default()
                .into_iter()
                .try_for_each(|(a, b)| compare(a, b, found))
        }
        _ => Err(crate::shortened(format!("{from:?} against {to:?}"))),
    }
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
/// that holds it, when the map holds the array's address, value and datum
/// hash and nothing else.
fn output_fields<'a>(
    array: &'a [Item],
    map: &'a [(Item, Item)],
) -> Option<Vec<(&'a Item, &'a Item)>> {
    let field = |n: u8| {
        map.iter()
            .find(|(k, _)| *k == Item::Leaf(vec![n]))
            .map(|(_, v)| v)
    };
    if !(2..=3).contains(&array.len()) || map.len() != array.len() {
        return None;
    }
    let mut fields = vec![(&array[0], field(0)?), (&array[1], field(1)?)];
    if let Some(hash) = array.get(2) {
        match field(2)? {
            Item::Array(datum) if datum.len() == 2 && datum[0] == Item::Leaf(vec![0]) => {
                fields.push((hash, &datum[1]));
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

fn compare_maps(
    from: &[(Item, Item)],
    to: &[(Item, Item)],
    found: &mut BTreeSet<Change>,
) -> Result<(), String> {
    let mut keys: Vec<&Item> = Vec::new();
    for (key, _) in from {
        match keys.contains(&key) {
            true => {
                found.insert(Change::DuplicateKeysMerged);
            }
            false => keys.push(key),
        }
    }
    let mut read_keys: Vec<&Item> = Vec::new();
    for (key, _) in to {
        let read = match keys.iter().find(|k| **k == key) {
            Some(read) => *read,
            None => {
                let (read, inner) = keys
                    .iter()
                    .filter(|k| !to.iter().any(|(t, _)| t == **k))
                    .find_map(|k| {
                        let mut inner = BTreeSet::new();
                        compare(k, key, &mut inner).ok().map(|()| (*k, inner))
                    })
                    .ok_or_else(|| {
                        crate::shortened(format!("an entry with key {key:?} is added"))
                    })?;
                found.extend(inner);
                read
            }
        };
        read_keys.push(read);
    }
    if read_keys.len() < keys.len() {
        found.insert(Change::EntriesDropped);
    }
    let kept: Vec<&Item> = keys
        .iter()
        .copied()
        .filter(|k| read_keys.contains(k))
        .collect();
    if kept != read_keys {
        found.insert(Change::EntriesReordered);
    }
    for ((_, value), read_key) in to.iter().zip(&read_keys) {
        let mut tried = Err(String::new());
        for (_, candidate) in from.iter().filter(|(k, _)| k == *read_key) {
            let mut inner = BTreeSet::new();
            tried = compare(candidate, value, &mut inner).map(|()| inner);
            if tried.is_ok() {
                break;
            }
        }
        found.extend(tried?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(from: &str, to: &str) -> Result<BTreeSet<Change>, String> {
        let read = |h: &str| items(&hex::decode(h).expect("hex")).expect("items");
        changes(&read(from), &read(to))
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
            Ok([Change::SetTagAdded, Change::TagNumber].into())
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
            Ok([Change::TagNumber].into())
        );
        assert!(changed("c4820204", "d81e820103").is_err());
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
}
