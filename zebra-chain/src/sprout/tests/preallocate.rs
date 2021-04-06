//! Tests for trusted preallocation during deserialization.

use super::super::joinsplit::{JoinSplit, BCTV14_JOINSPLIT_SIZE, GROTH16_JOINSPLIT_SIZE};

use crate::{
    block::MAX_BLOCK_BYTES,
    primitives::{Bctv14Proof, Groth16Proof},
    serialization::{TrustedPreallocate, ZcashSerialize},
};

use proptest::{prelude::*, proptest};
use std::convert::TryInto;

proptest! {
    #[test]
    /// Confirm that each  JoinSplit<Btcv14Proof> takes exactly BCTV14_JOINSPLIT_SIZE bytes when serialized.
    /// This verifies that our calculated `TrustedPreallocate::max_allocation()` is indeed an upper bound.
    fn joinsplit_btcv14_size_is_correct(joinsplit in <JoinSplit<Bctv14Proof>>::arbitrary_with(())) {
        let serialized = joinsplit.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        prop_assert!(serialized.len() as u64 == BCTV14_JOINSPLIT_SIZE)
    }

    #[test]
    /// Confirm that each  JoinSplit<Btcv14Proof> takes exactly  GROTH16_JOINSPLIT_SIZE bytes when serialized.
    /// This verifies that our calculated `TrustedPreallocate::max_allocation()` is indeed an upper bound.
    fn joinsplit_groth16_size_is_correct(joinsplit in <JoinSplit<Groth16Proof>>::arbitrary_with(())) {
        let serialized = joinsplit.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        prop_assert!(serialized.len() as u64 == GROTH16_JOINSPLIT_SIZE)
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Verify that...
    /// 1. The smallest disallowed vector of `JoinSplit<Bctv14Proof>`s is too large to fit in a Zcash block
    /// 2. The largest allowed vector is small enough to fit in a legal Zcash block
    #[test]
    fn joinsplit_btcv14_max_allocation_is_correct(joinsplit in <JoinSplit<Bctv14Proof>>::arbitrary_with(())) {

        let max_allocation: usize = <JoinSplit<Bctv14Proof>>::max_allocation().try_into().unwrap();
        let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
        for _ in 0..(<JoinSplit<Bctv14Proof>>::max_allocation()+1) {
            smallest_disallowed_vec.push(joinsplit.clone());
        }
        let smallest_disallowed_serialized = smallest_disallowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        // Check that our smallest_disallowed_vec is only one item larger than the limit
        prop_assert!(((smallest_disallowed_vec.len() - 1) as u64) == <JoinSplit<Bctv14Proof>>::max_allocation());
        // Check that our smallest_disallowed_vec is too big to be included in a valid block
        // Note that a serialized block always includes at least one byte for the number of transactions,
        // so any serialized Vec<<JoinSplit<Bctv14Proof>>> at least MAX_BLOCK_BYTES long is too large to fit in a block.
        prop_assert!((smallest_disallowed_serialized.len() as u64) >= MAX_BLOCK_BYTES);

        // Create largest_allowed_vec by removing one element from smallest_disallowed_vec without copying (for efficiency)
        smallest_disallowed_vec.pop();
        let largest_allowed_vec = smallest_disallowed_vec;
        let largest_allowed_serialized = largest_allowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");

        // Check that our largest_allowed_vec contains the maximum number of <JoinSplit<Bctv14Proof>>
        prop_assert!((largest_allowed_vec.len() as u64) == <JoinSplit<Bctv14Proof>>::max_allocation());
        // Check that our largest_allowed_vec is small enough to fit in a Zcash block.
        prop_assert!((largest_allowed_serialized.len() as u64) < MAX_BLOCK_BYTES);
    }

    /// Verify that...
    /// 1. The smallest disallowed vector of `JoinSplit<Groth16Proof>`s is too large to fit in a Zcash block
    /// 2. The largest allowed vector is small enough to fit in a legal Zcash block
    #[test]
    fn joinsplit_groth16_max_allocation_is_correct(joinsplit in <JoinSplit<Groth16Proof>>::arbitrary_with(())) {

        let max_allocation: usize = <JoinSplit<Groth16Proof>>::max_allocation().try_into().unwrap();
        let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
        for _ in 0..(<JoinSplit<Groth16Proof>>::max_allocation()+1) {
            smallest_disallowed_vec.push(joinsplit.clone());
        }
        let smallest_disallowed_serialized = smallest_disallowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        // Check that our smallest_disallowed_vec is only one item larger than the limit
        prop_assert!(((smallest_disallowed_vec.len() - 1) as u64) == <JoinSplit<Groth16Proof>>::max_allocation());
        // Check that our smallest_disallowed_vec is too big to be included in a valid block
        // Note that a serialized block always includes at least one byte for the number of transactions,
        // so any serialized Vec<<JoinSplit<Groth16Proof>>> at least MAX_BLOCK_BYTES long is too large to fit in a block.
        prop_assert!((smallest_disallowed_serialized.len() as u64) >= MAX_BLOCK_BYTES);

        // Create largest_allowed_vec by removing one element from smallest_disallowed_vec without copying (for efficiency)
        smallest_disallowed_vec.pop();
        let largest_allowed_vec = smallest_disallowed_vec;
        let largest_allowed_serialized = largest_allowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");

        // Check that our largest_allowed_vec contains the maximum number of <JoinSplit<Groth16Proof>>
        prop_assert!((largest_allowed_vec.len() as u64) == <JoinSplit<Groth16Proof>>::max_allocation());
        // Check that our largest_allowed_vec is small enough to fit in a Zcash block.
        prop_assert!((largest_allowed_serialized.len() as u64) < MAX_BLOCK_BYTES);
    }
}
