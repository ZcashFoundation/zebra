//! Focused tests for feedback attribution across synchronization stages.

use std::collections::HashSet;

use indexmap::IndexSet;
use tokio::sync::mpsc;
use zebra_chain::{
    block::{Hash, Height},
    chain_tip::mock::MockChainTip,
};
use zebra_network::{self as zn, FindResponseFeedback, FindResponseFeedbackObserver};
use zebra_state as zs;
use zebra_test::mock_service::{MockService, PanicAssertion};

use super::super::{ChainSync, CheckedTip, FANOUT};
use crate::config::ZebradConfig;

/// An obtain-tips response containing an unknown hash receives useful feedback.
#[tokio::test]
async fn obtain_response_with_unknown_hash_reports_useful_feedback() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();

    let observer = test.hashes_for_obtain_tips(vec![Hash([2; 32])]).await;

    assert_eq!(observer.try_outcome(), Ok(Some(true)));
}

/// An empty obtain-tips response receives stall feedback without queuing downloads.
#[tokio::test]
async fn empty_obtain_response_reports_stall() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();
    let known_hash = Hash([0; 32]);
    let (feedback, observer) = FindResponseFeedback::new_for_test();

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

/// An obtain-tips response with only already-known hashes receives stall feedback.
#[tokio::test]
async fn obtain_response_with_only_known_hashes_reports_stall() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();
    let known_hash = Hash([0; 32]);
    let (feedback, observer) = FindResponseFeedback::new_for_test();

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
                hashes: vec![known_hash],
                feedback: Some(feedback),
            });

        test.state
            .expect_request(zs::Request::KnownBlock(known_hash))
            .await
            .respond(zs::Response::KnownBlock(Some(zs::KnownBlock::BestChain)));

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

/// An extend-tips response with the expected overlap and a continuation receives useful feedback.
#[tokio::test]
async fn extend_response_with_continuation_reports_useful_feedback() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();

    let observer = test.hashes_for_extend_tips(vec![Hash([2; 32])]).await;

    assert_eq!(observer.try_outcome(), Ok(Some(true)));
}

/// An empty extend-tips response receives stall feedback.
#[tokio::test]
async fn empty_extend_response_reports_stall() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();

    let observer = test.raw_hashes_for_extend_tips(vec![]).await;

    assert_eq!(observer.try_outcome(), Ok(Some(false)));
}

/// An extend-tips response whose only hash differs from the expected overlap reports a stall.
#[tokio::test]
async fn extend_response_with_one_unexpected_hash_reports_stall() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();
    let unexpected_hash = Hash([4; 32]);

    let observer = test.raw_hashes_for_extend_tips(vec![unexpected_hash]).await;

    assert_eq!(observer.try_outcome(), Ok(Some(false)));
}

/// An extend-tips response reports a stall when neither of its first two hashes matches the overlap.
///
/// Both positions matter because a matching second hash permits an unrelated first hash.
#[tokio::test]
async fn extend_response_with_two_unexpected_hashes_reports_stall() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();
    let first_unexpected_hash = Hash([4; 32]);
    let second_unexpected_hash = Hash([5; 32]);

    let observer = test
        .raw_hashes_for_extend_tips(vec![first_unexpected_hash, second_unexpected_hash])
        .await;

    assert_eq!(observer.try_outcome(), Ok(Some(false)));
}

/// An extend-tips response containing the expected overlap but no continuation reports a stall.
#[tokio::test]
async fn extend_response_without_continuation_reports_stall() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();
    let expected_overlap_hash = Hash([1; 32]);

    let observer = test
        .raw_hashes_for_extend_tips(vec![expected_overlap_hash])
        .await;

    assert_eq!(observer.try_outcome(), Ok(Some(false)));
}

/// A mock service with strict request assertions.
type Mock<Req, Resp> = MockService<Req, Resp, PanicAssertion>;

/// A syncer whose network, state and verifier are controlled by the test.
type TestSync = ChainSync<
    Mock<zn::Request, zn::Response>,
    Mock<zs::Request, zs::Response>,
    Mock<zs::ReadRequest, zs::ReadResponse>,
    Mock<zebra_consensus::Request, Hash>,
    MockChainTip,
>;

/// Services used to exercise one sync stage without spawning the entire sync loop.
struct TestScenario {
    sync: TestSync,
    peers: Mock<zn::Request, zn::Response>,
    state: Mock<zs::Request, zs::Response>,
}

impl TestScenario {
    /// Creates a syncer with an empty mocked state.
    fn new() -> Self {
        let peers = MockService::build().for_unit_tests();
        let state = MockService::build().for_unit_tests();
        let read_state = MockService::build().for_unit_tests();
        let verifier = MockService::build().for_unit_tests();

        let (tip, _tip_sender) = MockChainTip::new();
        let (misbehavior, _receiver) = mpsc::channel(1);

        let (sync, _) = ChainSync::new(
            &ZebradConfig::default(),
            Height(0),
            peers.clone(),
            verifier,
            state.clone(),
            read_state,
            tip,
            misbehavior,
        );

        Self { sync, peers, state }
    }

    /// Queues an obtain response whose hashes are absent from local state.
    async fn hashes_for_obtain_tips(&mut self, hashes: Vec<Hash>) -> FindResponseFeedbackObserver {
        let (feedback, observer) = FindResponseFeedback::new_for_test();

        let mock_responses = async {
            self.state
                .expect_request(zs::Request::BlockLocator)
                .await
                .respond(zs::Response::BlockLocator(vec![Hash([0; 32])]));

            self.peers
                .expect_request(zn::Request::FindBlocks {
                    known_blocks: vec![Hash([0; 32])],
                    stop: None,
                })
                .await
                .respond(zn::Response::BlockHashes {
                    hashes: hashes.clone(),
                    feedback: Some(feedback),
                });

            self.state
                .expect_request(zs::Request::KnownBlock(hashes[0]))
                .await
                .respond(zs::Response::KnownBlock(None));

            for _ in 1..FANOUT {
                self.peers
                    .expect_request(zn::Request::FindBlocks {
                        known_blocks: vec![Hash([0; 32])],
                        stop: None,
                    })
                    .await
                    .respond(Err(zn::BoxError::from("unused fanout response")));
            }

            for hash in hashes.into_iter().collect::<IndexSet<_>>() {
                self.state
                    .expect_request(zs::Request::KnownBlock(hash))
                    .await
                    .respond(zs::Response::KnownBlock(None));
            }
        };

        let (result, ()) = tokio::join!(self.sync.obtain_tips(), mock_responses);
        result.expect("mocked obtain stage queues its hashes");

        observer
    }

    /// Queues an extend response with the expected overlap and supplied continuation.
    async fn hashes_for_extend_tips(&mut self, hashes: Vec<Hash>) -> FindResponseFeedbackObserver {
        self.raw_hashes_for_extend_tips([vec![Hash([1; 32])], hashes].concat())
            .await
    }

    /// Supplies one extend response without assuming that it contains valid overlap.
    async fn raw_hashes_for_extend_tips(
        &mut self,
        hashes: Vec<Hash>,
    ) -> FindResponseFeedbackObserver {
        let (feedback, observer) = FindResponseFeedback::new_for_test();

        self.sync.prospective_tips = HashSet::from([CheckedTip {
            tip: Hash([0; 32]),
            expected_next: Hash([1; 32]),
        }]);

        let mock_responses = async {
            self.peers
                .expect_request(zn::Request::FindBlocks {
                    known_blocks: vec![Hash([0; 32])],
                    stop: None,
                })
                .await
                .respond(zn::Response::BlockHashes {
                    hashes,
                    feedback: Some(feedback),
                });

            for _ in 1..FANOUT {
                self.peers
                    .expect_request(zn::Request::FindBlocks {
                        known_blocks: vec![Hash([0; 32])],
                        stop: None,
                    })
                    .await
                    .respond(Err(zn::BoxError::from("unused fanout response")));
            }
        };

        let (result, ()) = tokio::join!(self.sync.extend_tips(), mock_responses);
        result.expect("mocked extend stage queues its hashes");

        observer
    }
}
