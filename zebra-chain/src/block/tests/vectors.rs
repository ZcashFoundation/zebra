use std::{
    io::{Cursor, Write},
    sync::Arc,
};

use chrono::{DateTime, Duration, LocalResult, TimeZone, Utc};

use crate::serialization::{sha256d, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize};
use crate::transaction::LockTime;

use super::super::{serialize::MAX_BLOCK_BYTES, *};
use super::generate; // XXX this should be rewritten as strategies

#[test]
fn blockheaderhash_debug() {
    let preimage = b"foo bar baz";
    let mut sha_writer = sha256d::Writer::default();
    let _ = sha_writer.write_all(preimage);

    let hash = Hash(sha_writer.finish());

    assert_eq!(
        format!("{:?}", hash),
        "BlockHeaderHash(\"bf46b4b5030752fedac6f884976162bbfb29a9398f104a280b3e34d51b416631\")"
    );
}

#[test]
fn blockheaderhash_from_blockheader() {
    let blockheader = generate::block_header();

    let hash = Hash::from(&blockheader);

    assert_eq!(
        format!("{:?}", hash),
        "BlockHeaderHash(\"39c92b8c6b582797830827c78d58674c7205fcb21991887c124d1dbe4b97d6d1\")"
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
    // https://explorer.zcha.in/blocks/415000
    let _header = zebra_test::vectors::HEADER_MAINNET_415000_BYTES
        .zcash_deserialize_into::<Header>()
        .expect("blockheader test vector should deserialize");
}

#[test]
fn deserialize_block() {
    zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block test vector should deserialize");
    zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block test vector should deserialize");
    // https://explorer.zcha.in/blocks/415000
    zebra_test::vectors::BLOCK_MAINNET_415000_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block test vector should deserialize");
    // https://explorer.zcha.in/blocks/434873
    // this one has a bad version field
    zebra_test::vectors::BLOCK_MAINNET_434873_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block test vector should deserialize");
}

#[test]
fn block_limits_multi_tx() {
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
    // Test block limit with a big single transaction

    // Create a block just below the limit
    let mut block = generate::large_single_transaction_block();

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
    block = generate::oversized_single_transaction_block();

    let mut data = Vec::new();
    block
        .zcash_serialize(&mut data)
        .expect("block should serialize as we are not limiting generation yet");

    assert!(data.len() > MAX_BLOCK_BYTES as usize);

    // Will fail as block overall size is above limit
    Block::zcash_deserialize(&data[..]).expect_err("block should not deserialize");
}

#[test]
fn time_check_past_block() {
    // This block is also verified as part of the BlockVerifier service
    // tests.
    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])
            .expect("block should deserialize");
    let now = Utc::now();

    // This check is non-deterministic, but BLOCK_MAINNET_415000 is
    // a long time in the past. So it's unlikely that the test machine
    // will have a clock that's far enough in the past for the test to
    // fail.
    block
        .header
        .is_time_valid_at(now)
        .expect("the header time from a mainnet block should be valid");
}

/// Test wrapper for `BlockHeader.is_time_valid_at`.
///
/// Generates a block header, sets its `time` to `block_header_time`, then
/// calls `is_time_valid_at`.
fn node_time_check(block_header_time: DateTime<Utc>, now: DateTime<Utc>) -> Result<(), Error> {
    let mut header = generate::block_header();
    header.time = block_header_time;
    header.is_time_valid_at(now)
}

#[test]
fn time_check_now() {
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
