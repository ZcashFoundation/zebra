//! Property tests for spentness-hint artifacts.

use std::sync::Arc;

use proptest::prelude::*;
use sha2::{Digest, Sha256};

use crate::{
    block::{self, Height},
    parameters::{
        spentness_hints::{
            ArtifactEncoder, MaxCheckpoint, ParsedArtifact, SpentnessHintsError, HEADER_LEN,
        },
        Network,
    },
};

/// The networks with a genesis hash an artifact can name.
fn network_strategy() -> impl Strategy<Value = Network> {
    prop_oneof![Just(Network::Mainnet), Just(Network::new_default_testnet())]
}

/// Survivor bits in canonical order; the genesis output is always spent.
fn bits_strategy(max_len: usize) -> impl Strategy<Value = Vec<bool>> {
    prop::collection::vec(any::<bool>(), 0..max_len).prop_map(|mut bits| {
        if let Some(genesis) = bits.first_mut() {
            *genesis = false;
        }
        bits
    })
}

/// Encodes `bits` for `network` at `height`/`hash`.
fn encode(network: &Network, height: Height, hash: block::Hash, bits: &[bool]) -> Vec<u8> {
    let count = u64::try_from(bits.len()).expect("test bit vectors are small");
    let mut encoder = ArtifactEncoder::new(network.genesis_hash(), height, hash, count)
        .expect("test bit vectors are in bounds");
    for bit in bits {
        encoder
            .push(*bit)
            .expect("fewer than output_count bits were pushed");
    }
    encoder
        .finish()
        .expect("exactly output_count bits were pushed")
}

proptest! {
    /// Encoding bits, parsing and verifying the result yields the same header
    /// fields and the same bits.
    #[test]
    fn round_trip(
        network in network_strategy(),
        height in 0..=Height::MAX.0,
        hash in any::<[u8; 32]>(),
        bits in bits_strategy(512),
    ) {
        let _init_guard = zebra_test::init();

        let (height, hash) = (Height(height), block::Hash(hash));
        let bytes = encode(&network, height, hash, &bits);
        let pin = MaxCheckpoint {
            height,
            hash,
            spentness_hash: Sha256::digest(&bytes).into(),
        };

        let parsed = ParsedArtifact::parse(Arc::from(bytes.clone()))
            .expect("the encoder always produces a valid artifact");
        let count = u64::try_from(bits.len()).expect("test bit vectors are small");
        prop_assert_eq!(parsed.header().output_count, count);
        prop_assert_eq!(parsed.header().terminal_height, height);
        prop_assert_eq!(parsed.header().terminal_hash, hash);
        prop_assert_eq!(parsed.header().chain_identity, network.genesis_hash());

        let verified = parsed.verify(&network, &pin).expect("the pin is computed from the bytes");
        prop_assert_eq!(&verified.bytes()[..], &bytes[..]);
        for (ordinal, bit) in (0..count).zip(&bits) {
            prop_assert_eq!(verified.retains(ordinal), Ok(*bit));
        }
        prop_assert_eq!(verified.retains(count), Err(SpentnessHintsError::Ordinal));
    }

    /// A valid artifact stops parsing when truncated or extended by any
    /// amount: only the exact bytes are accepted.
    #[test]
    fn rejects_resized_artifacts(
        network in network_strategy(),
        height in 0..=Height::MAX.0,
        bits in bits_strategy(256),
        cut in 1usize..HEADER_LEN + 8,
        extra in prop::collection::vec(any::<u8>(), 1..8),
    ) {
        let _init_guard = zebra_test::init();

        let bytes = encode(&network, Height(height), block::Hash([7; 32]), &bits);

        let truncated = bytes[..bytes.len().saturating_sub(cut)].to_vec();
        prop_assert!(ParsedArtifact::parse(Arc::from(truncated)).is_err());

        let mut extended = bytes;
        extended.extend_from_slice(&extra);
        prop_assert!(ParsedArtifact::parse(Arc::from(extended)).is_err());
    }

    /// An artifact never verifies for the other network.
    #[test]
    fn rejects_other_network(
        height in 0..=Height::MAX.0,
        bits in bits_strategy(256),
    ) {
        let _init_guard = zebra_test::init();

        let (height, hash) = (Height(height), block::Hash([7; 32]));
        let bytes = encode(&Network::Mainnet, height, hash, &bits);
        let pin = MaxCheckpoint {
            height,
            hash,
            spentness_hash: Sha256::digest(&bytes).into(),
        };

        let parsed = ParsedArtifact::parse(Arc::from(bytes))
            .expect("the encoder always produces a valid artifact");
        prop_assert!(matches!(
            parsed.verify(&Network::new_default_testnet(), &pin),
            Err(SpentnessHintsError::PinMismatch(_))
        ));
    }
}
