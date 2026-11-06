//! Tests for known-hash list pins and chunk verification.

use std::sync::Arc;

use sha2::{Digest, Sha256};

use super::*;
use crate::{
    block::Block,
    parameters::testnet,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
};

/// A spec over `heights` heights with placeholder pins, for the arithmetic
/// tests that never hash.
fn placeholder_spec(heights: u32) -> KnownHashListSpec {
    let chunk_count = heights.div_ceil(CHUNK_HEIGHTS);

    KnownHashListSpec {
        max_height: block::Height(heights - 1),
        chunk_heights: CHUNK_HEIGHTS,
        chunk_hashes: Cow::Owned(vec![[0xAA; 32]; chunk_count as usize]),
    }
}

/// The Mainnet genesis block's entry, derived from the block itself.
fn mainnet_genesis_entry() -> SyncHashEntry {
    let bytes = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES.as_slice();
    let block: Arc<Block> = bytes.zcash_deserialize_into().expect("genesis is valid");

    SyncHashEntry {
        hash: block.hash(),
        size: bytes.len() as u64,
        txs: block.transactions.len() as u64,
        txouts: block
            .transactions
            .iter()
            .map(|tx| tx.outputs().len() as u64)
            .sum(),
        notes: 0,
    }
}

/// Distinct fake entries for heights `1..len`, after `genesis`.
fn fake_entries(genesis: SyncHashEntry, len: usize) -> Vec<SyncHashEntry> {
    let mut entries = vec![genesis];
    entries.extend((1..len).map(|i| {
        let mut hash = [0xAA; 32];
        hash[..8].copy_from_slice(&(i as u64).to_le_bytes());

        SyncHashEntry {
            hash: block::Hash(hash),
            size: 1_000 + i as u64,
            txs: 1,
            txouts: 1,
            notes: (i % 3) as u64,
        }
    }));

    entries
}

/// A Regtest network, which accepts configured pins, with a final checkpoint
/// at `final_checkpoint`.
fn regtest_with_final_checkpoint(final_checkpoint: block::Height) -> Network {
    let genesis = Network::new_regtest(testnet::RegtestParameters::default()).genesis_hash();
    let mut checkpoints = vec![(block::Height(0), genesis)];
    if final_checkpoint > block::Height(0) {
        checkpoints.push((final_checkpoint, block::Hash([0x11; 32])));
    }

    Network::new_regtest(testnet::RegtestParameters {
        checkpoints: Some(testnet::ConfiguredCheckpoints::HeightsAndHashes(
            checkpoints,
        )),
        ..Default::default()
    })
}

/// A Regtest network, which accepts configured pins, with checkpoints up to
/// every height the tests use.
fn regtest() -> Network {
    regtest_with_final_checkpoint(block::Height(3 * CHUNK_HEIGHTS))
}

/// The genesis chunk of a one-height list is the canonical body of the
/// genesis entry: a count, the internal-order hash, and four canonical
/// CompactSizes. Its pin is the SHA-256 of exactly those bytes, and it
/// verifies on Mainnet only.
#[test]
fn genesis_chunk_vector() {
    let _init_guard = zebra_test::init();

    let entry = mainnet_genesis_entry();
    assert_eq!(entry.hash, Network::Mainnet.genesis_hash());
    assert_eq!(
        entry.size, 1_692,
        "the Mainnet genesis block is 1,692 bytes"
    );
    assert_eq!((entry.txs, entry.txouts, entry.notes), (1, 1, 0));

    // `CS 1 ‖ hash ‖ CS 1692 ‖ CS 1 ‖ CS 1 ‖ CS 0`.
    let mut expected = vec![0x01];
    expected.extend_from_slice(&entry.hash.0);
    expected.extend_from_slice(&[0xfd, 0x9c, 0x06, 0x01, 0x01, 0x00]);

    let mut body = Vec::new();
    write_sync_hashes_body(&[entry], &mut body).expect("writing to a Vec never fails");
    assert_eq!(body, expected, "the genesis chunk is the canonical body");

    let pin: [u8; 32] = Sha256::digest(&expected).into();
    assert_eq!(
        chunk_pin(&[entry]),
        pin,
        "the pin hashes the canonical body"
    );

    let spec = KnownHashListSpec {
        max_height: block::Height(0),
        chunk_heights: CHUNK_HEIGHTS,
        chunk_hashes: Cow::Owned(vec![pin]),
    };
    assert_eq!(spec.chunk_len(0), Some(1));

    assert_eq!(spec.verify_chunk(&Network::Mainnet, 0, &[entry]), Ok(()));

    // The same bytes are not a valid list for another network.
    let testnet = Network::new_default_testnet();
    assert_eq!(
        spec.verify_chunk(&testnet, 0, &[entry]),
        Err(KnownHashError::GenesisMismatch {
            list_genesis: entry.hash,
            network_genesis: testnet.genesis_hash(),
        }),
    );
}

/// A multi-chunk list verifies every chunk against its own pin, and rejects
/// a chunk with any changed field, or another chunk's entries.
#[test]
fn chunks_verify_against_their_pins() {
    let _init_guard = zebra_test::init();

    let network = regtest();
    let genesis = SyncHashEntry {
        hash: network.genesis_hash(),
        ..mainnet_genesis_entry()
    };

    // Two full chunks and a 7-height tail.
    let heights = 2 * CHUNK_HEIGHTS as usize + 7;
    let entries = fake_entries(genesis, heights);
    let chunks: Vec<&[SyncHashEntry]> = entries.chunks(CHUNK_HEIGHTS as usize).collect();
    let pins = chunks.iter().map(|chunk| chunk_pin(chunk)).collect();

    let spec = KnownHashListSpec::from_config(&network, block::Height(heights as u32 - 1), pins)
        .expect("the pins cover the list");
    assert_eq!(spec.chunk_count(), 3);

    for (index, chunk) in chunks.iter().enumerate() {
        assert_eq!(spec.verify_chunk(&network, index, chunk), Ok(()));
    }

    // Any field change in any entry changes the pin.
    let mut tampered = chunks[2].to_vec();
    tampered[3].txouts += 1;
    assert_eq!(
        spec.verify_chunk(&network, 2, &tampered),
        Err(KnownHashError::ChunkHashMismatch { index: 2 }),
    );

    // A chunk of the right length at the wrong index does not match.
    assert_eq!(
        spec.verify_chunk(&network, 1, chunks[0]),
        Err(KnownHashError::ChunkHashMismatch { index: 1 }),
    );

    // Chunk 0 must start at this network's genesis, even with a matching pin.
    let mut wrong_genesis = chunks[0].to_vec();
    wrong_genesis[0].hash = Network::Mainnet.genesis_hash();
    let wrong_spec = KnownHashListSpec {
        chunk_hashes: Cow::Owned(vec![chunk_pin(&wrong_genesis), [0; 32], [0; 32]]),
        ..spec.clone()
    };
    assert_eq!(
        wrong_spec.verify_chunk(&network, 0, &wrong_genesis),
        Err(KnownHashError::GenesisMismatch {
            list_genesis: Network::Mainnet.genesis_hash(),
            network_genesis: network.genesis_hash(),
        }),
    );
}

/// The index and length are checked before anything is encoded or hashed.
#[test]
fn verify_chunk_checks_index_then_length() {
    let _init_guard = zebra_test::init();

    let spec = placeholder_spec(CHUNK_HEIGHTS + 10);
    let network = regtest();

    assert_eq!(
        spec.verify_chunk(&network, 2, &[]),
        Err(KnownHashError::ChunkIndexOutOfRange {
            index: 2,
            chunk_count: 2,
        }),
    );

    // An out-of-range index wins over a wrong length.
    let entries = fake_entries(mainnet_genesis_entry(), 3);
    assert_eq!(
        spec.verify_chunk(&network, usize::MAX, &entries),
        Err(KnownHashError::ChunkIndexOutOfRange {
            index: usize::MAX,
            chunk_count: 2,
        }),
    );

    assert_eq!(
        spec.verify_chunk(&network, 1, &entries),
        Err(KnownHashError::ChunkLength {
            index: 1,
            expected: 10,
            actual: 3,
        }),
    );
    assert_eq!(
        spec.verify_chunk(&network, 0, &[]),
        Err(KnownHashError::ChunkLength {
            index: 0,
            expected: CHUNK_HEIGHTS,
            actual: 0,
        }),
    );
}

/// The chunk arithmetic covers `0..=max_height` exactly, at the chunk
/// boundaries and with a full or partial last chunk.
#[test]
fn chunk_arithmetic() {
    let _init_guard = zebra_test::init();

    for heights in [
        1,
        2,
        CHUNK_HEIGHTS - 1,
        CHUNK_HEIGHTS,
        CHUNK_HEIGHTS + 1,
        3 * CHUNK_HEIGHTS,
        3 * CHUNK_HEIGHTS + 17,
    ] {
        let spec = placeholder_spec(heights);
        let count = spec.chunk_count();

        assert_eq!(spec.len(), u64::from(heights));
        assert!(!spec.is_empty());
        assert_eq!(count, heights.div_ceil(CHUNK_HEIGHTS) as usize);

        let total: u64 = (0..count)
            .map(|index| u64::from(spec.chunk_len(index).expect("index is in range")))
            .sum();
        assert_eq!(total, spec.len(), "chunk lengths sum to the list length");

        for index in 0..count {
            let start = spec.chunk_start(index).expect("index is in range");
            let len = spec.chunk_len(index).expect("index is in range");
            let last = block::Height(start.0 + len - 1);

            assert_eq!(start.0, index as u32 * CHUNK_HEIGHTS);
            assert_eq!(spec.chunk_index_for(start), Some(index));
            assert_eq!(spec.chunk_index_for(last), Some(index));
            if index + 1 < count {
                assert_eq!(len, CHUNK_HEIGHTS, "only the last chunk is short");
            } else {
                assert_eq!(last, spec.max_height, "the last chunk ends at max_height");
            }
        }

        assert_eq!(spec.chunk_start(count), None);
        assert_eq!(spec.chunk_len(count), None);
        assert_eq!(spec.chunk_len(usize::MAX), None);
        assert_eq!(spec.chunk_index_for(block::Height(heights)), None);
        assert_eq!(spec.chunk_index_for(block::Height::MAX), None);
    }
}

/// Pins are only configurable on Regtest and custom testnets, and must cover
/// exactly `0..=max_height`.
#[test]
fn from_config_refuses_pinned_networks_and_bad_counts() {
    let _init_guard = zebra_test::init();

    for network in [Network::Mainnet, Network::new_default_testnet()] {
        assert!(matches!(
            KnownHashListSpec::from_config(&network, block::Height(0), vec![[0; 32]]),
            Err(KnownHashError::PinsNotConfigurable { .. }),
        ));
    }

    let network = regtest();
    let max_height = block::Height(CHUNK_HEIGHTS);

    for count in [0, 1, 3] {
        assert!(matches!(
            KnownHashListSpec::from_config(&network, max_height, vec![[0; 32]; count]),
            Err(KnownHashError::InconsistentSpec(_)),
        ));
    }

    let spec = KnownHashListSpec::from_config(&network, max_height, vec![[1; 32], [2; 32]])
        .expect("two chunks cover 50,001 heights");
    assert_eq!(spec.chunk_heights, CHUNK_HEIGHTS);
    assert_eq!(spec.chunk_len(1), Some(1));
    assert_eq!(&spec.chunk_hashes[..], &[[1; 32], [2; 32]]);

    // A list cannot reach above the final checkpoint.
    let low = regtest_with_final_checkpoint(block::Height(CHUNK_HEIGHTS - 1));
    assert_eq!(
        KnownHashListSpec::from_config(&low, max_height, vec![[1; 32], [2; 32]]),
        Err(KnownHashError::AboveFinalCheckpoint {
            max_height,
            final_checkpoint: block::Height(CHUNK_HEIGHTS - 1),
        }),
    );
    let at_final = block::Height(CHUNK_HEIGHTS - 1);
    assert!(KnownHashListSpec::from_config(&low, at_final, vec![[1; 32]]).is_ok());

    // A custom testnet takes configured pins too.
    let custom = testnet::Parameters::build()
        .with_network_name("KnownHashTest")
        .expect("the name is valid")
        .to_network()
        .expect("the parameters are valid");
    assert!(KnownHashListSpec::from_config(&custom, block::Height(0), vec![[0; 32]]).is_ok());
}

/// The pinned lists, once they are added, are consistent with their own
/// coverage and with the checkpoint list. Regtest and custom testnets never
/// have compiled-in pins.
#[test]
fn pinned_lists_are_consistent() {
    let _init_guard = zebra_test::init();

    for network in [Network::Mainnet, Network::new_default_testnet()] {
        let Some(spec) = KnownHashListSpec::for_network(&network) else {
            continue;
        };

        assert_eq!(spec.chunk_heights, CHUNK_HEIGHTS, "{network}");
        assert_eq!(spec.check_consistent(), Ok(()), "{network}");
        assert!(
            spec.max_height <= network.checkpoint_list().max_height(),
            "the {network} list must not extend past the final checkpoint",
        );
        assert!(
            matches!(spec.chunk_hashes, Cow::Borrowed(_)),
            "compiled-in pins are static data",
        );
    }

    assert_eq!(KnownHashListSpec::for_network(&regtest()), None);
}

/// The pin streams the canonical body into the hasher, so it equals the
/// SHA-256 of the body bytes.
#[test]
fn chunk_pin_is_sha256_of_body() {
    let _init_guard = zebra_test::init();

    let entries = fake_entries(mainnet_genesis_entry(), 300);

    let mut body = Vec::new();
    write_sync_hashes_body(&entries, &mut body).expect("writing to a Vec never fails");

    let mut entry_bytes = Vec::new();
    for entry in &entries {
        entry
            .zcash_serialize(&mut entry_bytes)
            .expect("writing to a Vec never fails");
    }
    // 300 needs a 3-byte CompactSize.
    assert_eq!(&body[..3], &[0xfd, 0x2c, 0x01]);
    assert_eq!(&body[3..], &entry_bytes[..]);

    let expected: [u8; 32] = Sha256::digest(&body).into();
    assert_eq!(chunk_pin(&entries), expected);
}
