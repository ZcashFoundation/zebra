//! Fixed test vectors for state snapshot commitments.

use crate::{
    block,
    parameters::{
        testnet::{
            ConfiguredActivationHeights, ConfiguredCheckpoints, ConfiguredFundingStreams,
            RegtestParameters,
        },
        Network,
    },
    state_snapshot::{
        ObjectHash, StateSnapshotCommitment, MAINNET_STATE_SNAPSHOT_BLOCK_HASH,
        MAINNET_STATE_SNAPSHOT_HEIGHT, MAINNET_STATE_SNAPSHOT_MANIFEST,
    },
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

/// The manifest hash of the Mainnet snapshot at 3,522,060, as the export
/// printed it.
const MAINNET_MANIFEST_HEX: &str =
    "08e3821f8e4e39a75b347590d94513d4bbe27e704e908e0a2e306c798b9f3eed";

/// Only Mainnet pins a commitment: the pinned height, block hash and manifest.
#[test]
fn only_mainnet_pins_a_commitment() {
    let _init_guard = zebra_test::init();

    for network in Network::iter().chain([Network::new_regtest(Default::default())]) {
        let pinned = network.state_snapshot_commitment();
        if network == Network::Mainnet {
            let pinned = pinned.expect("Mainnet pins a state snapshot commitment");
            assert_eq!(pinned.height, MAINNET_STATE_SNAPSHOT_HEIGHT);
            assert_eq!(
                pinned.block_hash.to_string(),
                MAINNET_STATE_SNAPSHOT_BLOCK_HASH
            );
            assert_eq!(Some(pinned.manifest), MAINNET_STATE_SNAPSHOT_MANIFEST);
        } else {
            assert_eq!(pinned, None, "{network}");
        }
    }
}

/// The pinned Mainnet snapshot block is the checkpoint at its height, and the
/// block after it is at or below the last checkpoint, which the import
/// preconditions require.
#[test]
fn the_pinned_mainnet_snapshot_block_is_a_checkpoint() {
    let _init_guard = zebra_test::init();

    let checkpoints = Network::Mainnet.checkpoint_list();
    let hash: block::Hash = MAINNET_STATE_SNAPSHOT_BLOCK_HASH.parse().unwrap();
    assert_eq!(checkpoints.hash(MAINNET_STATE_SNAPSHOT_HEIGHT), Some(hash));
    assert!(
        (MAINNET_STATE_SNAPSHOT_HEIGHT + 1).unwrap() <= checkpoints.max_height(),
        "the block after the pinned snapshot is checkpoint-verified"
    );
    assert!(
        crate::parameters::NetworkUpgrade::Nu5
            .activation_height(&Network::Mainnet)
            .is_some_and(|nu5| nu5 <= MAINNET_STATE_SNAPSHOT_HEIGHT),
        "NU5 is active at the pinned snapshot height"
    );
}

/// The pinned Mainnet manifest is the hash the export printed, in the byte
/// order commitments display and parse it in.
#[test]
fn the_pinned_mainnet_manifest_is_the_exported_one() {
    let _init_guard = zebra_test::init();

    let manifest = MAINNET_STATE_SNAPSHOT_MANIFEST.expect("the Mainnet manifest is pinned");
    assert_eq!(manifest.to_string(), MAINNET_MANIFEST_HEX);
    assert_eq!(MAINNET_MANIFEST_HEX.parse::<ObjectHash>(), Ok(manifest));
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

/// A Regtest network with funding streams that have several recipients, so the
/// recipients' hash map has more than one possible iteration order.
fn regtest_with_funding_streams(checkpoints: Option<ConfiguredCheckpoints>) -> Network {
    let default_testnet = Network::new_default_testnet();
    let funding_streams = default_testnet
        .all_funding_streams()
        .iter()
        .map(|streams| {
            let mut configured = ConfiguredFundingStreams::from(streams);
            configured.height_range = Some(
                streams.height_range().start
                    ..(streams.height_range().start + 20).expect("the height is valid"),
            );
            configured
        })
        .collect();

    Network::new_regtest(RegtestParameters {
        activation_heights: (&default_testnet.activation_list()).into(),
        funding_streams: Some(funding_streams),
        checkpoints,
        ..Default::default()
    })
}

#[test]
fn parameters_hash_identifies_custom_networks_by_their_consensus_parameters() {
    let _init_guard = zebra_test::init();

    // Mainnet and the default Testnet are identified by their genesis hash.
    assert_eq!(Network::Mainnet.state_snapshot_parameters_hash(), None);
    assert_eq!(
        Network::new_default_testnet().state_snapshot_parameters_hash(),
        None
    );

    let regtest = Network::new_regtest(Default::default())
        .state_snapshot_parameters_hash()
        .expect("Regtest carries a parameters hash");
    assert_eq!(
        Network::new_regtest(Default::default()).state_snapshot_parameters_hash(),
        Some(regtest),
        "the hash only depends on the parameters",
    );

    // Funding stream recipients are kept in a hash map, so equal parameters built separately can
    // iterate in different orders, and must still hash the same.
    let with_streams = regtest_with_funding_streams(None)
        .state_snapshot_parameters_hash()
        .expect("Regtest carries a parameters hash");
    assert!(
        regtest_with_funding_streams(None)
            .all_funding_streams()
            .iter()
            .any(|streams| streams.recipients().len() > 1),
        "the test needs a stream with several recipients",
    );
    for _ in 0..8 {
        assert_eq!(
            regtest_with_funding_streams(None).state_snapshot_parameters_hash(),
            Some(with_streams),
        );
    }
    assert_ne!(
        with_streams, regtest,
        "funding streams are consensus parameters"
    );

    // Checkpoints are not consensus parameters.
    let checkpoints = ConfiguredCheckpoints::HeightsAndHashes(vec![
        (
            block::Height(0),
            Network::new_regtest(Default::default()).genesis_hash(),
        ),
        (block::Height(1), block::Hash([0x42; 32])),
    ]);
    assert_eq!(
        regtest_with_funding_streams(Some(checkpoints)).state_snapshot_parameters_hash(),
        Some(with_streams),
    );

    // Activation heights are.
    let later_nu5 = Network::new_regtest(
        ConfiguredActivationHeights {
            nu5: Some(100),
            ..Default::default()
        }
        .into(),
    )
    .state_snapshot_parameters_hash()
    .expect("Regtest carries a parameters hash");
    let earlier_nu5 = Network::new_regtest(
        ConfiguredActivationHeights {
            nu5: Some(10),
            ..Default::default()
        }
        .into(),
    )
    .state_snapshot_parameters_hash()
    .expect("Regtest carries a parameters hash");
    assert_ne!(later_nu5, earlier_nu5);
    assert_ne!(later_nu5, regtest);

    // So is the coinbase spending rule.
    let shielded_coinbase = Network::new_regtest(RegtestParameters {
        should_allow_unshielded_coinbase_spends: Some(false),
        ..Default::default()
    })
    .state_snapshot_parameters_hash()
    .expect("Regtest carries a parameters hash");
    assert_ne!(shielded_coinbase, regtest);
}

/// The hashes of the default Regtest parameters, and of a Regtest with the
/// default Testnet's activation heights and funding streams, pinned so that a
/// change to the encoding, or to those parameters, is noticed: an existing
/// manifest's hash stops matching its network's, and the encoding version in
/// `PARAMETERS_HASH_DOMAIN` must change with the encoding.
#[test]
fn regtest_parameters_hashes_are_pinned() {
    let _init_guard = zebra_test::init();

    let hash = Network::new_regtest(Default::default())
        .state_snapshot_parameters_hash()
        .expect("Regtest carries a parameters hash");
    assert_eq!(
        hex::encode(hash),
        "26359074319fec7be15c45a63150907624b1c388110fd816020c7bcc860e955b",
    );

    // With every network upgrade's activation height, and funding stream addresses.
    let with_streams = regtest_with_funding_streams(None)
        .state_snapshot_parameters_hash()
        .expect("Regtest carries a parameters hash");
    assert_eq!(
        hex::encode(with_streams),
        "a7ae9d0c1a290b72d56ea4756b0454690dfdf28a3dff97d2a638a7b89a6c5353",
    );
}
