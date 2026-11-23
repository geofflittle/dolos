// Copyright 2026 PRAGMA
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
// Changed from crates/amaru-kernel/tests/common/normalize.rs of
// https://github.com/pragma-org/amaru at d11baf417f03a256f7f74784428e721491c15529.

//! The normal form of a CBOR item by the Normalization table of the
//! cardano-cbor-dataset VERIFICATION.md at ed6c49a4.

use std::convert::Infallible;

use minicbor::{
    data::{IanaTag, Int, Tag, Type},
    decode, encode, Decoder, Encoder,
};
use num_bigint::{BigInt, Sign};
use num_rational::BigRational;
use thiserror::Error;

/// The tag of a rational number, an array of its numerator and denominator.
const RATIONAL: Tag = Tag::new(30);

/// Why an input has no normal form.
#[derive(Debug, Error)]
pub enum NormalizeError {
    /// The input is not exactly one well formed CBOR item.
    #[error("cbor: {0}")]
    Decode(#[from] decode::Error),
    /// Writing the normal form failed.
    #[error("writing the normal form: {0}")]
    Encode(#[from] encode::Error<Infallible>),
}

/// Rewrites one CBOR item into its normal form.
pub fn normalize(cbor: &[u8]) -> Result<Vec<u8>, NormalizeError> {
    let mut d = Decoder::new(cbor);
    let mut out = Vec::with_capacity(cbor.len());
    rewrite(&mut d, &mut out)?;
    if d.position() != cbor.len() {
        return Err(refused("bytes follow the item", d.position()));
    }
    Ok(out)
}

/// Writes the normal form of the item at the decoder's position.
fn rewrite(d: &mut Decoder<'_>, out: &mut Vec<u8>) -> Result<(), NormalizeError> {
    match d.datatype()? {
        Type::Array | Type::ArrayIndef => {
            let len = d.array()?;
            let (count, items) = items(d, len)?;
            Encoder::new(&mut *out).array(count)?;
            out.extend_from_slice(&items);
        }
        Type::Map | Type::MapIndef => {
            let len = d.map()?;
            let (count, items) = items(d, len.map(|n| n.saturating_mul(2)))?;
            if count % 2 != 0 {
                return Err(refused("a map key has no value", d.position()));
            }
            Encoder::new(&mut *out).map(count / 2)?;
            out.extend_from_slice(&items);
        }
        Type::Bytes | Type::BytesIndef => {
            let bytes = joined_bytes(d)?;
            Encoder::new(&mut *out).bytes(&bytes)?;
        }
        Type::String | Type::StringIndef => {
            let mut text = String::new();
            for chunk in d.str_iter()? {
                text.push_str(chunk?);
            }
            Encoder::new(&mut *out).str(&text)?;
        }
        Type::U8
        | Type::U16
        | Type::U32
        | Type::U64
        | Type::I8
        | Type::I16
        | Type::I32
        | Type::I64
        | Type::Int => {
            Encoder::new(&mut *out).int(d.int()?)?;
        }
        Type::Tag => {
            let tag = d.tag()?;
            if let Some(negative) = bignum_sign(tag) {
                integer(&bignum(negative, &joined_bytes(d)?), out)?;
            } else {
                let mut item = Vec::new();
                rewrite(d, &mut item)?;
                if tag == RATIONAL {
                    if let Some(reduced) = reduced(&item)? {
                        item = reduced;
                    }
                }
                Encoder::new(&mut *out).tag(tag)?;
                out.extend_from_slice(&item);
            }
        }
        Type::Bool
        | Type::Null
        | Type::Undefined
        | Type::Simple
        | Type::F16
        | Type::F32
        | Type::F64 => {
            let start = d.position();
            d.skip()?;
            out.extend_from_slice(&d.input()[start..d.position()]);
        }
        Type::Break => {
            return Err(refused("a break outside an indefinite item", d.position()));
        }
        Type::Unknown(byte) => {
            return Err(refused(
                format!("reserved initial byte {byte:#04x}"),
                d.position(),
            ));
        }
    }
    Ok(())
}

/// Writes the normal form of each item of an array or map body and counts
/// them, reading to the break when the length is indefinite.
fn items(d: &mut Decoder<'_>, len: Option<u64>) -> Result<(u64, Vec<u8>), NormalizeError> {
    let mut items = Vec::new();
    let mut count = 0;
    loop {
        match len {
            Some(n) if count == n => break,
            None if d.datatype()? == Type::Break => {
                d.set_position(d.position() + 1);
                break;
            }
            _ => {}
        }
        rewrite(d, &mut items)?;
        count += 1;
    }
    Ok((count, items))
}

/// The chunks of a byte string concatenated.
fn joined_bytes(d: &mut Decoder<'_>) -> Result<Vec<u8>, NormalizeError> {
    let mut bytes = Vec::new();
    for chunk in d.bytes_iter()? {
        bytes.extend_from_slice(chunk?);
    }
    Ok(bytes)
}

/// Whether a tag is the negative bignum, or `None` when it is no bignum.
fn bignum_sign(tag: Tag) -> Option<bool> {
    if tag == IanaTag::PosBignum.tag() {
        Some(false)
    } else if tag == IanaTag::NegBignum.tag() {
        Some(true)
    } else {
        None
    }
}

/// The value of a bignum, `n` for `#6.2(n)` and `-1 - n` for `#6.3(n)`.
fn bignum(negative: bool, magnitude: &[u8]) -> BigInt {
    let n = BigInt::from_bytes_be(Sign::Plus, magnitude);
    if negative {
        -(n + 1u32)
    } else {
        n
    }
}

/// Writes an integer with a native head when one holds it, otherwise as a
/// bignum whose magnitude has no leading zero byte.
fn integer(value: &BigInt, out: &mut Vec<u8>) -> Result<(), NormalizeError> {
    let mut e = Encoder::new(out);
    let native = i128::try_from(value)
        .ok()
        .and_then(|n| Int::try_from(n).ok());
    match native {
        Some(n) => e.int(n)?,
        None if value.sign() == Sign::Minus => e
            .tag(IanaTag::NegBignum)?
            .bytes(&(-(value + 1u32)).to_bytes_be().1)?,
        None => e.tag(IanaTag::PosBignum)?.bytes(&value.to_bytes_be().1)?,
    };
    Ok(())
}

/// The integer at the decoder's position in a normal form, or `None` when the
/// item there is not one.
fn integer_at(d: &mut Decoder<'_>) -> Result<Option<BigInt>, NormalizeError> {
    Ok(match d.datatype()? {
        Type::U8
        | Type::U16
        | Type::U32
        | Type::U64
        | Type::I8
        | Type::I16
        | Type::I32
        | Type::I64
        | Type::Int => Some(BigInt::from(i128::from(d.int()?))),
        Type::Tag => match bignum_sign(d.tag()?) {
            Some(negative) => Some(bignum(negative, d.bytes()?)),
            None => None,
        },
        _ => None,
    })
}

/// The reduced normal form of a rational's normal form, or `None` when it is
/// not an array of an integer and a positive integer.
fn reduced(item: &[u8]) -> Result<Option<Vec<u8>>, NormalizeError> {
    let mut d = Decoder::new(item);
    if d.datatype()? != Type::Array || d.array()? != Some(2) {
        return Ok(None);
    }
    let Some(numerator) = integer_at(&mut d)? else {
        return Ok(None);
    };
    let Some(denominator) = integer_at(&mut d)? else {
        return Ok(None);
    };
    if denominator.sign() != Sign::Plus {
        return Ok(None);
    }
    let ratio = BigRational::new(numerator, denominator);
    let mut out = Vec::new();
    Encoder::new(&mut out).array(2)?;
    integer(ratio.numer(), &mut out)?;
    integer(ratio.denom(), &mut out)?;
    Ok(Some(out))
}

/// A decode error naming what makes the input malformed and its offset.
fn refused(what: impl std::fmt::Display, at: usize) -> NormalizeError {
    decode::Error::message(what).at(at).into()
}

#[cfg(test)]
mod tests {
    use super::normalize;

    fn normal(input: &str) -> String {
        let bytes = hex::decode(input).unwrap();
        hex::encode(normalize(&bytes).unwrap_or_else(|e| panic!("{input}: {e}")))
    }

    #[test]
    fn a_rational_is_reduced() {
        assert_eq!(normal("d81e820204"), "d81e820102");
        assert_eq!(normal("d81e822104"), "d81e822002");
        assert_eq!(normal("d81e820005"), "d81e820001");
        assert_eq!(
            normal("d81e82c24901000000000000000002"),
            "d81e821b800000000000000001"
        );
    }

    #[test]
    fn a_tag_30_item_that_is_not_a_rational_is_kept() {
        assert_eq!(normal("d81e820200"), "d81e820200");
        assert_eq!(normal("d81e83020406"), "d81e83020406");
        assert_eq!(normal("d81e8202f93c00"), "d81e8202f93c00");
    }

    #[test]
    fn a_bignum_too_large_drops_its_leading_zeros() {
        assert_eq!(normal("c24a00010000000000000000"), "c249010000000000000000");
        assert_eq!(normal("c34a00010000000000000000"), "c349010000000000000000");
    }

    #[test]
    fn input_that_is_not_one_well_formed_item_is_refused() {
        for input in ["", "ff", "0000", "9f01", "bf01ff", "1c", "5f01ff"] {
            let bytes = hex::decode(input).unwrap();
            assert!(normalize(&bytes).is_err(), "{input}");
        }
    }
}
