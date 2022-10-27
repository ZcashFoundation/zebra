//! Randomised property tests for Orchard keys.

use proptest::prelude::*;

use crate::orchard::arbitrary::keys::*;

proptest! {

    #[test]
    #[allow(clippy::clone_on_copy, clippy::cmp_owned)]
    fn generate_keys(spending_key in any::<SpendingKey>()) {
        let _init_guard = zebra_test::init();

        // Test ConstantTimeEq, Eq, PartialEq
        assert_eq!(spending_key, SpendingKey::from_bytes(spending_key.bytes, spending_key.network()));

        let spend_authorizing_key = SpendAuthorizingKey::from(spending_key);
        // Test ConstantTimeEq, Eq, PartialEq
        assert_eq!(spend_authorizing_key, spend_authorizing_key.clone());

        // ConstantTimeEq not implemented as it's a public value
        let spend_validating_key = SpendValidatingKey::from(spend_authorizing_key);

        let nullifier_deriving_key = NullifierDerivingKey::from(spending_key);
        // Test ConstantTimeEq, Eq, PartialEq
        assert_eq!(nullifier_deriving_key, nullifier_deriving_key.clone());

        let ivk_commit_randomness = IvkCommitRandomness::from(spending_key);
        // Test ConstantTimeEq, Eq, PartialEq
        assert_eq!(ivk_commit_randomness, ivk_commit_randomness.clone());

        let full_viewing_key = FullViewingKey {
            spend_validating_key,
            nullifier_deriving_key,
            ivk_commit_randomness,
        };
        // Test ConstantTimeEq, Eq, PartialEq
        assert_eq!(full_viewing_key, full_viewing_key.clone());

        let diversifier_key = DiversifierKey::from(full_viewing_key);
        // Test ConstantTimeEq, Eq, PartialEq
        assert_eq!(diversifier_key, diversifier_key.clone());

        let incoming_viewing_key = IncomingViewingKey::try_from(full_viewing_key).expect("a valid incoming viewing key");
        // Test ConstantTimeEq, Eq, PartialEq
        assert_eq!(incoming_viewing_key, incoming_viewing_key.clone());

        let outgoing_viewing_key = OutgoingViewingKey::from(full_viewing_key);
        // Test ConstantTimeEq, Eq, PartialEq
        assert_eq!(outgoing_viewing_key, outgoing_viewing_key.clone());

        // ConstantTimeEq not implemented for Diversifier as it's a public value
        let diversifier = Diversifier::from(diversifier_key);

        // ConstantTimeEq not implemented as it's a public value
        let _transmission_key = TransmissionKey::from((incoming_viewing_key, diversifier));

    }
}
