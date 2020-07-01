use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use std::io::{Cursor, ErrorKind, Write};

use proptest::{
    arbitrary::{any, Arbitrary},
    prelude::*,
};

use crate::equihash_solution::EquihashSolution;
use crate::merkle_tree::MerkleTreeRootHash;
use crate::note_commitment_tree::SaplingNoteTreeRootHash;
use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};
use crate::sha256d_writer::Sha256dWriter;
use crate::transaction::TransparentInput;
use crate::transaction::TransparentOutput;
use crate::types::LockTime;

use super::*;

#[cfg(test)]
impl Arbitrary for BlockHeader {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        (
            // version is interpreted as i32 in the spec, so we are limited to i32::MAX here
            (4u32..(i32::MAX as u32)),
            any::<BlockHeaderHash>(),
            any::<MerkleTreeRootHash>(),
            any::<SaplingNoteTreeRootHash>(),
            // time is interpreted as u32 in the spec, but rust timestamps are i64
            (0i64..(u32::MAX as i64)),
            any::<u32>(),
            any::<[u8; 32]>(),
            any::<EquihashSolution>(),
        )
            .prop_map(
                |(
                    version,
                    previous_block_hash,
                    merkle_root_hash,
                    final_sapling_root_hash,
                    timestamp,
                    bits,
                    nonce,
                    solution,
                )| BlockHeader {
                    version,
                    previous_block_hash,
                    merkle_root_hash,
                    final_sapling_root_hash,
                    time: Utc.timestamp(timestamp, 0),
                    bits,
                    nonce,
                    solution,
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

#[test]
fn blockheaderhash_debug() {
    let preimage = b"foo bar baz";
    let mut sha_writer = Sha256dWriter::default();
    let _ = sha_writer.write_all(preimage);

    let hash = BlockHeaderHash(sha_writer.finish());

    assert_eq!(
        format!("{:?}", hash),
        "BlockHeaderHash(\"bf46b4b5030752fedac6f884976162bbfb29a9398f104a280b3e34d51b416631\")"
    );
}

fn generate_block_header() -> BlockHeader {
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

#[test]
fn blockheaderhash_from_blockheader() {
    let blockheader = generate_block_header();

    let hash = BlockHeaderHash::from(&blockheader);

    assert_eq!(
        format!("{:?}", hash),
        "BlockHeaderHash(\"39c92b8c6b582797830827c78d58674c7205fcb21991887c124d1dbe4b97d6d1\")"
    );

    let mut bytes = Cursor::new(Vec::new());

    blockheader
        .zcash_serialize(&mut bytes)
        .expect("these bytes to serialize from a blockheader without issue");

    bytes.set_position(0);
    let other_header = BlockHeader::zcash_deserialize(&mut bytes)
        .expect("these bytes to deserialize into a blockheader without issue");

    assert_eq!(blockheader, other_header);
}

#[test]
fn deserialize_blockheader() {
    // https://explorer.zcha.in/blocks/415000
    let _header =
        BlockHeader::zcash_deserialize(&zebra_test::vectors::HEADER_MAINNET_415000_BYTES[..])
            .expect("blockheader test vector should deserialize");
}

#[test]
fn deserialize_block() {
    Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
        .expect("block test vector should deserialize");
    Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..])
        .expect("block test vector should deserialize");
    // https://explorer.zcha.in/blocks/415000
    Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])
        .expect("block test vector should deserialize");
    // https://explorer.zcha.in/blocks/434873
    // this one has a bad version field
    Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_434873_BYTES[..])
        .expect("block test vector should deserialize");
}

#[test]
fn block_limits_multi_tx() {
    // Test multiple small transactions to fill a block max size
    // A dummy transaction
    let tx = Transaction::zcash_deserialize(&zebra_test::vectors::DUMMY_TX1[..]).unwrap();

    // A block header
    let blockheader = generate_block_header();

    // Serialize header
    let mut data_header = Vec::new();
    blockheader
        .zcash_serialize(&mut data_header)
        .expect("Block header should serialize");

    // Calculate the number of transactions we need
    let mut max_transactions_in_block =
        (MAX_BLOCK_BYTES as usize - data_header.len()) / *&zebra_test::vectors::DUMMY_TX1[..].len();

    // Create transactions to be just below the limit
    let mut many_transactions = std::iter::repeat(Arc::new(tx.clone()))
        .take(max_transactions_in_block)
        .collect::<Vec<_>>();

    // Add the transactions into a block
    let mut block = Block {
        header: blockheader,
        transactions: many_transactions.clone(),
    };

    // Serialize the block
    let mut data = Vec::new();
    block
        .zcash_serialize(&mut data)
        .expect("block should serialize as we are not limiting generation yet");

    // Deserialize by now is ok as we are lower than the limit
    let block2 = Block::zcash_deserialize(&data[..])
        .expect("block should deserialize as we are just below limit");

    assert_eq!(block, block2);
    assert_eq!(max_transactions_in_block, block2.transactions.len());

    // Add 1 more transaction to the block, limit will be reached
    max_transactions_in_block = max_transactions_in_block + 1;
    many_transactions.push(Arc::new(tx));
    block.transactions = many_transactions.clone();
    assert_eq!(max_transactions_in_block, many_transactions.len());

    // Serialize will still be fine
    let mut data = Vec::new();
    block
        .zcash_serialize(&mut data)
        .expect("block should serialize as we are not limiting generation yet");

    // Deserialize will now fail
    Block::zcash_deserialize(&data[..]).expect_err("block should not deserialize");
}

#[test]
fn block_limits_single_tx() {
    // Test block limit with a big single transaction
    // Dummy input and output
    let input =
        TransparentInput::zcash_deserialize(&zebra_test::vectors::DUMMY_INPUT1[..]).unwrap();
    let output =
        TransparentOutput::zcash_deserialize(&zebra_test::vectors::DUMMY_OUTPUT1[..]).unwrap();

    // A block header
    let blockheader = generate_block_header();

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

    let mut outputs = Vec::new();

    // Create inputs to be just below the limit
    let mut inputs = std::iter::repeat(input.clone())
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
    let block = Block {
        header: blockheader,
        transactions: transactions.clone(),
    };

    let mut data = Vec::new();
    block
        .zcash_serialize(&mut data)
        .expect("block should serialize as we are not limiting generation yet");

    Block::zcash_deserialize(&data[..])
        .expect("block should deserialize as we are just below limit");

    // Add 1 more input to the transaction, limit will be reached
    max_inputs_in_tx = max_inputs_in_tx + 1;
    inputs.push(input);
    assert_eq!(max_inputs_in_tx, inputs.len());

    let new_big_transaction = Transaction::V1 {
        inputs: inputs.clone(),
        outputs: outputs.clone(),
        lock_time: locktime,
    };

    let mut transactions = Vec::new();
    transactions.push(Arc::new(new_big_transaction.clone()));
    let new_block = Block {
        header: blockheader,
        transactions: transactions,
    };

    let mut data = Vec::new();
    new_block
        .zcash_serialize(&mut data)
        .expect("block should serialize as we are not limiting generation yet");

    // Will fail as block overall size is above limit
    Block::zcash_deserialize(&data[..]).expect_err("block should not deserialize");
}

proptest! {

    #[test]
    fn blockheaderhash_roundtrip(hash in any::<BlockHeaderHash>()) {
        let mut bytes = Cursor::new(Vec::new());
        hash.zcash_serialize(&mut bytes)?;

        bytes.set_position(0);
        let other_hash = BlockHeaderHash::zcash_deserialize(&mut bytes)?;

        prop_assert_eq![hash, other_hash];
    }

    #[test]
    fn blockheader_roundtrip(header in any::<BlockHeader>()) {
        let mut bytes = Cursor::new(Vec::new());
        header.zcash_serialize(&mut bytes)?;

        bytes.set_position(0);
        let other_header = BlockHeader::zcash_deserialize(&mut bytes)?;

        prop_assert_eq![header, other_header];
    }

    #[test]
    fn block_roundtrip(block in any::<Block>()) {
        let mut bytes = Cursor::new(Vec::new());
        block.zcash_serialize(&mut bytes)?;

        // Check the block size limit
        if bytes.position() <= MAX_BLOCK_BYTES {
            bytes.set_position(0);
            let other_block = Block::zcash_deserialize(&mut bytes)?;

            prop_assert_eq![block, other_block];
        } else {
            let serialization_err = Block::zcash_deserialize(&mut bytes)
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
