//! Focused tests for feedback attribution across synchronization stages.

use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::{future::Ready, StreamExt};
use indexmap::IndexSet;
use tokio::sync::mpsc::{self, error::TryRecvError};
use tokio::time::error::Elapsed;
use tower::Service;
use zebra_chain::{
    block::{Block, Hash, Height},
    chain_tip::mock::MockChainTip,
    serialization::ZcashDeserializeInto,
};
use zebra_network::{self as zn, FindResponseFeedback, FindResponseFeedbackObserver};
use zebra_state as zs;
use zebra_test::mock_service::{MockService, PanicAssertion};

use super::super::{
    downloads::BlockDownloadVerifyError, ChainSync, CheckedTip, BLOCK_DOWNLOAD_RETRY_LIMIT, FANOUT,
    MAX_BLOCK_REOBTAIN_RETRIES,
};
use crate::config::ZebradConfig;

mod retries;

/// Queuing an obtain tips candidate must not credit an unverified block hash.
#[tokio::test]
async fn obtain_tips_feedback_waits_for_verification() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();

    let observer = test.hashes_for_obtain_tips(vec![Hash([2; 32])]).await;

    assert_eq!(observer.try_outcome(), Err(TryRecvError::Empty));
}

/// A committed hash credits the obtain response that advertised it.
#[tokio::test]
async fn verified_obtain_hash_credits_response() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();
    let hash = Hash([2; 32]);

    let observer = test.hashes_for_obtain_tips(vec![hash]).await;

    test.sync
        .handle_block_response(Ok((Height(1), hash)))
        .unwrap();

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

/// Queuing an extend continuation must not credit an unverified block hash.
#[tokio::test]
async fn extend_feedback_waits_for_verification() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();

    let observer = test.hashes_for_extend_tips(vec![Hash([2; 32])]).await;

    assert_eq!(observer.try_outcome(), Err(TryRecvError::Empty));
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

/// A downloader error preserves the requested hash for response attribution.
#[tokio::test]
async fn failed_download_preserves_hash() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let hash = block.hash();

    test.sync.downloads.download_and_verify(hash).await.unwrap();

    test.peers
        .expect_request(zn::Request::BlocksByHash([hash].into_iter().collect()))
        .await
        .respond(zn::Response::Blocks(vec![
            zn::InventoryResponse::Available((block.clone(), None)),
        ]));

    test.verifier
        .expect_request(zebra_consensus::Request::Commit(block))
        .await
        .respond(Err(zn::BoxError::from("local verifier failure")));
    let (_, actual_hash) = test.sync.downloads.next().await.unwrap().unwrap_err();

    assert_eq!(actual_hash, hash);
}

/// Exhausted retries stall a response even when another advertised block commits.
///
/// Both hashes come from the same response. Committing one block must not hide
/// the failure to obtain the other after all `NotFound` retries are exhausted.
#[tokio::test]
async fn exhausted_missing_hash_stalls_response_despite_committed_block() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();
    let committed_hash = Hash([2; 32]);
    let missing_hash = Hash([3; 32]);

    let observer = test
        .hashes_for_obtain_tips(vec![committed_hash, missing_hash])
        .await;

    test.sync
        .handle_block_response(Ok((Height(1), committed_hash)))
        .unwrap();

    // Report the initial failure and each permitted retry's failure.
    for _ in 0..=MAX_BLOCK_REOBTAIN_RETRIES {
        // Simulate taking the hash from the retry queue for another attempt.
        test.sync.reobtain_hashes.shift_remove(&missing_hash);
        test.sync
            .handle_download_response(Err((
                BlockDownloadVerifyError::DownloadFailed {
                    error: "NotFound".into(),
                    hash: missing_hash,
                },
                missing_hash,
            )))
            .unwrap();
    }

    assert_eq!(
        observer.try_outcome(),
        Ok(Some(false)),
        "a committed block must not hide an exhausted missing hash in the same response",
    );
}

/// A conclusively invalid block stalls its announcing response.
#[tokio::test]
async fn invalid_block_stalls_response() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();
    let hash = Hash([2; 32]);

    let observer = test.hashes_for_obtain_tips(vec![hash]).await;

    let _result = test.sync.handle_download_response(Err((
        BlockDownloadVerifyError::Invalid {
            error: zebra_consensus::VerifyBlockError::from(
                zebra_consensus::BlockError::MissingHeight(hash),
            )
            .into(),
            height: Height(1),
            hash,
            advertiser_addr: None,
        },
        hash,
    )));

    assert_eq!(observer.try_outcome(), Ok(Some(false)));
}

/// A downloader height rejection stalls the response that advertised the block.
///
/// This exercises [`BlockDownloadVerifyError::InvalidHeight`] rather than an
/// error returned by the consensus verifier.
#[tokio::test]
async fn invalid_height_stalls_response() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();
    let hash = Hash([2; 32]);

    let observer = test.hashes_for_obtain_tips(vec![hash]).await;

    test.sync
        .handle_download_response(Err((
            BlockDownloadVerifyError::InvalidHeight {
                hash,
                advertiser_addr: None,
            },
            hash,
        )))
        .unwrap();

    assert_eq!(
        observer.try_outcome(),
        Ok(Some(false)),
        "an invalid block height must stall the announcing response",
    );
}

/// A wrapped local state error releases feedback without accusing the peer.
#[tokio::test]
async fn state_service_failure_releases_neutral_feedback() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();
    let hash = Hash([2; 32]);

    let observer = test.hashes_for_obtain_tips(vec![hash]).await;

    let _result = test.sync.handle_download_response(Err((
        BlockDownloadVerifyError::Invalid {
            error: zebra_consensus::VerifyBlockError::StateService {
                source: "state unavailable".into(),
                hash,
            }
            .into(),
            height: Height(1),
            hash,
            advertiser_addr: None,
        },
        hash,
    )));

    assert_eq!(observer.try_outcome(), Ok(None));
}

/// Superseding a checkpoint request does not prove that its hash was committed.
#[tokio::test]
async fn superseded_request_releases_neutral_feedback() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();
    let hash = Hash([2; 32]);

    let observer = test.hashes_for_obtain_tips(vec![hash]).await;

    test.sync
        .handle_download_response(Err((
            BlockDownloadVerifyError::Invalid {
                error: zebra_consensus::VerifyCheckpointError::NewerRequest {
                    height: Height(1),
                    hash,
                }
                .into(),
                height: Height(1),
                hash,
                advertiser_addr: None,
            },
            hash,
        )))
        .unwrap();

    assert_eq!(observer.try_outcome(), Ok(None));
}

/// A final missing download exhausts its retries even without prospective tips.
#[tokio::test]
async fn final_missing_download_exhausts_retries() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();
    let hash = Hash([2; 32]);
    let (feedback, observer) = FindResponseFeedback::new_for_test();

    let obtain_responses = test.respond_to_obtain_tips(vec![hash], feedback);
    let mock_responses = async {
        obtain_responses.await;

        let block_retries =
            (usize::from(MAX_BLOCK_REOBTAIN_RETRIES) + 1) * (BLOCK_DOWNLOAD_RETRY_LIMIT + 1);
        for _ in 0..block_retries {
            test.peers
                .expect_request(zn::Request::BlocksByHash([hash].into_iter().collect()))
                .await
                .respond(Err(zn::BoxError::from("NotFound")));
        }
    };

    let round = test.sync.try_to_sync();
    tokio::pin!(round);
    tokio::select! { biased;
        () = mock_responses => {}
        result = &mut round => {
            panic!("sync returned before completing missing-block retries: {result:?}");
        }
    }
    round.await.unwrap();

    assert_eq!(observer.try_outcome(), Ok(Some(false)));
}

/// Final-download processing times out even when verifier readiness never completes.
#[tokio::test(start_paused = true)]
async fn final_download_bounds_verifier_readiness() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();
    let (tip, _sender) = MockChainTip::new();
    let read_state: Mock<zs::ReadRequest, zs::ReadResponse> = MockService::build().for_unit_tests();
    let (misbehavior, _receiver) = mpsc::channel(1);
    let (mut sync, _) = ChainSync::new(
        &ZebradConfig::default(),
        Height(0),
        test.peers.clone(),
        NeverReadyVerifier,
        test.state.clone(),
        read_state,
        tip,
        misbehavior,
    );
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let hash = block.hash();
    let (feedback, _observer) = FindResponseFeedback::new_for_test();

    let obtain_responses = test.respond_to_obtain_tips(vec![hash], feedback);
    let mock_responses = async {
        obtain_responses.await;

        test.peers
            .expect_request(zn::Request::BlocksByHash([hash].into_iter().collect()))
            .await
            .respond(zn::Response::Blocks(vec![
                zn::InventoryResponse::Available((block, None)),
            ]));
    };

    let (result, ()) = tokio::join!(
        tokio::time::timeout(super::super::BLOCK_VERIFY_TIMEOUT * 2, sync.try_to_sync()),
        mock_responses,
    );

    assert!(
        matches!(&result, Ok(Err(error)) if error.is::<Elapsed>()),
        "expected the syncer's timeout before the test deadline, got {result:?}",
    );
}

/// Cancelling downloads releases feedback for hashes not yet queued as well.
#[tokio::test]
async fn cancellation_releases_deferred_feedback() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();
    let (feedback, observer) = FindResponseFeedback::new_for_test();

    test.sync
        .track_find_response(&[Hash([2; 32])], Some(feedback));

    test.sync.cancel_downloads();

    assert_eq!(observer.try_outcome(), Ok(None));
    assert!(test.sync.find_response_progress.is_empty());
}

/// A final committed singleton produces useful feedback before the round returns.
#[tokio::test]
async fn final_verified_download_credits_response() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let hash = block.hash();
    let (feedback, observer) = FindResponseFeedback::new_for_test();

    let obtain_responses = test.respond_to_obtain_tips(vec![hash], feedback);
    let mock_responses = async {
        obtain_responses.await;

        test.peers
            .expect_request(zn::Request::BlocksByHash([hash].into_iter().collect()))
            .await
            .respond(zn::Response::Blocks(vec![
                zn::InventoryResponse::Available((block.clone(), None)),
            ]));

        test.verifier
            .expect_request(zebra_consensus::Request::Commit(block))
            .await
            .respond(hash);
    };

    let (result, ()) = tokio::join!(test.sync.try_to_sync(), mock_responses);
    result.unwrap();

    assert_eq!(observer.try_outcome(), Ok(Some(true)));
}

/// Repeated hashes in one response require only one successful commitment.
#[tokio::test]
async fn duplicate_response_hashes_count_once() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();
    let hash = Hash([2; 32]);
    let (feedback, observer) = FindResponseFeedback::new_for_test();

    test.sync.track_find_response(&[hash, hash], Some(feedback));
    test.sync
        .handle_block_response(Ok((Height(1), hash)))
        .unwrap();

    assert_eq!(observer.try_outcome(), Ok(Some(true)));
}

/// One committed hash advances every response that advertised it.
#[tokio::test]
async fn shared_hash_credits_each_response() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();
    let hash = Hash([2; 32]);
    let (first, first_observer) = FindResponseFeedback::new_for_test();
    let (second, second_observer) = FindResponseFeedback::new_for_test();

    test.sync.track_find_response(&[hash], Some(first));
    test.sync.track_find_response(&[hash], Some(second));
    test.sync
        .handle_block_response(Ok((Height(1), hash)))
        .unwrap();

    assert_eq!(first_observer.try_outcome(), Ok(Some(true)));
    assert_eq!(second_observer.try_outcome(), Ok(Some(true)));
}

/// A missing hash retains attribution while download retries remain.
#[tokio::test]
async fn retryable_missing_hash_keeps_feedback_pending() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();
    let hash = Hash([2; 32]);

    let observer = test.hashes_for_obtain_tips(vec![hash]).await;

    test.sync
        .handle_download_response(Err((
            BlockDownloadVerifyError::DownloadFailed {
                error: "NotFound".into(),
                hash,
            },
            hash,
        )))
        .unwrap();

    assert_eq!(observer.try_outcome(), Err(TryRecvError::Empty));
    assert!(test.sync.reobtain_hashes.contains(&hash));
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
    verifier: Mock<zebra_consensus::Request, Hash>,
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
            verifier.clone(),
            state.clone(),
            read_state,
            tip,
            misbehavior,
        );

        Self {
            sync,
            peers,
            state,
            verifier,
        }
    }

    /// Queues an obtain response whose hashes are absent from local state.
    async fn hashes_for_obtain_tips(&mut self, hashes: Vec<Hash>) -> FindResponseFeedbackObserver {
        let (feedback, observer) = FindResponseFeedback::new_for_test();

        let mock_responses = self.respond_to_obtain_tips(hashes, feedback);

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

    /// Supplies an obtain tips response and answers the corresponding state queries.
    ///
    /// Clones the mock handles before returning so the future does not borrow `self`.
    fn respond_to_obtain_tips(
        &self,
        hashes: Vec<Hash>,
        feedback: FindResponseFeedback,
    ) -> impl Future<Output = ()> + 'static {
        let mut peers = self.peers.clone();
        let mut state = self.state.clone();

        async move {
            state
                .expect_request(zs::Request::BlockLocator)
                .await
                .respond(zs::Response::BlockLocator(vec![Hash([0; 32])]));

            peers
                .expect_request(zn::Request::FindBlocks {
                    known_blocks: vec![Hash([0; 32])],
                    stop: None,
                })
                .await
                .respond(zn::Response::BlockHashes {
                    hashes: hashes.clone(),
                    feedback: Some(feedback),
                });

            state
                .expect_request(zs::Request::KnownBlock(hashes[0]))
                .await
                .respond(zs::Response::KnownBlock(None));

            for _ in 1..FANOUT {
                peers
                    .expect_request(zn::Request::FindBlocks {
                        known_blocks: vec![Hash([0; 32])],
                        stop: None,
                    })
                    .await
                    .respond(Err(zn::BoxError::from("unused fanout response")));
            }

            for hash in hashes.into_iter().collect::<IndexSet<_>>() {
                state
                    .expect_request(zs::Request::KnownBlock(hash))
                    .await
                    .respond(zs::Response::KnownBlock(None));
            }
        }
    }
}

/// A verifier whose readiness deliberately never completes.
#[derive(Clone)]
struct NeverReadyVerifier;

impl Service<zebra_consensus::Request> for NeverReadyVerifier {
    type Response = Hash;
    type Error = zn::BoxError;
    type Future = Ready<Result<Hash, zn::BoxError>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Pending
    }

    fn call(&mut self, _request: zebra_consensus::Request) -> Self::Future {
        unreachable!("this verifier never becomes ready")
    }
}
