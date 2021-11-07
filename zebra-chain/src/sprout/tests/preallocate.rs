//! Tests for trusted preallocation during deserialization.

use proptest::{prelude::*, proptest};

use crate::{
    block::MAX_BLOCK_BYTES,
    primitives::{Bctv14Proof, Groth16Proof},
    serialization::{arbitrary::max_allocation_is_big_enough, TrustedPreallocate, ZcashSerialize},
    sprout::joinsplit::{JoinSplit, BCTV14_JOINSPLIT_SIZE, GROTH16_JOINSPLIT_SIZE},
};

proptest! {
    /// Confirm that each JoinSplit<Btcv14Proof> takes exactly BCTV14_JOINSPLIT_SIZE bytes when serialized.
    /// This verifies that our calculated [`TrustedPreallocate::max_allocation`] is indeed an upper bound.
    #[test]
    fn joinsplit_btcv14_size_is_correct(joinsplit in <JoinSplit<Bctv14Proof>>::arbitrary_with(())) {
        let serialized = joinsplit.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
        prop_assert!(serialized.len() as u64 == BCTV14_JOINSPLIT_SIZE)
    }

    /// Confirm that each  JoinSplit<Btcv14Proof> takes exactly  GROTH16_JOINSPLIT_SIZE bytes when serialized.
    /// This verifies that our calculated [`TrustedPreallocate::max_allocation`] is indeed an upper bound.
    #[test]
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
        let (
            smallest_disallowed_vec_len,
            smallest_disallowed_serialized_len,
            largest_allowed_vec_len,
            largest_allowed_serialized_len,
        ) = max_allocation_is_big_enough(joinsplit);

        // Check that our smallest_disallowed_vec is only one item larger than the limit
        assert!(((smallest_disallowed_vec_len - 1) as u64) == <JoinSplit<Bctv14Proof>>::max_allocation());
        // Check that our smallest_disallowed_vec is too big to fit in a valid Zcash Block.
        assert!(smallest_disallowed_serialized_len as u64 > MAX_BLOCK_BYTES);

        // Check that our largest_allowed_vec contains the maximum number of JoinSplits
        assert!((largest_allowed_vec_len as u64) == <JoinSplit<Bctv14Proof>>::max_allocation());
        // Check that our largest_allowed_vec is small enough to fit in a Zcash Block.
        assert!(largest_allowed_serialized_len as u64 <= MAX_BLOCK_BYTES);
    }

    /// Verify that...
    /// 1. The smallest disallowed vector of `JoinSplit<Groth16Proof>`s is too large to fit in a Zcash block
    /// 2. The largest allowed vector is small enough to fit in a legal Zcash block
    #[test]
    fn joinsplit_groth16_max_allocation_is_correct(joinsplit in <JoinSplit<Groth16Proof>>::arbitrary_with(())) {

        let (
            smallest_disallowed_vec_len,
            smallest_disallowed_serialized_len,
            largest_allowed_vec_len,
            largest_allowed_serialized_len,
        ) = max_allocation_is_big_enough(joinsplit);

        // Check that our smallest_disallowed_vec is only one item larger than the limit
        assert!(((smallest_disallowed_vec_len - 1) as u64) == <JoinSplit<Groth16Proof>>::max_allocation());
        // Check that our smallest_disallowed_vec is too big to fit in a valid Zcash Block.
        assert!(smallest_disallowed_serialized_len as u64 > MAX_BLOCK_BYTES);

        // Check that our largest_allowed_vec contains the maximum number of JoinSplits
        assert!((largest_allowed_vec_len as u64) == <JoinSplit<Groth16Proof>>::max_allocation());
        // Check that our largest_allowed_vec is small enough to fit in a Zcash Block.
        assert!(largest_allowed_serialized_len as u64 <= MAX_BLOCK_BYTES);
    }
}
