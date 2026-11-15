//! Field-mask strategies for gRPC responses.
//!
//! A `google.protobuf.FieldMask` lets a client request a subset of a response's
//! fields. Two strategies for honoring it coexist here, picked per endpoint:
//!
//! - [`apply_mask`] is a *post-hoc projection*: it round-trips an already-built
//!   response through JSON and keeps only the requested paths. It works for any
//!   message but pays the cost of building (and serializing) fields the client
//!   discards, so it suits low-volume Query responses.
//! - [`BlockMask`] is a *source-level projection*: it interprets the mask up
//!   front so the producer can skip building representations the client did not
//!   request. This suits the high-volume Sync block streams, where parsing each
//!   block into its `AnyChainBlock` representation is the dominant cost.

use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Map, Value};

/// The JSON key of the message field a mask path segment names.
fn json_key(segment: &str) -> String {
    let mut parts = segment.split('_');
    let mut key = parts.next().unwrap_or_default().to_string();
    for part in parts {
        let mut chars = part.chars();
        key.extend(chars.next().map(|c| c.to_ascii_uppercase()));
        key.push_str(chars.as_str());
    }
    key
}

/// The part of `value` the path `keys` names, applied to each item of a list.
fn select(value: &Value, keys: &[&str]) -> Option<Value> {
    let Some((key, rest)) = keys.split_first() else {
        return Some(value.clone());
    };
    match value {
        Value::Object(map) => {
            let key = json_key(key);
            let inner = select(map.get(&key)?, rest)?;
            Some(Value::Object(Map::from_iter([(key, inner)])))
        }
        Value::Array(items) => {
            let items: Vec<_> = items.iter().map(|x| select(x, keys)).collect();
            items.iter().any(Option::is_some).then(|| {
                Value::Array(
                    items
                        .into_iter()
                        .map(|x| x.unwrap_or_else(|| json!({})))
                        .collect(),
                )
            })
        }
        _ => None,
    }
}

/// Adds the selection `part` to `whole`.
fn merge(whole: &mut Value, part: Value) {
    match (whole, part) {
        (Value::Object(whole), Value::Object(part)) => {
            for (key, value) in part {
                match whole.get_mut(&key) {
                    Some(existing) => merge(existing, value),
                    None => {
                        whole.insert(key, value);
                    }
                }
            }
        }
        (Value::Array(whole), Value::Array(part)) => {
            for (existing, value) in whole.iter_mut().zip(part) {
                merge(existing, value);
            }
        }
        (whole, part) => *whole = part,
    }
}

/// The message `obj` holding only the fields `paths` name, or all of it when
/// `paths` is empty.
pub fn apply_mask<T>(obj: T, paths: Vec<String>) -> Result<T, serde_json::Error>
where
    T: DeserializeOwned + Serialize,
{
    if paths.is_empty() {
        return Ok(obj);
    }

    let json_value = serde_json::to_value(obj)?;
    let mut new_json = json!({});

    for path in paths {
        let keys: Vec<&str> = path.split('.').collect();
        if let Some(part) = select(&json_value, &keys) {
            merge(&mut new_json, part);
        }
    }
    serde_json::from_value::<T>(new_json)
}

/// Source-level projection of an `AnyChainBlock` from a `FieldMask`.
///
/// `AnyChainBlock` carries two representations of the same block: the raw
/// `native_bytes` and a fully parsed `chain` block. This mask records which of
/// them a producer should populate, so the parsed representation is only built
/// (and the raw bytes only copied) when actually requested.
///
/// Unlike [`apply_mask`], this does not touch a built response — the caller
/// consults the flags while constructing the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockMask {
    pub native_bytes: bool,
    pub chain: bool,
}

impl BlockMask {
    /// A mask that selects every representation.
    pub const fn all() -> Self {
        Self {
            native_bytes: true,
            chain: true,
        }
    }

    /// A mask that selects no representation.
    pub const fn none() -> Self {
        Self {
            native_bytes: false,
            chain: false,
        }
    }

    /// Interprets a `FieldMask`'s paths against `AnyChainBlock`.
    ///
    /// An absent or empty mask (the caller passes `&[]`) selects everything, so
    /// existing clients that send no mask are unaffected. Paths are matched
    /// leniently: a leading `block.`, `apply.` or `undo.` segment (the response
    /// field that holds the block) is tolerated, as is a bare leaf name.
    /// Recognized leaves are `native_bytes` and the parsed chain
    /// (`cardano`/`chain`). Selecting the whole field that holds the block
    /// keeps both.
    pub fn from_paths(paths: &[String]) -> Self {
        if paths.is_empty() {
            return Self::all();
        }

        let mut mask = Self::none();

        for path in paths {
            // Tolerate the response-relative prefix.
            let field = strip_action(path);
            let field = field.strip_prefix("block.").unwrap_or(field);
            // Only the first segment matters for deciding what to populate.
            match field.split('.').next().unwrap_or("") {
                "native_bytes" => mask.native_bytes = true,
                "cardano" | "chain" => mask.chain = true,
                // The whole block (or an unrecognized empty path) keeps both.
                "" | "block" | "apply" | "undo" => return Self::all(),
                _ => {}
            }
        }

        mask
    }
}

/// The path below a leading `apply.` or `undo.` segment, or the path itself.
fn strip_action(path: &str) -> &str {
    path.strip_prefix("apply.")
        .or_else(|| path.strip_prefix("undo."))
        .unwrap_or(path)
}

/// The parts of a watched `AnyChainTx` a `FieldMask` selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxMask {
    /// Whether the answer holds the mapped transaction.
    pub tx: bool,
    /// The representations of the enclosing block the answer holds, where none
    /// omits the block.
    pub block: BlockMask,
}

impl TxMask {
    /// Interprets a `FieldMask`'s paths against `AnyChainTx`, read under the
    /// `apply` or `undo` action of the response or from the transaction itself.
    pub fn from_paths(paths: &[String]) -> Self {
        if paths.is_empty() {
            return Self {
                tx: true,
                block: BlockMask::all(),
            };
        }

        let mut tx = false;
        let mut block_paths = Vec::new();

        for path in paths {
            let field = strip_action(path);
            match field.split('.').next().unwrap_or("") {
                "cardano" | "chain" => tx = true,
                "block" => block_paths.push(field.to_string()),
                "" | "apply" | "undo" => return Self::from_paths(&[]),
                _ => {}
            }
        }

        let block = if block_paths.is_empty() {
            BlockMask::none()
        } else {
            BlockMask::from_paths(&block_paths)
        };

        Self { tx, block }
    }
}

/// The parts of a `TxInMempool` a `FieldMask` selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MempoolTxMask {
    pub r#ref: bool,
    pub native_bytes: bool,
    pub stage: bool,
    pub parsed: bool,
}

impl MempoolTxMask {
    /// A mask that selects every part.
    pub const fn all() -> Self {
        Self {
            r#ref: true,
            native_bytes: true,
            stage: true,
            parsed: true,
        }
    }

    /// Interprets a `FieldMask`'s paths against `TxInMempool`, read under the
    /// `tx` field of the response or from the transaction itself.
    pub fn from_paths(paths: &[String]) -> Self {
        if paths.is_empty() {
            return Self::all();
        }

        let mut mask = Self {
            r#ref: false,
            native_bytes: false,
            stage: false,
            parsed: false,
        };

        for path in paths {
            let field = path.strip_prefix("tx.").unwrap_or(path);
            match field.split('.').next().unwrap_or("") {
                "ref" => mask.r#ref = true,
                "native_bytes" => mask.native_bytes = true,
                "stage" => mask.stage = true,
                "cardano" => mask.parsed = true,
                "" | "tx" => return Self::all(),
                _ => {}
            }
        }

        mask
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[test]
    fn block_mask_from_paths_semantics() {
        // Absent/empty mask keeps everything.
        assert_eq!(BlockMask::from_paths(&[]), BlockMask::all());

        // Bare leaf names.
        let bytes_only = BlockMask::from_paths(&["native_bytes".to_string()]);
        assert!(bytes_only.native_bytes && !bytes_only.chain);

        let chain_only = BlockMask::from_paths(&["cardano".to_string()]);
        assert!(!chain_only.native_bytes && chain_only.chain);

        // Response-relative `block.` prefix is tolerated.
        let prefixed = BlockMask::from_paths(&["block.native_bytes".to_string()]);
        assert!(prefixed.native_bytes && !prefixed.chain);

        // Deeper paths into the parsed block select the chain representation.
        let deep = BlockMask::from_paths(&["block.cardano.header".to_string()]);
        assert!(!deep.native_bytes && deep.chain);

        // Selecting the whole block keeps both.
        assert_eq!(
            BlockMask::from_paths(&["block".to_string()]),
            BlockMask::all()
        );

        // Multiple paths accumulate.
        let both = BlockMask::from_paths(&["native_bytes".to_string(), "cardano".to_string()]);
        assert!(both.native_bytes && both.chain);
    }

    /// A path names a field of the response, so a follow answer's block is
    /// reached through its `apply` or `undo` action.
    #[test]
    fn block_mask_reads_paths_under_an_action() {
        let paths = |p: &str| vec![p.to_string()];
        assert_eq!(
            BlockMask::from_paths(&paths("apply.native_bytes")),
            BlockMask {
                native_bytes: true,
                chain: false
            }
        );
        assert_eq!(
            BlockMask::from_paths(&paths("undo.cardano.header")),
            BlockMask {
                native_bytes: false,
                chain: true
            }
        );
        assert_eq!(BlockMask::from_paths(&paths("apply")), BlockMask::all());
        assert_eq!(
            BlockMask::from_paths(&paths("tip")),
            BlockMask {
                native_bytes: false,
                chain: false
            }
        );
    }

    #[test]
    fn tx_mask_from_paths_semantics() {
        let mask =
            |ps: &[&str]| TxMask::from_paths(&ps.iter().map(|p| p.to_string()).collect::<Vec<_>>());
        let everything = TxMask {
            tx: true,
            block: BlockMask::all(),
        };

        assert_eq!(mask(&[]), everything);
        assert_eq!(mask(&["apply"]), everything);
        assert_eq!(
            mask(&["apply.cardano.hash"]),
            TxMask {
                tx: true,
                block: BlockMask::none()
            }
        );
        assert_eq!(
            mask(&["undo.block.native_bytes"]),
            TxMask {
                tx: false,
                block: BlockMask {
                    native_bytes: true,
                    chain: false
                }
            }
        );
        assert_eq!(
            mask(&["cardano", "block"]),
            TxMask {
                tx: true,
                block: BlockMask::all()
            }
        );
        assert_eq!(
            mask(&["idle.slot"]),
            TxMask {
                tx: false,
                block: BlockMask::none()
            }
        );
    }

    #[test]
    fn mempool_tx_mask_from_paths_semantics() {
        let mask = |ps: &[&str]| {
            MempoolTxMask::from_paths(&ps.iter().map(|p| p.to_string()).collect::<Vec<_>>())
        };
        let nothing = MempoolTxMask {
            r#ref: false,
            native_bytes: false,
            stage: false,
            parsed: false,
        };

        assert_eq!(mask(&[]), MempoolTxMask::all());
        assert_eq!(mask(&["tx"]), MempoolTxMask::all());
        assert_eq!(
            mask(&["tx.native_bytes"]),
            MempoolTxMask {
                native_bytes: true,
                ..nothing
            }
        );
        assert_eq!(
            mask(&["tx.cardano.hash", "stage"]),
            MempoolTxMask {
                stage: true,
                parsed: true,
                ..nothing
            }
        );
        assert_eq!(
            mask(&["tx.ref"]),
            MempoolTxMask {
                r#ref: true,
                ..nothing
            }
        );
        assert_eq!(mask(&["tip"]), nothing);
    }

    #[derive(Deserialize, Serialize, Default, Debug, PartialEq, Clone)]
    #[serde(default)]
    struct Bar {
        pub abc: usize,
    }

    #[derive(Deserialize, Serialize, Default, Debug, Clone)]
    #[serde(default)]
    struct Foo {
        pub foo: String,
        pub bar: Bar,
    }

    #[test]
    fn test_apply_mask() {
        let foobar = Foo {
            foo: "foo".into(),
            bar: Bar { abc: 10 },
        };
        let paths = vec!["foo".to_string()];
        let masked_foobar = apply_mask(foobar.clone(), paths).expect("Failed to serde");
        assert_eq!(masked_foobar.foo, "foo");
        assert_eq!(masked_foobar.bar, Bar::default());

        let paths = vec!["bar".to_string()];
        let masked_foobar = apply_mask(foobar, paths).expect("Failed to serde");
        assert_eq!(masked_foobar.foo, String::default());
        assert_eq!(masked_foobar.bar.abc, 10);
    }

    #[test]
    fn apply_mask_reads_paths_in_field_names() {
        use pallas::interop::utxorpc::v1::spec::query::{AnyChainTx, ChainPoint, ReadTxResponse};

        let point = |slot| {
            Some(ChainPoint {
                slot,
                ..Default::default()
            })
        };
        let response = ReadTxResponse {
            tx: Some(AnyChainTx {
                native_bytes: vec![1, 2, 3].into(),
                block_ref: point(9),
                ..Default::default()
            }),
            ledger_tip: point(7),
        };

        assert_eq!(apply_mask(response.clone(), vec![]).unwrap(), response);

        let masked = apply_mask(response.clone(), vec!["tx.native_bytes".into()]).unwrap();
        let tx = masked
            .tx
            .expect("the path names a field of the transaction");
        assert_eq!(tx.native_bytes.to_vec(), vec![1, 2, 3]);
        assert_eq!(tx.block_ref, None);
        assert_eq!(masked.ledger_tip, None);

        for path in ["ledger_tip", "ledgerTip"] {
            let masked = apply_mask(response.clone(), vec![path.into()]).unwrap();
            assert_eq!(masked.ledger_tip, point(7), "{path}");
            assert_eq!(masked.tx, None, "{path}");
        }
    }

    #[test]
    fn apply_mask_applies_a_path_through_a_list_to_each_item() {
        use pallas::interop::utxorpc::v1::spec::query::{AnyUtxoData, ReadUtxosResponse, TxoRef};

        let item = |index: u32| AnyUtxoData {
            native_bytes: vec![index as u8].into(),
            txo_ref: Some(TxoRef {
                index,
                ..Default::default()
            }),
            ..Default::default()
        };
        let bytes_only = |index: u32| AnyUtxoData {
            native_bytes: vec![index as u8].into(),
            ..Default::default()
        };
        let response = ReadUtxosResponse {
            items: vec![item(1), item(2)],
            ledger_tip: None,
        };

        let masked = apply_mask(response.clone(), vec!["items.native_bytes".into()]).unwrap();
        assert_eq!(masked.items, vec![bytes_only(1), bytes_only(2)]);

        let masked = apply_mask(response.clone(), vec!["items".into()]).unwrap();
        assert_eq!(masked.items, response.items);

        let masked = apply_mask(response, vec!["ledger_tip".into()]).unwrap();
        assert_eq!(masked.items, vec![]);
    }
}
