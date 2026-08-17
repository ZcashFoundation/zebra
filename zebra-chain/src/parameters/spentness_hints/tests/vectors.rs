//! Fixed test vectors for spentness-hint artifacts.

use crate::{
    block::Height,
    parameters::{
        spentness_hints::{
            max_checkpoint, SpentnessHints, SpentnessHintsError, SPENTNESS_HINTS_MAGIC,
            SPENTNESS_HINTS_VERSION,
        },
        Magic, Network,
    },
};

/// A valid 10-bit Mainnet artifact: outputs 0, 3, and 8 spent, height
/// 1_000_000.
fn valid_artifact_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&SPENTNESS_HINTS_MAGIC);
    bytes.push(SPENTNESS_HINTS_VERSION);
    bytes.extend_from_slice(&Network::Mainnet.magic().0);
    bytes.extend_from_slice(&1_000_000u32.to_le_bytes());
    bytes.extend_from_slice(&10u64.to_le_bytes());
    bytes.extend_from_slice(&[0b0000_1001, 0b0000_0001]);
    bytes
}

#[test]
fn parses_valid_artifact() {
    let _init_guard = zebra_test::init();

    let bytes = valid_artifact_bytes();
    let hints = SpentnessHints::from_bytes(&Network::Mainnet, bytes.clone())
        .expect("hand-built vector satisfies every framing rule");

    assert_eq!(hints.max_height(), Height(1_000_000));
    assert_eq!(hints.output_count(), 10);
    assert_eq!(hints.as_bytes(), bytes);

    let spent: Vec<u64> = (0..10)
        .filter(|&i| hints.is_spent(i) == Some(true))
        .collect();
    assert_eq!(spent, [0, 3, 8]);
}

#[test]
fn hash_stability_vector() {
    let _init_guard = zebra_test::init();

    let hints = SpentnessHints::from_bytes(&Network::Mainnet, valid_artifact_bytes())
        .expect("hand-built vector satisfies every framing rule");

    assert_eq!(
        hex::encode(hints.artifact_hash()),
        "6f6a5d0448bea6673dcd9a09f94055242f451e614512a4871a4ee185ca69b24b",
    );
}

#[test]
fn ordinal_edge_cases() {
    let _init_guard = zebra_test::init();

    let hints = SpentnessHints::from_bytes(&Network::Mainnet, valid_artifact_bytes())
        .expect("hand-built vector satisfies every framing rule");

    assert_eq!(hints.is_spent(0), Some(true));
    assert_eq!(hints.is_spent(hints.output_count() - 1), Some(false));
    assert_eq!(hints.is_spent(hints.output_count()), None);
    assert_eq!(hints.is_spent(u64::MAX), None);

    let empty = SpentnessHints::from_bits(&Network::Mainnet, Height(0), std::iter::empty());
    assert_eq!(empty.output_count(), 0);
    assert_eq!(empty.is_spent(0), None);
}

#[test]
fn rejects_truncated_header() {
    let _init_guard = zebra_test::init();

    let bytes = valid_artifact_bytes();
    for len in 0..21 {
        assert!(matches!(
            SpentnessHints::from_bytes(&Network::Mainnet, bytes[..len].to_vec()),
            Err(SpentnessHintsError::Truncated { .. })
        ));
    }
}

#[test]
fn rejects_truncated_bitmap() {
    let _init_guard = zebra_test::init();

    let mut bytes = valid_artifact_bytes();
    bytes.pop();
    assert_eq!(
        SpentnessHints::from_bytes(&Network::Mainnet, bytes),
        Err(SpentnessHintsError::Truncated {
            expected: 23,
            actual: 22
        })
    );
}

#[test]
fn rejects_trailing_bytes() {
    let _init_guard = zebra_test::init();

    let mut bytes = valid_artifact_bytes();
    bytes.push(0);
    assert_eq!(
        SpentnessHints::from_bytes(&Network::Mainnet, bytes),
        Err(SpentnessHintsError::TrailingBytes {
            expected: 23,
            actual: 24
        })
    );
}

#[test]
fn rejects_wrong_magic() {
    let _init_guard = zebra_test::init();

    let mut bytes = valid_artifact_bytes();
    bytes[0..4].copy_from_slice(b"ZSH2");
    assert_eq!(
        SpentnessHints::from_bytes(&Network::Mainnet, bytes),
        Err(SpentnessHintsError::WrongMagic(*b"ZSH2"))
    );
}

#[test]
fn rejects_wrong_version() {
    let _init_guard = zebra_test::init();

    let mut bytes = valid_artifact_bytes();
    bytes[4] = 2;
    assert_eq!(
        SpentnessHints::from_bytes(&Network::Mainnet, bytes),
        Err(SpentnessHintsError::UnsupportedVersion(2))
    );
}

#[test]
fn rejects_network_mismatch() {
    let _init_guard = zebra_test::init();

    let testnet = Network::new_default_testnet();
    assert_eq!(
        SpentnessHints::from_bytes(&testnet, valid_artifact_bytes()),
        Err(SpentnessHintsError::NetworkMismatch {
            expected: testnet.magic(),
            artifact: Network::Mainnet.magic(),
        })
    );
}

#[test]
fn rejects_height_out_of_range() {
    let _init_guard = zebra_test::init();

    let mut bytes = valid_artifact_bytes();
    bytes[9..13].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
        SpentnessHints::from_bytes(&Network::Mainnet, bytes),
        Err(SpentnessHintsError::HeightOutOfRange(u32::MAX))
    );
}

#[test]
fn rejects_count_length_mismatch() {
    let _init_guard = zebra_test::init();

    // Declares 17 bits (3 bitmap bytes) but supplies the original 2.
    let mut bytes = valid_artifact_bytes();
    bytes[13..21].copy_from_slice(&17u64.to_le_bytes());
    assert_eq!(
        SpentnessHints::from_bytes(&Network::Mainnet, bytes),
        Err(SpentnessHintsError::Truncated {
            expected: 24,
            actual: 23
        })
    );

    // Declares 3 bits (1 bitmap byte) but supplies 2.
    let mut bytes = valid_artifact_bytes();
    bytes[13..21].copy_from_slice(&3u64.to_le_bytes());
    bytes[21] = 0b0000_0001;
    assert_eq!(
        SpentnessHints::from_bytes(&Network::Mainnet, bytes),
        Err(SpentnessHintsError::TrailingBytes {
            expected: 22,
            actual: 23
        })
    );
}

#[test]
fn rejects_nonzero_padding() {
    let _init_guard = zebra_test::init();

    // 10 bits leave 6 padding bits in the final byte; set the top one.
    let mut bytes = valid_artifact_bytes();
    let last = bytes.len() - 1;
    bytes[last] |= 0b1000_0000;
    assert_eq!(
        SpentnessHints::from_bytes(&Network::Mainnet, bytes),
        Err(SpentnessHintsError::NonZeroPadding)
    );
}

#[test]
fn encoder_matches_hand_built_vector() {
    let _init_guard = zebra_test::init();

    let bits = (0..10).map(|i| [0, 3, 8].contains(&i));
    let hints = SpentnessHints::from_bits(&Network::Mainnet, Height(1_000_000), bits);

    assert_eq!(hints.as_bytes(), valid_artifact_bytes());
}

#[test]
fn max_checkpoint_placeholders_are_absent() {
    let _init_guard = zebra_test::init();

    // Release tooling has not cut any artifact yet, so every network's pin is
    // absent and hinted sync stays disabled.
    assert_eq!(max_checkpoint(&Network::Mainnet), None);
    assert_eq!(max_checkpoint(&Network::new_default_testnet()), None);
}

#[test]
fn magic_type_matches_network_magic() {
    let _init_guard = zebra_test::init();

    // The framing embeds the same magic bytes the P2P handshake uses.
    assert_eq!(Network::Mainnet.magic(), Magic([0x24, 0xe9, 0x27, 0x64]));
}
