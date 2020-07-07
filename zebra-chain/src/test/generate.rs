//! Generate blockchain testing constructions
use chrono::{DateTime, NaiveDateTime, Utc};
use std::sync::Arc;

use crate::{
    block::{Block, BlockHeader, BlockHeaderHash, MAX_BLOCK_BYTES},
    equihash_solution::EquihashSolution,
    merkle_tree::MerkleTreeRootHash,
    note_commitment_tree::SaplingNoteTreeRootHash,
    serialization::{ZcashDeserialize, ZcashSerialize},
    transaction::{Transaction, TransparentInput, TransparentOutput},
    types::LockTime,
};

/// Generate a block header
pub fn block_header() -> BlockHeader {
    let some_bytes = [0; 32];
    BlockHeader {
        version: 4,
        previous_block_hash: BlockHeaderHash(some_bytes),
        merkle_root_hash: MerkleTreeRootHash(some_bytes),
        final_sapling_root_hash: SaplingNoteTreeRootHash(some_bytes),
        time: DateTime::<Utc>::from_utc(NaiveDateTime::from_timestamp(61, 0), Utc),
        bits: 0,
        nonce: some_bytes,
        solution: EquihashSolution([0; 1344]),
    }
}

/// Generate a block with multiple transactions just below limit
pub fn large_multi_transaction_block() -> Block {
    multi_transaction_block(false)
}

/// Generate a block with one transaction and multiple inputs just below limit
pub fn large_single_transaction_block() -> Block {
    single_transaction_block(false)
}

/// Generate a block with multiple transactions just above limit
pub fn oversized_multi_transaction_block() -> Block {
    multi_transaction_block(true)
}

/// Generate a block with one transaction and multiple inputs just above limit
pub fn oversized_single_transaction_block() -> Block {
    single_transaction_block(true)
}

// Implementation of block generation with multiple transactions
fn multi_transaction_block(oversized: bool) -> Block {
    // A dummy transaction
    let tx = Transaction::zcash_deserialize(&zebra_test::vectors::DUMMY_TX1[..]).unwrap();

    // A block header
    let blockheader = block_header();

    // Serialize header
    let mut data_header = Vec::new();
    blockheader
        .zcash_serialize(&mut data_header)
        .expect("Block header should serialize");

    // Calculate the number of transactions we need
    let mut max_transactions_in_block =
        (MAX_BLOCK_BYTES as usize - data_header.len()) / *&zebra_test::vectors::DUMMY_TX1[..].len();
    if oversized {
        max_transactions_in_block = max_transactions_in_block + 1;
    }

    // Create transactions to be just below or just above the limit
    let many_transactions = std::iter::repeat(Arc::new(tx.clone()))
        .take(max_transactions_in_block)
        .collect::<Vec<_>>();

    // Add the transactions into a block
    Block {
        header: blockheader,
        transactions: many_transactions,
    }
}

// Implementation of block generation with one transaction and multiple inputs
fn single_transaction_block(oversized: bool) -> Block {
    // Dummy input and output
    let input =
        TransparentInput::zcash_deserialize(&zebra_test::vectors::DUMMY_INPUT1[..]).unwrap();
    let output =
        TransparentOutput::zcash_deserialize(&zebra_test::vectors::DUMMY_OUTPUT1[..]).unwrap();

    // A block header
    let blockheader = block_header();

    // Serialize header
    let mut data_header = Vec::new();
    blockheader
        .zcash_serialize(&mut data_header)
        .expect("Block header should serialize");

    // Serialize a LockTime
    let locktime = LockTime::Time(DateTime::<Utc>::from_utc(
        NaiveDateTime::from_timestamp(61, 0),
        Utc,
    ));
    let mut data_locktime = Vec::new();
    locktime
        .zcash_serialize(&mut data_locktime)
        .expect("LockTime should serialize");

    // Calculate the number of inputs we need
    let mut max_inputs_in_tx = (MAX_BLOCK_BYTES as usize
        - data_header.len()
        - *&zebra_test::vectors::DUMMY_OUTPUT1[..].len()
        - data_locktime.len())
        / (*&zebra_test::vectors::DUMMY_INPUT1[..].len() - 1);

    if oversized {
        max_inputs_in_tx = max_inputs_in_tx + 1;
    }

    let mut outputs = Vec::new();

    // Create inputs to be just below the limit
    let inputs = std::iter::repeat(input.clone())
        .take(max_inputs_in_tx)
        .collect::<Vec<_>>();

    // 1 single output
    outputs.push(output);

    // Create a big transaction
    let big_transaction = Transaction::V1 {
        inputs: inputs.clone(),
        outputs: outputs.clone(),
        lock_time: locktime,
    };

    // Put the big transaction into a block
    let transactions = vec![Arc::new(big_transaction.clone())];
    Block {
        header: blockheader,
        transactions: transactions.clone(),
    }
}
