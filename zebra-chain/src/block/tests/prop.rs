//! Randomised property tests for Zcash blocks.

use std::{env, io::ErrorKind};

use proptest::{prelude::*, test_runner::Config};

use hex::{FromHex, ToHex};

use zebra_test::prelude::*;

use crate::{
    parameters::GENESIS_PREVIOUS_BLOCK_HASH,
    serialization::{SerializationError, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
};

use super::super::{
    arbitrary::{allow_all_transparent_coinbase_spends, PREVOUTS_CHAIN_HEIGHT},
    *,
};

const DEFAULT_BLOCK_ROUNDTRIP_PROPTEST_CASES: u32 = 16;

proptest! {
    #[test]
    fn block_hash_roundtrip(hash in any::<Hash>()) {
        let _init_guard = zebra_test::init();

        let bytes = hash.zcash_serialize_to_vec()?;
        let other_hash: Hash = bytes.zcash_deserialize_into()?;

        prop_assert_eq![&hash, &other_hash];

        let bytes2 = other_hash
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");
        prop_assert_eq![bytes, bytes2, "bytes must be equal if structs are equal"];
    }

    #[test]
    fn block_hash_display_fromstr_roundtrip(hash in any::<Hash>()) {
        let _init_guard = zebra_test::init();

        let display = format!("{hash}");
        let parsed = display.parse::<Hash>().expect("hash should parse");
        prop_assert_eq!(hash, parsed);
    }

    #[test]
    fn block_hash_hex_roundtrip(hash in any::<Hash>()) {
        let _init_guard = zebra_test::init();

        let hex_hash: String = hash.encode_hex();
        let new_hash = Hash::from_hex(hex_hash).expect("hex hash should parse");
        prop_assert_eq!(hash, new_hash);
    }

    #[test]
    fn blockheader_roundtrip(header in any::<Header>()) {
        let _init_guard = zebra_test::init();

        let bytes = header.zcash_serialize_to_vec()?;
        let other_header = bytes.zcash_deserialize_into()?;

        prop_assert_eq![&header, &other_header];

        let bytes2 = other_header
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");
        prop_assert_eq![bytes, bytes2, "bytes must be equal if structs are equal"];
    }

    #[test]
    fn commitment_roundtrip(
        bytes in any::<[u8; 32]>(),
        network in any::<Network>(),
        block_height in any::<Height>()
    ) {
        let _init_guard = zebra_test::init();

        // just skip the test if the bytes don't parse, because there's nothing
        // to compare with
        if let Ok(commitment) = Commitment::from_bytes(bytes, &network, block_height) {
            let other_bytes = commitment.to_bytes();

            prop_assert_eq![bytes, other_bytes];
        }
    }
}

proptest! {
    // The block roundtrip test can be really slow, so we use fewer cases by
    // default. Set the PROPTEST_CASES env var to override this default.
    #![proptest_config(Config::with_cases(env::var("PROPTEST_CASES")
                                          .ok()
                                          .and_then(|v| v.parse().ok())
                                          .unwrap_or(DEFAULT_BLOCK_ROUNDTRIP_PROPTEST_CASES)))]

    #[test]
    fn block_roundtrip(block in any::<Block>(), network in any::<Network>()) {
        let _init_guard = zebra_test::init();

        let has_coinbase_sapling_spends = block.transactions.iter().any(|tx| {
            tx.is_coinbase() && tx.sapling_spends().count() > 0
        });

        let bytes = block.zcash_serialize_to_vec()?;

        // Check the block commitment
        let commitment = block.commitment(&network);
        if let Ok(commitment) = commitment {
            let commitment_bytes = commitment.to_bytes();
            prop_assert_eq![block.header.commitment_bytes.0, commitment_bytes];
        }

        if has_coinbase_sapling_spends {
            // GHSA-rgwx-8r98-p34c fix: the parser now rejects coinbase
            // transactions with Sapling spends before allocating.
            bytes.zcash_deserialize_into::<Block>()
                .expect_err("block with coinbase Sapling spends must be rejected");
        } else if bytes.len() <= MAX_BLOCK_BYTES as _ {
            // Check deserialization
            let other_block = bytes.zcash_deserialize_into()?;

            prop_assert_eq![&block, &other_block];

            let bytes2 = other_block
                .zcash_serialize_to_vec()
                .expect("vec serialization is infallible");
            prop_assert_eq![bytes, bytes2, "bytes must be equal if structs are equal"];
        } else {
            let serialization_err = bytes.zcash_deserialize_into::<Block>()
                .expect_err("blocks larger than the maximum size should fail");
            match serialization_err {
                SerializationError::Io(io_err) => {
                    prop_assert_eq![io_err.kind(), ErrorKind::UnexpectedEof];
                }
                _ => {
                    prop_assert!(false,
                                 "blocks larger than the maximum size should fail with an io::Error");
                }
            }
        }
    }
}

/// Test [`Block::coinbase_height`].
///
/// Also makes sure our coinbase strategy correctly generates blocks with
/// coinbase transactions.
#[test]
fn blocks_have_coinbase() -> Result<()> {
    let _init_guard = zebra_test::init();

    let strategy =
        LedgerState::coinbase_strategy(None, None, false).prop_flat_map(Block::arbitrary_with);

    proptest!(|(block in strategy)| {
        let has_coinbase = block.coinbase_height().is_some();
        prop_assert!(has_coinbase);
    });

    Ok(())
}

/// Make sure our genesis strategy generates blocks with the correct coinbase
/// height and previous block hash.
#[test]
fn block_genesis_strategy() -> Result<()> {
    let _init_guard = zebra_test::init();

    let strategy =
        LedgerState::genesis_strategy(None, None, None, false).prop_flat_map(Block::arbitrary_with);

    proptest!(|(block in strategy)| {
        prop_assert_eq!(block.coinbase_height(), Some(Height(0)));
        prop_assert_eq!(block.header.previous_block_hash, GENESIS_PREVIOUS_BLOCK_HASH);
    });

    Ok(())
}

/// Make sure our genesis partial chain strategy generates a chain with:
/// - correct coinbase heights
/// - correct previous block hashes
/// - no transparent spends in the genesis block, because genesis transparent outputs are ignored
#[test]
fn genesis_partial_chain_strategy() -> Result<()> {
    let _init_guard = zebra_test::init();

    let strategy = LedgerState::genesis_strategy(None, None, None, false).prop_flat_map(|init| {
        Block::partial_chain_strategy(
            init,
            PREVOUTS_CHAIN_HEIGHT,
            allow_all_transparent_coinbase_spends,
            false,
        )
    });

    proptest!(|(chain in strategy)| {
        let mut height = Height(0);
        let mut previous_block_hash = GENESIS_PREVIOUS_BLOCK_HASH;

        for block in chain {
            prop_assert_eq!(block.coinbase_height(), Some(height));
            prop_assert_eq!(block.header.previous_block_hash, previous_block_hash);

            // block 1 can have spends of transparent outputs
            // of previous transactions in the same block
            if height == Height(0) {
                prop_assert_eq!(
                    block
                        .transactions
                        .iter()
                        .flat_map(|t| t.inputs())
                        .filter_map(|i| i.outpoint())
                        .count(),
                    0,
                    "unexpected transparent prevout inputs at height {:?}: genesis transparent outputs are ignored",
                    height,
                );
            }

            height = Height(height.0 + 1);
            previous_block_hash = block.hash();
        }
    });

    Ok(())
}

/// Make sure our block height strategy generates a chain with:
/// - correct coinbase heights
/// - correct previous block hashes
#[test]
fn arbitrary_height_partial_chain_strategy() -> Result<()> {
    let _init_guard = zebra_test::init();

    let strategy = any::<Height>()
        .prop_flat_map(|height| LedgerState::height_strategy(height, None, None, false))
        .prop_flat_map(|init| {
            Block::partial_chain_strategy(
                init,
                PREVOUTS_CHAIN_HEIGHT,
                allow_all_transparent_coinbase_spends,
                false,
            )
        });

    proptest!(|(chain in strategy)| {
        let mut height = None;
        let mut previous_block_hash = None;

        for block in chain {
            if height.is_none() {
                prop_assert!(block.coinbase_height().is_some());
                height = block.coinbase_height();
            } else {
                height = Some(Height(height.unwrap().0 + 1));
                prop_assert_eq!(block.coinbase_height(), height);
                prop_assert_eq!(Some(block.header.previous_block_hash), previous_block_hash);
            }

            previous_block_hash = Some(block.hash());
        }
    });

    Ok(())
}

/// The default number of proptest cases for the parallel block decoder tests.
const DEFAULT_PARALLEL_DECODE_PROPTEST_CASES: u32 = 64;

/// Returns blocks that the parallel decoder decodes in parallel: the test vectors with at least
/// 64 transactions, and blocks of 64 copies of each ZIP 244 test vector transaction.
fn parallel_decode_blocks() -> Vec<Vec<u8>> {
    let mut blocks: Vec<Vec<u8>> = zebra_test::vectors::BLOCKS
        .iter()
        .filter(|bytes| {
            Block::zcash_deserialize(**bytes).is_ok_and(|block| block.transactions.len() >= 64)
        })
        .map(|bytes| bytes.to_vec())
        .collect();

    for vector in zebra_test::zip0244::TEST_VECTORS.iter() {
        let transaction = Arc::new(
            crate::transaction::Transaction::zcash_deserialize(vector.tx.as_slice())
                .expect("the test vector is a valid transaction"),
        );
        let block = Block {
            header: Arc::new(super::generate::block_header().0),
            transactions: vec![transaction; 64],
        };
        blocks.push(
            block
                .zcash_serialize_to_vec()
                .expect("vec writes can't fail"),
        );
    }

    for bytes in &blocks {
        assert!(
            super::super::serialize::parallel_block(bytes).is_some(),
            "the block is decoded in parallel"
        );
    }

    blocks
}

/// The parallel decoder gives the serial decoder's block or error, and consumes the same bytes,
/// for large blocks with changed bytes, and truncated or extended.
#[test]
fn parallel_decode_matches_serial_for_changed_blocks() -> Result<()> {
    let _init_guard = zebra_test::init();

    let blocks = parallel_decode_blocks();

    proptest!(
        Config::with_cases(
            env::var("PROPTEST_CASES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_PARALLEL_DECODE_PROPTEST_CASES)
        ),
        |(block in any::<prop::sample::Index>(),
          changes in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..4),
          end in any::<prop::sample::Index>(),
          end_change in 0..3u8,
          extra in prop::collection::vec(any::<u8>(), 0..8))| {
            let mut bytes = block.get(&blocks).clone();
            for (position, byte) in changes {
                *position.get_mut(&mut bytes) = byte;
            }
            match end_change {
                0 => {}
                1 => bytes.truncate(end.index(bytes.len())),
                _ => bytes.extend_from_slice(&extra),
            }

            super::vectors::assert_parallel_decode_matches_serial(&bytes);
        }
    );

    Ok(())
}

proptest! {
    #![proptest_config(Config::with_cases(
        env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_BLOCK_ROUNDTRIP_PROPTEST_CASES)
    ))]

    /// The layout scan ends every transaction where the decoder does, and a large block of
    /// copies of a transaction decodes the same in parallel as serially.
    #[test]
    fn layout_scan_finds_where_each_transaction_ends(
        transaction in any::<crate::transaction::Transaction>()
    ) {
        let _init_guard = zebra_test::init();

        let encoded = transaction.zcash_serialize_to_vec()?;
        let mut decoded = encoded.as_slice();
        if crate::transaction::Transaction::zcash_deserialize(&mut decoded).is_ok() {
            prop_assert!(decoded.is_empty());

            let mut scanned = encoded.as_slice();
            prop_assert_eq!(super::super::serialize::scan_transaction(&mut scanned), Some(()));
            prop_assert!(scanned.is_empty(), "the scan ends where the decoder ends");
        }

        // Copies of one transaction are enough for the decoder, which doesn't check duplicates.
        let transaction = Arc::new(transaction);
        let block = Block {
            header: Arc::new(super::generate::block_header().0),
            transactions: vec![transaction; 64],
        };
        super::vectors::assert_parallel_decode_matches_serial(&block.zcash_serialize_to_vec()?);
    }
}
