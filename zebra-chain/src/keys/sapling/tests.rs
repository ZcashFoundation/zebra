use super::*;

#[cfg(test)]
use proptest::prelude::*;

#[cfg(test)]
impl Arbitrary for TransmissionKey {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (any::<SpendingKey>())
            .prop_map(|spending_key| {
                let spend_authorizing_key = SpendAuthorizingKey::from(spending_key);
                let proof_authorizing_key = ProofAuthorizingKey::from(spending_key);

                let authorizing_key = AuthorizingKey::from(spend_authorizing_key);
                let nullifier_deriving_key = NullifierDerivingKey::from(proof_authorizing_key);

                let incoming_viewing_key =
                    IncomingViewingKey::from((authorizing_key, nullifier_deriving_key));

                let diversifier = Diversifier::from(spending_key);

                return Self::from(incoming_viewing_key, diversifier);
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

#[cfg(test)]
mod tests {

    use rand_core::OsRng;

    use super::*;

    #[test]
    fn derive() {
        let spending_key = SpendingKey::new(&mut OsRng);

        let spend_authorizing_key = SpendAuthorizingKey::from(spending_key);
        let proof_authorizing_key = ProofAuthorizingKey::from(spending_key);
        let outgoing_viewing_key = OutgoingViewingKey::from(spending_key);

        let authorizing_key = AuthorizingKey::from(spend_authorizing_key);
        let nullifier_deriving_key = NullifierDerivingKey::from(proof_authorizing_key);
        // "If ivk = 0, discard this key and start over with a new
        // [spending key]."
        // https://zips.z.cash/protocol/protocol.pdf#saplingkeycomponents
        let incoming_viewing_key =
            IncomingViewingKey::from((authorizing_key, nullifier_deriving_key));

        let diversifier = Diversifier::new(&mut OsRng);
        let _transmission_key = TransmissionKey::from(incoming_viewing_key, diversifier);

        let _full_viewing_key = FullViewingKey {
            network: Network::default(),
            authorizing_key,
            nullifier_deriving_key,
            outgoing_viewing_key,
        };
    }

    #[test]
    fn derive_for_each_test_vector() {
        for test_vector in test_vectors::TEST_VECTORS.iter() {
            let spending_key = SpendingKey::from(test_vector.sk);

            let spend_authorizing_key = SpendAuthorizingKey::from(spending_key);
            assert_eq!(spend_authorizing_key.to_bytes(), test_vector.ask);
            let proof_authorizing_key = ProofAuthorizingKey::from(spending_key);
            assert_eq!(proof_authorizing_key.to_bytes(), test_vector.nsk);
            let outgoing_viewing_key = OutgoingViewingKey::from(spending_key);
            assert_eq!(outgoing_viewing_key, test_vector.ovk);

            let authorizing_key = AuthorizingKey::from(spend_authorizing_key);
            assert_eq!(authorizing_key, test_vector.ak);
            let nullifier_deriving_key = NullifierDerivingKey::from(proof_authorizing_key);
            assert_eq!(nullifier_deriving_key.to_bytes(), test_vector.nk);
            let incoming_viewing_key =
                IncomingViewingKey::from((authorizing_key, nullifier_deriving_key));
            assert_eq!(incoming_viewing_key, test_vector.ivk);

            let diversifier = Diversifier::from(spending_key);
            assert_eq!(diversifier, test_vector.default_d);

            let transmission_key = TransmissionKey::from(incoming_viewing_key, diversifier);
            assert_eq!(transmission_key.to_bytes(), test_vector.default_pk_d);

            let _full_viewing_key = FullViewingKey {
                network: Network::default(),
                authorizing_key,
                nullifier_deriving_key,
                outgoing_viewing_key,
            };
        }
    }
}

#[cfg(test)]
proptest! {

    #[test]
    fn string_roundtrips(spending_key in any::<SpendingKey>()) {
        let sk_string = spending_key.to_string();
        let spending_key_2: SpendingKey = sk_string.parse().unwrap();
        prop_assert_eq![spending_key, spending_key_2];

        let spend_authorizing_key = SpendAuthorizingKey::from(spending_key);
        let proof_authorizing_key = ProofAuthorizingKey::from(spending_key);
        let outgoing_viewing_key = OutgoingViewingKey::from(spending_key);

        let authorizing_key = AuthorizingKey::from(spend_authorizing_key);
        let nullifier_deriving_key = NullifierDerivingKey::from(proof_authorizing_key);
        let mut incoming_viewing_key =
            IncomingViewingKey::from((authorizing_key, nullifier_deriving_key));
        incoming_viewing_key.network = spending_key.network;

        let ivk_string = incoming_viewing_key.to_string();
        let incoming_viewing_key_2: IncomingViewingKey = ivk_string.parse().unwrap();
        prop_assert_eq![incoming_viewing_key, incoming_viewing_key_2];

        let full_viewing_key = FullViewingKey {
            network: spending_key.network,
            authorizing_key,
            nullifier_deriving_key,
            outgoing_viewing_key,
        };

        let fvk_string = full_viewing_key.to_string();
        let full_viewing_key_2: FullViewingKey = fvk_string.parse().unwrap();
        prop_assert_eq![full_viewing_key, full_viewing_key_2];

    }
}
