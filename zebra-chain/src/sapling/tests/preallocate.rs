//! Tests for trusted preallocation during deserialization.

use crate::{
    block::MAX_BLOCK_BYTES,
    primitives::Groth16Proof,
    sapling::{
        output::{
            Output, OutputInTransactionV4, OutputPrefixInTransactionV5, OUTPUT_PREFIX_SIZE,
            OUTPUT_SIZE,
        },
        spend::{
            Spend, SpendPrefixInTransactionV5, ANCHOR_PER_SPEND_SIZE,
            SHARED_ANCHOR_SPEND_PREFIX_SIZE, SHARED_ANCHOR_SPEND_SIZE,
        },
        PerSpendAnchor, SharedAnchor,
    },
    serialization::{tests::max_allocation_is_big_enough, TrustedPreallocate, ZcashSerialize},
};

use proptest::prelude::*;
use std::cmp::max;

proptest! {
    /// Confirm that each `Spend<PerSpendAnchor>` takes exactly
    /// ANCHOR_PER_SPEND_SIZE bytes when serialized.
    ///
    /// This verifies that our calculated [`TrustedPreallocate::max_allocation`]
    /// is indeed an upper bound.
    #[test]
    fn anchor_per_spend_size_is_small_enough(spend in Spend::<PerSpendAnchor>::arbitrary_with(())) {
        let serialized = spend.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        prop_assert!(serialized.len() as u64 == ANCHOR_PER_SPEND_SIZE)
    }

    /// Confirm that each `Spend<SharedAnchor>` takes exactly SHARED_SPEND_SIZE
    /// bytes when serialized.
    #[test]
    fn shared_anchor_spend_size_is_small_enough(spend in Spend::<SharedAnchor>::arbitrary_with(())) {
        let (prefix, zkproof, spend_auth_sig) = spend.into_v5_parts();
        let mut serialized_len = prefix.zcash_serialize_to_vec().expect("Serialization to vec must succeed").len();
        serialized_len += zkproof.zcash_serialize_to_vec().expect("Serialization to vec must succeed").len();
        serialized_len += spend_auth_sig.zcash_serialize_to_vec().expect("Serialization to vec must succeed").len();
        prop_assert!(serialized_len as u64 == SHARED_ANCHOR_SPEND_SIZE)
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
        ) = max_allocation_is_big_enough(spend);

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

    /// Verify trusted preallocation for `Spend<SharedAnchor>` and its split fields
    #[test]
    fn shared_spend_max_allocation_is_big_enough(spend in Spend::<SharedAnchor>::arbitrary_with(())) {
        let (prefix, zkproof, spend_auth_sig) = spend.into_v5_parts();
        let (
            smallest_disallowed_vec_len,
            smallest_disallowed_serialized_len,
            largest_allowed_vec_len,
            largest_allowed_serialized_len,
        ) = max_allocation_is_big_enough(prefix);

        // Calculate the actual size of all required Spend fields
        prop_assert!((smallest_disallowed_serialized_len as u64)/SHARED_ANCHOR_SPEND_PREFIX_SIZE*SHARED_ANCHOR_SPEND_SIZE >= MAX_BLOCK_BYTES);
        prop_assert!((largest_allowed_serialized_len as u64)/SHARED_ANCHOR_SPEND_PREFIX_SIZE*SHARED_ANCHOR_SPEND_SIZE <= MAX_BLOCK_BYTES);

        // Now check the serialization limits
        prop_assert!(((smallest_disallowed_vec_len - 1) as u64) == SpendPrefixInTransactionV5::max_allocation());
        prop_assert!((largest_allowed_vec_len as u64) == SpendPrefixInTransactionV5::max_allocation());
        prop_assert!((largest_allowed_serialized_len as u64) <= MAX_BLOCK_BYTES);

        // And check the other fields
        let (
            smallest_disallowed_vec_len,
            _smallest_disallowed_serialized_len,
            largest_allowed_vec_len,
            largest_allowed_serialized_len,
        ) = max_allocation_is_big_enough(zkproof);

        // Proofs are special-cased, because a proof array is deserialized as
        // part of both spends and outputs.
        prop_assert!(((smallest_disallowed_vec_len - 1) as u64) == Groth16Proof::max_allocation());
        prop_assert!((largest_allowed_vec_len as u64) == Groth16Proof::max_allocation());
        prop_assert!((largest_allowed_serialized_len as u64) <= MAX_BLOCK_BYTES);

        // Regardless of where they are deserialized, proofs must not exceed the
        // greatest upper bound across spends and outputs.
        prop_assert!((largest_allowed_vec_len as u64) <= max(SpendPrefixInTransactionV5::max_allocation(), OutputPrefixInTransactionV5::max_allocation()));


        let (
            smallest_disallowed_vec_len,
            _smallest_disallowed_serialized_len,
            largest_allowed_vec_len,
            largest_allowed_serialized_len,
        ) = max_allocation_is_big_enough(spend_auth_sig);

        prop_assert!(((smallest_disallowed_vec_len - 1) as u64) == SpendPrefixInTransactionV5::max_allocation());
        prop_assert!((largest_allowed_vec_len as u64) == SpendPrefixInTransactionV5::max_allocation());
        prop_assert!((largest_allowed_serialized_len as u64) <= MAX_BLOCK_BYTES);
    }
}

proptest! {
    /// Confirm that each output takes exactly OUTPUT_SIZE bytes when serialized
    /// in a V4 or V5 transaction.
    ///
    /// This verifies that our calculated [`TrustedPreallocate::max_allocation`]
    /// is indeed an upper bound.
    #[test]
    fn output_size_is_small_enough(output in Output::arbitrary_with(())) {
        let v4_serialized = output.clone().into_v4().zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        prop_assert!(v4_serialized.len() as u64 == OUTPUT_SIZE);

        let (prefix, zkproof) = output.into_v5_parts();
        let mut v5_serialized_len = prefix.zcash_serialize_to_vec().expect("Serialization to vec must succeed").len();
        v5_serialized_len += zkproof.zcash_serialize_to_vec().expect("Serialization to vec must succeed").len();
        prop_assert!(v5_serialized_len as u64 == OUTPUT_SIZE)
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Verify that...
    /// 1. The smallest disallowed vector of `Outputs`s is too large to fit in a Zcash block
    /// 2. The largest allowed vector is small enough to fit in a legal Zcash block
    ///
    /// when serialized in a V4 or V5 transaction.
    #[test]
    fn output_max_allocation_is_big_enough(output in Output::arbitrary_with(())) {

        let (
            smallest_disallowed_vec_len,
            smallest_disallowed_serialized_len,
            largest_allowed_vec_len,
            largest_allowed_serialized_len,
        ) = max_allocation_is_big_enough(output.clone().into_v4());

        // Check that our smallest_disallowed_vec is only one item larger than the limit
        prop_assert!(((smallest_disallowed_vec_len - 1) as u64) == OutputInTransactionV4::max_allocation());
        // Check that our smallest_disallowed_vec is too big to send as a protocol message
        // Note that a serialized block always includes at least one byte for the number of transactions,
        // so any serialized Vec<Spend> at least MAX_BLOCK_BYTES long is too large to fit in a block.
        prop_assert!((smallest_disallowed_serialized_len as u64) >= MAX_BLOCK_BYTES);

        // Check that our largest_allowed_vec contains the maximum number of spends
        prop_assert!((largest_allowed_vec_len as u64) == OutputInTransactionV4::max_allocation());
        // Check that our largest_allowed_vec is small enough to send as a protocol message
        prop_assert!((largest_allowed_serialized_len as u64) <= MAX_BLOCK_BYTES);

        let (prefix, zkproof) = output.into_v5_parts();
        let (
            smallest_disallowed_vec_len,
            smallest_disallowed_serialized_len,
            largest_allowed_vec_len,
            largest_allowed_serialized_len,
        ) = max_allocation_is_big_enough(prefix);

        // Calculate the actual size of all required Output fields
        prop_assert!((smallest_disallowed_serialized_len as u64)/OUTPUT_PREFIX_SIZE*OUTPUT_SIZE >= MAX_BLOCK_BYTES);
        prop_assert!((largest_allowed_serialized_len as u64)/OUTPUT_PREFIX_SIZE*OUTPUT_SIZE <= MAX_BLOCK_BYTES);

        // Now check the serialization limits
        prop_assert!(((smallest_disallowed_vec_len - 1) as u64) == OutputPrefixInTransactionV5::max_allocation());
        prop_assert!((largest_allowed_vec_len as u64) == OutputPrefixInTransactionV5::max_allocation());
        prop_assert!((largest_allowed_serialized_len as u64) <= MAX_BLOCK_BYTES);

        // And check the other fields
        let (
            smallest_disallowed_vec_len,
            _smallest_disallowed_serialized_len,
            largest_allowed_vec_len,
            largest_allowed_serialized_len,
        ) = max_allocation_is_big_enough(zkproof);

        // Proofs are special-cased, because a proof array is deserialized as
        // part of both spends and outputs.
        prop_assert!(((smallest_disallowed_vec_len - 1) as u64) == Groth16Proof::max_allocation());
        prop_assert!((largest_allowed_vec_len as u64) == Groth16Proof::max_allocation());
        prop_assert!((largest_allowed_serialized_len as u64) <= MAX_BLOCK_BYTES);

        // Regardless of where they are deserialized, proofs must not exceed the
        // greatest upper bound across spends and outputs.
        prop_assert!((largest_allowed_vec_len as u64) <= max(SpendPrefixInTransactionV5::max_allocation(), OutputPrefixInTransactionV5::max_allocation()));
    }
}
