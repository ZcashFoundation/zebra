use super::super::*;

use crate::{
    block::Block,
    serialization::{ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
};

/// Includes the 32-byte nonce.
const EQUIHASH_SOLUTION_BLOCK_OFFSET: usize = equihash::Solution::INPUT_LENGTH + 32;

/// Includes the 3-byte equihash length field.
const BLOCK_HEADER_LENGTH: usize = EQUIHASH_SOLUTION_BLOCK_OFFSET + 3 + equihash::SOLUTION_SIZE;

#[test]
fn equihash_solution_test_vectors() {
    zebra_test::init();

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
    zebra_test::init();

    for block in zebra_test::vectors::BLOCKS.iter() {
        let block =
            Block::zcash_deserialize(&block[..]).expect("block test vector should deserialize");

        block.header.solution.check(&block.header)?;
    }

    Ok(())
}
