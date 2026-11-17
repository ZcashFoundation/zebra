//! Sapling transmission-key decoding regressions.

use crate::sapling::keys::TransmissionKey;

#[test]
fn transmission_key_rejects_invalid_and_noncanonical_points() {
    let _init_guard = zebra_test::init();

    assert!(TransmissionKey::try_from([0xff; 32]).is_err());
    let mut identity = [0; 32];
    identity[0] = 1;
    assert!(TransmissionKey::try_from(identity).is_ok());
    identity[31] = 0x80;
    assert!(TransmissionKey::try_from(identity).is_err());
}
