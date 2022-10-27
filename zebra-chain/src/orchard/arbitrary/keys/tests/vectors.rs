//! Fixed test vectors for Orchard keys.

use crate::orchard::{arbitrary::keys::*, tests::vectors::KEY_COMPONENTS};

#[test]
fn generate_keys_from_test_vectors() {
    let _init_guard = zebra_test::init();

    for test_vector in KEY_COMPONENTS.iter() {
        let spending_key = SpendingKey::from_bytes(test_vector.sk, Network::Mainnet);

        let spend_authorizing_key = SpendAuthorizingKey::from(spending_key);
        assert_eq!(spend_authorizing_key, test_vector.ask);

        let spend_validating_key = SpendValidatingKey::from(spend_authorizing_key);
        assert_eq!(<[u8; 32]>::from(spend_validating_key), test_vector.ak);

        let nullifier_deriving_key = NullifierDerivingKey::from(spending_key);
        assert_eq!(nullifier_deriving_key, test_vector.nk);

        let ivk_commit_randomness = IvkCommitRandomness::from(spending_key);
        assert_eq!(ivk_commit_randomness, test_vector.rivk);

        let full_viewing_key = FullViewingKey {
            spend_validating_key,
            nullifier_deriving_key,
            ivk_commit_randomness,
        };

        let diversifier_key = DiversifierKey::from(full_viewing_key);
        assert_eq!(diversifier_key, test_vector.dk);

        let incoming_viewing_key =
            IncomingViewingKey::try_from(full_viewing_key).expect("a valid incoming viewing key");
        assert_eq!(<[u8; 32]>::from(incoming_viewing_key.ivk), test_vector.ivk);

        let outgoing_viewing_key = OutgoingViewingKey::from(full_viewing_key);
        assert_eq!(outgoing_viewing_key, test_vector.ovk);

        let diversifier = Diversifier::from(diversifier_key);
        assert_eq!(diversifier, test_vector.default_d);

        let transmission_key = TransmissionKey::from((incoming_viewing_key, diversifier));
        assert_eq!(transmission_key, test_vector.default_pk_d);
    }
}
