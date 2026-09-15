//! Feedback ownership when missing-block retries are queued.

use std::sync::Arc;
use std::task::{Context, Poll};

use futures::{future::Ready, StreamExt};
use tokio::{
    sync::mpsc::{self, error::TryRecvError},
    time::timeout,
};
use tower::Service;

use zebra_chain::{
    block::{Block, Hash, Height},
    chain_tip::mock::MockChainTip,
    serialization::ZcashDeserializeInto,
};
use zebra_network as zn;
use zebra_state as zs;
use zebra_test::mock_service::MockService;

use super::{Mock, TestScenario};
use crate::{
    components::sync::{downloads::BlockDownloadVerifyError, ChainSync, BLOCK_VERIFY_TIMEOUT},
    config::ZebradConfig,
};

/// A retry queued during an active download is ignored without losing response feedback.
#[tokio::test]
async fn retry_during_active_download_preserves_feedback() {
    let _test_guard = zebra_test::init();

    let mut test = TestScenario::new();
    let (feedback, observer) = zn::FindResponseFeedback::new_for_test();

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let hash = block.hash();

    test.sync.track_find_response(&[hash], Some(feedback));
    test.sync.downloads.download_and_verify(hash).await.unwrap();

    // Queue a retry while the original download is still in progress.
    test.sync.reobtain_hashes.insert(hash);

    timeout(BLOCK_VERIFY_TIMEOUT, test.sync.reobtain_missing_blocks())
        .await
        .expect("queuing a duplicate retry must not wait for the existing download")
        .expect("an existing download makes a duplicate retry harmless");

    assert!(test.sync.reobtain_hashes.is_empty());
    assert_eq!(test.sync.downloads.in_flight(), 1);
    assert_eq!(observer.try_outcome(), Err(TryRecvError::Empty));

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

    let response = timeout(BLOCK_VERIFY_TIMEOUT, test.sync.downloads.next())
        .await
        .expect("the mocked download and verification must complete")
        .expect("the existing download must yield its result");

    test.sync.handle_download_response(response).unwrap();

    assert_eq!(observer.try_outcome(), Ok(Some(true)));
    assert_eq!(observer.try_outcome(), Err(TryRecvError::Disconnected));
}

/// Failed retry scheduling propagates the error and releases feedback neutrally.
#[tokio::test]
async fn failed_retry_enqueue_releases_neutral_feedback() {
    let _test_guard = zebra_test::init();

    let mut sync = ChainSync::with_unavailable_network();
    let (feedback, observer) = zn::FindResponseFeedback::new_for_test();
    let hash = Hash([2; 32]);

    sync.track_find_response(&[hash], Some(feedback));
    sync.reobtain_hashes.insert(hash);

    let result = timeout(BLOCK_VERIFY_TIMEOUT, sync.finish_pending_download())
        .await
        .expect("a failed retry enqueue must not wait for a download result");

    assert!(
        matches!(
            result,
            Err(BlockDownloadVerifyError::NetworkServiceError { .. })
        ),
        "a retry readiness failure must propagate, got {result:?}",
    );
    assert_eq!(observer.try_outcome(), Ok(None));
    assert_eq!(observer.try_outcome(), Err(TryRecvError::Disconnected));
    assert!(sync.find_response_progress.is_empty());
    assert!(sync.reobtain_hashes.is_empty());
    assert_eq!(sync.downloads.in_flight(), 0);
}

/// A syncer whose block network fails before a request can be queued.
type UnavailableNetworkSync = ChainSync<
    UnavailableNetwork,
    Mock<zs::Request, zs::Response>,
    Mock<zs::ReadRequest, zs::ReadResponse>,
    Mock<zebra_consensus::Request, Hash>,
    MockChainTip,
>;

impl UnavailableNetworkSync {
    /// Creates a [`ChainSync`] with a network readiness failure and no background downloads.
    fn with_unavailable_network() -> Self {
        let state = MockService::build().for_unit_tests();
        let read_state = MockService::build().for_unit_tests();
        let verifier = MockService::build().for_unit_tests();
        let (tip, _tip_sender) = MockChainTip::new();
        let (misbehavior, _receiver) = mpsc::channel(1);

        let (sync, _) = Self::new(
            &ZebradConfig::default(),
            Height(0),
            UnavailableNetwork,
            verifier,
            state,
            read_state,
            tip,
            misbehavior,
        );

        sync
    }
}

/// A network service that rejects readiness without accepting any requests.
#[derive(Clone)]
struct UnavailableNetwork;

impl Service<zn::Request> for UnavailableNetwork {
    type Response = zn::Response;
    type Error = zn::BoxError;
    type Future = Ready<Result<zn::Response, zn::BoxError>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Err("network unavailable".into()))
    }

    fn call(&mut self, _request: zn::Request) -> Self::Future {
        unreachable!("the network never becomes ready")
    }
}
