use std::{
    collections::HashSet,
    io::{Cursor, Write},
};

use chrono::{DateTime, Duration, LocalResult, TimeZone, Utc};

use crate::{
    block::{
        serialize::MAX_BLOCK_BYTES, Block, BlockTimeError, Commitment::*, Hash, Header, Height,
    },
    parameters::{
        Network::{self, *},
        NetworkUpgrade::*,
    },
    serialization::{sha256d, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
    transaction::LockTime,
};

use super::generate; // XXX this should be rewritten as strategies

#[test]
fn blockheaderhash_debug() {
    let _init_guard = zebra_test::init();

    let preimage = b"foo bar baz";
    let mut sha_writer = sha256d::Writer::default();
    let _ = sha_writer.write_all(preimage);

    let hash = Hash(sha_writer.finish());

    assert_eq!(
        format!("{:?}", hash),
        "block::Hash(\"3166411bd5343e0b284a108f39a929fbbb62619784f8c6dafe520703b5b446bf\")"
    );
}

#[test]
fn blockheaderhash_from_blockheader() {
    let _init_guard = zebra_test::init();

    let (blockheader, _blockheader_bytes) = generate::block_header();

    let hash = Hash::from(&blockheader);

    assert_eq!(
        format!("{:?}", hash),
        "block::Hash(\"d1d6974bbe1d4d127c889119b2fc05724c67588dc72708839727586b8c2bc939\")"
    );

    let mut bytes = Cursor::new(Vec::new());

    blockheader
        .zcash_serialize(&mut bytes)
        .expect("these bytes to serialize from a blockheader without issue");

    bytes.set_position(0);
    let other_header = bytes
        .zcash_deserialize_into()
        .expect("these bytes to deserialize into a blockheader without issue");

    assert_eq!(blockheader, other_header);
}

#[test]
fn deserialize_blockheader() {
    let _init_guard = zebra_test::init();

    // Includes the 32-byte nonce and 3-byte equihash length field.
    const BLOCK_HEADER_LENGTH: usize = crate::work::equihash::Solution::INPUT_LENGTH
        + 32
        + 3
        + crate::work::equihash::SOLUTION_SIZE;

    for block in zebra_test::vectors::BLOCKS.iter() {
        let header_bytes = &block[..BLOCK_HEADER_LENGTH];

        let _header = header_bytes
            .zcash_deserialize_into::<Header>()
            .expect("blockheader test vector should deserialize");
    }
}

#[test]
fn round_trip_blocks() {
    let _init_guard = zebra_test::init();

    // this one has a bad version field, but it is still valid
    zebra_test::vectors::BLOCK_MAINNET_434873_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("bad version block test vector should deserialize");

    // now do a round-trip test on all the block test vectors
    for block_bytes in zebra_test::vectors::BLOCKS.iter() {
        let block = block_bytes
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        let round_trip_bytes = block
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");

        assert_eq!(&round_trip_bytes[..], *block_bytes);
    }
}

#[test]
fn coinbase_parsing_rejects_above_0x80() {
    let _init_guard = zebra_test::init();

    zebra_test::vectors::BAD_BLOCK_MAINNET_202_BYTES
        .zcash_deserialize_into::<Block>()
        .expect_err("parsing fails");
}

#[test]
fn block_test_vectors_unique() {
    let _init_guard = zebra_test::init();

    let block_count = zebra_test::vectors::BLOCKS.len();
    let block_hashes: HashSet<_> = zebra_test::vectors::BLOCKS
        .iter()
        .map(|b| {
            b.zcash_deserialize_into::<Block>()
                .expect("block is structurally valid")
                .hash()
        })
        .collect();

    // putting the same block in two files is an easy mistake to make
    assert_eq!(
        block_count,
        block_hashes.len(),
        "block test vectors must be unique"
    );
}

#[test]
fn block_test_vectors_height_mainnet() {
    let _init_guard = zebra_test::init();

    block_test_vectors_height(Mainnet);
}

#[test]
fn block_test_vectors_height_testnet() {
    let _init_guard = zebra_test::init();

    block_test_vectors_height(Testnet);
}

/// Test that the block test vector indexes match the heights in the block data,
/// and that each post-sapling block has a corresponding final sapling root.
fn block_test_vectors_height(network: Network) {
    let (block_iter, sapling_roots) = match network {
        Mainnet => (
            zebra_test::vectors::MAINNET_BLOCKS.iter(),
            zebra_test::vectors::MAINNET_FINAL_SAPLING_ROOTS.clone(),
        ),
        Testnet => (
            zebra_test::vectors::TESTNET_BLOCKS.iter(),
            zebra_test::vectors::TESTNET_FINAL_SAPLING_ROOTS.clone(),
        ),
    };

    for (&height, block) in block_iter {
        let block = block
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");
        assert_eq!(
            block.coinbase_height().expect("block height is valid").0,
            height,
            "deserialized height must match BTreeMap key height"
        );

        if height
            >= Sapling
                .activation_height(network)
                .expect("sapling activation height is set")
                .0
        {
            assert!(
                sapling_roots.contains_key(&height),
                "post-sapling block test vectors must have matching sapling root test vectors: missing {} {}",
                network,
                height
            );
        }
    }
}

#[test]
fn block_commitment_mainnet() {
    let _init_guard = zebra_test::init();

    block_commitment(Mainnet);
}

#[test]
fn block_commitment_testnet() {
    let _init_guard = zebra_test::init();

    block_commitment(Testnet);
}

/// Check that the block commitment field parses without errors.
/// For sapling and blossom blocks, also check the final sapling root value.
///
/// TODO: add chain history test vectors?
fn block_commitment(network: Network) {
    let (block_iter, sapling_roots) = match network {
        Mainnet => (
            zebra_test::vectors::MAINNET_BLOCKS.iter(),
            zebra_test::vectors::MAINNET_FINAL_SAPLING_ROOTS.clone(),
        ),
        Testnet => (
            zebra_test::vectors::TESTNET_BLOCKS.iter(),
            zebra_test::vectors::TESTNET_FINAL_SAPLING_ROOTS.clone(),
        ),
    };

    for (height, block) in block_iter {
        let block = block
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        let commitment = block.commitment(network).unwrap_or_else(|_| {
            panic!(
                "unexpected structurally invalid block commitment at {} {}",
                network, height
            )
        });

        if let FinalSaplingRoot(final_sapling_root) = commitment {
            let expected_final_sapling_root = *sapling_roots
                .get(height)
                .expect("unexpected missing final sapling root test vector");
            assert_eq!(
                final_sapling_root,
                crate::sapling::tree::Root::try_from(*expected_final_sapling_root).unwrap(),
                "unexpected invalid final sapling root commitment at {} {}",
                network,
                height
            );
        }
    }
}

#[test]
fn block_limits_multi_tx() {
    let _init_guard = zebra_test::init();

    // Test multiple small transactions to fill a block max size

    // Create a block just below the limit
    let mut block = generate::large_multi_transaction_block();

    // Serialize the block
    let mut data = Vec::new();
    block
        .zcash_serialize(&mut data)
        .expect("block should serialize as we are not limiting generation yet");

    assert!(data.len() <= MAX_BLOCK_BYTES as usize);

    // Deserialize by now is ok as we are lower than the limit
    let block2 = Block::zcash_deserialize(&data[..])
        .expect("block should deserialize as we are just below limit");
    assert_eq!(block, block2);

    // Add 1 more transaction to the block, limit will be reached
    block = generate::oversized_multi_transaction_block();

    // Serialize will still be fine
    let mut data = Vec::new();
    block
        .zcash_serialize(&mut data)
        .expect("block should serialize as we are not limiting generation yet");

    assert!(data.len() > MAX_BLOCK_BYTES as usize);

    // Deserialize will now fail
    Block::zcash_deserialize(&data[..]).expect_err("block should not deserialize");
}

#[test]
fn block_limits_single_tx() {
    let _init_guard = zebra_test::init();

    // Test block limit with a big single transaction

    // Create a block just below the limit
    let mut block = generate::large_single_transaction_block_many_inputs();

    // Serialize the block
    let mut data = Vec::new();
    block
        .zcash_serialize(&mut data)
        .expect("block should serialize as we are not limiting generation yet");

    assert!(data.len() <= MAX_BLOCK_BYTES as usize);

    // Deserialize by now is ok as we are lower than the limit
    Block::zcash_deserialize(&data[..])
        .expect("block should deserialize as we are just below limit");

    // Add 1 more input to the transaction, limit will be reached
    block = generate::oversized_single_transaction_block_many_inputs();

    let mut data = Vec::new();
    block
        .zcash_serialize(&mut data)
        .expect("block should serialize as we are not limiting generation yet");

    assert!(data.len() > MAX_BLOCK_BYTES as usize);

    // Will fail as block overall size is above limit
    Block::zcash_deserialize(&data[..]).expect_err("block should not deserialize");
}

/// Test wrapper for `BlockHeader.time_is_valid_at`.
///
/// Generates a block header, sets its `time` to `block_header_time`, then
/// calls `time_is_valid_at`.
fn node_time_check(
    block_header_time: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), BlockTimeError> {
    let (mut header, _header_bytes) = generate::block_header();
    header.time = block_header_time;
    // pass a zero height and hash - they are only used in the returned error
    header.time_is_valid_at(now, &Height(0), &Hash([0; 32]))
}

#[test]
fn time_check_now() {
    let _init_guard = zebra_test::init();

    // These checks are deteministic, because all the times are offset
    // from the current time.
    let now = Utc::now();
    let three_hours_in_the_past = now - Duration::hours(3);
    let two_hours_in_the_future = now + Duration::hours(2);
    let two_hours_and_one_second_in_the_future = now + Duration::hours(2) + Duration::seconds(1);

    node_time_check(now, now).expect("the current time should be valid as a block header time");
    node_time_check(three_hours_in_the_past, now)
        .expect("a past time should be valid as a block header time");
    node_time_check(two_hours_in_the_future, now)
        .expect("2 hours in the future should be valid as a block header time");
    node_time_check(two_hours_and_one_second_in_the_future, now)
        .expect_err("2 hours and 1 second in the future should be invalid as a block header time");

    // Now invert the tests
    // 3 hours in the future should fail
    node_time_check(now, three_hours_in_the_past)
        .expect_err("3 hours in the future should be invalid as a block header time");
    // The past should succeed
    node_time_check(now, two_hours_in_the_future)
        .expect("2 hours in the past should be valid as a block header time");
    node_time_check(now, two_hours_and_one_second_in_the_future)
        .expect("2 hours and 1 second in the past should be valid as a block header time");
}

/// Valid unix epoch timestamps for blocks, in seconds
static BLOCK_HEADER_VALID_TIMESTAMPS: &[i64] = &[
    // These times are currently invalid DateTimes, but they could
    // become valid in future chrono versions
    i64::MIN,
    i64::MIN + 1,
    // These times are valid DateTimes
    (i32::MIN as i64) - 1,
    (i32::MIN as i64),
    (i32::MIN as i64) + 1,
    -1,
    0,
    1,
    LockTime::MIN_TIMESTAMP - 1,
    LockTime::MIN_TIMESTAMP,
    LockTime::MIN_TIMESTAMP + 1,
];

/// Invalid unix epoch timestamps for blocks, in seconds
static BLOCK_HEADER_INVALID_TIMESTAMPS: &[i64] = &[
    (i32::MAX as i64) - 1,
    (i32::MAX as i64),
    (i32::MAX as i64) + 1,
    LockTime::MAX_TIMESTAMP - 1,
    LockTime::MAX_TIMESTAMP,
    LockTime::MAX_TIMESTAMP + 1,
    // These times are currently invalid DateTimes, but they could
    // become valid in future chrono versions
    i64::MAX - 1,
    i64::MAX,
];

#[test]
fn time_check_fixed() {
    let _init_guard = zebra_test::init();

    // These checks are non-deterministic, but the times are all in the
    // distant past or far future. So it's unlikely that the test
    // machine will have a clock that makes these tests fail.
    let now = Utc::now();

    for valid_timestamp in BLOCK_HEADER_VALID_TIMESTAMPS {
        let block_header_time = match Utc.timestamp_opt(*valid_timestamp, 0) {
            LocalResult::Single(time) => time,
            LocalResult::None => {
                // Skip the test if the timestamp is invalid
                continue;
            }
            LocalResult::Ambiguous(_, _) => {
                // Utc doesn't have ambiguous times
                unreachable!();
            }
        };
        node_time_check(block_header_time, now)
            .expect("the time should be valid as a block header time");
        // Invert the check, leading to an invalid time
        node_time_check(now, block_header_time)
            .expect_err("the inverse comparison should be invalid");
    }

    for invalid_timestamp in BLOCK_HEADER_INVALID_TIMESTAMPS {
        let block_header_time = match Utc.timestamp_opt(*invalid_timestamp, 0) {
            LocalResult::Single(time) => time,
            LocalResult::None => {
                // Skip the test if the timestamp is invalid
                continue;
            }
            LocalResult::Ambiguous(_, _) => {
                // Utc doesn't have ambiguous times
                unreachable!();
            }
        };
        node_time_check(block_header_time, now)
            .expect_err("the time should be invalid as a block header time");
        // Invert the check, leading to a valid time
        node_time_check(now, block_header_time).expect("the inverse comparison should be valid");
    }
}
