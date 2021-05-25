#![allow(clippy::module_inception)]
use super::*;

#[cfg(test)]
use proptest::prelude::*;

#[cfg(test)]
impl Arbitrary for TransmissionKey {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (any::<SpendingKey>())
            .prop_map(|spending_key| {
                let full_viewing_key = FullViewingKey::from_spending_key(spending_key);

                let diversifier_key = DiversifierKey::from(full_viewing_key);

                let diversifier = Diversifier::from(diversifier_key);
                let incoming_viewing_key = IncomingViewingKey::from(full_viewing_key);

                Self::from((incoming_viewing_key, diversifier))
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

#[cfg(test)]
proptest! {

    #[test]
    fn generate_keys(spending_key in any::<SpendingKey>()) {
        zebra_test::init();

        // Test ConstantTimeEq, Eq, PartialEq
        assert!(spending_key == SpendingKey::from_bytes(spending_key.bytes, spending_key.network));

        let spend_authorizing_key = SpendAuthorizingKey::from(spending_key);
        // Test ConstantTimeEq, Eq, PartialEq
        assert!(spend_authorizing_key == <[u8; 32]>::from(spend_authorizing_key));

        // ConstantTimeEq not implemented as it's a public value
        let spend_validating_key = SpendValidatingKey::from(spend_authorizing_key);

        let nullifier_deriving_key = NullifierDerivingKey::from(spending_key);
        // Test ConstantTimeEq, Eq, PartialEq
        assert!(nullifier_deriving_key == <[u8; 32]>::from(nullifier_deriving_key));

        let ivk_commit_randomness = IvkCommitRandomness::from(spending_key);
         // Test ConstantTimeEq, Eq, PartialEq
        assert!(ivk_commit_randomness == <[u8; 32]>::from(ivk_commit_randomness));

        let full_viewing_key = FullViewingKey {
            network: spending_key.network,
            spend_validating_key,
            nullifier_deriving_key,
            ivk_commit_randomness,
        };

        let diversifier_key = DiversifierKey::from(full_viewing_key);
        // Test ConstantTimeEq, Eq, PartialEq
        assert!(diversifier_key == <[u8; 32]>::from(diversifier_key));

        let incoming_viewing_key = IncomingViewingKey::from(full_viewing_key);
        // Test ConstantTimeEq, Eq, PartialEq
        assert!(incoming_viewing_key ==
                IncomingViewingKey::from_bytes(incoming_viewing_key.scalar.into(),
                                               incoming_viewing_key.network));


        let outgoing_viewing_key = OutgoingViewingKey::from(full_viewing_key);
        // Test ConstantTimeEq, Eq, PartialEq
        assert!(outgoing_viewing_key == <[u8; 32]>::from(outgoing_viewing_key));

        // ConstantTimeEq not implemented for Diversifier as it's a public value
        let diversifier = Diversifier::from(diversifier_key);

        // ConstantTimeEq not implemented as it's a public value
        let _transmission_key = TransmissionKey::from((incoming_viewing_key, diversifier));

    }
}
