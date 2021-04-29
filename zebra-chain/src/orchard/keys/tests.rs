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

        let diversifier_key = DiversifierKey::from(full_viewing_key);
        let incoming_viewing_key = IncomingViewingKey::from(full_viewing_key);

        let _outgoing_viewing_key = OutgoingViewingKey::from(full_viewing_key);

        let diversifier = Diversifier::from(diversifier_key);
        let _transmission_key = TransmissionKey::from((incoming_viewing_key, diversifier));

    }
}
