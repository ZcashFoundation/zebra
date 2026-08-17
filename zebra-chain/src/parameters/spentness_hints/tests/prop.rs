//! Property tests for spentness-hint artifacts.

use proptest::prelude::*;
use sha2::{Digest, Sha256};

use crate::{
    block::Height,
    parameters::{spentness_hints::SpentnessHints, Network},
};

/// One of the networks the artifact framing distinguishes.
fn network_strategy() -> impl Strategy<Value = Network> {
    prop_oneof![Just(Network::Mainnet), Just(Network::new_default_testnet()),]
}

proptest! {
    /// Encoding bits and parsing the result yields the same header fields and
    /// the same bits, and the hash is SHA-256 of the exact bytes.
    #[test]
    fn round_trip(
        network in network_strategy(),
        height in 0..=Height::MAX.0,
        bits in prop::collection::vec(any::<bool>(), 0..512),
    ) {
        let _init_guard = zebra_test::init();

        let encoded = SpentnessHints::from_bits(&network, Height(height), bits.iter().copied());

        prop_assert_eq!(encoded.max_height(), Height(height));
        prop_assert_eq!(encoded.output_count(), bits.len() as u64);
        prop_assert_eq!(
            encoded.artifact_hash(),
            <[u8; 32]>::from(Sha256::digest(encoded.as_bytes()))
        );

        let parsed = SpentnessHints::from_bytes(&network, encoded.as_bytes().to_vec())
            .expect("the encoder always produces a canonical artifact");
        prop_assert_eq!(&parsed, &encoded);

        for (ordinal, bit) in bits.iter().enumerate() {
            prop_assert_eq!(parsed.is_spent(ordinal as u64), Some(*bit));
        }
        prop_assert_eq!(parsed.is_spent(bits.len() as u64), None);
    }

    /// A canonical artifact stops parsing when truncated or extended by any
    /// amount: only the exact bytes are accepted.
    #[test]
    fn rejects_resized_artifacts(
        network in network_strategy(),
        height in 0..=Height::MAX.0,
        bits in prop::collection::vec(any::<bool>(), 0..256),
        cut in 1usize..8,
        extra in prop::collection::vec(any::<u8>(), 1..8),
    ) {
        let _init_guard = zebra_test::init();

        let encoded = SpentnessHints::from_bits(&network, Height(height), bits.iter().copied());
        let bytes = encoded.as_bytes();

        let truncated = bytes[..bytes.len() - cut.min(bytes.len())].to_vec();
        prop_assert!(SpentnessHints::from_bytes(&network, truncated).is_err());

        let mut extended = bytes.to_vec();
        extended.extend_from_slice(&extra);
        prop_assert!(SpentnessHints::from_bytes(&network, extended).is_err());
    }

    /// An artifact never parses for the other network.
    #[test]
    fn rejects_other_network(
        height in 0..=Height::MAX.0,
        bits in prop::collection::vec(any::<bool>(), 0..256),
    ) {
        let _init_guard = zebra_test::init();

        let testnet = Network::new_default_testnet();
        let encoded = SpentnessHints::from_bits(&Network::Mainnet, Height(height), bits);

        prop_assert!(
            SpentnessHints::from_bytes(&testnet, encoded.as_bytes().to_vec()).is_err()
        );
    }
}
