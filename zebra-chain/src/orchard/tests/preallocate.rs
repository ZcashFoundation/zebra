//! Tests for trusted preallocation during deserialization.

use crate::{
    block::MAX_BLOCK_BYTES,
    orchard::{shielded_data::AUTHORIZED_ACTION_SIZE, Action, AuthorizedAction},
    primitives::redpallas::{Signature, SpendAuth},
    serialization::{TrustedPreallocate, ZcashSerialize},
};

use proptest::{prelude::*, proptest};
use std::convert::TryInto;

proptest! {
    /// Confirm that each `AuthorizedAction` takes exactly AUTHORIZED_ACTION_SIZE
    /// bytes when serialized.
    #[test]
    fn authorized_action_size_is_small_enough(authorized_action in <AuthorizedAction>::arbitrary_with(())) {
        let (action, spend_auth_sig) = authorized_action.into_parts();
        let mut serialized_len = action.zcash_serialize_to_vec().expect("Serialization to vec must succeed").len();
        serialized_len += spend_auth_sig.zcash_serialize_to_vec().expect("Serialization to vec must succeed").len();
        prop_assert!(serialized_len as u64 == AUTHORIZED_ACTION_SIZE)
    }

    /// Verify trusted preallocation for `AuthorizedAction` and its split fields
    #[test]
    fn authorized_action_max_allocation_is_big_enough(authorized_action in <AuthorizedAction>::arbitrary_with(())) {
        let (action, spend_auth_sig) = authorized_action.into_parts();

        let (
            smallest_disallowed_vec_len,
            _smallest_disallowed_serialized_len,
            largest_allowed_vec_len,
            largest_allowed_serialized_len,
        ) = max_allocation_is_big_enough(action);

        // Check the serialization limits for `Action`
        prop_assert!(((smallest_disallowed_vec_len - 1) as u64) == Action::max_allocation());
        prop_assert!((largest_allowed_vec_len as u64) == Action::max_allocation());
        prop_assert!((largest_allowed_serialized_len as u64) <= MAX_BLOCK_BYTES);

        let (
            smallest_disallowed_vec_len,
            _smallest_disallowed_serialized_len,
            largest_allowed_vec_len,
            largest_allowed_serialized_len,
        ) = max_allocation_is_big_enough(spend_auth_sig);

        // Check the serialization limits for `Signature::<SpendAuth>`
        prop_assert!(((smallest_disallowed_vec_len - 1) as u64) == Signature::<SpendAuth>::max_allocation());
        prop_assert!((largest_allowed_vec_len as u64) == Signature::<SpendAuth>::max_allocation());
        prop_assert!((largest_allowed_serialized_len as u64) <= MAX_BLOCK_BYTES);
    }
}

/// Return the following calculations on `item`:
///   smallest_disallowed_vec_len
///   smallest_disallowed_serialized_len
///   largest_allowed_vec_len
///   largest_allowed_serialized_len
fn max_allocation_is_big_enough<T>(item: T) -> (usize, usize, usize, usize)
where
    T: TrustedPreallocate + ZcashSerialize + Clone,
{
    let max_allocation: usize = T::max_allocation().try_into().unwrap();
    let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
    for _ in 0..(max_allocation + 1) {
        smallest_disallowed_vec.push(item.clone());
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
