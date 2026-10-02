//! Fixed response scenarios for synchronization feedback.

use zebra_chain::block::Hash;
use zebra_network::{self as zn, FindResponseFeedback};
use zebra_state as zs;

use super::{TestScenario, FANOUT};

/// An empty obtain-tips response receives stall feedback without queuing downloads.
#[tokio::test]
async fn empty_obtain_response_reports_stall() {
    let _test_guard = zebra_test::init();

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
                hashes: vec![],
                feedback: Some(feedback),
            });

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
}

/// An extend-tips response containing the expected overlap but no continuation reports a stall.
#[tokio::test]
async fn extend_response_without_continuation_reports_stall() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();
    let expected_overlap_hash = Hash([1; 32]);

    let mut observer = test
        .raw_hashes_for_extend_tips(vec![expected_overlap_hash])
        .await;

    assert_eq!(observer.try_outcome(), Ok(Some(false)));
}
