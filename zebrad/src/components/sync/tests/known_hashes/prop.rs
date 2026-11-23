//! Property tests for known hashes in discovery responses.

use std::collections::{HashMap, HashSet};

use proptest::{collection::btree_set, prelude::*};
use tokio::runtime::Builder;

use zebra_chain::block::Hash;
use zebra_state as zs;

use super::{assert_downloaded_hashes, known_hash_strategy::KnownHashStrategy, TestScenario};
use crate::components::sync::CheckedTip;

proptest! {
    /// Mixed discovery responses download exactly their unknown hashes without aborting.
    #[test]
    fn known_suffix_preserves_unknown_downloads(
        entries in hashes_with_known_flags(KnownHashStrategy::new(".*uK.*", 10)),
    ) {
        let _test_guard = zebra_test::init();

        let runtime = Builder::new_current_thread().enable_all().build().unwrap();

        runtime.block_on(async {
            let mut test = TestScenario::new();
            let hashes = Vec::from_iter(entries.iter().map(|(hash, _)| *hash));
            let remaining_hashes = HashSet::<Hash>::from_iter(
                entries.iter().filter_map(|(hash, is_known)| (!is_known).then_some(*hash)),
            );
            let is_known_by_hash = HashMap::<_, _>::from_iter(entries);

            let result = test
                .obtain_tips_with_hashes(hashes, |hash, _| {
                    is_known_by_hash[&hash].then_some(zs::KnownBlock::BestChain)
                })
                .await;

            prop_assert!(result.is_ok(), "a known suffix must not abort discovery: {result:?}");
            assert_downloaded_hashes(&mut test, remaining_hashes).await;

            Ok(())
        })?;
    }

    /// A later known-successor response preserves the earlier eligible pair for any prefix.
    #[test]
    fn known_successor_does_not_replace_earlier_tip(
        entries in hashes_with_known_flags(KnownHashStrategy::new(".*uuK", 10)),
    ) {
        let _test_guard = zebra_test::init();
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();

        runtime.block_on(async {
            let mut test = TestScenario::new();
            let hashes = Vec::from_iter(entries.iter().map(|(hash, _)| *hash));
            let eligible_hashes = hashes[..hashes.len() - 1].to_vec();
            let expected_tip = CheckedTip {
                tip: hashes[hashes.len() - 3],
                expected_next: hashes[hashes.len() - 2],
            };
            let unknown_hashes = Vec::from_iter(
                entries.iter().filter_map(|(hash, is_known)| (!is_known).then_some(*hash)),
            );
            let is_known_by_hash = HashMap::<_, _>::from_iter(entries);

            test.obtain_tips_with_responses(vec![eligible_hashes, hashes], |hash, _| {
                is_known_by_hash[&hash].then_some(zs::KnownBlock::BestChain)
            })
            .await
            .expect("an ineligible response must not abort discovery");

            prop_assert_eq!(
                &test.sync.prospective_tips,
                &HashSet::from_iter([expected_tip]),
                "the original eligible tip must survive the later ineligible response",
            );
            assert_downloaded_hashes(&mut test, unknown_hashes).await;

            Ok(())
        })?;
    }

    /// An earlier known-successor response preserves the later eligible pair for any prefix.
    #[test]
    fn known_successor_does_not_suppress_later_tip(
        entries in hashes_with_known_flags(KnownHashStrategy::new(".*uuK", 10)),
    ) {
        let _test_guard = zebra_test::init();
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();

        runtime.block_on(async {
            let mut test = TestScenario::new();
            let hashes = Vec::from_iter(entries.iter().map(|(hash, _)| *hash));
            let eligible_hashes = hashes[..hashes.len() - 1].to_vec();
            let expected_tip = CheckedTip {
                tip: hashes[hashes.len() - 3],
                expected_next: hashes[hashes.len() - 2],
            };
            let unknown_hashes = Vec::from_iter(
                entries.iter().filter_map(|(hash, is_known)| (!is_known).then_some(*hash)),
            );
            let is_known_by_hash = HashMap::<_, _>::from_iter(entries);

            test.obtain_tips_with_responses(vec![hashes, eligible_hashes], |hash, _| {
                is_known_by_hash[&hash].then_some(zs::KnownBlock::BestChain)
            })
            .await
            .expect("an ineligible response must not abort discovery");

            prop_assert_eq!(
                &test.sync.prospective_tips,
                &HashSet::from_iter([expected_tip]),
                "the later eligible tip must survive the earlier ineligible response",
            );
            assert_downloaded_hashes(&mut test, unknown_hashes).await;

            Ok(())
        })?;
    }

    /// Eligible overlapping responses keep the further tip for any length and either order.
    #[test]
    fn eligible_overlap_keeps_further_tip_in_both_orders(
        hashes in btree_set(any::<[u8; 32]>(), 3..=10)
            .prop_map(|hashes| Vec::from_iter(hashes.into_iter().map(Hash)))
            .prop_shuffle(),
        longer_response_first in any::<bool>(),
    ) {
        let _test_guard = zebra_test::init();
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();

        runtime.block_on(async {
            let expected_tip = CheckedTip {
                tip: hashes[hashes.len() - 2],
                expected_next: hashes[hashes.len() - 1],
            };

            let mut test = TestScenario::new();
            let mut responses = vec![hashes[..hashes.len() - 1].to_vec(), hashes.clone()];
            if longer_response_first {
                responses.reverse();
            }

            test.obtain_tips_with_responses(responses, |_, _| None)
                .await
                .expect("eligible overlapping responses must not abort discovery");

            prop_assert_eq!(
                &test.sync.prospective_tips,
                &HashSet::from_iter([expected_tip]),
                "eligible merging must preserve the further tip, longer response first: {}",
                longer_response_first,
            );
            assert_downloaded_hashes(&mut test, hashes.iter().copied()).await;

            Ok(())
        })?;
    }

    /// A later known-locator response preserves the earlier eligible pair for any prefix.
    #[test]
    fn known_locator_does_not_replace_earlier_tip(
        entries in hashes_with_known_flags(KnownHashStrategy::new(".*uuKu", 10)),
    ) {
        let _test_guard = zebra_test::init();
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();

        runtime.block_on(async {
            let mut test = TestScenario::new();
            let hashes = Vec::from_iter(entries.iter().map(|(hash, _)| *hash));
            let eligible_hashes = hashes[..hashes.len() - 2].to_vec();
            let expected_tip = CheckedTip {
                tip: hashes[hashes.len() - 4],
                expected_next: hashes[hashes.len() - 3],
            };
            let unknown_hashes = Vec::from_iter(
                entries.iter().filter_map(|(hash, is_known)| (!is_known).then_some(*hash)),
            );
            let is_known_by_hash = HashMap::<_, _>::from_iter(entries);

            test.obtain_tips_with_responses(vec![eligible_hashes, hashes], |hash, _| {
                is_known_by_hash[&hash].then_some(zs::KnownBlock::BestChain)
            })
            .await
            .expect("an ineligible response must not abort discovery");

            prop_assert_eq!(
                &test.sync.prospective_tips,
                &HashSet::from_iter([expected_tip]),
                "a known locator must not let a later response erase an eligible tip",
            );
            assert_downloaded_hashes(&mut test, unknown_hashes).await;

            Ok(())
        })?;
    }

    /// An earlier known-locator response preserves the later eligible pair for any prefix.
    #[test]
    fn known_locator_does_not_suppress_later_tip(
        entries in hashes_with_known_flags(KnownHashStrategy::new(".*uuKu", 10)),
    ) {
        let _test_guard = zebra_test::init();
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();

        runtime.block_on(async {
            let mut test = TestScenario::new();
            let hashes = Vec::from_iter(entries.iter().map(|(hash, _)| *hash));
            let eligible_hashes = hashes[..hashes.len() - 2].to_vec();
            let expected_tip = CheckedTip {
                tip: hashes[hashes.len() - 4],
                expected_next: hashes[hashes.len() - 3],
            };
            let unknown_hashes = Vec::from_iter(
                entries.iter().filter_map(|(hash, is_known)| (!is_known).then_some(*hash)),
            );
            let is_known_by_hash = HashMap::<_, _>::from_iter(entries);

            test.obtain_tips_with_responses(vec![hashes, eligible_hashes], |hash, _| {
                is_known_by_hash[&hash].then_some(zs::KnownBlock::BestChain)
            })
            .await
            .expect("an ineligible response must not abort discovery");

            prop_assert_eq!(
                &test.sync.prospective_tips,
                &HashSet::from_iter([expected_tip]),
                "a known locator must not let an earlier response suppress an eligible tip",
            );
            assert_downloaded_hashes(&mut test, unknown_hashes).await;

            Ok(())
        })?;
    }

    /// A known trailing successor is discarded regardless of the preceding hashes.
    #[test]
    fn known_successor_is_not_a_prospective_tip(
        entries in hashes_with_known_flags(KnownHashStrategy::new(".*uK", 10)),
    ) {
        let _test_guard = zebra_test::init();

        let runtime = Builder::new_current_thread().enable_all().build().unwrap();

        runtime.block_on(async {
            let mut test = TestScenario::new();
            let hashes = Vec::from_iter(entries.iter().map(|(hash, _)| *hash));
            let is_known_by_hash = HashMap::<_, _>::from_iter(entries);

            test.obtain_tips_with_hashes(hashes, |hash, _| {
                is_known_by_hash[&hash].then_some(zs::KnownBlock::BestChain)
            })
            .await
            .unwrap();

            prop_assert!(
                test.sync.prospective_tips.is_empty(),
                "a known successor must not guide extension",
            );

            Ok(())
        })?;
    }

    /// Removing a known trailing locator never joins separated unknown hashes into a tip.
    #[test]
    fn filtering_does_not_manufacture_tip_pairs(
        entries in hashes_with_known_flags(KnownHashStrategy::new(".*u.*Ku", 10)),
    ) {
        let _test_guard = zebra_test::init();

        let runtime = Builder::new_current_thread().enable_all().build().unwrap();

        runtime.block_on(async {
            let mut test = TestScenario::new();
            let hashes = Vec::from_iter(entries.iter().map(|(hash, _)| *hash));
            let unknown_count = entries.iter().filter(|(_, is_known)| !is_known).count();
            let is_known_by_hash = HashMap::<_, _>::from_iter(entries);

            test.obtain_tips_with_hashes(hashes, |hash, _| {
                is_known_by_hash[&hash].then_some(zs::KnownBlock::BestChain)
            })
            .await
            .unwrap();

            prop_assert_eq!(test.sync.downloads.in_flight(), unknown_count);
            prop_assert!(
                test.sync.prospective_tips.is_empty(),
                "neither the known locator nor a manufactured pair may guide extension",
            );

            Ok(())
        })?;
    }

    /// Filtering a mixed prefix preserves the exact original unknown trailing pair.
    #[test]
    fn filtering_preserves_unknown_trailing_pair(
        entries in hashes_with_known_flags(KnownHashStrategy::new(".*u.*K.*uu", 10)),
    ) {
        let _test_guard = zebra_test::init();

        let runtime = Builder::new_current_thread().enable_all().build().unwrap();

        runtime.block_on(async {
            let mut test = TestScenario::new();
            let hashes = Vec::from_iter(entries.iter().map(|(hash, _)| *hash));
            let tip = hashes[hashes.len() - 2];
            let expected_next = hashes[hashes.len() - 1];
            let unknown_count = entries.iter().filter(|(_, is_known)| !is_known).count();
            let is_known_by_hash = HashMap::<_, _>::from_iter(entries);

            test.obtain_tips_with_hashes(hashes, |hash, _| {
                is_known_by_hash[&hash].then_some(zs::KnownBlock::Finalized)
            })
            .await
            .unwrap();

            prop_assert_eq!(test.sync.downloads.in_flight(), unknown_count);
            prop_assert_eq!(
                test.sync.prospective_tips,
                HashSet::from_iter([CheckedTip { tip, expected_next }]),
            );

            Ok(())
        })?;
    }

    /// Progress on the first candidate preserves every unknown suffix download.
    ///
    /// An empty suffix also checks that filtering all candidates succeeds.
    #[test]
    fn progress_on_first_candidate_preserves_unknown_suffix(
        hashes in btree_set(any::<[u8; 32]>(), 1..=10)
            .prop_map(|hashes| Vec::from_iter(hashes.into_iter().map(Hash)))
            .prop_shuffle(),
    ) {
        let _test_guard = zebra_test::init();

        let runtime = Builder::new_current_thread().enable_all().build().unwrap();

        runtime.block_on(async {
            let mut test = TestScenario::new();
            let transitioning_hash = hashes[0];
            let remaining_hashes = HashSet::<Hash>::from_iter(hashes.iter().skip(1).copied());

            // The first candidate is queried by both the initial scan and filtering.
            test.obtain_tips_with_hashes(hashes, |hash, observation| {
                (hash == transitioning_hash && observation > 1)
                    .then_some(zs::KnownBlock::WriteChannel)
            })
            .await
            .expect("concurrent state progress must not abort discovery");

            if remaining_hashes.is_empty() {
                prop_assert!(test.sync.prospective_tips.is_empty());
            }

            assert_downloaded_hashes(&mut test, remaining_hashes).await;

            Ok(())
        })?;
    }

    /// Progress after an unknown prefix preserves all other candidate downloads.
    ///
    /// The initial scan stops in the prefix, so the transitioning candidate is
    /// first observed in state during filtering, when it is already known.
    #[test]
    fn progress_after_unknown_prefix_preserves_other_downloads(
        (prefix_length, hashes) in (1usize..=5, 0usize..=5)
            .prop_flat_map(|(prefix_length, suffix_length)| {
                let hash_count = prefix_length + 1 + suffix_length;
                let hashes = btree_set(any::<[u8; 32]>(), hash_count)
                    .prop_map(|hashes| Vec::from_iter(hashes.into_iter().map(Hash)))
                    .prop_shuffle();

                (Just(prefix_length), hashes)
            }),
    ) {
        let _test_guard = zebra_test::init();

        let runtime = Builder::new_current_thread().enable_all().build().unwrap();

        runtime.block_on(async {
            let mut test = TestScenario::new();
            let transitioning_hash = hashes[prefix_length];
            let mut remaining_hashes = HashSet::<Hash>::from_iter(hashes.iter().copied());
            remaining_hashes.remove(&transitioning_hash);

            // Unlike the first candidate, this hash has no initial-scan lookup.
            test.obtain_tips_with_hashes(hashes, |hash, _| {
                (hash == transitioning_hash).then_some(zs::KnownBlock::WriteChannel)
            })
            .await
            .expect("concurrent state progress must not abort discovery");

            assert_downloaded_hashes(&mut test, remaining_hashes).await;
        });
    }

    /// Progress after filtering the first candidate must not abort any downloads.
    ///
    /// The candidate is unknown during the initial scan and filtering. Only
    /// a redundant third lookup observes it committed and exposes the old abort.
    #[test]
    fn progress_after_filtering_first_candidate_preserves_all_downloads(
        hashes in btree_set(any::<[u8; 32]>(), 1..=10)
            .prop_map(|hashes| Vec::from_iter(hashes.into_iter().map(Hash)))
            .prop_shuffle(),
    ) {
        let _test_guard = zebra_test::init();

        let runtime = Builder::new_current_thread().enable_all().build().unwrap();

        runtime.block_on(async {
            let mut test = TestScenario::new();
            let transitioning_hash = hashes[0];
            let remaining_hashes = HashSet::<Hash>::from_iter(hashes.iter().copied());

            test.obtain_tips_with_hashes(hashes, |hash, observation| {
                (hash == transitioning_hash && observation > 2)
                    .then_some(zs::KnownBlock::BestChain)
            })
            .await
            .expect("progress after filtering must not abort discovery");

            assert_downloaded_hashes(&mut test, remaining_hashes).await;
        });
    }

    /// Progress after filtering a later candidate must not abort any downloads.
    ///
    /// The initial scan stops in the unknown prefix. The candidate's first
    /// lookup is filtering; a redundant second lookup exposes the old abort.
    #[test]
    fn progress_after_filtering_later_candidate_preserves_all_downloads(
        (prefix_length, hashes) in (1usize..=5, 0usize..=5)
            .prop_flat_map(|(prefix_length, suffix_length)| {
                let hash_count = prefix_length + 1 + suffix_length;
                let hashes = btree_set(any::<[u8; 32]>(), hash_count)
                    .prop_map(|hashes| Vec::from_iter(hashes.into_iter().map(Hash)))
                    .prop_shuffle();

                (Just(prefix_length), hashes)
            }),
    ) {
        let _test_guard = zebra_test::init();

        let runtime = Builder::new_current_thread().enable_all().build().unwrap();

        runtime.block_on(async {
            let mut test = TestScenario::new();
            let transitioning_hash = hashes[prefix_length];
            let remaining_hashes = HashSet::<Hash>::from_iter(hashes.iter().copied());

            test.obtain_tips_with_hashes(hashes, |hash, observation| {
                (hash == transitioning_hash && observation > 1)
                    .then_some(zs::KnownBlock::BestChain)
            })
            .await
            .expect("progress after filtering must not abort discovery");

            assert_downloaded_hashes(&mut test, remaining_hashes).await;
        });
    }

    /// State-queued responses retain every download and their original trailing pair.
    #[test]
    fn queued_hashes_remain_downloadable(
        hashes in btree_set(any::<[u8; 32]>(), 2..=10)
            .prop_map(|hashes| Vec::from_iter(hashes.into_iter().map(Hash)))
            .prop_shuffle(),
    ) {
        let _test_guard = zebra_test::init();

        let runtime = Builder::new_current_thread().enable_all().build().unwrap();

        runtime.block_on(async {
            let mut test = TestScenario::new();
            let hash_count = hashes.len();
            let tip = hashes[hash_count - 2];
            let expected_next = hashes[hash_count - 1];

            test.obtain_tips_with_hashes(hashes, |_, _| Some(zs::KnownBlock::Queue))
                .await
                .unwrap();

            prop_assert_eq!(test.sync.downloads.in_flight(), hash_count);
            prop_assert_eq!(
                test.sync.prospective_tips,
                HashSet::from_iter([CheckedTip { tip, expected_next }]),
            );

            Ok(())
        })?;
    }
}

/// Pairs unique, shuffled hashes with flags in the order produced by `flags`.
fn hashes_with_known_flags(
    flags: impl Strategy<Value = Vec<bool>>,
) -> impl Strategy<Value = Vec<(Hash, bool)>> {
    flags.prop_flat_map(|flags| {
        btree_set(any::<[u8; 32]>(), flags.len())
            .prop_map(|hashes| Vec::from_iter(hashes.into_iter().map(Hash)))
            .prop_shuffle()
            .prop_map(move |hashes| Vec::from_iter(hashes.into_iter().zip(flags.iter().copied())))
    })
}
