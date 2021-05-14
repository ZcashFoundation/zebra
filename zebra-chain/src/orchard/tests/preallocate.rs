//! Tests for trusted preallocation during deserialization.

use crate::{
    block::MAX_BLOCK_BYTES,
    orchard::{
        shielded_data::{ACTION_SIZE, SPEND_AUTH_SIG_SIZE},
        Action,
    },
    primitives::redpallas::{Signature, SpendAuth},
    serialization::{TrustedPreallocate, ZcashSerialize},
};

use proptest::{prelude::*, proptest};
use std::convert::TryInto;

proptest! {
    /// Confirm that each Action takes exactly ACTION_SIZE bytes when serialized.
    /// This verifies that our calculated `TrustedPreallocate::max_allocation()` is indeed an upper bound.
    #[test]
    fn action_size_is_correct(action in <Action>::arbitrary_with(())) {
        let serialized = action.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        prop_assert!(serialized.len() as u64 == ACTION_SIZE)
    }
    /// Confirm that each Signature<SpendAuth> takes exactly SPEND_AUTH_SIG_SIZE bytes when serialized.
    /// This verifies that our calculated `TrustedPreallocate::max_allocation()` is indeed an upper bound.
    #[test]
    fn spend_auth_sig_size_is_correct(sig in <Signature<SpendAuth>>::arbitrary_with(())) {
        let serialized = sig.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        prop_assert!(serialized.len() as u64 == SPEND_AUTH_SIG_SIZE)
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Verify that...
    /// 1. The smallest disallowed vector of `Action`s is too large to fit in a Zcash block
    /// 2. The largest allowed vector is small enough to fit in a legal Zcash block
    #[test]
    fn action_max_allocation_is_correct(action in <Action>::arbitrary_with(())) {

        let max_allocation: usize = <Action>::max_allocation().try_into().unwrap();
        let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
        for _ in 0..(<Action>::max_allocation()+1) {
            smallest_disallowed_vec.push(action.clone());
        }
        let smallest_disallowed_serialized = smallest_disallowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        // Check that our smallest_disallowed_vec is only one item larger than the limit
        prop_assert!(((smallest_disallowed_vec.len() - 1) as u64) == <Action>::max_allocation());
        // Check that our smallest_disallowed_vec is too big to be included in a valid block
        // Note that a serialized block always includes at least one byte for the number of transactions,
        // so any serialized Vec<Action> at least MAX_BLOCK_BYTES long is too large to fit in a block.
        prop_assert!((smallest_disallowed_serialized.len() as u64) >= MAX_BLOCK_BYTES);

        // Create largest_allowed_vec by removing one element from smallest_disallowed_vec without copying (for efficiency)
        smallest_disallowed_vec.pop();
        let largest_allowed_vec = smallest_disallowed_vec;
        let largest_allowed_serialized = largest_allowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");

        // Check that our largest_allowed_vec contains the maximum number of <Action>
        prop_assert!((largest_allowed_vec.len() as u64) == <Action>::max_allocation());
        // Check that our largest_allowed_vec is small enough to fit in a Zcash block.
        prop_assert!((largest_allowed_serialized.len() as u64) < MAX_BLOCK_BYTES);
    }

    /// Verify that...
    /// 1. The smallest disallowed vector of `Signature<SpendAuth>`s is too large to fit in a Zcash block
    /// 2. The largest allowed vector is small enough to fit in a legal Zcash block
    #[test]
    fn spend_auth_sig_max_allocation_is_correct(sig in <Signature<SpendAuth>>::arbitrary_with(())) {

        let max_allocation: usize = <Signature<SpendAuth>>::max_allocation().try_into().unwrap();
        let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
        for _ in 0..(<Signature<SpendAuth>>::max_allocation()+1) {
            smallest_disallowed_vec.push(sig.clone());
        }
        let smallest_disallowed_serialized = smallest_disallowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        // Check that our smallest_disallowed_vec is only one item larger than the limit
        prop_assert!(((smallest_disallowed_vec.len() - 1) as u64) == <Signature<SpendAuth>>::max_allocation());
        // Check that our smallest_disallowed_vec is too big to be included in a valid block
        // Note that a serialized block always includes at least one byte for the number of transactions,
        // so any serialized Vec<Action> at least MAX_BLOCK_BYTES long is too large to fit in a block.
        prop_assert!((smallest_disallowed_serialized.len() as u64) >= MAX_BLOCK_BYTES);

        // Create largest_allowed_vec by removing one element from smallest_disallowed_vec without copying (for efficiency)
        smallest_disallowed_vec.pop();
        let largest_allowed_vec = smallest_disallowed_vec;
        let largest_allowed_serialized = largest_allowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");

        // Check that our largest_allowed_vec contains the maximum number of <Action>
        prop_assert!((largest_allowed_vec.len() as u64) == <Signature<SpendAuth>>::max_allocation());
        // Check that our largest_allowed_vec is small enough to fit in a Zcash block.
        prop_assert!((largest_allowed_serialized.len() as u64) < MAX_BLOCK_BYTES);
    }
}
