//! The encoding pallas writes for a value it read.

use pallas_codec::minicbor::{self, Encode};
use pallas_primitives::KeepRaw;

/// The encoding of a value read whole.
pub fn whole<T: Encode<()>>(value: &T) -> Result<Vec<u8>, String> {
    minicbor::to_vec(value).map_err(|e| e.to_string())
}

/// The encoding of the value inside bytes kept as read.
pub fn kept<T: Encode<()>>(raw: &KeepRaw<'_, T>) -> Result<Vec<u8>, String> {
    whole(&**raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_kept_as_read_are_written_from_the_value() {
        let read = hex::decode("9f01ff").expect("hex");
        let raw: KeepRaw<'_, Vec<u64>> = minicbor::decode(&read).expect("read");
        assert_eq!(minicbor::to_vec(&raw).expect("encode"), read);
        assert_eq!(kept(&raw), Ok(vec![0x81, 0x01]));
    }
}
