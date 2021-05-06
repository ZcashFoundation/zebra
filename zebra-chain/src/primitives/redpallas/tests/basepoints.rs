use group::GroupEncoding;
use halo2::pasta::{arithmetic::CurveExt, pallas};

use super::super::constants;

#[test]
fn orchard_spendauth_basepoint() {
    assert_eq!(
        // An instance of _GroupHash^P_
        pallas::Point::hash_to_curve("z.cash:Orchard")(b"G").to_bytes(),
        constants::SPENDAUTHSIG_BASEPOINT_BYTES
    );
}

#[test]
fn orchard_binding_basepoint() {
    assert_eq!(
        // An instance of _GroupHash^P_
        pallas::Point::hash_to_curve("z.cash:Orchard-cv")(b"r").to_bytes(),
        constants::BINDINGSIG_BASEPOINT_BYTES
    );
}
