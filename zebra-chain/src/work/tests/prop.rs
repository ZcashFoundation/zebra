use proptest::prelude::*;

use crate::block::{self, Block};
use crate::serialization::{ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize};

use super::super::*;

#[test]
fn equihash_solution_roundtrip() {
    proptest!(|(solution in any::<equihash::Solution>())| {
            let data = solution
                .zcash_serialize_to_vec()
                .expect("randomized EquihashSolution should serialize");
            let solution2 = data
                .zcash_deserialize_into()
                .expect("randomized EquihashSolution should deserialize");

            prop_assert_eq![solution, solution2];
        });
}

prop_compose! {
    fn randomized_solutions(real_header: block::Header)
        (fake_solution in any::<equihash::Solution>()
            .prop_filter("solution must not be the actual solution", move |s| {
                s != &real_header.solution
            })
        ) -> block::Header {
        let mut fake_header = real_header;
        fake_header.solution = fake_solution;
        fake_header
    }
}

#[test]
fn equihash_prop_test_solution() -> color_eyre::eyre::Result<()> {
    zebra_test::init();

    for block_bytes in zebra_test::vectors::TEST_BLOCKS.iter() {
        let block = Block::zcash_deserialize(&block_bytes[..])
            .expect("block test vector should deserialize");
        block.header.is_equihash_solution_valid()?;

        proptest!(|(fake_header in randomized_solutions(block.header))| {
                fake_header
                    .is_equihash_solution_valid()
                    .expect_err("block header should not validate on randomized solution");
            });
    }

    Ok(())
}

prop_compose! {
    fn randomized_nonce(real_header: block::Header)
        (fake_nonce in proptest::array::uniform32(any::<u8>())
            .prop_filter("nonce must not be the actual nonce", move |fake_nonce| {
                fake_nonce != &real_header.nonce
            })
        ) -> block::Header {
            let mut fake_header = real_header;
            fake_header.nonce = fake_nonce;
            fake_header
    }
}

#[test]
fn equihash_prop_test_nonce() -> color_eyre::eyre::Result<()> {
    zebra_test::init();

    for block_bytes in zebra_test::vectors::TEST_BLOCKS.iter() {
        let block = Block::zcash_deserialize(&block_bytes[..])
            .expect("block test vector should deserialize");
        block.header.is_equihash_solution_valid()?;

        proptest!(|(fake_header in randomized_nonce(block.header))| {
                fake_header
                    .is_equihash_solution_valid()
                    .expect_err("block header should not validate on randomized nonce");
            });
    }

    Ok(())
}

prop_compose! {
    fn randomized_input(real_header: block::Header)
        (fake_header in any::<block::Header>()
            .prop_map(move |mut fake_header| {
                fake_header.nonce = real_header.nonce;
                fake_header.solution = real_header.solution;
                fake_header
            })
            .prop_filter("input must not be the actual input", move |fake_header| {
                fake_header != &real_header
            })
        ) -> block::Header {
            fake_header
    }
}

#[test]
fn equihash_prop_test_input() -> color_eyre::eyre::Result<()> {
    zebra_test::init();

    for block_bytes in zebra_test::vectors::TEST_BLOCKS.iter() {
        let block = Block::zcash_deserialize(&block_bytes[..])
            .expect("block test vector should deserialize");
        block.header.is_equihash_solution_valid()?;

        proptest!(|(fake_header in randomized_input(block.header))| {
                fake_header
                    .is_equihash_solution_valid()
                    .expect_err("equihash solution should not validate on randomized input");
            });
    }

    Ok(())
}
