use super::super::*;

use crate::{
    block::Block,
    serialization::{ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
};

const EQUIHASH_SOLUTION_BLOCK_OFFSET: usize = equihash::Solution::INPUT_LENGTH + 32;

#[test]
fn equihash_solution_test_vector() {
    zebra_test::init();
    let solution_bytes =
        &zebra_test::vectors::HEADER_MAINNET_415000_BYTES[EQUIHASH_SOLUTION_BLOCK_OFFSET..];

    let solution = solution_bytes
        .zcash_deserialize_into::<equihash::Solution>()
        .expect("Test vector EquihashSolution should deserialize");

    let mut data = Vec::new();
    solution
        .zcash_serialize(&mut data)
        .expect("Test vector EquihashSolution should serialize");

    assert_eq!(solution_bytes, data.as_slice());
}

#[test]
fn equihash_solution_test_vector_is_valid() -> color_eyre::eyre::Result<()> {
    zebra_test::init();

    let block = Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])
        .expect("block test vector should deserialize");
    block.header.solution.check(&block.header)?;

    Ok(())
}
