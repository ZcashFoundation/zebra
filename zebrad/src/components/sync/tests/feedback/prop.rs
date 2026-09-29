//! Property tests for synchronization response feedback.

use proptest::{
    collection::{hash_set, vec},
    prelude::*,
};
use zebra_chain::block::Hash;
use zebra_network::{self as zn, FindResponseFeedback};
use zebra_state as zs;

use super::{TestScenario, FANOUT};

proptest! {
    /// An extend response without matching overlap receives stalled feedback.
    #[test]
    fn extend_response_without_matching_overlap_reports_stall(
        hashes in vec(
            any::<[u8; 32]>().prop_filter(
                "response hashes differ from the expected overlap",
                |hash| *hash != [1; 32],
            ),
            0..=0,
        ),
    ) {
        let (runtime, _test_guard) = zebra_test::init_async();

        runtime.block_on(async {
            let mut test = TestScenario::new();
            let hashes = hashes.into_iter().map(Hash).collect();

            let mut observer = test.raw_hashes_for_extend_tips(hashes).await;

            assert_eq!(observer.try_outcome(), Ok(Some(false)));
        });
    }

    /// A matching overlap followed by a non-empty continuation receives useful feedback.
    #[test]
    fn extend_response_with_continuation_reports_useful_feedback(
        hashes in hash_set(
            any::<[u8; 32]>().prop_filter(
                "continuation hashes differ from the locator and overlap",
                |hash| *hash != [0; 32] && *hash != [1; 32],
            ),
            1..=10,
        ),
    ) {
        let (runtime, _test_guard) = zebra_test::init_async();

        runtime.block_on(async {
            let mut test = TestScenario::new();
            let hashes = hashes.into_iter().map(Hash).collect();

            let mut observer = test.hashes_for_extend_tips(hashes).await;

            assert_eq!(observer.try_outcome(), Ok(Some(true)));
        });
    }

    /// A non-empty response containing only unknown hashes receives useful feedback.
    #[test]
    fn obtain_response_with_unknown_hash_reports_useful_feedback(
        hashes in hash_set(any::<[u8; 32]>(), 1..=10),
    ) {
        let (runtime, _test_guard) = zebra_test::init_async();

        runtime.block_on(async {
            let mut test = TestScenario::new();
            let hashes = hashes.into_iter().map(Hash).collect();

            let mut observer = test.hashes_for_obtain_tips(hashes).await;

            assert_eq!(observer.try_outcome(), Ok(Some(true)));
        });
    }

    /// A non-empty response containing only known hashes receives stall feedback.
    #[test]
    fn obtain_response_with_only_known_hashes_reports_stall(
        hashes in vec(any::<[u8; 32]>(), 1..=10),
    ) {
        let (runtime, _test_guard) = zebra_test::init_async();

        runtime.block_on(async {
            let hashes: Vec<_> = hashes.into_iter().map(Hash).collect();
            let mut test = TestScenario::new();
            let known_hash = Hash([0; 32]);
            let (feedback, mut observer) = FindResponseFeedback::new_for_test();

            let mock_responses = async {
                test.state
                    .expect_request(zs::Request::BlockLocator)
                    .await
                    .respond(zs::Response::BlockLocator(vec![known_hash]));

                test.peers
                    .expect_request(zn::Request::FindBlocks {
                        known_blocks: vec![known_hash],
                        stop: None,
                    })
                    .await
                    .respond(zn::Response::BlockHashes {
                        hashes: hashes.clone(),
                        feedback: Some(feedback),
                    });

                for hash in hashes {
                    test.state
                        .expect_request(zs::Request::KnownBlock(hash))
                        .await
                        .respond(zs::Response::KnownBlock(Some(zs::KnownBlock::BestChain)));
                }

                for _ in 1..FANOUT {
                    test.peers
                        .expect_request(zn::Request::FindBlocks {
                            known_blocks: vec![known_hash],
                            stop: None,
                        })
                        .await
                        .respond(Err(zn::BoxError::from("unused fanout response")));
                }
            };

            let (result, ()) = tokio::join!(test.sync.obtain_tips(), mock_responses);

            assert!(result.unwrap().is_empty());
            assert_eq!(observer.try_outcome(), Ok(Some(false)));
        });
    }
}
