#![allow(clippy::unwrap_in_result)]

mod preallocate;
mod prop;

use std::io::Cursor;

use crate::{
    sapling::commitment::ValueCommitment,
    serialization::{
        is_trusted_source, TrustedDeserializationGuard, ZcashDeserialize, ZcashSerialize,
    },
};

/// Test that the serde JSON format for `ValueCommitment` round-trips correctly.
///
/// The old implementation used `#[serde(with = "serde_helpers::ValueCommitment")]` on a
/// newtype tuple struct, which serialized as `{"bytes": [...]}` (transparent newtype +
/// remote derive). The new manual `Serialize`/`Deserialize` impls must produce the same
/// format.
#[test]
fn value_commitment_serde_json_roundtrip() {
    let _init_guard = zebra_test::init();

    // Deserialize a known-valid ValueCommitment from Zcash binary format
    // (this exercises the untrusted path with full validation).
    // These bytes are from the first Sapling output cv in block 419200.
    let cv_bytes: [u8; 32] = [
        0x12, 0x9b, 0x33, 0x76, 0x02, 0xf1, 0x42, 0xda, 0x5e, 0x66, 0x71, 0xc2, 0x36, 0x07,
        0xc6, 0x1c, 0x81, 0x79, 0x67, 0xd4, 0x48, 0x28, 0x43, 0xa0, 0x3c, 0xd6, 0xf1, 0x3e,
        0x6e, 0x4a, 0x09, 0xb4,
    ];

    let vc = ValueCommitment::zcash_deserialize(Cursor::new(&cv_bytes))
        .expect("known-valid ValueCommitment bytes should deserialize");

    // Serialize to JSON
    let json = serde_json::to_string(&vc).expect("ValueCommitment should serialize to JSON");

    // Verify it has the expected structure: {"bytes":[...]}
    let json_value: serde_json::Value =
        serde_json::from_str(&json).expect("JSON should parse");
    assert!(
        json_value.get("bytes").is_some(),
        "JSON must have a 'bytes' field, got: {json}"
    );

    // Deserialize back from JSON
    let vc_parsed: ValueCommitment =
        serde_json::from_str(&json).expect("ValueCommitment should deserialize from JSON");

    assert_eq!(vc, vc_parsed, "serde JSON round-trip should preserve equality");
    assert_eq!(
        vc.to_bytes(),
        vc_parsed.to_bytes(),
        "serde JSON round-trip should preserve bytes"
    );
}

/// Test that the trusted deserialization guard is re-entrant.
#[test]
fn trusted_guard_reentrant() {
    let _init_guard = zebra_test::init();

    assert!(!is_trusted_source());

    let guard1 = TrustedDeserializationGuard::new();
    assert!(is_trusted_source());

    {
        let _guard2 = TrustedDeserializationGuard::new();
        assert!(is_trusted_source());
    }
    // Inner guard dropped, but outer guard still active
    assert!(
        is_trusted_source(),
        "dropping inner guard must not disable trusted mode while outer guard is active"
    );

    drop(guard1);
    assert!(
        !is_trusted_source(),
        "dropping outermost guard must disable trusted mode"
    );
}

/// Test that untrusted deserialization rejects the identity point (all zeros).
#[test]
fn value_commitment_rejects_identity_point() {
    let _init_guard = zebra_test::init();

    let zero_bytes = [0u8; 32];
    let result = ValueCommitment::zcash_deserialize(Cursor::new(&zero_bytes));
    assert!(
        result.is_err(),
        "untrusted deserialization must reject the identity point (all zeros)"
    );
}

/// Test that trusted deserialization of invalid bytes defers the error to try_inner().
#[test]
fn value_commitment_trusted_invalid_bytes_defers_error() {
    let _init_guard = zebra_test::init();

    let zero_bytes = [0u8; 32];
    let vc = {
        let _guard = TrustedDeserializationGuard::new();
        ValueCommitment::zcash_deserialize(Cursor::new(&zero_bytes))
            .expect("trusted deserialization should accept any bytes")
    };

    // Raw bytes should be accessible without error
    assert_eq!(vc.to_bytes(), zero_bytes);

    // But try_inner() should return an error when the bytes are invalid
    assert!(
        vc.try_inner().is_err(),
        "try_inner() must return an error for invalid bytes (identity point)"
    );
}

/// Test that trusted deserialization skips validation and untrusted does not.
#[test]
fn value_commitment_trusted_vs_untrusted() {
    let _init_guard = zebra_test::init();

    // Valid ValueCommitment bytes
    let cv_bytes: [u8; 32] = [
        0x12, 0x9b, 0x33, 0x76, 0x02, 0xf1, 0x42, 0xda, 0x5e, 0x66, 0x71, 0xc2, 0x36, 0x07,
        0xc6, 0x1c, 0x81, 0x79, 0x67, 0xd4, 0x48, 0x28, 0x43, 0xa0, 0x3c, 0xd6, 0xf1, 0x3e,
        0x6e, 0x4a, 0x09, 0xb4,
    ];

    // Untrusted: deserialize with full validation
    let vc_untrusted = ValueCommitment::zcash_deserialize(Cursor::new(&cv_bytes))
        .expect("valid bytes should deserialize in untrusted mode");

    // Trusted: deserialize with deferred validation
    let vc_trusted = {
        let _guard = TrustedDeserializationGuard::new();
        ValueCommitment::zcash_deserialize(Cursor::new(&cv_bytes))
            .expect("valid bytes should deserialize in trusted mode")
    };

    // Both should produce equal results
    assert_eq!(vc_untrusted, vc_trusted);
    assert_eq!(vc_untrusted.to_bytes(), vc_trusted.to_bytes());

    // Both should serialize to the same Zcash binary output
    let bytes_untrusted = vc_untrusted.zcash_serialize_to_vec().unwrap();
    let bytes_trusted = vc_trusted.zcash_serialize_to_vec().unwrap();
    assert_eq!(bytes_untrusted, bytes_trusted);

    // Trusted-deserialized value should lazily validate when inner() is called
    assert_eq!(
        vc_trusted.inner().to_bytes(),
        vc_untrusted.inner().to_bytes(),
        "lazy validation should produce the same inner value"
    );
}
