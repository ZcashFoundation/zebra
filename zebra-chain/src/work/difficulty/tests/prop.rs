use proptest::prelude::*;

use super::super::*;

proptest! {
    /// Check Expanded, Compact, and Work conversions.
    ///
    /// Make sure the conversions don't panic, and that they round-trip and compare
    /// correctly.
    #[test]
    fn prop_difficulty_conversion(expanded_seed in any::<block::Hash>()) {
        let expanded_seed = ExpandedDifficulty::from_hash(&expanded_seed);

        let hash_zero = block::Hash([0; 32]);
        let hash_max = block::Hash([0xff; 32]);
        prop_assert!(expanded_seed >= hash_zero);
        prop_assert!(expanded_seed <= hash_max);

        // Skip invalid seeds
        prop_assume!(expanded_seed != hash_zero);

        let compact = expanded_seed.to_compact();
        let expanded_trunc = compact.to_expanded();
        let work = compact.to_work();

        if let Some(expanded_trunc) = expanded_trunc {
            // zero compact values are invalid, and return None on conversion
            prop_assert!(expanded_trunc > hash_zero);
            // the maximum compact value has less precision than a hash
            prop_assert!(expanded_trunc < hash_max);

            // the truncated value should be less than or equal to the seed
            prop_assert!(expanded_trunc <= expanded_seed);

            // roundtrip
            let compact_trip = expanded_trunc.to_compact();
            prop_assert_eq!(compact, compact_trip);

            let expanded_trip = compact_trip.to_expanded().expect("roundtrip expanded is valid");
            prop_assert_eq!(expanded_trunc, expanded_trip);

            // Some impossibly hard compact values are not valid work values in Zebra
            if let Some(work) = work {
                // roundtrip
                let work_trip = compact_trip.to_work().expect("roundtrip work is valid if work is valid");
                prop_assert_eq!(work, work_trip);
            }
        }
    }

    /// Check Work and PartialCumulativeWork conversions.
    ///
    /// Make sure the conversions don't panic, and that they compare correctly.
    #[test]
    fn prop_work_conversion(work in any::<Work>()) {
        let work_zero = Work(0);
        let work_max = Work(u128::MAX);

        prop_assert!(work > work_zero);
        // similarly, the maximum compact value has less precision than work
        prop_assert!(work < work_max);

        let partial_work = PartialCumulativeWork::from(work);
        prop_assert!(partial_work > PartialCumulativeWork::from(work_zero));
        prop_assert!(partial_work < PartialCumulativeWork::from(work_max));

        // Now try adding zero to convert to PartialCumulativeWork
        prop_assert!(partial_work > work_zero + work_zero);
        prop_assert!(partial_work < work_max + work_zero);
    }

    /// Check that a random ExpandedDifficulty compares correctly with fixed block::Hash
    #[test]
    fn prop_expanded_cmp(expanded in any::<ExpandedDifficulty>()) {
        let hash_zero = block::Hash([0; 32]);
        let hash_max = block::Hash([0xff; 32]);

        // zero compact values are invalid, and return None on conversion
        prop_assert!(expanded > hash_zero);
        // the maximum compact value has less precision than a hash
        prop_assert!(expanded < hash_max);
    }

    /// Check that ExpandedDifficulty compares correctly with a random block::Hash.
    #[test]
    fn prop_hash_cmp(hash in any::<block::Hash>()) {
        let ex_zero = ExpandedDifficulty(U256::zero());
        let ex_one = ExpandedDifficulty(U256::one());
        let ex_max = ExpandedDifficulty(U256::MAX);

        prop_assert!(hash >= ex_zero);
        prop_assert!(hash <= ex_max);
        prop_assert!(hash >= ex_one || hash == ex_zero);
    }

    /// Check that a random ExpandedDifficulty and block::Hash compare without panicking.
    #[test]
    #[allow(clippy::double_comparisons)]
    fn prop_expanded_hash_cmp(expanded in any::<ExpandedDifficulty>(), hash in any::<block::Hash>()) {
        prop_assert!(expanded < hash || expanded > hash || expanded == hash);
    }

    /// Check that two random CompactDifficulty values compare and round-trip correctly.
    #[test]
    #[allow(clippy::double_comparisons)]
    fn prop_compact_roundtrip(compact1 in any::<CompactDifficulty>(), compact2 in any::<CompactDifficulty>()) {
        prop_assert!(compact1 != compact2 || compact1 == compact2);

        let expanded1 = compact1.to_expanded().expect("arbitrary compact values are valid");
        let expanded2 = compact2.to_expanded().expect("arbitrary compact values are valid");

        if expanded1 != expanded2 {
            prop_assert!(compact1 != compact2);
        } else {
            prop_assert!(compact1 == compact2);
        }

        let compact1_trip = expanded1.to_compact();
        let compact2_trip = expanded2.to_compact();

        if compact1_trip != compact2_trip {
            prop_assert!(compact1 != compact2);
        } else {
            prop_assert!(compact1 == compact2);
        }
    }

    /// Check that two random ExpandedDifficulty values compare and round-trip correctly.
    #[test]
    fn prop_expanded_roundtrip(expanded1_seed in any::<block::Hash>(), expanded2_seed in any::<block::Hash>()) {
        let expanded1_seed = ExpandedDifficulty::from_hash(&expanded1_seed);
        let expanded2_seed = ExpandedDifficulty::from_hash(&expanded2_seed);

        // Skip invalid seeds
        let expanded_zero = ExpandedDifficulty(U256::zero());
        prop_assume!(expanded1_seed != expanded_zero);
        prop_assume!(expanded2_seed != expanded_zero);

        let compact1 = expanded1_seed.to_compact();
        let compact2 = expanded2_seed.to_compact();
        let expanded1_trunc = compact1.to_expanded();
        let expanded2_trunc = compact2.to_expanded();
        let work1 = compact1.to_work();
        let work2 = compact2.to_work();

        match expanded1_seed.cmp(&expanded2_seed) {
            Ordering::Greater => {
                // seed to compact truncation can turn expanded and work inequalities into equalities
                prop_assert!(expanded1_trunc >= expanded2_trunc);
            }
            Ordering::Equal => {
                prop_assert!(compact1 == compact2);
                prop_assert!(expanded1_trunc == expanded2_trunc);
            }
            Ordering::Less => {
                prop_assert!(expanded1_trunc <= expanded2_trunc);
            }
        }

        if expanded1_trunc == expanded2_trunc {
            prop_assert!(compact1 == compact2);
        }

        // Skip impossibly hard work values
        prop_assume!(work1.is_some());
        prop_assume!(work2.is_some());

        match expanded1_seed.cmp(&expanded2_seed) {
            Ordering::Greater => {
                // work comparisons are reversed, because conversion involves division
                prop_assert!(work1 <= work2);
            }
            Ordering::Equal => {
                prop_assert!(work1 == work2);
            }
            Ordering::Less => {
                prop_assert!(work1 >= work2);
            }
        }

        match expanded1_trunc.cmp(&expanded2_trunc) {
            Ordering::Greater => {
                // expanded to work truncation can turn work inequalities into equalities
                prop_assert!(work1 <= work2);
            }
            Ordering::Equal => {
                prop_assert!(work1 == work2);
            }
            Ordering::Less => {
                prop_assert!(work1 >= work2);
            }
        }
    }

    /// Check that two random Work values compare, add, and subtract correctly.
    #[test]
    fn prop_work_sum(work1 in any::<Work>(), work2 in any::<Work>()) {

        // If the sum won't panic
        if work1.0.checked_add(work2.0).is_some() {
            let work_total = work1 + work2;
            prop_assert!(work_total >= PartialCumulativeWork::from(work1));
            prop_assert!(work_total >= PartialCumulativeWork::from(work2));

            prop_assert_eq!(work_total - work1, PartialCumulativeWork::from(work2));
            prop_assert_eq!(work_total - work2, PartialCumulativeWork::from(work1));
        }

        if work1.0.checked_add(work1.0).is_some() {
            let work_total = work1 + work1;
            prop_assert!(work_total >= PartialCumulativeWork::from(work1));

            prop_assert_eq!(work_total - work1, PartialCumulativeWork::from(work1));
        }

        if work2.0.checked_add(work2.0).is_some() {
            let work_total = work2 + work2;
            prop_assert!(work_total >= PartialCumulativeWork::from(work2));

            prop_assert_eq!(work_total - work2, PartialCumulativeWork::from(work2));
        }
    }
}
