//! Tests for format-aware Orchard flag-byte parsing (the NU6.3 `enableCrossAddress` bit).

use crate::{orchard::Flags, serialization::ZcashDeserialize};

use crate::orchard::FlagsV6;

/// In the pre-NU6.3 format (the default `Flags` codec, v5 Orchard bundles), only bits 0..1 are
/// valid; bits 2..7 are reserved and rejected.
#[test]
fn pre_nu6_3_format_rejects_reserved_bits() {
    // Valid combinations of the two defined pre-NU6.3 flags.
    for byte in 0u8..=0b011 {
        assert!(
            Flags::zcash_deserialize(&[byte][..]).is_ok(),
            "pre-NU6.3 flags byte {byte:#05b} must parse"
        );
    }

    // Bit 2 (`enableCrossAddress`) is reserved before NU6.3 and MUST be rejected.
    assert!(
        Flags::zcash_deserialize(&[0b100][..]).is_err(),
        "pre-NU6.3 must reject the enableCrossAddress bit"
    );

    // Any higher reserved bit is rejected too.
    assert!(Flags::zcash_deserialize(&[0b1000][..]).is_err());
    assert!(Flags::zcash_deserialize(&[0xFF][..]).is_err());
}

/// In the NU6.3 format (the `FlagsV6` codec, v6 Orchard and Ironwood bundles), bit 2 is the valid
/// `enableCrossAddress` flag; bits 3..7 stay reserved.
#[test]
fn nu6_3_format_accepts_cross_address_but_rejects_higher_bits() {
    // Bits 0..2 are all valid in the NU6.3 format.
    for byte in 0u8..=0b111 {
        let flags = Flags::from(
            FlagsV6::zcash_deserialize(&[byte][..])
                .expect("NU6.3 flags byte with only bits 0..2 must parse"),
        );
        assert_eq!(flags.bits(), byte);
    }

    // The enableCrossAddress flag round-trips.
    let flags = Flags::from(FlagsV6::zcash_deserialize(&[0b100][..]).expect("valid"));
    assert!(flags.contains(Flags::ENABLE_CROSS_ADDRESS));

    // Bits 3..7 remain reserved even in the NU6.3 format.
    assert!(FlagsV6::zcash_deserialize(&[0b1000][..]).is_err());
    assert!(FlagsV6::zcash_deserialize(&[0xFF][..]).is_err());
}
