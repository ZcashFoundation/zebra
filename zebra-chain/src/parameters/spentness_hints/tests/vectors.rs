//! Fixed test vectors for spentness-hint artifacts.

use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::{
    block::{self, Height},
    parameters::{
        spentness_hints::{
            artifact_len, ArtifactEncoder, MaxCheckpoint, Mode, ParsedArtifact,
            SpentnessHintsError, VerifiedArtifact, CHAIN_IDENTITY_OFFSET, FORMAT_VERSION,
            HEADER_LEN, MAX_ARTIFACT_LEN, OUTPUT_COUNT_OFFSET, TERMINAL_HASH_OFFSET,
            TERMINAL_HEIGHT_OFFSET, VERSION_OFFSET,
        },
        Network,
    },
    state_snapshot::ObjectHash,
};

/// The vector's terminal height.
const TERMINAL_HEIGHT: Height = Height(1_000_000);

/// The vector's terminal block hash.
const TERMINAL_HASH: block::Hash = block::Hash([0x22; 32]);

/// A valid 10-bit Mainnet artifact: outputs 3 and 8 retained, all others spent.
fn valid_artifact_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"ZKSHINT\0");
    bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&Network::Mainnet.genesis_hash().0);
    bytes.extend_from_slice(&TERMINAL_HEIGHT.0.to_le_bytes());
    bytes.extend_from_slice(&TERMINAL_HASH.0);
    bytes.extend_from_slice(&10u64.to_le_bytes());
    bytes.extend_from_slice(&[0b0000_1000, 0b0000_0001]);
    bytes
}

/// The pin that authenticates [`valid_artifact_bytes`].
fn valid_pin() -> MaxCheckpoint {
    MaxCheckpoint {
        height: TERMINAL_HEIGHT,
        hash: TERMINAL_HASH,
        spentness_hash: Sha256::digest(valid_artifact_bytes()).into(),
    }
}

fn parse(bytes: Vec<u8>) -> Result<ParsedArtifact, SpentnessHintsError> {
    ParsedArtifact::parse(Arc::from(bytes))
}

fn verified() -> VerifiedArtifact {
    parse(valid_artifact_bytes())
        .expect("hand-built vector satisfies every format rule")
        .verify(&Network::Mainnet, &valid_pin())
        .expect("the pin is computed from the vector")
}

#[test]
fn parses_valid_artifact() {
    let _init_guard = zebra_test::init();

    let bytes = valid_artifact_bytes();
    assert_eq!(bytes.len(), HEADER_LEN + 2);

    let parsed = parse(bytes.clone()).expect("hand-built vector satisfies every format rule");
    let header = *parsed.header();
    assert_eq!(header.chain_identity, Network::Mainnet.genesis_hash());
    assert_eq!(header.terminal_height, TERMINAL_HEIGHT);
    assert_eq!(header.terminal_hash, TERMINAL_HASH);
    assert_eq!(header.output_count, 10);

    let verified = parsed
        .verify(&Network::Mainnet, &valid_pin())
        .expect("the pin is computed from the vector");
    assert_eq!(verified.header(), &header);
    assert_eq!(verified.pin(), &valid_pin());
    assert_eq!(&verified.bytes()[..], &bytes[..]);

    let retained: Vec<u64> = (0..10)
        .filter(|&ordinal| verified.retains(ordinal).expect("ordinal is in range"))
        .collect();
    assert_eq!(retained, [3, 8]);
}

#[test]
fn hash_stability_vector() {
    let _init_guard = zebra_test::init();

    // Computed independently: SHA-256 of the 88 vector bytes.
    assert_eq!(
        hex::encode(valid_pin().spentness_hash),
        "2b26b32ea9dc50210643db3e5e7eb283a7a6fa1de0eedf87dc68bb925f045feb",
    );
    assert_eq!(
        valid_pin().object_hash(),
        ObjectHash::of(&valid_artifact_bytes())
    );
}

#[test]
fn ordinal_edge_cases() {
    let _init_guard = zebra_test::init();

    let verified = verified();
    assert_eq!(verified.retains(0), Ok(false));
    assert_eq!(verified.retains(9), Ok(false));
    assert_eq!(verified.retains(10), Err(SpentnessHintsError::Ordinal));
    assert_eq!(
        verified.retains(u64::MAX),
        Err(SpentnessHintsError::Ordinal)
    );

    let empty = ArtifactEncoder::new(
        Network::Mainnet.genesis_hash(),
        Height(0),
        Network::Mainnet.genesis_hash(),
        0,
    )
    .expect("an empty artifact is in bounds")
    .finish()
    .expect("no bits were expected");
    assert_eq!(empty.len(), HEADER_LEN);
    let pin = MaxCheckpoint {
        height: Height(0),
        hash: Network::Mainnet.genesis_hash(),
        spentness_hash: Sha256::digest(&empty).into(),
    };
    let empty = parse(empty)
        .expect("the encoder output parses")
        .verify(&Network::Mainnet, &pin)
        .expect("the pin is computed from the artifact");
    assert_eq!(empty.retains(0), Err(SpentnessHintsError::Ordinal));
}

#[test]
fn rejects_truncated_and_trailing_bytes() {
    let _init_guard = zebra_test::init();

    let bytes = valid_artifact_bytes();
    for end in 0..bytes.len() {
        assert!(
            matches!(
                parse(bytes[..end].to_vec()),
                Err(SpentnessHintsError::Format(_))
            ),
            "a {end}-byte prefix must not parse",
        );
    }

    let mut trailing = bytes;
    trailing.push(0);
    assert!(matches!(
        parse(trailing),
        Err(SpentnessHintsError::Format(_))
    ));
}

#[test]
fn rejects_bad_header_fields() {
    let _init_guard = zebra_test::init();

    // Unknown magic and version are format errors.
    for index in [0, VERSION_OFFSET] {
        let mut bytes = valid_artifact_bytes();
        bytes[index] ^= 1;
        assert!(matches!(parse(bytes), Err(SpentnessHintsError::Format(_))));
    }

    // A terminal height above `Height::MAX` is a format error.
    let mut bytes = valid_artifact_bytes();
    bytes[TERMINAL_HEIGHT_OFFSET..TERMINAL_HASH_OFFSET].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(matches!(parse(bytes), Err(SpentnessHintsError::Format(_))));

    // An output count that disagrees with the length is a format error,
    // in both directions.
    for count in [3u64, 17, u64::MAX] {
        let mut bytes = valid_artifact_bytes();
        bytes[OUTPUT_COUNT_OFFSET..HEADER_LEN].copy_from_slice(&count.to_le_bytes());
        assert!(matches!(parse(bytes), Err(SpentnessHintsError::Format(_))));
    }

    // A flipped bit anywhere else in the header, or in the bits, still
    // parses (or not) but never verifies against the pin.
    for index in [
        0,
        VERSION_OFFSET,
        CHAIN_IDENTITY_OFFSET,
        TERMINAL_HEIGHT_OFFSET,
        TERMINAL_HASH_OFFSET,
        OUTPUT_COUNT_OFFSET,
        HEADER_LEN,
        HEADER_LEN + 1,
    ] {
        let mut bytes = valid_artifact_bytes();
        bytes[index] ^= 1 << 1;
        let result = parse(bytes).and_then(|parsed| parsed.verify(&Network::Mainnet, &valid_pin()));
        assert!(result.is_err(), "a flipped bit at {index} must not verify");
    }
}

#[test]
fn rejects_nonzero_unused_bits() {
    let _init_guard = zebra_test::init();

    // 10 bits leave 6 unused bits in the last byte.
    for bit in 2..8 {
        let mut bytes = valid_artifact_bytes();
        let last = bytes.len() - 1;
        bytes[last] |= 1 << bit;
        assert_eq!(
            parse(bytes).map(|_| ()),
            Err(SpentnessHintsError::Format("nonzero unused bits"))
        );
    }
}

#[test]
fn rejects_retained_genesis_output() {
    let _init_guard = zebra_test::init();

    let mut bytes = valid_artifact_bytes();
    bytes[HEADER_LEN] |= 1;
    assert_eq!(
        parse(bytes).map(|_| ()),
        Err(SpentnessHintsError::Format(
            "genesis output must not be retained"
        ))
    );

    let mut encoder = ArtifactEncoder::new(
        Network::Mainnet.genesis_hash(),
        TERMINAL_HEIGHT,
        TERMINAL_HASH,
        1,
    )
    .expect("one output is in bounds");
    assert!(matches!(
        encoder.push(true),
        Err(SpentnessHintsError::Format(_))
    ));
}

#[test]
fn verify_rejects_pin_and_network_mismatches() {
    let _init_guard = zebra_test::init();

    let mismatch = |network: &Network, pin: &MaxCheckpoint| {
        matches!(
            parse(valid_artifact_bytes())
                .expect("hand-built vector satisfies every format rule")
                .verify(network, pin),
            Err(SpentnessHintsError::PinMismatch(_))
        )
    };

    assert!(mismatch(&Network::new_default_testnet(), &valid_pin()));
    assert!(mismatch(
        &Network::Mainnet,
        &MaxCheckpoint {
            height: Height(TERMINAL_HEIGHT.0 + 1),
            ..valid_pin()
        }
    ));
    assert!(mismatch(
        &Network::Mainnet,
        &MaxCheckpoint {
            hash: block::Hash([0x23; 32]),
            ..valid_pin()
        }
    ));
    let mut spentness_hash = valid_pin().spentness_hash;
    spentness_hash[0] ^= 1;
    assert!(mismatch(
        &Network::Mainnet,
        &MaxCheckpoint {
            spentness_hash,
            ..valid_pin()
        }
    ));
}

#[test]
fn encoder_matches_hand_built_vector() {
    let _init_guard = zebra_test::init();

    let mut encoder = ArtifactEncoder::new(
        Network::Mainnet.genesis_hash(),
        TERMINAL_HEIGHT,
        TERMINAL_HASH,
        10,
    )
    .expect("ten outputs are in bounds");
    for ordinal in 0..10 {
        encoder
            .push([3, 8].contains(&ordinal))
            .expect("fewer than output_count bits were pushed");
    }
    assert_eq!(encoder.push(false), Err(SpentnessHintsError::Ordinal));
    assert_eq!(
        encoder
            .finish()
            .expect("exactly output_count bits were pushed"),
        valid_artifact_bytes()
    );
}

#[test]
fn encoder_rejects_wrong_bit_counts() {
    let _init_guard = zebra_test::init();

    let mut encoder = ArtifactEncoder::new(
        Network::Mainnet.genesis_hash(),
        TERMINAL_HEIGHT,
        TERMINAL_HASH,
        10,
    )
    .expect("ten outputs are in bounds");
    encoder.push(false).expect("first bit is in range");
    assert!(matches!(
        encoder.finish(),
        Err(SpentnessHintsError::Format(_))
    ));

    assert!(matches!(
        ArtifactEncoder::new(
            Network::Mainnet.genesis_hash(),
            TERMINAL_HEIGHT,
            TERMINAL_HASH,
            u64::MAX,
        ),
        Err(SpentnessHintsError::Format(_))
    ));
}

#[test]
fn artifact_len_bounds() {
    let _init_guard = zebra_test::init();

    let header_len = u64::try_from(HEADER_LEN).expect("header length fits in u64");
    assert_eq!(artifact_len(0), Ok(header_len));
    assert_eq!(artifact_len(1), Ok(header_len + 1));
    assert_eq!(artifact_len(8), Ok(header_len + 1));
    assert_eq!(artifact_len(9), Ok(header_len + 2));

    let max_count = (MAX_ARTIFACT_LEN - header_len) * 8;
    assert_eq!(artifact_len(max_count), Ok(MAX_ARTIFACT_LEN));
    assert!(artifact_len(max_count + 1).is_err());
    assert!(artifact_len(MAX_ARTIFACT_LEN * 8).is_err());
    assert!(artifact_len(u64::MAX).is_err());
}

/// Every pinned terminal checkpoint is in its network's checkpoint list.
#[test]
fn max_checkpoints_are_in_the_checkpoint_list() {
    let _init_guard = zebra_test::init();

    for network in [Network::Mainnet, Network::new_default_testnet()] {
        let Some(pin) = MaxCheckpoint::for_network(&network) else {
            continue;
        };
        assert_eq!(
            network.checkpoint_list().hash(pin.height),
            Some(pin.hash),
            "the {network} pin must be a checkpoint",
        );
    }
}

#[test]
fn max_checkpoints_are_absent_until_pinned() {
    let _init_guard = zebra_test::init();

    // No release has pinned an artifact yet, and Regtest never has one.
    assert_eq!(MaxCheckpoint::for_network(&Network::Mainnet), None);
    assert_eq!(
        MaxCheckpoint::for_network(&Network::new_default_testnet()),
        None
    );
    assert_eq!(
        MaxCheckpoint::for_network(&Network::new_regtest(Default::default())),
        None
    );
}

#[test]
fn mode_config_names() {
    let _init_guard = zebra_test::init();

    assert_eq!(Mode::default(), Mode::Off);
    for (mode, name) in [
        (Mode::Off, "\"off\""),
        (Mode::Auto, "\"auto\""),
        (Mode::Require, "\"require\""),
    ] {
        assert_eq!(serde_json::to_string(&mode).expect("serializes"), name);
        assert_eq!(
            serde_json::from_str::<Mode>(name).expect("deserializes"),
            mode
        );
    }
}

/// A pin is found by its artifact hash among the current and retired pins, and
/// no network pins anything yet (F-348).
#[test]
fn pins_are_found_by_their_artifact_hash() {
    let _init_guard = zebra_test::init();

    let pin = |byte: u8| MaxCheckpoint {
        height: Height(u32::from(byte) * 1000),
        hash: block::Hash([byte; 32]),
        spentness_hash: [byte; 32],
    };
    let (current, retired, revoked) = (pin(3), [pin(1), pin(2)], pin(9));

    assert_eq!(
        super::super::find_pin(Some(current), &retired, [3; 32]),
        Some(current)
    );
    assert_eq!(
        super::super::find_pin(Some(current), &retired, [2; 32]),
        Some(retired[1])
    );
    assert_eq!(
        super::super::find_pin(None, &retired, [1; 32]),
        Some(retired[0])
    );
    assert_eq!(
        super::super::find_pin(Some(current), &retired, revoked.spentness_hash),
        None
    );

    for network in [Network::Mainnet, Network::new_default_testnet()] {
        assert_eq!(MaxCheckpoint::for_commitment(&network, [3; 32]), None);
    }
}
