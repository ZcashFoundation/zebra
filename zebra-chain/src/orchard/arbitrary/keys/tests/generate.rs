//! Key and address gewneration tests for Orchard.

use rand_core::OsRng;

use crate::{
    orchard::{address::Address, arbitrary::keys::*, keys::*},
    parameters::Network,
};

#[test]
fn derive_keys_and_addresses() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;

    let spending_key = SpendingKey::new(&mut OsRng, network);

    let full_viewing_key = FullViewingKey::from(spending_key);

    // Default diversifier, where index = 0.
    let diversifier_key = DiversifierKey::from(full_viewing_key);

    // This should fail with negligible probability.
    let incoming_viewing_key =
        IncomingViewingKey::try_from(full_viewing_key).expect("a valid incoming viewing key");

    let diversifier = Diversifier::from(diversifier_key);
    let transmission_key = TransmissionKey::from((incoming_viewing_key, diversifier));

    let _orchard_shielded_address = Address {
        diversifier,
        transmission_key,
    };
}
