//! Tests for trusted preallocation during deserialization.

use super::super::{
    output::{Output, OUTPUT_SIZE},
    spend::{
        Spend, ANCHOR_PER_SPEND_SIZE, SHARED_ANCHOR_SPEND_FULL_SIZE,
        SHARED_ANCHOR_SPEND_INITIAL_SIZE,
    },
};

use crate::{
    block::MAX_BLOCK_BYTES,
    sapling::{AnchorVariant, PerSpendAnchor, SharedAnchor},
    serialization::{TrustedPreallocate, ZcashSerialize},
};

use proptest::prelude::*;
use std::convert::TryInto;

proptest! {
    /// Confirm that each spend takes exactly ANCHOR_PER_SPEND_SIZE bytes when serialized.
    /// This verifies that our calculated `TrustedPreallocate::max_allocation()` is indeed an upper bound.
    #[test]
    fn anchor_per_spend_size_is_small_enough(spend in Spend::<PerSpendAnchor>::arbitrary_with(())) {
        let serialized = spend.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        prop_assert!(serialized.len() as u64 == ANCHOR_PER_SPEND_SIZE)
    }

    /// Confirm that each spend takes exactly SHARED_SPEND_SIZE bytes when serialized.
    #[test]
    fn shared_anchor_spend_size_is_small_enough(spend in Spend::<SharedAnchor>::arbitrary_with(())) {
        let mut serialized_len = spend.zcash_serialize_to_vec().expect("Serialization to vec must succeed").len();
        serialized_len += spend.zkproof.zcash_serialize_to_vec().expect("Serialization to vec must succeed").len();
        serialized_len += &<[u8; 64]>::from(spend.spend_auth_sig).len();
        prop_assert!(serialized_len as u64 == SHARED_ANCHOR_SPEND_FULL_SIZE)
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Verify that...
    /// 1. The smallest disallowed vector of `Spend`s is too large to fit in a Zcash block
    /// 2. The largest allowed vector is small enough to fit in a legal Zcash block
    #[test]
    fn anchor_per_spend_max_allocation_is_big_enough(spend in Spend::<PerSpendAnchor>::arbitrary_with(())) {
        let (
            smallest_disallowed_vec_len,
            smallest_disallowed_serialized_len,
            largest_allowed_vec_len,
            largest_allowed_serialized_len,
        ) = spend_max_allocation_is_big_enough(spend);

        // Check that our smallest_disallowed_vec is only one item larger than the limit
        prop_assert!(((smallest_disallowed_vec_len - 1) as u64) == Spend::<PerSpendAnchor>::max_allocation());
        // Check that our smallest_disallowed_vec is too big to send as a protocol message
        // Note that a serialized block always includes at least one byte for the number of transactions,
        // so any serialized Vec<Spend> at least MAX_BLOCK_BYTES long is too large to fit in a block.
        prop_assert!((smallest_disallowed_serialized_len as u64) >= MAX_BLOCK_BYTES);

        // Check that our largest_allowed_vec contains the maximum number of spends
        prop_assert!((largest_allowed_vec_len as u64) == Spend::<PerSpendAnchor>::max_allocation());
        // Check that our largest_allowed_vec is small enough to send as a protocol message
        prop_assert!((largest_allowed_serialized_len as u64) <= MAX_BLOCK_BYTES);
    }

    /// Verify trusted preallocation for `Spend<SharedAnchor>`
    #[test]
    fn shared_spend_max_allocation_is_big_enough(spend in Spend::<SharedAnchor>::arbitrary_with(())) {
        let (
            smallest_disallowed_vec_len,
            smallest_disallowed_serialized_len,
            largest_allowed_vec_len,
            largest_allowed_serialized_len,
        ) = spend_max_allocation_is_big_enough(spend);

        prop_assert!(((smallest_disallowed_vec_len - 1) as u64) == Spend::<SharedAnchor>::max_allocation());
        // Calculate the actual size of all required Spend fields
        //
        // TODO: modify the test to serialize the associated zkproof and
        // spend_auth_sig fields
        prop_assert!((smallest_disallowed_serialized_len as u64)/SHARED_ANCHOR_SPEND_INITIAL_SIZE*SHARED_ANCHOR_SPEND_FULL_SIZE >= MAX_BLOCK_BYTES);

        prop_assert!((largest_allowed_vec_len as u64) == Spend::<SharedAnchor>::max_allocation());
        prop_assert!((largest_allowed_serialized_len as u64) <= MAX_BLOCK_BYTES);
    }
}

/// Return the
fn spend_max_allocation_is_big_enough<AnchorV>(
    spend: Spend<AnchorV>,
) -> (usize, usize, usize, usize)
where
    AnchorV: AnchorVariant,
    Spend<AnchorV>: TrustedPreallocate + ZcashSerialize + Clone,
{
    let max_allocation: usize = Spend::max_allocation().try_into().unwrap();
    let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
    for _ in 0..(Spend::max_allocation() + 1) {
        smallest_disallowed_vec.push(spend.clone());
    }
    let smallest_disallowed_serialized = smallest_disallowed_vec
        .zcash_serialize_to_vec()
        .expect("Serialization to vec must succeed");
    let smallest_disallowed_vec_len = smallest_disallowed_vec.len();

    // Create largest_allowed_vec by removing one element from smallest_disallowed_vec without copying (for efficiency)
    smallest_disallowed_vec.pop();
    let largest_allowed_vec = smallest_disallowed_vec;
    let largest_allowed_serialized = largest_allowed_vec
        .zcash_serialize_to_vec()
        .expect("Serialization to vec must succeed");

    (
        smallest_disallowed_vec_len,
        smallest_disallowed_serialized.len(),
        largest_allowed_vec.len(),
        largest_allowed_serialized.len(),
    )
}

proptest! {
    /// Confirm that each output takes exactly OUTPUT_SIZE bytes when serialized.
    /// This verifies that our calculated `TrustedPreallocate::max_allocation()` is indeed an upper bound.
    #[test]
    fn output_size_is_small_enough(output in Output::arbitrary_with(())) {
        let serialized = output.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        prop_assert!(serialized.len() as u64 == OUTPUT_SIZE)
    }

}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Verify that...
    /// 1. The smallest disallowed vector of `Outputs`s is too large to fit in a Zcash block
    /// 2. The largest allowed vector is small enough to fit in a legal Zcash block
    #[test]
    fn output_max_allocation_is_big_enough(output in Output::arbitrary_with(())) {

        let max_allocation: usize = Output::max_allocation().try_into().unwrap();
        let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
        for _ in 0..(Output::max_allocation()+1) {
            smallest_disallowed_vec.push(output.clone());
        }
        let smallest_disallowed_serialized = smallest_disallowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        // Check that our smallest_disallowed_vec is only one item larger than the limit
        prop_assert!(((smallest_disallowed_vec.len() - 1) as u64) == Output::max_allocation());
        // Check that our smallest_disallowed_vec is too big to be included in a valid block
        // Note that a serialized block always includes at least one byte for the number of transactions,
        // so any serialized Vec<Output> at least MAX_BLOCK_BYTES long is too large to fit in a block.
        prop_assert!((smallest_disallowed_serialized.len() as u64) >= MAX_BLOCK_BYTES);

        // Create largest_allowed_vec by removing one element from smallest_disallowed_vec without copying (for efficiency)
        smallest_disallowed_vec.pop();
        let largest_allowed_vec = smallest_disallowed_vec;
        let largest_allowed_serialized = largest_allowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");

        // Check that our largest_allowed_vec contains the maximum number of Outputs
        prop_assert!((largest_allowed_vec.len() as u64) == Output::max_allocation());
        // Check that our largest_allowed_vec is small enough to fit in a Zcash block.
        prop_assert!((largest_allowed_serialized.len() as u64) < MAX_BLOCK_BYTES);
    }
}
