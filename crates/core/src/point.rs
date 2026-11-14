use std::{fmt::Display, str::FromStr};

use hex;
use pallas::{crypto::hash::Hash, network::miniprotocols::Point as PallasPoint};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::{Block, BlockHash, BlockSlot};

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub enum ChainPoint {
    Origin,
    Slot(BlockSlot),
    Specific(BlockSlot, BlockHash),
}

impl ChainPoint {
    pub fn slot(&self) -> BlockSlot {
        match self {
            Self::Origin => 0,
            Self::Slot(slot) => *slot,
            Self::Specific(slot, _) => *slot,
        }
    }

    pub fn hash(&self) -> Option<BlockHash> {
        match self {
            Self::Specific(_, hash) => Some(*hash),
            _ => None,
        }
    }

    /// Returns true if this point can be used as an intersection point.
    /// Origin and Specific points with non-zero hashes are fully defined;
    /// Slot-only points and zero-hash Specifics are not.
    pub fn is_fully_defined(&self) -> bool {
        match self {
            Self::Origin => true,
            Self::Specific(_, hash) => hash.as_slice() != [0u8; 32],
            Self::Slot(_) => false,
        }
    }

    /// Whether two points can be the same block, comparing only what both of
    /// them carry, so a slot without a hash matches the block at that slot.
    ///
    /// This is not equality. It holds between a hashless point and two
    /// different blocks at one slot, which are not each other, so it cannot be
    /// [`PartialEq`] and a caller that wants the same block twice wants `==`.
    pub fn may_be_same_block(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Specific(l, _), Self::Slot(r)) | (Self::Slot(l), Self::Specific(r, _)) => l == r,
            _ => self == other,
        }
    }

    /// Orders the variants at one slot with no hash, which nothing else
    /// distinguishes.
    fn rank(&self) -> u8 {
        match self {
            Self::Origin => 0,
            Self::Slot(_) => 1,
            Self::Specific(_, _) => 2,
        }
    }
}

impl Display for ChainPoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Origin => write!(f, "Origin"),
            Self::Slot(slot) => write!(f, "{slot}"),
            Self::Specific(slot, hash) => write!(f, "{slot}({hash})"),
        }
    }
}

impl Ord for ChainPoint {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let l_slot = self.slot();
        let r_slot = other.slot();

        // if slots are different, we can compare them directly
        if l_slot != r_slot {
            return l_slot.cmp(&r_slot);
        }

        // if the slots are the same, we need to compare hashes

        let l_hash = self.hash();
        let r_hash = other.hash();

        // Origin and Slot(0) are both slot zero with no hash, and equality
        // tells them apart, so the order has to as well.
        l_hash
            .cmp(&r_hash)
            .then_with(|| self.rank().cmp(&other.rank()))
    }
}

impl PartialOrd for ChainPoint {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl From<PallasPoint> for ChainPoint {
    fn from(value: PallasPoint) -> Self {
        match value {
            PallasPoint::Origin => ChainPoint::Origin,
            PallasPoint::Specific(s, h) => ChainPoint::Specific(s, h.as_slice().into()),
        }
    }
}

impl TryFrom<ChainPoint> for PallasPoint {
    type Error = ();

    fn try_from(value: ChainPoint) -> Result<Self, Self::Error> {
        match value {
            ChainPoint::Origin => Ok(PallasPoint::Origin),
            ChainPoint::Specific(s, h) => Ok(PallasPoint::Specific(s, h.to_vec())),
            ChainPoint::Slot(_) => Err(()),
        }
    }
}

impl<T> From<&T> for ChainPoint
where
    T: Block,
{
    fn from(value: &T) -> Self {
        let slot = value.slot();
        let hash = value.hash();
        ChainPoint::Specific(slot, hash)
    }
}

impl ChainPoint {
    pub fn into_bytes(self) -> [u8; 40] {
        let slot = self.slot();

        let hash = match self.hash() {
            Some(hash) => *hash,
            None => [0u8; 32],
        };

        let mut out = [0u8; 40];
        out[0..8].copy_from_slice(&slot.to_be_bytes());
        out[8..40].copy_from_slice(hash.as_slice());
        out
    }

    const ORIGIN_BYTES: [u8; 40] = [0u8; 40];

    pub fn from_bytes(value: [u8; 40]) -> Self {
        if value == Self::ORIGIN_BYTES {
            return ChainPoint::Origin;
        }

        let slot_half: [u8; 8] = value[0..8].try_into().unwrap();
        let hash_half: [u8; 32] = value[8..40].try_into().unwrap();
        let slot = u64::from_be_bytes(slot_half);
        let hash = Hash::new(hash_half);
        ChainPoint::Specific(slot, hash)
    }
}

impl FromStr for ChainPoint {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();

        // Handle "Origin" case
        if s == "Origin" {
            return Ok(ChainPoint::Origin);
        }

        // Regex to match slot(hash) format where hash is 64 hex characters (32
        // bytes)
        let re = Regex::new(r"^(\d+)\(([0-9a-fA-F]{64})\)$").unwrap();

        if let Some(caps) = re.captures(s) {
            let slot: BlockSlot = caps[1].parse().map_err(|_| "invalid slot")?;
            let hash_bytes = hex::decode(&caps[2]).map_err(|_| "invalid hash")?;
            let hash_array: [u8; 32] = hash_bytes.try_into().map_err(|_| "invalid hash")?;
            let hash = Hash::new(hash_array);
            return Ok(ChainPoint::Specific(slot, hash));
        }

        // Try to parse as slot-only (no parentheses)
        if let Ok(slot) = s.parse::<BlockSlot>() {
            return Ok(ChainPoint::Slot(slot));
        }

        Err("invalid format".to_string())
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use proptest::proptest;

    use super::*;

    prop_compose! {
      fn any_hash() (bytes in any::<[u8; 32]>()) -> Hash<32> {
            Hash::new(bytes)
        }
    }

    prop_compose! {
      fn any_specific_point() (slot in any::<BlockSlot>(), hash in any_hash()) -> ChainPoint {
            ChainPoint::Specific(slot, hash)
        }
    }

    proptest! {
        #[test]
        fn test_binary_order_is_maintained(point1 in any_specific_point(), point2 in any_specific_point()) {
            let bytes1 = point1.clone().into_bytes();
            let bytes2 = point2.clone().into_bytes();

            let point_cmp = point1.cmp(&point2);
            let bytes_cmp = bytes1.cmp(&bytes2);

            assert_eq!(point_cmp, bytes_cmp);
        }
    }

    /// Every pair worth naming, so an arm below that reads them all is reading
    /// the same list as the next one.
    fn every_pair() -> Vec<(ChainPoint, ChainPoint)> {
        let points = [
            ChainPoint::Origin,
            ChainPoint::Slot(0),
            ChainPoint::Slot(7),
            ChainPoint::Specific(0, Hash::new([1u8; 32])),
            ChainPoint::Specific(7, Hash::new([1u8; 32])),
            ChainPoint::Specific(7, Hash::new([2u8; 32])),
        ];

        points
            .iter()
            .flat_map(|l| points.iter().map(|r| (l.clone(), r.clone())))
            .collect()
    }

    /// The must-fire case. A point that carries a hash and one that does not
    /// have to answer the same both ways round, because a caller has no say in
    /// which of the two it holds.
    #[test]
    fn equality_answers_the_same_whichever_side_holds_the_hash() {
        for (left, right) in every_pair() {
            assert_eq!(
                left == right,
                right == left,
                "{left} against {right} answers differently each way round"
            );
        }
    }

    /// `Ord` claims a total order over the same relation `Eq` claims, so the
    /// two have to agree on every pair rather than on the ones a caller
    /// happens to try.
    #[test]
    fn the_order_calls_a_pair_equal_exactly_when_equality_does() {
        for (left, right) in every_pair() {
            assert_eq!(
                left.cmp(&right) == std::cmp::Ordering::Equal,
                left == right,
                "{left} against {right}"
            );
        }
    }

    /// The must-not case. Making equality exact must not take away the looser
    /// question, which is the one a caller holding a slot and no hash asks.
    #[test]
    fn a_slot_without_a_hash_may_be_the_block_at_that_slot() {
        let slot = ChainPoint::Slot(7);
        let block = ChainPoint::Specific(7, Hash::new([1u8; 32]));
        let elsewhere = ChainPoint::Specific(8, Hash::new([1u8; 32]));

        assert!(slot.may_be_same_block(&block));
        assert!(block.may_be_same_block(&slot));
        assert!(!slot.may_be_same_block(&elsewhere));
        assert!(!ChainPoint::Origin.may_be_same_block(&slot));
        assert!(block.may_be_same_block(&block));
    }

    #[test]
    fn test_from_str_origin() {
        assert_eq!("Origin".parse::<ChainPoint>().unwrap(), ChainPoint::Origin);
    }

    #[test]
    fn test_from_str_slot_only() {
        assert_eq!(
            "12345".parse::<ChainPoint>().unwrap(),
            ChainPoint::Slot(12345)
        );
    }

    #[test]
    fn test_from_str_slot_hash() {
        let hash_bytes = [1u8; 32];
        let hash_hex = hex::encode(hash_bytes);
        let input = format!("12345({})", hash_hex);

        let result: ChainPoint = input.parse().unwrap();
        match result {
            ChainPoint::Specific(slot, hash) => {
                assert_eq!(slot, 12345);
                assert_eq!(hash.as_slice(), &hash_bytes);
            }
            _ => panic!("Expected Specific variant"),
        }
    }

    #[test]
    fn test_from_str_invalid() {
        assert!("invalid".parse::<ChainPoint>().is_err());
        assert!("12345(invalid)".parse::<ChainPoint>().is_err());
        assert!("12345(short)".parse::<ChainPoint>().is_err());
    }
}
