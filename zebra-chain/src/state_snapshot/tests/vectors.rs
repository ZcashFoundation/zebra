//! Fixed test vectors for state snapshot commitments.

use crate::{
    block,
    parameters::Network,
    state_snapshot::{ObjectHash, StateSnapshotCommitment},
};

/// The SHA-256 hash of the empty string, from FIPS 180-4's test vectors.
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[test]
fn object_hash_is_sha256_of_the_object() {
    let _init_guard = zebra_test::init();

    let hash = ObjectHash::of(b"");
    assert_eq!(hash.to_string(), EMPTY_SHA256);
    assert_eq!(
        ObjectHash::of(b"abc").to_string(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    );
}

#[test]
fn object_hash_hex_round_trips() {
    let _init_guard = zebra_test::init();

    let hash: ObjectHash = EMPTY_SHA256.parse().expect("valid hex");
    assert_eq!(hash, ObjectHash::of(b""));
    assert_eq!(hash.to_string(), EMPTY_SHA256);
    // `Debug` is the same hex string, so logs and errors show the hash as it is configured.
    assert_eq!(format!("{hash:?}"), EMPTY_SHA256);
    assert_eq!(format!("{:?}", Some(hash)), format!("Some({EMPTY_SHA256})"));

    // The wrong length, and non-hex digits, are rejected.
    assert!(EMPTY_SHA256[..62].parse::<ObjectHash>().is_err());
    assert!(format!("{EMPTY_SHA256}00").parse::<ObjectHash>().is_err());
    assert!(format!("zz{}", &EMPTY_SHA256[2..])
        .parse::<ObjectHash>()
        .is_err());
}

#[test]
fn commitment_serializes_as_height_and_hex_hashes() {
    let _init_guard = zebra_test::init();

    let commitment = StateSnapshotCommitment {
        height: block::Height(3_522_060),
        block_hash: block::Hash([0x11; 32]),
        manifest: ObjectHash([0x22; 32]),
    };

    let json = serde_json::to_value(commitment).expect("serializes");
    assert_eq!(
        json,
        serde_json::json!({
            "height": 3_522_060,
            "hash": "11".repeat(32),
            "manifest": "22".repeat(32),
        }),
    );

    let parsed: StateSnapshotCommitment = serde_json::from_value(json).expect("deserializes");
    assert_eq!(parsed, commitment);

    // Unknown fields are rejected, so a typo in a config file is an error.
    let unknown = serde_json::json!({
        "height": 1,
        "hash": "11".repeat(32),
        "manifest": "22".repeat(32),
        "size": 1,
    });
    assert!(serde_json::from_value::<StateSnapshotCommitment>(unknown).is_err());
}

#[test]
fn no_network_pins_a_commitment_yet() {
    let _init_guard = zebra_test::init();

    for network in Network::iter().chain([Network::new_regtest(Default::default())]) {
        assert_eq!(network.state_snapshot_commitment(), None, "{network}");
    }
}

/// Bytes `0, 1, ..., 31`, which read differently in each byte order.
fn ascending_bytes() -> [u8; 32] {
    std::array::from_fn(|index| u8::try_from(index).expect("32 indexes fit in a byte"))
}

#[test]
fn object_hash_hex_is_in_byte_order_and_case_insensitive() {
    let _init_guard = zebra_test::init();

    let hash = ObjectHash(ascending_bytes());
    let hex = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    assert_eq!(hash.to_string(), hex);
    assert_eq!(hex.parse::<ObjectHash>(), Ok(hash));
    assert_eq!(ObjectHash::from(ascending_bytes()), hash);

    // Uppercase digits parse to the same hash, which displays in lowercase.
    let upper: ObjectHash = hex.to_uppercase().parse().expect("uppercase hex is valid");
    assert_eq!(upper, hash);
    assert_eq!(upper.to_string(), hex);

    // Serde uses the same string.
    assert_eq!(
        serde_json::to_value(hash).expect("serializes"),
        serde_json::json!(hex)
    );
    assert_eq!(
        serde_json::from_value::<ObjectHash>(serde_json::json!(hex)).expect("deserializes"),
        hash,
    );
    assert!(serde_json::from_value::<ObjectHash>(serde_json::json!(&hex[2..])).is_err());
    assert!(serde_json::from_value::<ObjectHash>(serde_json::json!(ascending_bytes())).is_err());

    // Hashes order by their bytes, so sorted lists are in hex order too.
    assert!(ObjectHash([0x00; 32]) < ObjectHash([0x01; 32]));
    assert!(ObjectHash([0xFF; 32]) > hash);
}

#[test]
fn commitment_block_hash_is_in_display_order_and_manifest_in_byte_order() {
    let _init_guard = zebra_test::init();

    let commitment = StateSnapshotCommitment {
        height: block::Height(7),
        block_hash: block::Hash(ascending_bytes()),
        manifest: ObjectHash(ascending_bytes()),
    };

    let forward = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    let reversed = "1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100";
    assert_eq!(commitment.block_hash.to_string(), reversed);

    let json = serde_json::to_value(commitment).expect("serializes");
    assert_eq!(
        json,
        serde_json::json!({
            "height": 7,
            "hash": reversed,
            "manifest": forward,
        }),
    );
    assert_eq!(
        serde_json::from_value::<StateSnapshotCommitment>(json).expect("deserializes"),
        commitment,
    );

    // Each field is required, and must be well-formed.
    for (field, value) in [
        ("height", serde_json::json!("7")),
        ("hash", serde_json::json!(&reversed[2..])),
        ("manifest", serde_json::json!(format!("{forward}00"))),
    ] {
        let mut json = serde_json::to_value(commitment).expect("serializes");
        json[field] = value;
        assert!(
            serde_json::from_value::<StateSnapshotCommitment>(json).is_err(),
            "malformed {field}",
        );

        let mut json = serde_json::to_value(commitment).expect("serializes");
        json.as_object_mut()
            .expect("a commitment is an object")
            .remove(field);
        assert!(
            serde_json::from_value::<StateSnapshotCommitment>(json).is_err(),
            "missing {field}",
        );
    }
}
