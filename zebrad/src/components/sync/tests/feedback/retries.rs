//! Feedback ownership when missing-block retries are queued.

use std::sync::Arc;

use futures::StreamExt;
use tokio::{sync::mpsc::error::TryRecvError, time::timeout};

use zebra_chain::{block::Block, serialization::ZcashDeserializeInto};
use zebra_network as zn;

use super::TestScenario;
use crate::components::sync::BLOCK_VERIFY_TIMEOUT;

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

    let () = timeout(BLOCK_VERIFY_TIMEOUT, test.sync.reobtain_missing_blocks())
        .await
        .expect("queuing a duplicate retry must not wait for the existing download");

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
