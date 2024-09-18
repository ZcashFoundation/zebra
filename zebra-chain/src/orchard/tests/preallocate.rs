//! Tests for trusted preallocation during deserialization.

use reddsa::{orchard::SpendAuth, Signature};

use crate::{
    block::MAX_BLOCK_BYTES,
    orchard::{Action, AuthorizedAction, OrchardVanilla},
    serialization::{arbitrary::max_allocation_is_big_enough, TrustedPreallocate, ZcashSerialize},
};

use proptest::{prelude::*, proptest};

proptest! {
    /// Confirm that each `AuthorizedAction` takes exactly AUTHORIZED_ACTION_SIZE
    /// bytes when serialized.
    #[test]
    fn authorized_action_size_is_small_enough(authorized_action in <AuthorizedAction<OrchardVanilla>>::arbitrary_with(())) {
        let (action, spend_auth_sig) = authorized_action.into_parts();
        let mut serialized_len = action.zcash_serialize_to_vec().expect("Serialization to vec must succeed").len();
        serialized_len += spend_auth_sig.zcash_serialize_to_vec().expect("Serialization to vec must succeed").len();
        prop_assert!(serialized_len as u64 == AuthorizedAction::<OrchardVanilla>::AUTHORIZED_ACTION_SIZE)
    }

    /// Verify trusted preallocation for `AuthorizedAction` and its split fields
    #[test]
    fn authorized_action_max_allocation_is_big_enough(authorized_action in <AuthorizedAction<OrchardVanilla>>::arbitrary_with(())) {
        let (action, spend_auth_sig) = authorized_action.into_parts();

        let (
            smallest_disallowed_vec_len,
            smallest_disallowed_serialized_len,
            largest_allowed_vec_len,
            largest_allowed_serialized_len,
        ) = max_allocation_is_big_enough(action);

        // Calculate the actual size of all required Action fields
        prop_assert!((smallest_disallowed_serialized_len as u64)/AuthorizedAction::<OrchardVanilla>::ACTION_SIZE*
            AuthorizedAction::<OrchardVanilla>::AUTHORIZED_ACTION_SIZE >= MAX_BLOCK_BYTES);
        prop_assert!((largest_allowed_serialized_len as u64)/AuthorizedAction::<OrchardVanilla>::ACTION_SIZE*
            AuthorizedAction::<OrchardVanilla>::AUTHORIZED_ACTION_SIZE <= MAX_BLOCK_BYTES);

        // Check the serialization limits for `Action`
        prop_assert!(((smallest_disallowed_vec_len - 1) as u64) == Action::<OrchardVanilla>::max_allocation());
        prop_assert!((largest_allowed_vec_len as u64) == Action::<OrchardVanilla>::max_allocation());
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
