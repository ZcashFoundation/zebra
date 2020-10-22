use primitive_types::U256;
use proptest::{arbitrary::any, prelude::*, test_runner::Config};

use std::env;

use crate::block::{self, Block};
use crate::serialization::{ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize};

use super::super::{difficulty::*, *};

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

    for block_bytes in zebra_test::vectors::BLOCKS.iter() {
        let block = Block::zcash_deserialize(&block_bytes[..])
            .expect("block test vector should deserialize");
        block.header.solution.check(&block.header)?;

        // The equihash solution test can be really slow, so we use fewer cases by
        // default. Set the PROPTEST_CASES env var to override this default.
        proptest!(Config::with_cases(env::var("PROPTEST_CASES")
                                      .ok()
                                      .and_then(|v| v.parse().ok())
                                      .unwrap_or(64)),
                |(fake_header in randomized_solutions(block.header))| {
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

    for block_bytes in zebra_test::vectors::BLOCKS.iter() {
        let block = Block::zcash_deserialize(&block_bytes[..])
            .expect("block test vector should deserialize");
        block.header.solution.check(&block.header)?;

        proptest!(|(fake_header in randomized_nonce(block.header))| {
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

    for block_bytes in zebra_test::vectors::BLOCKS.iter() {
        let block = Block::zcash_deserialize(&block_bytes[..])
            .expect("block test vector should deserialize");
        block.header.solution.check(&block.header)?;

        proptest!(Config::with_cases(env::var("PROPTEST_CASES")
                                  .ok()
                                  .and_then(|v| v.parse().ok())
                                 .unwrap_or(64)),
              |(fake_header in randomized_input(block.header))| {
            fake_header.solution
                .check(&fake_header)
                .expect_err("equihash solution should not validate on randomized input");
        });
    }

    Ok(())
}

proptest! {
    /// Check that CompactDifficulty expands, and converts to work.
    ///
    /// Make sure the conversions don't panic, and that they compare correctly.
   #[test]
   fn prop_compact_expand_work(compact in any::<CompactDifficulty>()) {
       // TODO: use random ExpandedDifficulties, once we have ExpandedDifficulty::to_compact()
       //
       // This change will increase the number of valid random work values.
       let expanded = compact.to_expanded();
       let work = compact.to_work();

       let hash_zero = block::Hash([0; 32]);
       let hash_max = block::Hash([0xff; 32]);

       let work_zero = Work(0);
       let work_max = Work(u128::MAX);

       if let Some(expanded) = expanded {
           prop_assert!(expanded >= hash_zero);
           prop_assert!(expanded <= hash_max);
       }

       if let Some(work) = work {
           prop_assert!(work > work_zero);
           prop_assert!(work < work_max);
       }
   }

   /// Check that a random ExpandedDifficulty compares correctly with fixed block::Hash
   #[test]
   fn prop_expanded_order(expanded in any::<ExpandedDifficulty>()) {
       // TODO: round-trip test, once we have ExpandedDifficulty::to_compact()
       let hash_zero = block::Hash([0; 32]);
       let hash_max = block::Hash([0xff; 32]);

       prop_assert!(expanded >= hash_zero);
       prop_assert!(expanded <= hash_max);
   }

   /// Check that ExpandedDifficulty compares correctly with a random block::Hash.
   #[test]
   fn prop_hash_order(hash in any::<block::Hash>()) {
       let ex_zero = ExpandedDifficulty(U256::zero());
       let ex_one = ExpandedDifficulty(U256::one());
       let ex_max = ExpandedDifficulty(U256::MAX);

       prop_assert!(hash >= ex_zero);
       prop_assert!(hash <= ex_max);
       prop_assert!(hash >= ex_one || hash == ex_zero);
   }

   /// Check that a random ExpandedDifficulty and block::Hash compare correctly.
   #[test]
   #[allow(clippy::double_comparisons)]
   fn prop_expanded_hash_order(expanded in any::<ExpandedDifficulty>(), hash in any::<block::Hash>()) {
       prop_assert!(expanded < hash || expanded > hash || expanded == hash);
   }

    /// Check that the work values for two random ExpandedDifficulties add
    /// correctly.
   #[test]
    fn prop_work(compact1 in any::<CompactDifficulty>(), compact2 in any::<CompactDifficulty>()) {
        // TODO: use random ExpandedDifficulties, once we have ExpandedDifficulty::to_compact()
        //
        // This change will increase the number of valid random work values.
        let work1 = compact1.to_work();
        let work2 = compact2.to_work();

        if let (Some(work1), Some(work2)) = (work1, work2) {
            let work_max = PartialCumulativeWork::from(Work(u128::MAX));

            // If the sum won't panic
            if work_max - work1 <= PartialCumulativeWork::from(work2) {
                let work_total = work1 + work2;
                prop_assert!(work_total >= PartialCumulativeWork::from(work1));
                prop_assert!(work_total >= PartialCumulativeWork::from(work2));
            }


    /// Check that the work values for two random ExpandedDifficulties add
    /// correctly.
    #[test]
    fn prop_work(work1 in any::<Work>(), work2 in any::<Work>()) {
        // If the sum won't panic
        if work1.0.checked_add(work2.0).is_some() {
            let work_total = work1 + work2;
            prop_assert!(work_total >= PartialCumulativeWork::from(work1));
            prop_assert!(work_total >= PartialCumulativeWork::from(work2));
        }

        if work1.0.checked_add(work1.0).is_some() {
            let work_total = work1 + work1;
            prop_assert!(work_total >= PartialCumulativeWork::from(work1));
        }

        if work2.0.checked_add(work2.0).is_some() {
            let work_total = work2 + work2;
            prop_assert!(work_total >= PartialCumulativeWork::from(work2));
        }
    }
}
