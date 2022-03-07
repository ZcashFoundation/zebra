//! Generate blockchain testing constructions
use chrono::{DateTime, NaiveDateTime, Utc};
use std::sync::Arc;

use crate::{
    serialization::{ZcashDeserialize, ZcashSerialize},
    transaction::{LockTime, Transaction},
    transparent,
};

use super::super::{serialize::MAX_BLOCK_BYTES, Block, Header};

/// Generate a block header
pub fn block_header() -> Header {
    Header::zcash_deserialize(&zebra_test::vectors::DUMMY_HEADER[..]).unwrap()
}

/// Generate a block with multiple transactions just below limit
pub fn large_multi_transaction_block() -> Block {
    multi_transaction_block(false)
}

/// Generate a block with one transaction and multiple inputs just below limit
pub fn large_single_transaction_block_many_inputs() -> Block {
    single_transaction_block_many_inputs(false)
}

/// Generate a block with multiple transactions just above limit
pub fn oversized_multi_transaction_block() -> Block {
    multi_transaction_block(true)
}

/// Generate a block with one transaction and multiple inputs just above limit
pub fn oversized_single_transaction_block_many_inputs() -> Block {
    single_transaction_block_many_inputs(true)
}

// Implementation of block generation with multiple transactions
fn multi_transaction_block(oversized: bool) -> Block {
    // A dummy transaction
    let tx = Transaction::zcash_deserialize(&zebra_test::vectors::DUMMY_TX1[..]).unwrap();

    // A block header
    let header = block_header();

    // Serialize header
    let mut data_header = Vec::new();
    header
        .zcash_serialize(&mut data_header)
        .expect("Block header should serialize");

    // Calculate the number of transactions we need
    let mut max_transactions_in_block =
        (MAX_BLOCK_BYTES as usize - data_header.len()) / zebra_test::vectors::DUMMY_TX1[..].len();
    if oversized {
        max_transactions_in_block += 1;
    }

    // Create transactions to be just below or just above the limit
    let transactions = std::iter::repeat(Arc::new(tx))
        .take(max_transactions_in_block)
        .collect::<Vec<_>>();

    // Add the transactions into a block
    Block {
        header,
        transactions,
    }
}

// Implementation of block generation with one transaction and multiple inputs
fn single_transaction_block_many_inputs(oversized: bool) -> Block {
    // Dummy input and output
    let input =
        transparent::Input::zcash_deserialize(&zebra_test::vectors::DUMMY_INPUT1[..]).unwrap();
    let output =
        transparent::Output::zcash_deserialize(&zebra_test::vectors::DUMMY_OUTPUT1[..]).unwrap();

    // A block header
    let header = block_header();

    // Serialize header
    let mut data_header = Vec::new();
    header
        .zcash_serialize(&mut data_header)
        .expect("Block header should serialize");

    // Serialize a LockTime
    let lock_time = LockTime::Time(DateTime::<Utc>::from_utc(
        NaiveDateTime::from_timestamp(61, 0),
        Utc,
    ));
    let mut data_locktime = Vec::new();
    lock_time
        .zcash_serialize(&mut data_locktime)
        .expect("LockTime should serialize");

    // Calculate the number of inputs we need
    let mut max_inputs_in_tx = (MAX_BLOCK_BYTES as usize
        - data_header.len()
        - zebra_test::vectors::DUMMY_OUTPUT1[..].len()
        - data_locktime.len())
        / (zebra_test::vectors::DUMMY_INPUT1[..].len() - 1);

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
    Block {
        header,
        transactions,
    }
}
