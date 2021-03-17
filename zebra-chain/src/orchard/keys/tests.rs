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
                let full_viewing_key = FullViewingKey::from(spending_key);

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
    fn string_roundtrips(spending_key in any::<SpendingKey>()) {
        zebra_test::init();

        let sk_string = spending_key.to_string();
        let spending_key_2: SpendingKey = sk_string.parse().unwrap();
        prop_assert_eq![spending_key, spending_key_2];

        let spend_authorizing_key = SpendAuthorizingKey::from(spending_key);

        let spend_validating_key = SpendValidatingKey::from(spend_authorizing_key);
        let nullifier_deriving_key = NullifierDerivingKey::from(spending_key);
        let ivk_commit_randomness = IvkCommitRandomness::from(spending_key);

        let full_viewing_key = FullViewingKey {
            network: spending_key.network,
            spend_validating_key,
            nullifier_deriving_key,
            ivk_commit_randomness,
        };

        let fvk_string = full_viewing_key.to_string();
        let full_viewing_key_2: FullViewingKey = fvk_string.parse().unwrap();
        prop_assert_eq![full_viewing_key, full_viewing_key_2];

        let diversifier_key = DiversifierKey::from(full_viewing_key);

        let mut incoming_viewing_key = IncomingViewingKey::from(full_viewing_key);
        incoming_viewing_key.network = spending_key.network;

        let ivk_string = incoming_viewing_key.to_string();
        let incoming_viewing_key_2: IncomingViewingKey = ivk_string.parse().unwrap();
        prop_assert_eq![incoming_viewing_key, incoming_viewing_key_2];

        let _outgoing_viewing_key = OutgoingViewingKey::from(full_viewing_key);

        let diversifier = Diversifier::from(diversifier_key);
        let _transmission_key = TransmissionKey::from((incoming_viewing_key, diversifier));

    }
}
