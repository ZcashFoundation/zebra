use std::convert::TryInto;

use crate::{
    block::{Block, MAX_BLOCK_BYTES},
    serialization::{CompactSizeMessage, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
    work::equihash::{Solution, SOLUTION_SIZE},
};

use super::super::*;

/// Includes the 32-byte nonce.
const EQUIHASH_SOLUTION_BLOCK_OFFSET: usize = equihash::Solution::INPUT_LENGTH + 32;

/// Includes the 3-byte equihash length field.
const BLOCK_HEADER_LENGTH: usize = EQUIHASH_SOLUTION_BLOCK_OFFSET + 3 + equihash::SOLUTION_SIZE;

#[test]
fn equihash_solution_test_vectors() {
    let _init_guard = zebra_test::init();

    for block in zebra_test::vectors::BLOCKS.iter() {
        let solution_bytes = &block[EQUIHASH_SOLUTION_BLOCK_OFFSET..BLOCK_HEADER_LENGTH];

        let solution = solution_bytes
            .zcash_deserialize_into::<equihash::Solution>()
            .expect("Test vector EquihashSolution should deserialize");

        let mut data = Vec::new();
        solution
            .zcash_serialize(&mut data)
            .expect("Test vector EquihashSolution should serialize");

        assert_eq!(solution_bytes.len(), data.len());
        assert_eq!(solution_bytes, data.as_slice());
    }
}

#[test]
fn equihash_solution_test_vectors_are_valid() -> color_eyre::eyre::Result<()> {
    let _init_guard = zebra_test::init();

    for block in zebra_test::vectors::BLOCKS.iter() {
        let block =
            Block::zcash_deserialize(&block[..]).expect("block test vector should deserialize");

        block.header.solution.check(&block.header)?;
    }

    Ok(())
}

static EQUIHASH_SIZE_TESTS: &[usize] = &[
    0,
    1,
    SOLUTION_SIZE - 1,
    SOLUTION_SIZE,
    SOLUTION_SIZE + 1,
    (MAX_BLOCK_BYTES - 1) as usize,
    MAX_BLOCK_BYTES as usize,
];

#[test]
fn equihash_solution_size_field() {
    let _init_guard = zebra_test::init();

    for size in EQUIHASH_SIZE_TESTS.iter().copied() {
        let mut data = Vec::new();

        let size: CompactSizeMessage = size
            .try_into()
            .expect("test size fits in MAX_PROTOCOL_MESSAGE_LEN");
        size.zcash_serialize(&mut data)
            .expect("CompactSize should serialize");
        data.resize(data.len() + SOLUTION_SIZE, 0);

        let result = Solution::zcash_deserialize(data.as_slice());
        if size == SOLUTION_SIZE.try_into().unwrap() {
            result.expect("Correct size field in EquihashSolution should deserialize");
        } else {
            result.expect_err("Wrong size field in EquihashSolution should fail on deserialize");
        }
    }
}
