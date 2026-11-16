//! Queue-pressure recovery through the real downloader and sync response handler.

use std::{sync::Arc, time::Duration};

use futures::StreamExt;
use zebra_chain::{
    block::{Block, Hash, Height},
    serialization::ZcashDeserializeInto,
};
use zebra_consensus::{RouterError, VerifyBlockError, VerifyCheckpointError};
use zebra_network::{InventoryResponse, Request, Response};
use zebra_state::CommitBlockError;
use zebra_test::mock_service::MockService;

use super::{
    super::MAX_BLOCK_REOBTAIN_RETRIES,
    vectors::{new_chain_sync_with, TestChainSync},
};

#[tokio::test(start_paused = true)]
async fn queue_pressure_reaches_syncer_and_uses_bounded_redownloads() {
    let _init_guard = zebra_test::init();
    for checkpoint_queue in [true, false] {
        for recovers in [false, true] {
            let mut network = MockService::build().for_unit_tests();
            let mut verifier = MockService::build().for_unit_tests();
            let (mut chain_sync, mut misbehavior_rx) =
                new_chain_sync_with(network.clone(), verifier.clone());
            let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
                .zcash_deserialize_into()
                .unwrap();
            let hash = block.hash();
            let unrelated_hash = Hash([0xaa; 32]);
            chain_sync.queue_block(unrelated_hash).await.unwrap();
            let unrelated_request = network
                .expect_request(Request::BlockDownload {
                    hash: unrelated_hash,
                    advertiser: None,
                })
                .await;

            for attempt in 0..=MAX_BLOCK_REOBTAIN_RETRIES {
                if attempt == 0 {
                    chain_sync.queue_block(hash).await.unwrap();
                } else {
                    chain_sync.reobtain_missing_blocks().await;
                }
                network
                    .expect_request(Request::BlockDownload {
                        hash,
                        advertiser: None,
                    })
                    .await
                    .respond(Response::Blocks(vec![InventoryResponse::Available((
                        block.clone(),
                        Some("127.0.0.1:8233".parse().unwrap()),
                    ))]));
                let request = verifier
                    .expect_request(zebra_consensus::Request::Commit(block.clone()))
                    .await;
                let succeeds = recovers && attempt == MAX_BLOCK_REOBTAIN_RETRIES;
                if succeeds {
                    request.respond(hash);
                } else {
                    let error = if checkpoint_queue {
                        RouterError::from(VerifyCheckpointError::QueuedLimit)
                    } else {
                        RouterError::from(VerifyBlockError::Commit(
                            CommitBlockError::OrphanQueueFull,
                        ))
                    };
                    assert_eq!(error.misbehavior_score(), 0);
                    request.respond_error(error.into());
                }

                let response =
                    tokio::time::timeout(Duration::from_secs(1), chain_sync.downloads.next())
                        .await
                        .expect("queue pressure must reach the syncer without a local retry loop")
                        .expect("the requested download must produce a result");
                match response {
                    Ok(verified) => {
                        assert!(succeeds);
                        assert_eq!(verified, (Height(1), hash));
                        chain_sync.handle_block_response(Ok(verified)).unwrap();
                        assert!(!chain_sync.block_reobtain_retries.contains_key(&hash));
                    }
                    Err(error) => {
                        assert!(!succeeds);
                        assert_eq!(TestChainSync::block_error_kind(&error), "queue_full");
                        let result = chain_sync.handle_block_response(Err(error));
                        if attempt < MAX_BLOCK_REOBTAIN_RETRIES {
                            assert!(result.is_ok());
                            assert!(chain_sync.reobtain_hashes.contains(&hash));
                            assert_eq!(
                                chain_sync.block_reobtain_retries.get(&hash),
                                Some(&(attempt + 1))
                            );
                        } else {
                            assert!(
                                result.is_err(),
                                "persistent pressure must explicitly restart sync"
                            );
                            assert!(!chain_sync.block_reobtain_retries.contains_key(&hash));
                        }
                    }
                }
                assert!(misbehavior_rx.try_recv().is_err());
                assert!(
                    !chain_sync.downloads.is_empty(),
                    "handling pressure must not cancel the unrelated download"
                );
            }
            assert!(chain_sync.reobtain_hashes.is_empty());
            chain_sync.downloads.cancel_all();
            drop(unrelated_request);
            assert!(chain_sync.downloads.is_empty());
            network.expect_no_requests().await;
            verifier.expect_no_requests().await;
        }
    }
}
