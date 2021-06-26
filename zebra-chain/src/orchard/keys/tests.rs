#![allow(clippy::module_inception)]

use super::*;
use crate::orchard::test_vectors;

use proptest::prelude::*;

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

#[test]
fn generate_keys_from_test_vectors() {
    use std::panic;

    zebra_test::init();

    for (i, test_vector) in test_vectors::TEST_VECTORS.iter().enumerate() {
        println!("\nOrchard test vector: {:?}", i);

        let spending_key = SpendingKey::from_bytes(test_vector.sk, Network::Mainnet);
        println!("sk: {:x?}", spending_key);

        let spend_authorizing_key = SpendAuthorizingKey::from(spending_key);
        println!("ask: {:x?}", spend_authorizing_key);
        panic::catch_unwind(|| {
            assert_eq!(spend_authorizing_key, test_vector.ask);
        });

        let spend_validating_key = SpendValidatingKey::from(spend_authorizing_key);
        println!("ak: {:x?}", spend_validating_key);
        panic::catch_unwind(|| {
            assert_eq!(<[u8; 32]>::from(spend_validating_key), test_vector.ak);
        });

        let nullifier_deriving_key = NullifierDerivingKey::from(spending_key);
        println!("nk: {:x?}", nullifier_deriving_key);
        assert_eq!(nullifier_deriving_key, test_vector.nk);

        let ivk_commit_randomness = IvkCommitRandomness::from(spending_key);
        println!("rivk: {:x?}", ivk_commit_randomness);
        assert_eq!(ivk_commit_randomness, test_vector.rivk);

        let full_viewing_key = FullViewingKey {
            spend_validating_key,
            nullifier_deriving_key,
            ivk_commit_randomness,
        };

        let diversifier_key = DiversifierKey::from(full_viewing_key);
        println!("dk: {:x?}", <[u8; 32]>::from(diversifier_key));
        panic::catch_unwind(|| {
            assert_eq!(diversifier_key, test_vector.dk);
        });

        let incoming_viewing_key = IncomingViewingKey::from(full_viewing_key);
        println!("ivk: {:x?}", incoming_viewing_key);
        panic::catch_unwind(|| {
            assert_eq!(<[u8; 32]>::from(incoming_viewing_key.ivk), test_vector.ivk);
        });

        let outgoing_viewing_key = OutgoingViewingKey::from(full_viewing_key);
        println!("ovk: {:x?}", outgoing_viewing_key);
        panic::catch_unwind(|| {
            assert_eq!(outgoing_viewing_key, test_vector.ovk);
        });

        let diversifier = Diversifier::from(diversifier_key);
        println!("default_d: {:x?}", diversifier);
        panic::catch_unwind(|| {
            assert_eq!(diversifier, test_vector.default_d);
        });

        let transmission_key = TransmissionKey::from((incoming_viewing_key, diversifier));
        println!("default_pk_d: {:x?}", <[u8; 32]>::from(transmission_key));
        panic::catch_unwind(|| {
            assert_eq!(transmission_key, test_vector.default_pk_d);
        });
    }
}

proptest! {

    #[test]
    #[allow(clippy::clone_on_copy, clippy::cmp_owned)]
    fn generate_keys(spending_key in any::<SpendingKey>()) {
        zebra_test::init();

        // Test ConstantTimeEq, Eq, PartialEq
        assert!(spending_key == SpendingKey::from_bytes(spending_key.bytes, spending_key.network));

        let spend_authorizing_key = SpendAuthorizingKey::from(spending_key);
        // Test ConstantTimeEq, Eq, PartialEq
        assert!(spend_authorizing_key == spend_authorizing_key.clone());

        // ConstantTimeEq not implemented as it's a public value
        let spend_validating_key = SpendValidatingKey::from(spend_authorizing_key);

        let nullifier_deriving_key = NullifierDerivingKey::from(spending_key);
        // Test ConstantTimeEq, Eq, PartialEq
        assert!(nullifier_deriving_key == nullifier_deriving_key.clone());

        let ivk_commit_randomness = IvkCommitRandomness::from(spending_key);
        // Test ConstantTimeEq, Eq, PartialEq
        assert!(ivk_commit_randomness == ivk_commit_randomness.clone());

        let full_viewing_key = FullViewingKey {
            spend_validating_key,
            nullifier_deriving_key,
            ivk_commit_randomness,
        };
        // Test ConstantTimeEq, Eq, PartialEq
        assert!(full_viewing_key == full_viewing_key.clone());

        let diversifier_key = DiversifierKey::from(full_viewing_key);
        // Test ConstantTimeEq, Eq, PartialEq
        assert!(diversifier_key == diversifier_key.clone());

        let incoming_viewing_key = IncomingViewingKey::from(full_viewing_key);
        // Test ConstantTimeEq, Eq, PartialEq
        assert!(incoming_viewing_key == incoming_viewing_key.clone());

        let outgoing_viewing_key = OutgoingViewingKey::from(full_viewing_key);
        // Test ConstantTimeEq, Eq, PartialEq
        assert!(outgoing_viewing_key == outgoing_viewing_key.clone());

        // ConstantTimeEq not implemented for Diversifier as it's a public value
        let diversifier = Diversifier::from(diversifier_key);

        // ConstantTimeEq not implemented as it's a public value
        let _transmission_key = TransmissionKey::from((incoming_viewing_key, diversifier));


    }
}
