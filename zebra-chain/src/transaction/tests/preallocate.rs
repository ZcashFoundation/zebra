//! Tests for trusted preallocation during deserialization.

use super::super::{
    serialize::{MIN_TRANSPARENT_INPUT_SIZE, MIN_TRANSPARENT_OUTPUT_SIZE, MIN_TRANSPARENT_TX_SIZE},
    transparent::Input,
    transparent::Output,
    Transaction,
};

use crate::{
    block::MAX_BLOCK_BYTES,
    serialization::{TrustedPreallocate, ZcashSerialize},
};

use proptest::prelude::*;
use std::{convert::TryInto, sync::Arc};

proptest! {
    /// Confirm that each spend takes at least MIN_TRANSPARENT_TX_SIZE bytes when serialized.
    /// This verifies that our calculated [`TrustedPreallocate::max_allocation`] is indeed an upper bound.
    #[test]
    fn tx_size_is_small_enough(tx in Transaction::arbitrary()) {
        let serialized = tx.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        prop_assert!(serialized.len() as u64 >= MIN_TRANSPARENT_TX_SIZE)
    }

    /// Confirm that each spend takes at least MIN_TRANSPARENT_TX_SIZE bytes when serialized.
    /// This verifies that our calculated [`TrustedPreallocate::max_allocation`] is indeed an upper bound.
    #[test]
    fn transparent_input_size_is_small_enough(input in Input::arbitrary()) {
        let serialized = input.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        prop_assert!(serialized.len() as u64 >= MIN_TRANSPARENT_INPUT_SIZE)
    }

    /// Confirm that each spend takes at least MIN_TRANSPARENT_TX_SIZE bytes when serialized.
    /// This verifies that our calculated [`TrustedPreallocate::max_allocation`] is indeed an upper bound.
    #[test]
    fn transparent_output_size_is_small_enough(output in Output::arbitrary()) {
        let serialized = output.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        prop_assert!(serialized.len() as u64 >= MIN_TRANSPARENT_OUTPUT_SIZE)
    }

}

proptest! {
    // This test is pretty slow, so only run a few cases
    #![proptest_config(ProptestConfig::with_cases(8))]

    /// Verify the smallest disallowed vector of `Transaction`s is too large to fit in a Zcash block
    #[test]
    fn tx_max_allocation_is_big_enough(tx in Transaction::arbitrary()) {

        let max_allocation: usize = <Arc<Transaction>>::max_allocation().try_into().unwrap();
        let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
        for _ in 0..(<Arc<Transaction>>::max_allocation()+1) {
            smallest_disallowed_vec.push(Arc::new(tx.clone()));
        }
        let serialized = smallest_disallowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");

        // Check that our smallest_disallowed_vec is only one item larger than the limit
        prop_assert!(((smallest_disallowed_vec.len() - 1) as u64) == <Arc<Transaction>>::max_allocation());
        // Check that our smallest_disallowed_vec is too big to be included in a valid block
        prop_assert!(serialized.len() as u64 > MAX_BLOCK_BYTES);
    }

    /// Verify the smallest disallowed vector of `Input`s is too large to fit in a Zcash block
    #[test]
    fn input_max_allocation_is_big_enough(input in Input::arbitrary()) {

        let max_allocation: usize = Input::max_allocation().try_into().unwrap();
        let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
        for _ in 0..(Input::max_allocation()+1) {
            smallest_disallowed_vec.push(input.clone());
        }
        let serialized = smallest_disallowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");

        // Check that our smallest_disallowed_vec is only one item larger than the limit
        prop_assert!(((smallest_disallowed_vec.len() - 1) as u64) == Input::max_allocation());
        // Check that our smallest_disallowed_vec is too big to be included in a valid block
        // Note that a serialized block always includes at least one byte for the number of transactions,
        // so any serialized Vec<Input> at least MAX_BLOCK_BYTES long is too large to fit in a block.
        prop_assert!(serialized.len() as u64 >= MAX_BLOCK_BYTES);
    }

    /// Verify the smallest disallowed vector of `Output`s is too large to fit in a Zcash block
    #[test]
    fn output_max_allocation_is_big_enough(output in Output::arbitrary()) {

        let max_allocation: usize = Output::max_allocation().try_into().unwrap();
        let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
        for _ in 0..(Output::max_allocation()+1) {
            smallest_disallowed_vec.push(output.clone());
        }
        let serialized = smallest_disallowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");

        // Check that our smallest_disallowed_vec is only one item larger than the limit
        prop_assert!(((smallest_disallowed_vec.len() - 1) as u64) == Output::max_allocation());
        // Check that our smallest_disallowed_vec is too big to be included in a valid block
        // Note that a serialized block always includes at least one byte for the number of transactions,
        // so any serialized Vec<Output> at least MAX_BLOCK_BYTES long is too large to fit in a block.
        prop_assert!(serialized.len() as u64 >= MAX_BLOCK_BYTES);
    }
}
