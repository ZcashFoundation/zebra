//! Randomised property tests for Proof of Work.

use std::{env, sync::Arc};

use proptest::{prelude::*, test_runner::Config};

use crate::{
    block::{self, Block},
    serialization::{ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
};

use super::super::*;

const DEFAULT_TEST_INPUT_PROPTEST_CASES: u32 = 64;

#[test]
fn equihash_solution_roundtrip() {
    let _init_guard = zebra_test::init();

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
        ) -> Arc<block::Header> {

        let mut fake_header = real_header;
        fake_header.solution = fake_solution;

        Arc::new(fake_header)
    }
}

#[test]
fn equihash_prop_test_solution() -> color_eyre::eyre::Result<()> {
    let _init_guard = zebra_test::init();

    for block_bytes in zebra_test::vectors::BLOCKS.iter() {
        let block = Block::zcash_deserialize(&block_bytes[..])
            .expect("block test vector should deserialize");
        block.header.solution.check(&block.header)?;

        // The equihash solution test can be really slow, so we use fewer cases by
        // default. Set the PROPTEST_CASES env var to override this default.
        proptest!(Config::with_cases(env::var("PROPTEST_CASES")
                                      .ok()
                                      .and_then(|v| v.parse().ok())
                                      .unwrap_or(DEFAULT_TEST_INPUT_PROPTEST_CASES)),
                |(fake_header in randomized_solutions(*block.header.as_ref()))| {
            fake_header.solution
                .check(&fake_header)
                .expect_err("block header should not validate on randomized solution");
        });
    }

    Ok(())
}

prop_compose! {
    fn randomized_nonce(real_header: block::Header)
        (fake_nonce in proptest::array::uniform32(any::<u8>())
            .prop_filter("nonce must not be the actual nonce", move |fake_nonce| {
                fake_nonce != &real_header.nonce.0
            })
        ) -> Arc<block::Header> {

        let mut fake_header = real_header;
        fake_header.nonce = fake_nonce.into();

        Arc::new(fake_header)
    }
}

#[test]
fn equihash_prop_test_nonce() -> color_eyre::eyre::Result<()> {
    let _init_guard = zebra_test::init();

    for block_bytes in zebra_test::vectors::BLOCKS.iter() {
        let block = Block::zcash_deserialize(&block_bytes[..])
            .expect("block test vector should deserialize");
        block.header.solution.check(&block.header)?;

        proptest!(|(fake_header in randomized_nonce(*block.header.as_ref()))| {
            fake_header.solution
                .check(&fake_header)
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
                Arc::new(fake_header)
            })
            .prop_filter("input must not be the actual input", move |fake_header| {
                fake_header.as_ref() != &real_header
            })
        ) -> Arc<block::Header> {

        fake_header
    }
}

#[test]
fn equihash_prop_test_input() -> color_eyre::eyre::Result<()> {
    let _init_guard = zebra_test::init();

    for block_bytes in zebra_test::vectors::BLOCKS.iter() {
        let block = Block::zcash_deserialize(&block_bytes[..])
            .expect("block test vector should deserialize");
        block.header.solution.check(&block.header)?;

        proptest!(Config::with_cases(env::var("PROPTEST_CASES")
                                  .ok()
                                  .and_then(|v| v.parse().ok())
                                 .unwrap_or(DEFAULT_TEST_INPUT_PROPTEST_CASES)),
              |(fake_header in randomized_input(*block.header.as_ref()))| {
            fake_header.solution
                .check(&fake_header)
                .expect_err("equihash solution should not validate on randomized input");
        });
    }

    Ok(())
}
