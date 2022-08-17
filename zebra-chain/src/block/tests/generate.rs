//! Generate large transparent blocks and transactions for testing.

use chrono::{DateTime, NaiveDateTime, Utc};
use std::sync::Arc;

use crate::{
    block::{serialize::MAX_BLOCK_BYTES, Block, Header},
    serialization::{ZcashDeserialize, ZcashSerialize},
    transaction::{LockTime, Transaction},
    transparent,
};

/// The minimum size of the blocks produced by this module.
pub const MIN_LARGE_BLOCK_BYTES: u64 = MAX_BLOCK_BYTES - 100;

/// The maximum number of bytes used to serialize a CompactSize,
/// for the transaction, input, and output counts generated by this module.
pub const MAX_COMPACT_SIZE_BYTES: usize = 4;

/// The number of bytes used to serialize a version 1 transaction header.
pub const TX_V1_HEADER_BYTES: usize = 4;

/// Returns a generated block header, and its canonical serialized bytes.
pub fn block_header() -> (Header, Vec<u8>) {
    // Some of the test vectors are in a non-canonical format,
    // so we have to round-trip serialize them.

    let block_header = Header::zcash_deserialize(&zebra_test::vectors::DUMMY_HEADER[..]).unwrap();
    let block_header_bytes = block_header.zcash_serialize_to_vec().unwrap();

    (block_header, block_header_bytes)
}

/// Returns a generated transparent transaction, and its canonical serialized bytes.
pub fn transaction() -> (Transaction, Vec<u8>) {
    // Some of the test vectors are in a non-canonical format,
    // so we have to round-trip serialize them.

    let transaction = Transaction::zcash_deserialize(&zebra_test::vectors::DUMMY_TX1[..]).unwrap();
    let transaction_bytes = transaction.zcash_serialize_to_vec().unwrap();

    (transaction, transaction_bytes)
}

/// Returns a generated transparent lock time, and its canonical serialized bytes.
pub fn lock_time() -> (LockTime, Vec<u8>) {
    let lock_time = LockTime::Time(DateTime::<Utc>::from_utc(
        NaiveDateTime::from_timestamp(61, 0),
        Utc,
    ));
    let lock_time_bytes = lock_time.zcash_serialize_to_vec().unwrap();

    (lock_time, lock_time_bytes)
}

/// Returns a generated transparent input, and its canonical serialized bytes.
pub fn input() -> (transparent::Input, Vec<u8>) {
    // Some of the test vectors are in a non-canonical format,
    // so we have to round-trip serialize them.

    let input =
        transparent::Input::zcash_deserialize(&zebra_test::vectors::DUMMY_INPUT1[..]).unwrap();
    let input_bytes = input.zcash_serialize_to_vec().unwrap();

    (input, input_bytes)
}

/// Returns a generated transparent output, and its canonical serialized bytes.
pub fn output() -> (transparent::Output, Vec<u8>) {
    // Some of the test vectors are in a non-canonical format,
    // so we have to round-trip serialize them.

    let output =
        transparent::Output::zcash_deserialize(&zebra_test::vectors::DUMMY_OUTPUT1[..]).unwrap();
    let output_bytes = output.zcash_serialize_to_vec().unwrap();

    (output, output_bytes)
}

/// Generate a block with multiple transparent transactions just below limit
///
/// TODO: add a coinbase height to the returned block
pub fn large_multi_transaction_block() -> Block {
    multi_transaction_block(false)
}

/// Generate a block with one transaction and multiple transparent inputs just below limit
///
/// TODO: add a coinbase height to the returned block
///       make the returned block stable under round-trip serialization
pub fn large_single_transaction_block_many_inputs() -> Block {
    single_transaction_block_many_inputs(false)
}

/// Generate a block with one transaction and multiple transparent outputs just below limit
///
/// TODO: add a coinbase height to the returned block
///       make the returned block stable under round-trip serialization
pub fn large_single_transaction_block_many_outputs() -> Block {
    single_transaction_block_many_outputs(false)
}

/// Generate a block with multiple transparent transactions just above limit
///
/// TODO: add a coinbase height to the returned block
pub fn oversized_multi_transaction_block() -> Block {
    multi_transaction_block(true)
}

/// Generate a block with one transaction and multiple transparent inputs just above limit
///
/// TODO: add a coinbase height to the returned block
///       make the returned block stable under round-trip serialization
pub fn oversized_single_transaction_block_many_inputs() -> Block {
    single_transaction_block_many_inputs(true)
}

/// Generate a block with one transaction and multiple transparent outputs just above limit
///
/// TODO: add a coinbase height to the returned block
///       make the returned block stable under round-trip serialization
pub fn oversized_single_transaction_block_many_outputs() -> Block {
    single_transaction_block_many_outputs(true)
}

/// Implementation of block generation with multiple transparent transactions
fn multi_transaction_block(oversized: bool) -> Block {
    // A dummy transaction
    let (transaction, transaction_bytes) = transaction();

    // A block header
    let (block_header, block_header_bytes) = block_header();

    // Calculate the number of transactions we need,
    // subtracting the bytes used to serialize the expected transaction count.
    let mut max_transactions_in_block = (usize::try_from(MAX_BLOCK_BYTES).unwrap()
        - block_header_bytes.len()
        - MAX_COMPACT_SIZE_BYTES)
        / transaction_bytes.len();
    if oversized {
        max_transactions_in_block += 1;
    }

    // Create transactions to be just below or just above the limit
    let transactions = std::iter::repeat(Arc::new(transaction))
        .take(max_transactions_in_block)
        .collect::<Vec<_>>();

    // Add the transactions into a block
    let block = Block {
        header: block_header.into(),
        transactions,
    };

    let serialized_len = block.zcash_serialize_to_vec().unwrap().len();
    assert_eq!(
        oversized,
        serialized_len > MAX_BLOCK_BYTES.try_into().unwrap(),
        "block is over-sized if requested:\n\
         oversized: {},\n\
         serialized_len: {},\n\
         MAX_BLOCK_BYTES: {},",
        oversized,
        serialized_len,
        MAX_BLOCK_BYTES,
    );
    assert!(
        serialized_len > MIN_LARGE_BLOCK_BYTES.try_into().unwrap(),
        "block is large\n\
         oversized: {},\n\
         serialized_len: {},\n\
         MIN_LARGE_BLOCK_BYTES: {},",
        oversized,
        serialized_len,
        MIN_LARGE_BLOCK_BYTES,
    );

    block
}

/// Implementation of block generation with one transaction and multiple transparent inputs
fn single_transaction_block_many_inputs(oversized: bool) -> Block {
    // Dummy input and output
    let (input, input_bytes) = input();
    let (output, output_bytes) = output();

    // A block header
    let (block_header, block_header_bytes) = block_header();

    // A LockTime
    let (lock_time, lock_time_bytes) = lock_time();

    // Calculate the number of inputs we need,
    // subtracting the bytes used to serialize the expected input count,
    // transaction count, and output count.
    let mut max_inputs_in_tx = (usize::try_from(MAX_BLOCK_BYTES).unwrap()
        - block_header_bytes.len()
        - 1
        - TX_V1_HEADER_BYTES
        - lock_time_bytes.len()
        - MAX_COMPACT_SIZE_BYTES
        - 1
        - output_bytes.len())
        / input_bytes.len();

    if oversized {
        max_inputs_in_tx += 1;
    }

    let mut outputs = Vec::new();

    // Create inputs to be just below the limit
    let inputs = std::iter::repeat(input)
        .take(max_inputs_in_tx)
        .collect::<Vec<_>>();

    // 1 single output
    outputs.push(output);

    // Create a big transaction
    let big_transaction = Transaction::V1 {
        inputs,
        outputs,
        lock_time,
    };

    // Put the big transaction into a block
    let transactions = vec![Arc::new(big_transaction)];

    let block = Block {
        header: block_header.into(),
        transactions,
    };

    let serialized_len = block.zcash_serialize_to_vec().unwrap().len();
    assert_eq!(
        oversized,
        serialized_len > MAX_BLOCK_BYTES.try_into().unwrap(),
        "block is over-sized if requested:\n\
         oversized: {},\n\
         serialized_len: {},\n\
         MAX_BLOCK_BYTES: {},",
        oversized,
        serialized_len,
        MAX_BLOCK_BYTES,
    );
    assert!(
        serialized_len > MIN_LARGE_BLOCK_BYTES.try_into().unwrap(),
        "block is large\n\
         oversized: {},\n\
         serialized_len: {},\n\
         MIN_LARGE_BLOCK_BYTES: {},",
        oversized,
        serialized_len,
        MIN_LARGE_BLOCK_BYTES,
    );

    block
}

/// Implementation of block generation with one transaction and multiple transparent outputs
fn single_transaction_block_many_outputs(oversized: bool) -> Block {
    // Dummy input and output
    let (input, input_bytes) = input();
    let (output, output_bytes) = output();

    // A block header
    let (block_header, block_header_bytes) = block_header();

    // A LockTime
    let (lock_time, lock_time_bytes) = lock_time();

    // Calculate the number of outputs we need,
    // subtracting the bytes used to serialize the expected output count,
    // transaction count, and input count.
    let mut max_outputs_in_tx = (usize::try_from(MAX_BLOCK_BYTES).unwrap()
        - block_header_bytes.len()
        - 1
        - TX_V1_HEADER_BYTES
        - lock_time_bytes.len()
        - 1
        - input_bytes.len()
        - MAX_COMPACT_SIZE_BYTES)
        / output_bytes.len();

    if oversized {
        max_outputs_in_tx += 1;
    }

    // 1 single input
    let inputs = vec![input];

    // Create outputs to be just below the limit
    let outputs = std::iter::repeat(output)
        .take(max_outputs_in_tx)
        .collect::<Vec<_>>();

    // Create a big transaction
    let big_transaction = Transaction::V1 {
        inputs,
        outputs,
        lock_time,
    };

    // Put the big transaction into a block
    let transactions = vec![Arc::new(big_transaction)];

    let block = Block {
        header: block_header.into(),
        transactions,
    };

    let serialized_len = block.zcash_serialize_to_vec().unwrap().len();
    assert_eq!(
        oversized,
        serialized_len > MAX_BLOCK_BYTES.try_into().unwrap(),
        "block is over-sized if requested:\n\
         oversized: {},\n\
         serialized_len: {},\n\
         MAX_BLOCK_BYTES: {},",
        oversized,
        serialized_len,
        MAX_BLOCK_BYTES,
    );
    assert!(
        serialized_len > MIN_LARGE_BLOCK_BYTES.try_into().unwrap(),
        "block is large\n\
         oversized: {},\n\
         serialized_len: {},\n\
         MIN_LARGE_BLOCK_BYTES: {},",
        oversized,
        serialized_len,
        MIN_LARGE_BLOCK_BYTES,
    );

    block
}
