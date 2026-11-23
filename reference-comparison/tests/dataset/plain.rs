//! The encoding of a value pallas read, written from the value alone and not
//! from bytes it kept as read.

use pallas_codec::minicbor::{self, Encode};
use pallas_codec::utils::{CborWrap, MaybeIndefArray, NonEmptySet, Nullable};
use pallas_primitives::{KeepRaw, PlutusData, alonzo, babbage, conway, dijkstra};

/// The encoding of a value and whether bytes kept as read are left in it.
pub struct Plain {
    pub encoding: Result<Vec<u8>, String>,
    pub raw_left: bool,
}

fn plain<T: Encode<()> + std::fmt::Debug>(value: &T) -> Plain {
    let text = outside_embedded(&format!("{value:?}"));
    Plain {
        encoding: minicbor::to_vec(value).map_err(|e| e.to_string()),
        raw_left: text
            .match_indices("KeepRaw { raw: [")
            .any(|(at, found)| !text[at + found.len()..].starts_with(']')),
    }
}

/// The debug text of a value without what is inside a `CborWrap`, whose
/// encoded bytes are the value and are kept as read.
fn outside_embedded(text: &str) -> String {
    const OPEN: &str = "CborWrap(";
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(OPEN) {
        out.push_str(&rest[..at]);
        let inside = &rest[at + OPEN.len()..];
        let (mut depth, mut quoted, mut escaped, mut end) = (1usize, false, false, inside.len());
        for (i, c) in inside.char_indices() {
            match (quoted, escaped, c) {
                (true, true, _) => escaped = false,
                (true, false, '\\') => escaped = true,
                (true, false, '"') => quoted = false,
                (true, false, _) => {}
                (false, _, '"') => quoted = true,
                (false, _, '(' | '[' | '{') => depth += 1,
                (false, _, ')' | ']' | '}') => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                end = i + 1;
                break;
            }
        }
        out.push_str("CborWrap(..)");
        rest = &inside[end..];
    }
    out.push_str(rest);
    out
}

/// A value read whole.
pub fn whole<T: Encode<()> + std::fmt::Debug>(value: &T) -> Plain {
    plain(value)
}

/// A value read whole, written without the bytes kept as read inside it.
pub fn unrawed<T: Unraw + Clone + Encode<()> + std::fmt::Debug>(value: &T) -> Plain {
    let mut value = value.clone();
    value.unraw();
    plain(&value)
}

/// The value inside bytes kept as read.
pub fn kept<T: Encode<()> + std::fmt::Debug>(raw: &KeepRaw<'_, T>) -> Plain {
    plain(&**raw)
}

/// The value inside bytes kept as read, written without the bytes kept as
/// read inside it.
pub fn kept_unrawed<T: Unraw + Clone + Encode<()> + std::fmt::Debug>(
    raw: &KeepRaw<'_, T>,
) -> Plain {
    unrawed(&**raw)
}

/// A value whose bytes kept as read can be dropped, so that it encodes from
/// its fields.
pub trait Unraw {
    fn unraw(&mut self);
}

macro_rules! holds_no_raw {
    ($($t:ty),*) => {
        $(impl Unraw for $t {
            fn unraw(&mut self) {}
        })*
    };
}

holds_no_raw!(
    conway::Header,
    alonzo::AuxiliaryData,
    conway::NativeScript,
    PlutusData,
    conway::Redeemers,
    alonzo::TransactionOutput,
    dijkstra::Header,
    dijkstra::AuxiliaryData,
    dijkstra::NativeScript,
    dijkstra::Redeemers
);

impl<T: Unraw> Unraw for KeepRaw<'_, T> {
    fn unraw(&mut self) {
        self.clear_raw();
        (**self).unraw();
    }
}

impl<T> Unraw for CborWrap<T> {
    fn unraw(&mut self) {}
}

impl<T: Unraw> Unraw for Option<T> {
    fn unraw(&mut self) {
        if let Some(value) = self {
            value.unraw();
        }
    }
}

impl<T: Unraw> Unraw for Vec<T> {
    fn unraw(&mut self) {
        self.iter_mut().for_each(Unraw::unraw);
    }
}

impl<T: Unraw + Clone> Unraw for Nullable<T> {
    fn unraw(&mut self) {
        if let Nullable::Some(value) = self {
            value.unraw();
        }
    }
}

impl<T: Unraw + Clone> Unraw for NonEmptySet<T> {
    fn unraw(&mut self) {
        let mut items = self.clone().to_vec();
        items.unraw();
        if let Ok(set) = NonEmptySet::try_from(items) {
            *self = set;
        }
    }
}

impl<T: Unraw + Clone> Unraw for dijkstra::NonEmptySet<T> {
    fn unraw(&mut self) {
        let arm = self.arm();
        let mut items = self.clone().into_vec();
        items.unraw();
        if let Ok(set) = dijkstra::NonEmptySet::try_from(items) {
            *self = set.with_arm(arm);
        }
    }
}

impl<T: Unraw> Unraw for MaybeIndefArray<T> {
    fn unraw(&mut self) {
        match self {
            MaybeIndefArray::Def(items) | MaybeIndefArray::Indef(items) => items.unraw(),
        }
    }
}

impl Unraw for conway::Block<'_> {
    fn unraw(&mut self) {
        self.header.unraw();
        self.transaction_bodies.unraw();
        self.transaction_witness_sets.unraw();
        self.auxiliary_data_set.values_mut().for_each(Unraw::unraw);
    }
}

impl Unraw for conway::Tx<'_> {
    fn unraw(&mut self) {
        self.transaction_body.unraw();
        self.transaction_witness_set.unraw();
        self.auxiliary_data.unraw();
    }
}

impl Unraw for conway::TransactionBody<'_> {
    fn unraw(&mut self) {
        self.outputs.unraw();
        self.collateral_return.unraw();
    }
}

impl Unraw for conway::WitnessSet<'_> {
    fn unraw(&mut self) {
        self.native_script.unraw();
        self.plutus_data.unraw();
        self.redeemer.unraw();
    }
}

impl<T: Unraw> Unraw for babbage::GenTransactionOutput<'_, T> {
    fn unraw(&mut self) {
        match self {
            babbage::GenTransactionOutput::Legacy(output) => output.unraw(),
            babbage::GenTransactionOutput::PostAlonzo(output) => output.unraw(),
        }
    }
}

impl<V, S: Unraw> Unraw for babbage::GenPostAlonzoTransactionOutput<'_, V, S> {
    fn unraw(&mut self) {
        self.datum_option.unraw();
        self.script_ref.unraw();
    }
}

impl Unraw for babbage::DatumOption<'_> {
    fn unraw(&mut self) {
        if let babbage::DatumOption::Data(data) = self {
            data.unraw();
        }
    }
}

impl Unraw for conway::ScriptRef<'_> {
    fn unraw(&mut self) {
        if let conway::ScriptRef::NativeScript(script) = self {
            script.unraw();
        }
    }
}

impl Unraw for dijkstra::Block<'_> {
    fn unraw(&mut self) {
        self.header.unraw();
        self.block_body.unraw();
    }
}

impl Unraw for dijkstra::BlockBody<'_> {
    fn unraw(&mut self) {
        self.transactions.unraw();
    }
}

impl Unraw for dijkstra::BlockTransaction<'_> {
    fn unraw(&mut self) {
        self.transaction_body.unraw();
        self.transaction_witness_set.unraw();
        self.auxiliary_data.unraw();
    }
}

impl Unraw for dijkstra::MempoolTransaction<'_> {
    fn unraw(&mut self) {
        self.transaction_body.unraw();
        self.transaction_witness_set.unraw();
        self.auxiliary_data.unraw();
    }
}

impl Unraw for dijkstra::TransactionBody<'_> {
    fn unraw(&mut self) {
        self.outputs.unraw();
        self.collateral_return.unraw();
        self.sub_transactions.unraw();
    }
}

impl Unraw for dijkstra::SubTransactionBody<'_> {
    fn unraw(&mut self) {
        self.outputs.unraw();
    }
}

impl Unraw for dijkstra::SubTransaction<'_> {
    fn unraw(&mut self) {
        self.sub_transaction_body.unraw();
        self.transaction_witness_set.unraw();
        self.auxiliary_data.unraw();
    }
}

impl Unraw for dijkstra::WitnessSet<'_> {
    fn unraw(&mut self) {
        self.native_script.unraw();
        self.plutus_data.unraw();
        self.redeemer.unraw();
    }
}

impl Unraw for dijkstra::ScriptRef<'_> {
    fn unraw(&mut self) {
        if let dijkstra::ScriptRef::NativeScript(script) = self {
            script.unraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_kept_as_read_inside_a_value_are_dropped() {
        let read = hex::decode("82d81841029f01ff").expect("hex");
        type Pair<'b> = (
            CborWrap<KeepRaw<'b, PlutusData>>,
            Option<KeepRaw<'b, PlutusData>>,
        );
        let pair: Pair<'_> = minicbor::decode(&read).expect("read");
        assert!(whole(&pair).raw_left);
        let mut pair = pair;
        pair.1.unraw();
        assert!(!whole(&pair).raw_left);
    }

    #[test]
    fn bytes_inside_an_embedded_encoding_stay_as_read() {
        let read = hex::decode("d8184102").expect("hex");
        let data: CborWrap<KeepRaw<'_, PlutusData>> = minicbor::decode(&read).expect("read");
        assert!(!whole(&data).raw_left);
        assert_eq!(
            outside_embedded(r#"A(CborWrap(B { s: "x)(", r: [1] }), KeepRaw { raw: [1] })"#),
            "A(CborWrap(..), KeepRaw { raw: [1] })"
        );
    }

    #[test]
    fn bytes_kept_as_read_are_written_from_the_value() {
        let read = hex::decode("9f01ff").expect("hex");
        let raw: KeepRaw<'_, Vec<u64>> = minicbor::decode(&read).expect("read");
        assert_eq!(minicbor::to_vec(&raw).expect("encode"), read);
        assert_eq!(kept(&raw).encoding, Ok(vec![0x81, 0x01]));
    }
}
