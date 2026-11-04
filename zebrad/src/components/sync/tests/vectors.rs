//! Fixed test vectors for the syncer.

#![allow(clippy::unwrap_in_result)]

use std::{collections::HashMap, sync::Arc, time::Duration};

use color_eyre::Report;
use futures::{Future, FutureExt, StreamExt};

use zebra_chain::{
    block::{self, Block, Height},
    chain_tip::mock::{MockChainTip, MockChainTipSender},
    parameters::subsidy::SubsidyError,
    serialization::ZcashDeserializeInto,
};
use zebra_consensus::{
    error::TransactionError, Config as ConsensusConfig, RouterError, VerifyBlockError,
};
use zebra_network::{InventoryResponse, PeerSocketAddr};
use zebra_state::Config as StateConfig;
use zebra_test::mock_service::{MockService, PanicAssertion};

use zebra_network as zn;
use zebra_state as zs;

use crate::{
    components::{
        sync::{self, downloads::BlockDownloadVerifyError, RestartReason, SyncStatus},
        ChainSync,
    },
    config::ZebradConfig,
};

use InventoryResponse::*;

/// Maximum time to wait for a request to any test service.
///
/// Mock-only tests pause Tokio time, preserving the full no-request observation window without
/// spending that time idle.
const MAX_SERVICE_REQUEST_DELAY: Duration = Duration::from_millis(1000);

/// Test that the syncer downloads genesis, blocks 1-2 using obtain_tips, and blocks 3-4 using extend_tips.
///
/// This test also makes sure that the syncer downloads blocks in order.
#[tokio::test(start_paused = true)]
async fn sync_blocks_ok() -> Result<(), crate::BoxError> {
    // Get services
    let (
        chain_sync_future,
        _sync_status,
        mut block_verifier_router,
        mut peer_set,
        mut state_service,
        _mock_chain_tip_sender,
    ) = setup();

    // Get blocks
    let block0: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES.zcash_deserialize_into()?;
    let block0_hash = block0.hash();

    let block1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    let block1_hash = block1.hash();

    let block2: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES.zcash_deserialize_into()?;
    let block2_hash = block2.hash();

    let block3: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_3_BYTES.zcash_deserialize_into()?;
    let block3_hash = block3.hash();

    let block4: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_4_BYTES.zcash_deserialize_into()?;
    let block4_hash = block4.hash();

    // Start the syncer
    let chain_sync_task_handle = tokio::spawn(chain_sync_future);

    // ChainSync::request_genesis

    // State is checked for genesis
    state_service
        .expect_request(zs::Request::KnownBlock(block0_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    // Block 0 is fetched and committed to the state
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block0_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block0.clone(),
            None,
        ))]));

    block_verifier_router
        .expect_request(zebra_consensus::Request::Commit(block0))
        .await
        .respond(block0_hash);

    // Check that nothing unexpected happened.
    // We expect more requests to the state service, because the syncer keeps on running.
    peer_set.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;

    // State is checked for genesis again
    state_service
        .expect_request(zs::Request::KnownBlock(block0_hash))
        .await
        .respond(zs::Response::KnownBlock(Some(zs::KnownBlock::BestChain)));

    // ChainSync::obtain_tips

    // State is asked for a block locator.
    state_service
        .expect_request(zs::Request::BlockLocator)
        .await
        .respond(zs::Response::BlockLocator(vec![block0_hash]));

    // Network is sent the block locator
    peer_set
        .expect_request(zn::Request::FindBlocks {
            known_blocks: vec![block0_hash],
            stop: None,
        })
        .await
        .respond(zn::Response::BlockHashes(vec![
            block1_hash, // tip
            block2_hash, // expected_next
        ]));

    // State is checked for the first unknown block (block 1)
    state_service
        .expect_request(zs::Request::KnownBlock(block1_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    // Clear remaining block locator requests
    for _ in 0..(sync::FANOUT - 1) {
        peer_set
            .expect_request(zn::Request::FindBlocks {
                known_blocks: vec![block0_hash],
                stop: None,
            })
            .await
            .respond(Err(zn::BoxError::from("synthetic test obtain tips error")));
    }

    // Check that nothing unexpected happened.
    peer_set.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;

    // State is checked for all non-tip blocks (blocks 1 & 2) in response order
    state_service
        .expect_request(zs::Request::KnownBlock(block1_hash))
        .await
        .respond(zs::Response::KnownBlock(None));
    state_service
        .expect_request(zs::Request::KnownBlock(block2_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    // Blocks 1 & 2 are fetched in order, then verified concurrently
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block1_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block1.clone(),
            None,
        ))]));
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block2_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block2.clone(),
            None,
        ))]));

    // We can't guarantee the verification request order
    let mut remaining_blocks: HashMap<block::Hash, Arc<Block>> =
        [(block1_hash, block1), (block2_hash, block2)]
            .iter()
            .cloned()
            .collect();

    for _ in 1..=2 {
        block_verifier_router
            .expect_request_that(|req| remaining_blocks.remove(&req.block().hash()).is_some())
            .await
            .respond_with(|req| req.block().hash());
    }
    assert_eq!(
        remaining_blocks,
        HashMap::new(),
        "expected all non-tip blocks to be verified by obtain tips"
    );

    // Check that nothing unexpected happened.
    block_verifier_router.expect_no_requests().await;
    state_service.expect_no_requests().await;

    // ChainSync::extend_tips

    // Network is sent a block locator based on the tip
    peer_set
        .expect_request(zn::Request::FindBlocks {
            known_blocks: vec![block1_hash],
            stop: None,
        })
        .await
        .respond(zn::Response::BlockHashes(vec![
            block2_hash, // tip (discarded - already fetched)
            block3_hash, // expected_next
            block4_hash,
        ]));

    // Clear remaining block locator requests
    for _ in 0..(sync::FANOUT - 1) {
        peer_set
            .expect_request(zn::Request::FindBlocks {
                known_blocks: vec![block1_hash],
                stop: None,
            })
            .await
            .respond(Err(zn::BoxError::from("synthetic test extend tips error")));
    }

    // Check that nothing unexpected happened.
    block_verifier_router.expect_no_requests().await;
    state_service.expect_no_requests().await;

    // Blocks 3 & 4 are fetched in order, then verified concurrently
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block3_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block3.clone(),
            None,
        ))]));
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block4_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block4.clone(),
            None,
        ))]));

    // We can't guarantee the verification request order
    let mut remaining_blocks: HashMap<block::Hash, Arc<Block>> =
        [(block3_hash, block3), (block4_hash, block4)]
            .iter()
            .cloned()
            .collect();

    for _ in 3..=4 {
        block_verifier_router
            .expect_request_that(|req| remaining_blocks.remove(&req.block().hash()).is_some())
            .await
            .respond_with(|req| req.block().hash());
    }
    assert_eq!(
        remaining_blocks,
        HashMap::new(),
        "expected all non-tip blocks to be verified by extend tips"
    );

    // Check that nothing unexpected happened.
    block_verifier_router.expect_no_requests().await;
    state_service.expect_no_requests().await;

    let chain_sync_result = chain_sync_task_handle.now_or_never();
    assert!(
        chain_sync_result.is_none(),
        "unexpected error or panic in chain sync task: {chain_sync_result:?}",
    );

    Ok(())
}

/// Test that the syncer downloads a singleton unknown hash returned by obtain_tips.
#[tokio::test(start_paused = true)]
async fn sync_singleton_obtain_tips_ok() -> Result<(), crate::BoxError> {
    let (
        chain_sync_future,
        _sync_status,
        mut block_verifier_router,
        mut peer_set,
        mut state_service,
        _mock_chain_tip_sender,
    ) = setup();

    let block0: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES.zcash_deserialize_into()?;
    let block0_hash = block0.hash();

    let block1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    let block1_hash = block1.hash();

    let chain_sync_task_handle = tokio::spawn(chain_sync_future);

    // ChainSync::request_genesis

    state_service
        .expect_request(zs::Request::KnownBlock(block0_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block0_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block0.clone(),
            None,
        ))]));

    block_verifier_router
        .expect_request(zebra_consensus::Request::Commit(block0))
        .await
        .respond(block0_hash);

    peer_set.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;

    state_service
        .expect_request(zs::Request::KnownBlock(block0_hash))
        .await
        .respond(zs::Response::KnownBlock(Some(zs::KnownBlock::BestChain)));

    // ChainSync::obtain_tips

    state_service
        .expect_request(zs::Request::BlockLocator)
        .await
        .respond(zs::Response::BlockLocator(vec![block0_hash]));

    peer_set
        .expect_request(zn::Request::FindBlocks {
            known_blocks: vec![block0_hash],
            stop: None,
        })
        .await
        .respond(zn::Response::BlockHashes(vec![block1_hash]));

    // Find the first unknown hash in this peer response.
    state_service
        .expect_request(zs::Request::KnownBlock(block1_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    for _ in 1..sync::FANOUT {
        peer_set
            .expect_request(zn::Request::FindBlocks {
                known_blocks: vec![block0_hash],
                stop: None,
            })
            .await
            .respond(Err(zn::BoxError::from("synthetic test obtain tips error")));
    }

    peer_set.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;

    // Recheck every hash in the merged download set.
    state_service
        .expect_request(zs::Request::KnownBlock(block1_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block1_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block1.clone(),
            None,
        ))]));

    block_verifier_router
        .expect_request(zebra_consensus::Request::Commit(block1))
        .await
        .respond(block1_hash);

    let chain_sync_result = chain_sync_task_handle.now_or_never();
    assert!(
        chain_sync_result.is_none(),
        "unexpected error or panic in chain sync task: {chain_sync_result:?}",
    );

    Ok(())
}

/// Test that obtain_tips downloads blocks that are queued in the state, waiting for their parent.
///
/// A sync restart cancels in-flight downloads, but blocks that were already queued in the state
/// stay there, so a peer response can contain a missing parent followed by its queued children.
/// If queued blocks were treated as present, obtain_tips would reject every such download set with
/// "queued download of hash behind our chain tip", and the sync would stall forever.
#[tokio::test(start_paused = true)]
async fn sync_obtain_tips_downloads_queued_blocks() -> Result<(), crate::BoxError> {
    let (
        chain_sync_future,
        _sync_status,
        mut block_verifier_router,
        mut peer_set,
        mut state_service,
        _mock_chain_tip_sender,
    ) = setup();

    let block0: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES.zcash_deserialize_into()?;
    let block0_hash = block0.hash();

    let block1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    let block1_hash = block1.hash();

    let block2: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES.zcash_deserialize_into()?;
    let block2_hash = block2.hash();

    let chain_sync_task_handle = tokio::spawn(chain_sync_future);

    // ChainSync::request_genesis

    state_service
        .expect_request(zs::Request::KnownBlock(block0_hash))
        .await
        .respond(zs::Response::KnownBlock(Some(zs::KnownBlock::BestChain)));

    // ChainSync::obtain_tips

    state_service
        .expect_request(zs::Request::BlockLocator)
        .await
        .respond(zs::Response::BlockLocator(vec![block0_hash]));

    peer_set
        .expect_request(zn::Request::FindBlocks {
            known_blocks: vec![block0_hash],
            stop: None,
        })
        .await
        .respond(zn::Response::BlockHashes(vec![block1_hash, block2_hash]));

    // Block 1 is missing, because its download was cancelled by a sync restart.
    state_service
        .expect_request(zs::Request::KnownBlock(block1_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    for _ in 1..sync::FANOUT {
        peer_set
            .expect_request(zn::Request::FindBlocks {
                known_blocks: vec![block0_hash],
                stop: None,
            })
            .await
            .respond(Err(zn::BoxError::from("synthetic test obtain tips error")));
    }

    peer_set.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;

    // Recheck every hash in the merged download set: block 2 is queued, waiting for block 1.
    state_service
        .expect_request(zs::Request::KnownBlock(block1_hash))
        .await
        .respond(zs::Response::KnownBlock(None));
    state_service
        .expect_request(zs::Request::KnownBlock(block2_hash))
        .await
        .respond(zs::Response::KnownBlock(Some(zs::KnownBlock::Queue)));

    // Both blocks are downloaded, so the missing parent reaches the state.
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block1_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block1.clone(),
            None,
        ))]));
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block2_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block2.clone(),
            None,
        ))]));

    let mut remaining_blocks: HashMap<block::Hash, Arc<Block>> =
        [(block1_hash, block1), (block2_hash, block2)]
            .iter()
            .cloned()
            .collect();

    for _ in 1..=2 {
        block_verifier_router
            .expect_request_that(|req| remaining_blocks.remove(&req.block().hash()).is_some())
            .await
            .respond_with(|req| req.block().hash());
    }
    assert_eq!(
        remaining_blocks,
        HashMap::new(),
        "expected the missing parent and its queued child to be verified by obtain tips"
    );

    let chain_sync_result = chain_sync_task_handle.now_or_never();
    assert!(
        chain_sync_result.is_none(),
        "unexpected error or panic in chain sync task: {chain_sync_result:?}",
    );

    Ok(())
}

/// Test that the syncer downloads a singleton unknown hash returned by extend_tips.
#[tokio::test(start_paused = true)]
async fn sync_singleton_extend_tips_ok() -> Result<(), crate::BoxError> {
    let (
        chain_sync_future,
        _sync_status,
        mut block_verifier_router,
        mut peer_set,
        mut state_service,
        _mock_chain_tip_sender,
    ) = setup();

    let block0: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES.zcash_deserialize_into()?;
    let block0_hash = block0.hash();

    let block1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    let block1_hash = block1.hash();

    let block2: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES.zcash_deserialize_into()?;
    let block2_hash = block2.hash();

    let block3: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_3_BYTES.zcash_deserialize_into()?;
    let block3_hash = block3.hash();

    let chain_sync_task_handle = tokio::spawn(chain_sync_future);

    // ChainSync::request_genesis

    state_service
        .expect_request(zs::Request::KnownBlock(block0_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block0_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block0.clone(),
            None,
        ))]));

    block_verifier_router
        .expect_request(zebra_consensus::Request::Commit(block0))
        .await
        .respond(block0_hash);

    peer_set.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;

    state_service
        .expect_request(zs::Request::KnownBlock(block0_hash))
        .await
        .respond(zs::Response::KnownBlock(Some(zs::KnownBlock::BestChain)));

    // ChainSync::obtain_tips

    state_service
        .expect_request(zs::Request::BlockLocator)
        .await
        .respond(zs::Response::BlockLocator(vec![block0_hash]));

    peer_set
        .expect_request(zn::Request::FindBlocks {
            known_blocks: vec![block0_hash],
            stop: None,
        })
        .await
        .respond(zn::Response::BlockHashes(vec![block1_hash, block2_hash]));

    // Find the first unknown hash in this peer response.
    state_service
        .expect_request(zs::Request::KnownBlock(block1_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    for _ in 1..sync::FANOUT {
        peer_set
            .expect_request(zn::Request::FindBlocks {
                known_blocks: vec![block0_hash],
                stop: None,
            })
            .await
            .respond(Err(zn::BoxError::from("synthetic test obtain tips error")));
    }

    peer_set.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;

    // Recheck every hash in the merged download set.
    state_service
        .expect_request(zs::Request::KnownBlock(block1_hash))
        .await
        .respond(zs::Response::KnownBlock(None));
    state_service
        .expect_request(zs::Request::KnownBlock(block2_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block1_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block1.clone(),
            None,
        ))]));
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block2_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block2.clone(),
            None,
        ))]));

    let mut remaining_blocks: HashMap<block::Hash, Arc<Block>> =
        [(block1_hash, block1), (block2_hash, block2)]
            .iter()
            .cloned()
            .collect();

    for _ in 1..=2 {
        block_verifier_router
            .expect_request_that(|req| {
                matches!(req, zebra_consensus::Request::Commit(_))
                    && remaining_blocks.remove(&req.block().hash()).is_some()
            })
            .await
            .respond_with(|req| req.block().hash());
    }
    assert!(
        remaining_blocks.is_empty(),
        "expected obtain tips to verify blocks 1 and 2; remaining blocks: {:?}",
        remaining_blocks.keys().collect::<Vec<_>>(),
    );

    block_verifier_router.expect_no_requests().await;
    state_service.expect_no_requests().await;

    // ChainSync::extend_tips

    peer_set
        .expect_request(zn::Request::FindBlocks {
            known_blocks: vec![block1_hash],
            stop: None,
        })
        .await
        .respond(zn::Response::BlockHashes(vec![
            block2_hash, // expected overlap
            block3_hash, // singleton unknown hash
        ]));

    for _ in 1..sync::FANOUT {
        peer_set
            .expect_request(zn::Request::FindBlocks {
                known_blocks: vec![block1_hash],
                stop: None,
            })
            .await
            .respond(Err(zn::BoxError::from("synthetic test extend tips error")));
    }

    block_verifier_router.expect_no_requests().await;
    state_service.expect_no_requests().await;

    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block3_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block3.clone(),
            None,
        ))]));

    block_verifier_router
        .expect_request(zebra_consensus::Request::Commit(block3))
        .await
        .respond(block3_hash);

    let chain_sync_result = chain_sync_task_handle.now_or_never();
    assert!(
        chain_sync_result.is_none(),
        "unexpected error or panic in chain sync task: {chain_sync_result:?}",
    );

    Ok(())
}

/// Test that the syncer downloads genesis, blocks 1-2 using obtain_tips, and blocks 3-4 using extend_tips,
/// with duplicate block hashes.
///
/// This test also makes sure that the syncer downloads blocks in order.
#[tokio::test(start_paused = true)]
async fn sync_blocks_duplicate_hashes_ok() -> Result<(), crate::BoxError> {
    // Get services
    let (
        chain_sync_future,
        _sync_status,
        mut block_verifier_router,
        mut peer_set,
        mut state_service,
        _mock_chain_tip_sender,
    ) = setup();

    // Get blocks
    let block0: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES.zcash_deserialize_into()?;
    let block0_hash = block0.hash();

    let block1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    let block1_hash = block1.hash();

    let block2: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES.zcash_deserialize_into()?;
    let block2_hash = block2.hash();

    let block3: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_3_BYTES.zcash_deserialize_into()?;
    let block3_hash = block3.hash();

    let block4: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_4_BYTES.zcash_deserialize_into()?;
    let block4_hash = block4.hash();

    // Start the syncer
    let chain_sync_task_handle = tokio::spawn(chain_sync_future);

    // ChainSync::request_genesis

    // State is checked for genesis
    state_service
        .expect_request(zs::Request::KnownBlock(block0_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    // Block 0 is fetched and committed to the state
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block0_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block0.clone(),
            None,
        ))]));

    block_verifier_router
        .expect_request(zebra_consensus::Request::Commit(block0))
        .await
        .respond(block0_hash);

    // Check that nothing unexpected happened.
    // We expect more requests to the state service, because the syncer keeps on running.
    peer_set.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;

    // State is checked for genesis again
    state_service
        .expect_request(zs::Request::KnownBlock(block0_hash))
        .await
        .respond(zs::Response::KnownBlock(Some(zs::KnownBlock::BestChain)));

    // ChainSync::obtain_tips

    // State is asked for a block locator.
    state_service
        .expect_request(zs::Request::BlockLocator)
        .await
        .respond(zs::Response::BlockLocator(vec![block0_hash]));

    // Network is sent the block locator
    peer_set
        .expect_request(zn::Request::FindBlocks {
            known_blocks: vec![block0_hash],
            stop: None,
        })
        .await
        .respond(zn::Response::BlockHashes(vec![
            block1_hash,
            block1_hash,
            block1_hash, // tip
            block2_hash, // expected_next
        ]));

    // State is checked for the first unknown block (block 1)
    state_service
        .expect_request(zs::Request::KnownBlock(block1_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    // Clear remaining block locator requests
    for _ in 0..(sync::FANOUT - 1) {
        peer_set
            .expect_request(zn::Request::FindBlocks {
                known_blocks: vec![block0_hash],
                stop: None,
            })
            .await
            .respond(Err(zn::BoxError::from("synthetic test obtain tips error")));
    }

    // Check that nothing unexpected happened.
    peer_set.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;

    // State is checked for all non-tip blocks (blocks 1 & 2) in response order
    state_service
        .expect_request(zs::Request::KnownBlock(block1_hash))
        .await
        .respond(zs::Response::KnownBlock(None));
    state_service
        .expect_request(zs::Request::KnownBlock(block2_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    // Blocks 1 & 2 are fetched in order, then verified concurrently
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block1_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block1.clone(),
            None,
        ))]));
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block2_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block2.clone(),
            None,
        ))]));

    // We can't guarantee the verification request order
    let mut remaining_blocks: HashMap<block::Hash, Arc<Block>> =
        [(block1_hash, block1), (block2_hash, block2)]
            .iter()
            .cloned()
            .collect();

    for _ in 1..=2 {
        block_verifier_router
            .expect_request_that(|req| remaining_blocks.remove(&req.block().hash()).is_some())
            .await
            .respond_with(|req| req.block().hash());
    }
    assert_eq!(
        remaining_blocks,
        HashMap::new(),
        "expected all non-tip blocks to be verified by obtain tips"
    );

    // Check that nothing unexpected happened.
    block_verifier_router.expect_no_requests().await;
    state_service.expect_no_requests().await;

    // ChainSync::extend_tips

    // Network is sent a block locator based on the tip
    peer_set
        .expect_request(zn::Request::FindBlocks {
            known_blocks: vec![block1_hash],
            stop: None,
        })
        .await
        .respond(zn::Response::BlockHashes(vec![
            block2_hash, // tip (discarded - already fetched)
            block3_hash, // expected_next
            block4_hash,
            block3_hash,
            block4_hash,
        ]));

    // Clear remaining block locator requests
    for _ in 0..(sync::FANOUT - 1) {
        peer_set
            .expect_request(zn::Request::FindBlocks {
                known_blocks: vec![block1_hash],
                stop: None,
            })
            .await
            .respond(Err(zn::BoxError::from("synthetic test extend tips error")));
    }

    // Check that nothing unexpected happened.
    block_verifier_router.expect_no_requests().await;
    state_service.expect_no_requests().await;

    // Blocks 3 & 4 are fetched in order, then verified concurrently
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block3_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block3.clone(),
            None,
        ))]));
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block4_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block4.clone(),
            None,
        ))]));

    // We can't guarantee the verification request order
    let mut remaining_blocks: HashMap<block::Hash, Arc<Block>> =
        [(block3_hash, block3), (block4_hash, block4)]
            .iter()
            .cloned()
            .collect();

    for _ in 3..=4 {
        block_verifier_router
            .expect_request_that(|req| remaining_blocks.remove(&req.block().hash()).is_some())
            .await
            .respond_with(|req| req.block().hash());
    }
    assert_eq!(
        remaining_blocks,
        HashMap::new(),
        "expected all non-tip blocks to be verified by extend tips"
    );

    // Check that nothing unexpected happened.
    block_verifier_router.expect_no_requests().await;
    state_service.expect_no_requests().await;

    let chain_sync_result = chain_sync_task_handle.now_or_never();
    assert!(
        chain_sync_result.is_none(),
        "unexpected error or panic in chain sync task: {chain_sync_result:?}",
    );

    Ok(())
}

/// Test that zebra-network rejects blocks that are a long way ahead of the state tip.
#[tokio::test(start_paused = true)]
async fn sync_block_lookahead_drop() -> Result<(), crate::BoxError> {
    // Get services
    let (
        chain_sync_future,
        _sync_status,
        mut block_verifier_router,
        mut peer_set,
        mut state_service,
        _mock_chain_tip_sender,
    ) = setup();

    // Get blocks
    let block0: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES.zcash_deserialize_into()?;
    let block0_hash = block0.hash();

    // Get a block that is a long way away from genesis
    let block982k: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_982681_BYTES.zcash_deserialize_into()?;

    // Start the syncer
    let chain_sync_task_handle = tokio::spawn(chain_sync_future);

    // State is checked for genesis
    state_service
        .expect_request(zs::Request::KnownBlock(block0_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    // Block 0 is fetched, but the peer returns a much higher block.
    // (Mismatching hashes are usually ignored by the network service,
    // but we use them here to test the syncer lookahead.)
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block0_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block982k.clone(),
            None,
        ))]));

    // Block is dropped because it is too far ahead of the tip.
    // We expect more requests to the state service, because the syncer keeps on running.
    peer_set.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;

    let chain_sync_result = chain_sync_task_handle.now_or_never();
    assert!(
        chain_sync_result.is_none(),
        "unexpected error or panic in chain sync task: {chain_sync_result:?}",
    );

    Ok(())
}

/// Test that the sync downloader rejects blocks that are too high in obtain_tips.
///
/// TODO: also test that it rejects blocks behind the tip limit. (Needs ~100 fake blocks.)
#[tokio::test(start_paused = true)]
async fn sync_block_too_high_obtain_tips() -> Result<(), crate::BoxError> {
    // Get services
    let (
        chain_sync_future,
        _sync_status,
        mut block_verifier_router,
        mut peer_set,
        mut state_service,
        _mock_chain_tip_sender,
    ) = setup();

    // Get blocks
    let block0: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES.zcash_deserialize_into()?;
    let block0_hash = block0.hash();

    let block1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    let block1_hash = block1.hash();

    let block2: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES.zcash_deserialize_into()?;
    let block2_hash = block2.hash();

    // Also get a block that is a long way away from genesis
    let block982k: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_982681_BYTES.zcash_deserialize_into()?;
    let block982k_hash = block982k.hash();

    // Start the syncer
    let chain_sync_task_handle = tokio::spawn(chain_sync_future);

    // ChainSync::request_genesis

    // State is checked for genesis
    state_service
        .expect_request(zs::Request::KnownBlock(block0_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    // Block 0 is fetched and committed to the state
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block0_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block0.clone(),
            None,
        ))]));

    block_verifier_router
        .expect_request(zebra_consensus::Request::Commit(block0))
        .await
        .respond(block0_hash);

    // Check that nothing unexpected happened.
    // We expect more requests to the state service, because the syncer keeps on running.
    peer_set.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;

    // State is checked for genesis again
    state_service
        .expect_request(zs::Request::KnownBlock(block0_hash))
        .await
        .respond(zs::Response::KnownBlock(Some(zs::KnownBlock::BestChain)));

    // ChainSync::obtain_tips

    // State is asked for a block locator.
    state_service
        .expect_request(zs::Request::BlockLocator)
        .await
        .respond(zs::Response::BlockLocator(vec![block0_hash]));

    // Network is sent the block locator
    peer_set
        .expect_request(zn::Request::FindBlocks {
            known_blocks: vec![block0_hash],
            stop: None,
        })
        .await
        .respond(zn::Response::BlockHashes(vec![
            block982k_hash,
            block1_hash, // tip
            block2_hash, // expected_next
        ]));

    // State is checked for the first unknown block (block 982k)
    state_service
        .expect_request(zs::Request::KnownBlock(block982k_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    // Clear remaining block locator requests
    for _ in 0..(sync::FANOUT - 1) {
        peer_set
            .expect_request(zn::Request::FindBlocks {
                known_blocks: vec![block0_hash],
                stop: None,
            })
            .await
            .respond(Err(zn::BoxError::from("synthetic test obtain tips error")));
    }

    // Check that nothing unexpected happened.
    peer_set.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;

    // State is checked for all non-tip blocks (blocks 982k, 1, 2) in response order
    state_service
        .expect_request(zs::Request::KnownBlock(block982k_hash))
        .await
        .respond(zs::Response::KnownBlock(None));
    state_service
        .expect_request(zs::Request::KnownBlock(block1_hash))
        .await
        .respond(zs::Response::KnownBlock(None));
    state_service
        .expect_request(zs::Request::KnownBlock(block2_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    // Blocks 982k, 1, 2 are fetched in order, then verified concurrently,
    // but block 982k verification is skipped because it is too high.
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block982k_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block982k.clone(),
            None,
        ))]));
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block1_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block1.clone(),
            None,
        ))]));
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block2_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block2.clone(),
            None,
        ))]));

    // At this point, the following tasks race:
    // - The valid chain verifier requests
    // - The block too high error, which causes a syncer reset and ChainSync::obtain_tips
    // - ChainSync::extend_tips for the next tip

    let chain_sync_result = chain_sync_task_handle.now_or_never();
    assert!(
        chain_sync_result.is_none(),
        "unexpected error or panic in chain sync task: {chain_sync_result:?}",
    );

    Ok(())
}

/// Test that the sync downloader rejects blocks that are too high in extend_tips.
///
/// TODO: also test that it rejects blocks behind the tip limit. (Needs ~100 fake blocks.)
#[tokio::test(start_paused = true)]
async fn sync_block_too_high_extend_tips() -> Result<(), crate::BoxError> {
    // Get services
    let (
        chain_sync_future,
        _sync_status,
        mut block_verifier_router,
        mut peer_set,
        mut state_service,
        _mock_chain_tip_sender,
    ) = setup();

    // Get blocks
    let block0: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES.zcash_deserialize_into()?;
    let block0_hash = block0.hash();

    let block1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    let block1_hash = block1.hash();

    let block2: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES.zcash_deserialize_into()?;
    let block2_hash = block2.hash();

    let block3: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_3_BYTES.zcash_deserialize_into()?;
    let block3_hash = block3.hash();

    let block4: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_4_BYTES.zcash_deserialize_into()?;
    let block4_hash = block4.hash();

    // Also get a block that is a long way away from genesis
    let block982k: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_982681_BYTES.zcash_deserialize_into()?;
    let block982k_hash = block982k.hash();

    // Start the syncer
    let chain_sync_task_handle = tokio::spawn(chain_sync_future);

    // ChainSync::request_genesis

    // State is checked for genesis
    state_service
        .expect_request(zs::Request::KnownBlock(block0_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    // Block 0 is fetched and committed to the state
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block0_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block0.clone(),
            None,
        ))]));

    block_verifier_router
        .expect_request(zebra_consensus::Request::Commit(block0))
        .await
        .respond(block0_hash);

    // Check that nothing unexpected happened.
    // We expect more requests to the state service, because the syncer keeps on running.
    peer_set.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;

    // State is checked for genesis again
    state_service
        .expect_request(zs::Request::KnownBlock(block0_hash))
        .await
        .respond(zs::Response::KnownBlock(Some(zs::KnownBlock::BestChain)));

    // ChainSync::obtain_tips

    // State is asked for a block locator.
    state_service
        .expect_request(zs::Request::BlockLocator)
        .await
        .respond(zs::Response::BlockLocator(vec![block0_hash]));

    // Network is sent the block locator
    peer_set
        .expect_request(zn::Request::FindBlocks {
            known_blocks: vec![block0_hash],
            stop: None,
        })
        .await
        .respond(zn::Response::BlockHashes(vec![
            block1_hash, // tip
            block2_hash, // expected_next
        ]));

    // State is checked for the first unknown block (block 1)
    state_service
        .expect_request(zs::Request::KnownBlock(block1_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    // Clear remaining block locator requests
    for _ in 0..(sync::FANOUT - 1) {
        peer_set
            .expect_request(zn::Request::FindBlocks {
                known_blocks: vec![block0_hash],
                stop: None,
            })
            .await
            .respond(Err(zn::BoxError::from("synthetic test obtain tips error")));
    }

    // Check that nothing unexpected happened.
    peer_set.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;

    // State is checked for all non-tip blocks (blocks 1 & 2) in response order
    state_service
        .expect_request(zs::Request::KnownBlock(block1_hash))
        .await
        .respond(zs::Response::KnownBlock(None));
    state_service
        .expect_request(zs::Request::KnownBlock(block2_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    // Blocks 1 & 2 are fetched in order, then verified concurrently
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block1_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block1.clone(),
            None,
        ))]));
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block2_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block2.clone(),
            None,
        ))]));

    // We can't guarantee the verification request order
    let mut remaining_blocks: HashMap<block::Hash, Arc<Block>> =
        [(block1_hash, block1), (block2_hash, block2)]
            .iter()
            .cloned()
            .collect();

    for _ in 1..=2 {
        block_verifier_router
            .expect_request_that(|req| remaining_blocks.remove(&req.block().hash()).is_some())
            .await
            .respond_with(|req| req.block().hash());
    }
    assert_eq!(
        remaining_blocks,
        HashMap::new(),
        "expected all non-tip blocks to be verified by obtain tips"
    );

    // Check that nothing unexpected happened.
    block_verifier_router.expect_no_requests().await;
    state_service.expect_no_requests().await;

    // ChainSync::extend_tips

    // Network is sent a block locator based on the tip
    peer_set
        .expect_request(zn::Request::FindBlocks {
            known_blocks: vec![block1_hash],
            stop: None,
        })
        .await
        .respond(zn::Response::BlockHashes(vec![
            block2_hash, // tip (discarded - already fetched)
            block3_hash, // expected_next
            block4_hash,
            block982k_hash,
        ]));

    // Clear remaining block locator requests
    for _ in 0..(sync::FANOUT - 1) {
        peer_set
            .expect_request(zn::Request::FindBlocks {
                known_blocks: vec![block1_hash],
                stop: None,
            })
            .await
            .respond(Err(zn::BoxError::from("synthetic test extend tips error")));
    }

    // Check that nothing unexpected happened.
    block_verifier_router.expect_no_requests().await;
    state_service.expect_no_requests().await;

    // Blocks 3, 4, 982k are fetched in order, then verified concurrently,
    // but block 982k verification is skipped because it is too high.
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block3_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block3.clone(),
            None,
        ))]));
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block4_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block4.clone(),
            None,
        ))]));
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block982k_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block982k.clone(),
            None,
        ))]));

    // At this point, the following tasks race:
    // - The valid chain verifier requests
    // - The block too high error, which causes a syncer reset and ChainSync::obtain_tips
    // - ChainSync::extend_tips for the next tip

    let chain_sync_result = chain_sync_task_handle.now_or_never();
    assert!(
        chain_sync_result.is_none(),
        "unexpected error or panic in chain sync task: {chain_sync_result:?}",
    );

    Ok(())
}

/// Tests that a `BlockDownloadVerifyError::Invalid` wrapping a
/// `CommitBlockError::Duplicate` error does NOT trigger a sync restart.
#[tokio::test]
async fn should_restart_sync_returns_false() {
    let commit_error = zs::CommitBlockError::Duplicate {
        hash_or_height: None,
        location: zebra_state::KnownBlock::BestChain,
    };

    let verify_block_error = VerifyBlockError::Commit(commit_error);
    let router_error = RouterError::Block {
        source: Box::new(verify_block_error),
    };

    let err = BlockDownloadVerifyError::Invalid {
        error: router_error,
        height: block::Height(42),
        hash: block::Hash::from([0xAA; 32]),
        advertiser_addr: None,
    };

    let restart = ChainSync::<
        MockService<zn::Request, zn::Response, PanicAssertion>,
        MockService<zs::Request, zs::Response, PanicAssertion>,
        MockService<zs::ReadRequest, zs::ReadResponse, PanicAssertion>,
        MockService<zebra_consensus::Request, block::Hash, PanicAssertion>,
        MockChainTip,
    >::should_restart_sync(&err);
    assert!(
        !restart,
        "duplicate commit block errors should NOT trigger sync restart"
    );
}

/// Verifies fix for GHSA-gvjc-3w7c-92jx: `AboveLookaheadHeightLimit` now has
/// an explicit match arm in `should_restart_sync` that returns `false`.
#[tokio::test]
async fn above_lookahead_does_not_restart_sync() {
    let err = BlockDownloadVerifyError::AboveLookaheadHeightLimit {
        height: block::Height(60_000),
        hash: block::Hash::from([0xBB; 32]),
    };

    let restart = ChainSync::<
        MockService<zn::Request, zn::Response, PanicAssertion>,
        MockService<zs::Request, zs::Response, PanicAssertion>,
        MockService<zs::ReadRequest, zs::ReadResponse, PanicAssertion>,
        MockService<zebra_consensus::Request, block::Hash, PanicAssertion>,
        MockChainTip,
    >::should_restart_sync(&err);

    assert!(
        !restart,
        "AboveLookaheadHeightLimit should NOT trigger sync restart (GHSA-gvjc-3w7c-92jx fix)"
    );
}

/// Verifies fix for GHSA-gvjc-3w7c-92jx: both height-limit errors now
/// return `false` from `should_restart_sync` — symmetric handling.
#[tokio::test]
async fn both_height_limits_do_not_restart_sync() {
    let below = BlockDownloadVerifyError::BehindTipHeightLimit {
        height: block::Height(1),
        hash: block::Hash::from([0xDD; 32]),
        advertiser_addr: None,
    };

    let above = BlockDownloadVerifyError::AboveLookaheadHeightLimit {
        height: block::Height(60_000),
        hash: block::Hash::from([0xEE; 32]),
    };

    let restart_below = ChainSync::<
        MockService<zn::Request, zn::Response, PanicAssertion>,
        MockService<zs::Request, zs::Response, PanicAssertion>,
        MockService<zs::ReadRequest, zs::ReadResponse, PanicAssertion>,
        MockService<zebra_consensus::Request, block::Hash, PanicAssertion>,
        MockChainTip,
    >::should_restart_sync(&below);

    let restart_above = ChainSync::<
        MockService<zn::Request, zn::Response, PanicAssertion>,
        MockService<zs::Request, zs::Response, PanicAssertion>,
        MockService<zs::ReadRequest, zs::ReadResponse, PanicAssertion>,
        MockService<zebra_consensus::Request, block::Hash, PanicAssertion>,
        MockChainTip,
    >::should_restart_sync(&above);

    assert!(
        !restart_below,
        "BehindTipHeightLimit should NOT restart sync"
    );
    assert!(
        !restart_above,
        "AboveLookaheadHeightLimit should NOT restart sync (GHSA-gvjc-3w7c-92jx fix)"
    );
}

/// Verifies fix for GHSA-rj6c-83wx-jxf2: `InvalidHeight` does not trigger
/// sync restart and carries `advertiser_addr` for peer scoring.
#[tokio::test]
async fn invalid_height_does_not_restart_sync() {
    let addr: PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
    let err = BlockDownloadVerifyError::InvalidHeight {
        hash: block::Hash::from([0xFF; 32]),
        advertiser_addr: Some(addr),
    };

    let restart = ChainSync::<
        MockService<zn::Request, zn::Response, PanicAssertion>,
        MockService<zs::Request, zs::Response, PanicAssertion>,
        MockService<zs::ReadRequest, zs::ReadResponse, PanicAssertion>,
        MockService<zebra_consensus::Request, block::Hash, PanicAssertion>,
        MockChainTip,
    >::should_restart_sync(&err);

    assert!(
        !restart,
        "InvalidHeight should NOT trigger sync restart (GHSA-rj6c-83wx-jxf2 fix)"
    );

    let has_addr = match &err {
        BlockDownloadVerifyError::InvalidHeight {
            advertiser_addr, ..
        } => advertiser_addr.is_some(),
        _ => false,
    };
    assert!(
        has_addr,
        "InvalidHeight should carry advertiser_addr for peer scoring"
    );
}

/// A concrete `ChainSync` type for calling associated functions in tests.
type TestChainSync = ChainSync<
    MockService<zn::Request, zn::Response, PanicAssertion>,
    MockService<zs::Request, zs::Response, PanicAssertion>,
    MockService<zs::ReadRequest, zs::ReadResponse, PanicAssertion>,
    MockService<zebra_consensus::Request, block::Hash, PanicAssertion>,
    MockChainTip,
>;

/// A UTXO lookup timeout and an invalid transaction both leave unrelated downloads running.
#[tokio::test]
async fn transparent_input_not_found_does_not_restart_sync() {
    let make_invalid = |tx_error| BlockDownloadVerifyError::Invalid {
        error: RouterError::Block {
            source: Box::new(VerifyBlockError::Transaction(tx_error)),
        },
        height: block::Height(3_427_629),
        hash: block::Hash::from([0xCC; 32]),
        advertiser_addr: None,
    };

    assert!(
        !TestChainSync::should_restart_sync(&make_invalid(
            TransactionError::TransparentInputNotFound
        )),
        "a transparent input UTXO lookup timeout should NOT trigger sync restart (#11168)"
    );

    assert!(
        !TestChainSync::should_restart_sync(&make_invalid(TransactionError::CoinbasePosition)),
        "an invalid transaction must not cancel unrelated downloads"
    );
}

/// A poisoned `FindBlocks` batch can make every block in a lookahead wave time out on its
/// UTXO lookup with nothing committing in between. The #11168 exemption is bounded so that
/// case still restarts the sync, while a single near-tip race followed by a commit does not.
#[tokio::test]
async fn utxo_lookup_timeouts_without_a_commit_restart_sync() {
    let (mut chain_sync, _misbehavior_rx) = new_chain_sync_with_misbehavior();
    let limit = chain_sync
        .full_verify_concurrency_limit
        .max(sync::MIN_UTXO_RACE_DROPS_BEFORE_RESTART);

    let utxo_timeout = |i: u8| {
        Err(BlockDownloadVerifyError::Invalid {
            error: RouterError::Block {
                source: Box::new(VerifyBlockError::Transaction(
                    TransactionError::TransparentInputNotFound,
                )),
            },
            height: block::Height(3_427_629 + u32::from(i)),
            hash: block::Hash::from([i; 32]),
            advertiser_addr: None,
        })
    };

    for i in 0..(limit - 1) {
        assert!(
            chain_sync
                .handle_block_response(utxo_timeout(i as u8))
                .is_ok(),
            "UTXO lookup timeouts below the lookahead limit should not restart sync (#11168)"
        );
        assert!(
            chain_sync
                .reobtain_hashes
                .contains(&block::Hash::from([i as u8; 32])),
            "a dropped UTXO-race block must be re-requested, not left for a tip walk"
        );
    }

    // A verified block means the race resolved, so the count starts over.
    assert!(chain_sync
        .handle_block_response(Ok((
            block::Height(3_427_628),
            block::Hash::from([0xAA; 32])
        )))
        .is_ok());
    assert!(chain_sync.handle_block_response(utxo_timeout(0)).is_ok());

    for i in 1..(limit - 1) {
        assert!(chain_sync
            .handle_block_response(utxo_timeout(i as u8))
            .is_ok());
    }
    assert!(
        chain_sync
            .handle_block_response(utxo_timeout(0xFF))
            .is_err(),
        "a full lookahead wave of UTXO lookup timeouts with no verified block should restart sync"
    );
}

/// Verifies fix for #11168: the short post-final-checkpoint verify timeout (#5125) does
/// not trigger a sync restart, but the tower-level `BLOCK_VERIFY_TIMEOUT` still does (#5709).
#[tokio::test]
async fn verify_timeout_elapsed_does_not_restart_sync() {
    let tokio_elapsed = tokio::time::timeout(Duration::ZERO, std::future::pending::<()>())
        .await
        .expect_err("timeout on a pending future always elapses");

    let make_error = |error: crate::BoxError| BlockDownloadVerifyError::ValidationRequestError {
        error,
        height: block::Height(3_428_007),
        hash: block::Hash::from([0xCD; 32]),
    };

    assert!(
        !TestChainSync::should_restart_sync(&make_error(tokio_elapsed.into())),
        "the post-final-checkpoint verify timeout should NOT trigger sync restart (#11168)"
    );

    assert!(
        TestChainSync::should_restart_sync(&make_error(
            tower::timeout::error::Elapsed::new().into()
        )),
        "the tower block verify timeout should still trigger sync restart (#5709)"
    );

    let (mut chain_sync, _misbehavior_rx) = new_chain_sync_with_misbehavior();
    let tokio_elapsed = tokio::time::timeout(Duration::ZERO, std::future::pending::<()>())
        .await
        .expect_err("timeout on a pending future always elapses");
    assert!(chain_sync
        .handle_block_response(Err(make_error(tokio_elapsed.into())))
        .is_ok());
    assert!(
        chain_sync
            .reobtain_hashes
            .contains(&block::Hash::from([0xCD; 32])),
        "a block dropped on the post-checkpoint verify timeout must be re-requested"
    );
}

/// Regression test for GHSA-g95h-hw6g-pvgv: a behind-tip drop scores the supplying peer and
/// re-queues the hash the syncer still needs.
///
/// Before this fix the drop was free for the peer: it satisfied the download request without
/// delivering a usable block, was never scored, and the hash waited for the next sync round.
#[tokio::test]
async fn behind_tip_height_limit_scores_peer_and_requeues_hash() {
    let (mut chain_sync, mut misbehavior_rx) = new_chain_sync_with_misbehavior();

    let advertiser: PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
    let hash = block::Hash::from([0xAB; 32]);

    chain_sync
        .handle_block_response(Err(BlockDownloadVerifyError::BehindTipHeightLimit {
            height: block::Height(1),
            hash,
            advertiser_addr: Some(advertiser),
        }))
        .expect("behind-tip drop is non-fatal and must not restart the syncer");

    assert_eq!(
        misbehavior_rx
            .try_recv()
            .ok()
            .map(|report| (report.addr, report.score)),
        Some((advertiser, 100)),
        "BehindTipHeightLimit must score the supplying peer at the ban threshold"
    );
    assert!(
        chain_sync.reobtain_hashes.contains(&hash),
        "BehindTipHeightLimit must re-queue the hash the syncer still needs"
    );
}

/// A behind-tip drop with no peer attribution still re-queues the hash.
///
/// Responses from isolated connections have no transient address, so there is nothing to score, but
/// the hash is still missing.
#[tokio::test]
async fn behind_tip_height_limit_without_advertiser_is_still_requeued() {
    let (mut chain_sync, mut misbehavior_rx) = new_chain_sync_with_misbehavior();

    let hash = block::Hash::from([0xBC; 32]);

    chain_sync
        .handle_block_response(Err(BlockDownloadVerifyError::BehindTipHeightLimit {
            height: block::Height(1),
            hash,
            advertiser_addr: None,
        }))
        .expect("behind-tip drop is non-fatal and must not restart the syncer");

    assert!(
        matches!(
            misbehavior_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "an unattributed behind-tip drop must not score any peer"
    );
    assert!(
        chain_sync.reobtain_hashes.contains(&hash),
        "an unattributed behind-tip drop must still re-queue the hash"
    );
}

/// The behind-tip re-request is bounded, so a peer cannot turn it into an unbounded download loop.
#[tokio::test]
async fn behind_tip_height_limit_requeue_is_bounded() {
    let (mut chain_sync, _misbehavior_rx) = new_chain_sync_with_misbehavior();

    let advertiser: PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
    let hash = block::Hash::from([0xCD; 32]);

    for attempt in 1..=sync::MAX_BLOCK_REOBTAIN_RETRIES + 1 {
        chain_sync
            .handle_block_response(Err(BlockDownloadVerifyError::BehindTipHeightLimit {
                height: block::Height(1),
                hash,
                advertiser_addr: Some(advertiser),
            }))
            .expect("behind-tip drop is non-fatal and must not restart the syncer");

        if attempt <= sync::MAX_BLOCK_REOBTAIN_RETRIES {
            assert!(
                chain_sync.reobtain_hashes.contains(&hash),
                "attempt {attempt} of {} must re-queue the hash",
                sync::MAX_BLOCK_REOBTAIN_RETRIES
            );
        } else {
            assert!(
                chain_sync.reobtain_hashes.is_empty(),
                "the hash must not be re-queued after {} retries",
                sync::MAX_BLOCK_REOBTAIN_RETRIES
            );
            assert!(
                !chain_sync.block_reobtain_retries.contains_key(&hash),
                "exhausted retry bookkeeping must be dropped"
            );
        }

        // Stand in for `reobtain_missing_blocks()`, which drains the set each sync round.
        chain_sync.reobtain_hashes.clear();
    }
}

/// Builds the error the syncer sees when a peer serves a body that doesn't match the
/// authorizing data commitment in the header it was served under.
///
/// The error is wrapped exactly as the production stack wraps it: the state's contextual
/// validation failure, unwrapped by `map_commit_error` into `VerifyBlockError::Commit`,
/// boxed by the router into `RouterError::Block`, and reported by the downloader as
/// `BlockDownloadVerifyError::Invalid`.
fn auth_commitment_mismatch_error(
    hash: block::Hash,
    advertiser_addr: Option<PeerSocketAddr>,
) -> BlockDownloadVerifyError {
    let commit_error = zs::CommitBlockError::ValidateContextError(Box::new(
        zs::ValidateContextError::InvalidBlockCommitment(
            block::CommitmentError::InvalidChainHistoryBlockTxAuthCommitment {
                expected: [1; 32],
                actual: [2; 32],
            },
        ),
    ));

    BlockDownloadVerifyError::Invalid {
        error: RouterError::Block {
            source: Box::new(VerifyBlockError::Commit(commit_error)),
        },
        height: block::Height(42),
        hash,
        advertiser_addr,
    }
}

/// A body that fails the authorizing data commitment check must score the serving peer and
/// re-request the hash, without cancelling the sync round.
///
/// The hash is still canonical and still needed — only the served body was forged — so
/// restarting the round would let one forgery idle the syncer for `SYNC_RESTART_DELAY`,
/// and dropping the hash would leave it to a later round to rediscover.
#[tokio::test]
async fn auth_commitment_mismatch_scores_peer_and_requeues_hash_without_restart() {
    let (mut chain_sync, mut misbehavior_rx) = new_chain_sync_with_misbehavior();

    let advertiser: PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
    let hash = block::Hash::from([0x1A; 32]);

    chain_sync
        .handle_block_response(Err(auth_commitment_mismatch_error(hash, Some(advertiser))))
        .expect("a forged body is non-fatal and must not restart the syncer");

    assert_eq!(
        misbehavior_rx
            .try_recv()
            .ok()
            .map(|report| (report.addr, report.score)),
        Some((advertiser, 100)),
        "the peer that served the forged body must be scored at the ban threshold"
    );
    assert!(
        chain_sync.reobtain_hashes.contains(&hash),
        "the hash is still canonical and still needed, so it must be re-requested"
    );
}

/// The state rejects an honest block queued behind a forged parent along with it. The peer
/// that served the child didn't forge anything, so it must not be scored, but the child's
/// hash is still wanted, so it is re-requested without cancelling the sync round.
///
/// Otherwise one forged body would get every peer that served one of its queued
/// descendants banned too.
#[tokio::test]
async fn descendant_of_auth_commitment_mismatch_is_requeued_without_scoring() {
    let (mut chain_sync, mut misbehavior_rx) = new_chain_sync_with_misbehavior();

    let advertiser: PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
    let hash = block::Hash::from([0x6F; 32]);

    let commit_error = zs::CommitBlockError::ValidateContextError(Box::new(
        zs::ValidateContextError::InvalidBlockCommitment(
            block::CommitmentError::InvalidChainHistoryBlockTxAuthCommitment {
                expected: [1; 32],
                actual: [2; 32],
            },
        )
        .for_descendant(block::Hash::from([0x1A; 32])),
    ));
    let error = BlockDownloadVerifyError::Invalid {
        error: RouterError::Block {
            source: Box::new(VerifyBlockError::Commit(commit_error)),
        },
        height: block::Height(43),
        hash,
        advertiser_addr: Some(advertiser),
    };

    chain_sync
        .handle_block_response(Err(error))
        .expect("a child of a forged body is non-fatal and must not restart the syncer");

    assert!(
        matches!(
            misbehavior_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "the peer that served an honest child of a forged body must not be scored"
    );
    assert!(
        chain_sync.reobtain_hashes.contains(&hash),
        "the child's hash is still wanted, so it must be re-requested"
    );
}

/// The forged-body re-request is bounded, so a peer cannot turn it into an unbounded
/// download loop.
///
/// Misbehaviour reports only reach the address book on `MISBEHAVIOR_FLUSH_INTERVAL`, so
/// the first re-request can briefly land on the same peer again. This bound is what makes
/// that acceptable.
#[tokio::test]
async fn auth_commitment_mismatch_requeue_is_bounded() {
    let (mut chain_sync, _misbehavior_rx) = new_chain_sync_with_misbehavior();

    let advertiser: PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
    let hash = block::Hash::from([0x2B; 32]);

    for attempt in 1..=sync::MAX_BLOCK_REOBTAIN_RETRIES + 1 {
        chain_sync
            .handle_block_response(Err(auth_commitment_mismatch_error(hash, Some(advertiser))))
            .expect("a forged body is non-fatal and must not restart the syncer");

        if attempt <= sync::MAX_BLOCK_REOBTAIN_RETRIES {
            assert!(
                chain_sync.reobtain_hashes.contains(&hash),
                "attempt {attempt} of {} must re-queue the hash",
                sync::MAX_BLOCK_REOBTAIN_RETRIES
            );
        } else {
            assert!(
                chain_sync.reobtain_hashes.is_empty(),
                "the hash must not be re-queued after {} retries",
                sync::MAX_BLOCK_REOBTAIN_RETRIES
            );
        }

        // Stand in for `reobtain_missing_blocks()`, which drains the set each sync round.
        chain_sync.reobtain_hashes.clear();
    }
}

/// A forged body served over an isolated connection has no address to score, but the hash
/// is still missing, so it is still re-requested.
#[tokio::test]
async fn auth_commitment_mismatch_without_advertiser_is_still_requeued() {
    let (mut chain_sync, mut misbehavior_rx) = new_chain_sync_with_misbehavior();

    let hash = block::Hash::from([0x3C; 32]);

    chain_sync
        .handle_block_response(Err(auth_commitment_mismatch_error(hash, None)))
        .expect("a forged body is non-fatal and must not restart the syncer");

    assert!(
        matches!(
            misbehavior_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "an unattributed forged body must not score any peer"
    );
    assert!(
        chain_sync.reobtain_hashes.contains(&hash),
        "an unattributed forged body must still re-queue the hash"
    );
}

/// Invalid headers and checkpoint queue rejections do not need a different body.
#[tokio::test]
async fn other_invalid_errors_are_dropped_without_restart_or_requeue() {
    use zebra_consensus::VerifyCheckpointError;

    let (mut chain_sync, _misbehavior_rx) = new_chain_sync_with_misbehavior();

    let advertiser: PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
    let hash = block::Hash::from([0x4D; 32]);

    let errors = [
        RouterError::Block {
            source: Box::new(VerifyBlockError::Block {
                source: zebra_consensus::BlockError::InvalidDifficulty(Height(42), hash),
            }),
        },
        RouterError::Checkpoint {
            source: Box::new(VerifyCheckpointError::QueuedLimit),
        },
        RouterError::Checkpoint {
            source: Box::new(VerifyCheckpointError::Finished),
        },
    ];

    for error in errors {
        let err = BlockDownloadVerifyError::Invalid {
            error,
            height: block::Height(42),
            hash,
            advertiser_addr: Some(advertiser),
        };

        assert!(
            chain_sync.handle_block_response(Err(err)).is_ok(),
            "a block-specific failure must not cancel unrelated downloads"
        );
        assert!(
            !chain_sync.reobtain_hashes.contains(&hash),
            "a block the verifier rejected must not be re-requested"
        );
    }
}

/// Body validation errors share the bounded retry path in both verifier routes.
#[tokio::test]
async fn body_validation_failures_have_bounded_retries() {
    use zebra_consensus::{BlockError, VerifyCheckpointError};

    let (mut chain_sync, _misbehavior_rx) = new_chain_sync_with_misbehavior();
    let hash = block::Hash::from([0x5D; 32]);
    let errors = [
        BlockError::NoTransactions,
        BlockError::BadMerkleRoot {
            actual: block::merkle::Root([0; 32]),
            expected: block::merkle::Root([1; 32]),
        },
        BlockError::DuplicateTransaction,
        BlockError::WrongTransactionConsensusBranchId,
        BlockError::MissingHeight(hash),
        BlockError::MaxHeight(Height::MAX, hash, Height(1)),
        BlockError::Transaction(TransactionError::Subsidy(SubsidyError::NoCoinbase)),
        BlockError::Transaction(TransactionError::Groth16("invalid proof".into())),
        BlockError::Transaction(TransactionError::MalformedGroth16(
            "invalid encoding".into(),
        )),
        BlockError::Transaction(TransactionError::SaplingVerificationFailed),
        BlockError::Transaction(TransactionError::Halo2VerificationFailed),
        BlockError::Transaction(TransactionError::OrchardProofSize),
        BlockError::Transaction(TransactionError::IronwoodProofSize),
        BlockError::Transaction(TransactionError::Other(
            "transaction has Orchard actions (temporarily disabled)".into(),
        )),
    ];
    for checkpoint in [false, true] {
        for source in &errors {
            for attempt in 1..=sync::MAX_BLOCK_REOBTAIN_RETRIES + 1 {
                let error = VerifyBlockError::from(source.clone());
                let error = if checkpoint {
                    RouterError::Checkpoint {
                        source: Box::new(VerifyCheckpointError::VerifyBlock(error)),
                    }
                } else {
                    RouterError::Block {
                        source: Box::new(error),
                    }
                };
                chain_sync
                    .handle_block_response(Err(BlockDownloadVerifyError::Invalid {
                        error,
                        height: Height(42),
                        hash,
                        advertiser_addr: None,
                    }))
                    .unwrap();
                assert_eq!(
                    chain_sync.reobtain_hashes.shift_remove(&hash),
                    attempt <= sync::MAX_BLOCK_REOBTAIN_RETRIES,
                    "retry {attempt} for {source:?} with checkpoint={checkpoint}"
                );
            }
        }
    }
}

/// Canonical headers do not make a peer's transaction body trustworthy.
#[tokio::test(start_paused = true)]
async fn checkpoint_modified_body_is_retried() {
    canonical_header_modified_body_is_retried(true).await;
}

/// Full verification must recover the same hash after a body-only failure.
#[tokio::test(start_paused = true)]
async fn semantic_modified_body_is_retried() {
    canonical_header_modified_body_is_retried(false).await;
}

async fn canonical_header_modified_body_is_retried(checkpoint_sync: bool) {
    use tower::ServiceExt;
    use zebra_chain::parameters::Network;
    use zebra_consensus::{BlockError, VerifyCheckpointError};

    let _init_guard = zebra_test::init();
    let honest: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let hash = honest.hash();
    let height = honest.coinbase_height().unwrap();
    let parent_hash = honest.header.previous_block_hash;
    let parent_height = (height - 1).unwrap();
    let mut forged = honest.as_ref().clone();
    assert!(forged.transactions.len() > 1);
    forged.transactions.pop();
    let forged = Arc::new(forged);
    assert_eq!(
        forged.hash(),
        hash,
        "the peer preserves the canonical header"
    );
    assert_ne!(
        forged
            .transactions
            .iter()
            .map(|tx| tx.hash())
            .collect::<block::merkle::Root>(),
        honest.header.merkle_root
    );

    let state = tower::service_fn(move |request| {
        futures::future::ok::<_, crate::BoxError>(match request {
            zs::Request::Tip => zs::Response::Tip(Some((parent_height, parent_hash))),
            zs::Request::BestChainBlockHash(_) => zs::Response::BlockHash(None),
            zs::Request::KnownBlock(_) => zs::Response::KnownBlock(None),
            _ => unreachable!("a forged body must fail before transaction or state verification"),
        })
    });
    let (router, _mempool, tasks, max_checkpoint_height) = zebra_consensus::router::init_test(
        ConsensusConfig { checkpoint_sync },
        &Network::Mainnet,
        state,
    )
    .await;
    assert_eq!(height <= max_checkpoint_height, checkpoint_sync);
    let mut network = MockService::build().for_unit_tests();
    let (tip, tip_sender) = MockChainTip::new();
    tip_sender.send_best_tip_height(parent_height);
    let (misbehavior_tx, mut misbehavior_rx) = tokio::sync::mpsc::channel(1);
    let (mut chain_sync, _status) = ChainSync::new(
        &ZebradConfig::default(),
        max_checkpoint_height,
        network.clone(),
        router.map_err(crate::BoxError::from),
        state,
        MockService::build().for_unit_tests(),
        tip,
        misbehavior_tx,
    );
    chain_sync
        .downloads
        .download_and_verify(hash)
        .await
        .unwrap();
    let advertiser: PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
    network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            forged,
            Some(advertiser),
        ))]));
    let error = chain_sync.downloads.next().await.unwrap().unwrap_err();
    let BlockDownloadVerifyError::Invalid {
        error: ref router_error,
        ..
    } = error
    else {
        panic!("expected a body validation failure, got {error:?}");
    };
    let block_error = match router_error {
        RouterError::Block { source } => source.as_ref(),
        RouterError::Checkpoint { source } => match source.as_ref() {
            VerifyCheckpointError::VerifyBlock(error) => error,
            other => panic!("unexpected checkpoint error: {other:?}"),
        },
    };
    assert!(matches!(
        block_error,
        VerifyBlockError::Block {
            source: BlockError::BadMerkleRoot { .. }
        }
    ));
    chain_sync.handle_block_response(Err(error)).unwrap();
    assert!(
        chain_sync.reobtain_hashes.contains(&hash),
        "a bad body must not discard its still-needed canonical block hash"
    );
    assert_eq!(misbehavior_rx.try_recv().unwrap().score, 100);
    chain_sync.reobtain_missing_blocks().await;
    network
        .expect_request(zn::Request::BlockDownload {
            hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((honest, None))]));
    chain_sync.downloads.cancel_all();
    tasks.state_checkpoint_verify_handle.abort();
}

/// An honest body whose authorizing data matches its header must not be classified as a
/// forgery, whatever else went wrong with it.
///
/// This is the guard against over-banning and over-re-requesting: the errors a
/// behind-tip or far-ahead honest body produces, and the other contextual validation
/// failures, must all stay outside the new arm.
#[tokio::test]
async fn honest_bodies_are_not_classified_as_forgeries() {
    let advertiser: PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();

    // A duplicate commit is the benign case the syncer already tolerated.
    let duplicate = zs::CommitBlockError::Duplicate {
        hash_or_height: None,
        location: zs::KnownBlock::BestChain,
    };
    assert!(
        !duplicate.is_auth_commitment_mismatch(),
        "a duplicate block is not a forged body"
    );

    // A sibling commitment failure, which doesn't prove the *serving* peer forged
    // anything: the chain history root is the node's own view of the chain, so a
    // mismatch there can be a local disagreement rather than a forged body.
    let other_context = zs::CommitBlockError::ValidateContextError(Box::new(
        zs::ValidateContextError::InvalidBlockCommitment(
            block::CommitmentError::InvalidChainHistoryRoot {
                expected: [1; 32],
                actual: [2; 32],
            },
        ),
    ));
    assert!(
        !other_context.is_auth_commitment_mismatch(),
        "an unrelated commitment failure is not a forged body"
    );
    assert_eq!(
        other_context.misbehavior_score(),
        0,
        "an unrelated contextual failure must not score the serving peer"
    );

    // The height-limit errors an honest behind-tip or far-ahead body produces are not
    // `Invalid`, so they never reach the new arm.
    let (mut chain_sync, _misbehavior_rx) = new_chain_sync_with_misbehavior();
    let above_hash = block::Hash::from([0x5E; 32]);
    chain_sync
        .handle_block_response(Err(BlockDownloadVerifyError::AboveLookaheadHeightLimit {
            height: block::Height(60_000),
            hash: above_hash,
        }))
        .expect("a far-ahead block is non-fatal and must not restart the syncer");
    assert!(
        chain_sync.reobtain_hashes.is_empty(),
        "a far-ahead honest body must not be re-requested as a forgery"
    );

    let behind_hash = block::Hash::from([0x6F; 32]);
    chain_sync
        .handle_block_response(Err(BlockDownloadVerifyError::BehindTipHeightLimit {
            height: block::Height(1),
            hash: behind_hash,
            advertiser_addr: Some(advertiser),
        }))
        .expect("a behind-tip block is non-fatal and must not restart the syncer");
    assert!(
        chain_sync.reobtain_hashes.contains(&behind_hash),
        "the behind-tip re-request is unchanged by the forged-body arm"
    );
}

/// Every failed block download is re-requested without restarting the sync (#5709).
///
/// Both cases share one bounded re-queue with the behind-tip fix.
#[tokio::test]
async fn download_failures_are_requeued_without_restart() {
    let (mut chain_sync, mut misbehavior_rx) = new_chain_sync_with_misbehavior();

    let missing_hash = block::Hash::from([0xDE; 32]);
    chain_sync
        .handle_block_response(Err(BlockDownloadVerifyError::DownloadFailed {
            error: std::io::Error::new(std::io::ErrorKind::NotFound, "NotFoundResponse").into(),
            hash: missing_hash,
        }))
        .expect("a missing block is non-fatal and must not restart the syncer");

    assert!(
        chain_sync.reobtain_hashes.contains(&missing_hash),
        "a block no peer delivered must be re-queued (#5709)"
    );

    // Other download failures do not restart the sync, so they need the same retry.
    let failed_hash = block::Hash::from([0xEF; 32]);
    let restart = chain_sync
        .handle_block_response(Err(BlockDownloadVerifyError::DownloadFailed {
            error: std::io::Error::new(std::io::ErrorKind::ConnectionReset, "connection reset")
                .into(),
            hash: failed_hash,
        }))
        .is_err();
    assert!(
        !restart,
        "a failed download must not cancel unrelated downloads"
    );

    assert!(
        chain_sync.reobtain_hashes.contains(&failed_hash),
        "a download that failed for another reason must be re-queued"
    );

    // Neither case attributes misbehavior: the download never produced a block to judge.
    assert!(
        matches!(
            misbehavior_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "a failed download must not score any peer"
    );
}

/// A download round that expires is re-requested like a missing block, instead of restarting sync.
///
/// When one hedged request fails with `NotFound`, it waits for its sibling, so the round deadline
/// can drop that `NotFound`.
#[tokio::test]
async fn download_round_timeout_is_requeued_without_restart() {
    let (mut chain_sync, mut misbehavior_rx) = new_chain_sync_with_misbehavior();

    let timed_out_hash = block::Hash::from([0xA5; 32]);
    chain_sync
        .handle_block_response(Err(BlockDownloadVerifyError::DownloadTimedOut {
            hash: timed_out_hash,
        }))
        .expect("a download round timeout is non-fatal and must not restart the syncer");

    assert!(
        chain_sync.reobtain_hashes.contains(&timed_out_hash),
        "a block no peer delivered within the download round must be re-queued"
    );

    assert!(
        matches!(
            misbehavior_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "a timed out download must not score any peer"
    );
}

/// A block that arrives while its hash is queued for re-download is not re-requested.
#[tokio::test]
async fn verified_block_is_removed_from_reobtain_queue() {
    let (mut chain_sync, _misbehavior_rx) = new_chain_sync_with_misbehavior();

    let hash = block::Hash::from([0xC3; 32]);
    chain_sync.reobtain_hashes.insert(hash);
    chain_sync.block_reobtain_retries.insert(hash, 1);

    chain_sync
        .handle_block_response(Ok((Height(1), hash)))
        .expect("a verified block is not an error");

    assert!(
        !chain_sync.reobtain_hashes.contains(&hash),
        "a verified block must not be re-requested"
    );
    assert!(!chain_sync.block_reobtain_retries.contains_key(&hash));
}

/// Records counters, so tests can check what the syncer counts.
#[derive(Default)]
struct CounterRecorder(std::sync::Mutex<HashMap<metrics::Key, Arc<std::sync::atomic::AtomicU64>>>);

impl CounterRecorder {
    /// Returns the count of the counter with `name` and `labels`, in the order they're recorded.
    fn count(&self, name: &'static str, labels: &[(&'static str, &'static str)]) -> u64 {
        let labels: Vec<_> = labels
            .iter()
            .map(|&(key, value)| metrics::Label::new(key, value))
            .collect();

        self.0
            .lock()
            .expect("test recorder mutex is unpoisoned")
            .get(&metrics::Key::from_parts(name, labels))
            .map_or(0, |count| count.load(std::sync::atomic::Ordering::Relaxed))
    }
}

impl metrics::Recorder for CounterRecorder {
    fn describe_counter(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }

    fn describe_gauge(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }

    fn describe_histogram(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }

    fn register_counter(&self, key: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Counter {
        metrics::Counter::from_arc(
            self.0
                .lock()
                .expect("test recorder mutex is unpoisoned")
                .entry(key.clone())
                .or_default()
                .clone(),
        )
    }

    fn register_gauge(&self, _: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Gauge {
        metrics::Gauge::noop()
    }

    fn register_histogram(
        &self,
        _: &metrics::Key,
        _: &metrics::Metadata<'_>,
    ) -> metrics::Histogram {
        metrics::Histogram::noop()
    }
}

/// Block errors are counted by bounded kind, and restarts track whether the state tip advanced.
#[tokio::test]
async fn sync_diagnostics_count_block_errors_and_restarts() {
    let (mut chain_sync, _misbehavior_rx) = new_chain_sync_with_misbehavior();
    let diagnostics = chain_sync.diagnostics.subscribe();
    let hash = block::Hash::from([0xC1; 32]);
    let recorder = CounterRecorder::default();

    metrics::with_local_recorder(&recorder, || {
        for error in [
            BlockDownloadVerifyError::DownloadTimedOut { hash },
            BlockDownloadVerifyError::DownloadTimedOut { hash },
            BlockDownloadVerifyError::CancelledDuringDownload { hash },
        ] {
            let _ = chain_sync.handle_block_response(Err(error));
        }

        chain_sync.record_restart(RestartReason::BlockError, false);
        chain_sync.record_restart(RestartReason::ExhaustedTips, false);
        assert_eq!(diagnostics.borrow().restarts_without_progress, 2);

        chain_sync.record_restart(RestartReason::ExhaustedTips, true);
    });

    let error_count = |kind| recorder.count("sync.block.error.count", &[("kind", kind)]);
    assert_eq!(error_count("download_timed_out"), 2);
    assert_eq!(error_count("cancelled"), 1);

    let restart_count = |reason, made_progress| {
        recorder.count(
            "sync.restart.count",
            &[("reason", reason), ("made_progress", made_progress)],
        )
    };
    assert_eq!(restart_count("block_error", "false"), 1);
    assert_eq!(restart_count("exhausted_tips", "false"), 1);
    assert_eq!(restart_count("exhausted_tips", "true"), 1);

    let diagnostics = diagnostics.borrow();
    assert_eq!(diagnostics.last_restart, Some(RestartReason::ExhaustedTips));
    assert_eq!(diagnostics.restarts_without_progress, 0);
}

/// Re-downloads retain the queue if dispatch is cancelled or network readiness times out.
#[tokio::test(start_paused = true)]
async fn reobtain_keeps_blocks_when_cancelled_or_network_readiness_times_out() {
    use tower::limit::ConcurrencyLimit;

    let _init_guard = zebra_test::init();

    let config = ZebradConfig {
        consensus: ConsensusConfig::default(),
        state: StateConfig::ephemeral(),
        ..Default::default()
    };
    let (mock_chain_tip, _mock_chain_tip_sender) = MockChainTip::new();
    let (misbehavior_tx, _misbehavior_rx) = tokio::sync::mpsc::channel(4);
    // A peer set with no ready peers: a zero-permit limit never becomes ready.
    let never_ready_network = ConcurrencyLimit::new(MockService::build().for_unit_tests(), 0);
    let (mut chain_sync, _sync_status) = ChainSync::new(
        &config,
        Height(0),
        never_ready_network,
        MockService::build().for_unit_tests(),
        MockService::build().for_unit_tests(),
        MockService::build().for_unit_tests(),
        mock_chain_tip,
        misbehavior_tx,
    );

    let missing = [block::Hash::from([0xB1; 32]), block::Hash::from([0xB2; 32])];
    chain_sync.reobtain_hashes.extend(missing);

    assert!(
        tokio::time::timeout(
            sync::BLOCK_DOWNLOAD_ROUND_TIMEOUT / 2,
            chain_sync.reobtain_missing_blocks(),
        )
        .await
        .is_err(),
        "the dispatch must still be waiting for network capacity"
    );
    assert_eq!(
        chain_sync
            .reobtain_hashes
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        missing,
        "cancelling dispatch must preserve all queued retries"
    );

    let recorder = CounterRecorder::default();
    let metrics_guard = metrics::set_default_local_recorder(&recorder);
    let started = tokio::time::Instant::now();
    chain_sync.reobtain_missing_blocks().await;
    drop(metrics_guard);

    assert_eq!(
        chain_sync
            .reobtain_hashes
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        missing,
        "blocks that couldn't be re-requested must stay queued for the next sync round"
    );
    assert_eq!(
        tokio::time::Instant::now() - started,
        sync::BLOCK_DOWNLOAD_ROUND_TIMEOUT,
        "only the first block waits out its download deadline"
    );
    assert_eq!(
        recorder.count(
            "sync.block.error.count",
            &[("kind", "network_ready_timeout")]
        ),
        1,
        "a block that never got network capacity is counted once"
    );
}

/// Blocks that fail to queue while obtaining or extending tips restart the sync as block errors,
/// while other tip failures keep their stage.
#[test]
fn tip_block_submission_failures_restart_as_block_errors() {
    let block = TestChainSync::tips_restart_error(
        RestartReason::ExtendTipsError,
        BlockDownloadVerifyError::Timeout.into(),
    );
    assert_eq!(block.reason, RestartReason::BlockError);
    assert_eq!(block.kind, Some("network_ready_timeout"));

    let other = TestChainSync::tips_restart_error(
        RestartReason::ObtainTipsError,
        color_eyre::eyre::eyre!("queued download of hash behind our chain tip"),
    );
    assert_eq!(other.reason, RestartReason::ObtainTipsError);
    assert_eq!(other.kind, None);
}

/// A genesis block that can't get network capacity is counted as a block error.
#[tokio::test(start_paused = true)]
async fn genesis_submission_failures_are_counted() {
    let _init_guard = zebra_test::init();

    // A peer set with no ready peers: a zero-permit limit never becomes ready.
    let (mut chain_sync, _misbehavior_rx) = new_chain_sync_with(
        peer_set_with_permits(0),
        MockService::build().for_unit_tests(),
    );

    let recorder = CounterRecorder::default();
    let metrics_guard = metrics::set_default_local_recorder(&recorder);
    assert!(chain_sync.request_genesis_once().await.is_err());
    drop(metrics_guard);

    assert_eq!(
        recorder.count(
            "sync.block.error.count",
            &[("kind", "network_ready_timeout")]
        ),
        1,
    );
}

/// A genesis retry that finds the block still queued from a timed-out attempt doesn't count a
/// block error, because the syncer ignores it and keeps waiting for that download.
#[tokio::test(start_paused = true)]
async fn genesis_retries_while_the_block_is_queued_are_not_counted() {
    let _init_guard = zebra_test::init();

    // The genesis download gets the only permit, and the mock peer set never answers it.
    let (mut chain_sync, _misbehavior_rx) = new_chain_sync_with(
        peer_set_with_permits(1),
        MockService::build().for_unit_tests(),
    );

    let recorder = CounterRecorder::default();
    let metrics_guard = metrics::set_default_local_recorder(&recorder);

    for attempt in ["first", "retry"] {
        let genesis =
            tokio::time::timeout(Duration::from_secs(1), chain_sync.request_genesis_once());
        assert!(
            genesis.await.is_err(),
            "the {attempt} attempt never downloads"
        );
    }
    drop(metrics_guard);

    assert_eq!(
        recorder.count("sync.block.error.count", &[("kind", "duplicate_queued")]),
        0,
    );
}

/// A wait for network capacity that the obtain tips or genesis timeout cancels, because both are
/// shorter than the readiness deadline, is counted once as a network readiness timeout.
#[tokio::test(start_paused = true)]
async fn network_capacity_waits_cancelled_by_a_timeout_are_counted_once() {
    use indexmap::IndexSet;

    let _init_guard = zebra_test::init();

    // A peer set with no ready peers: a zero-permit limit never becomes ready.
    let (mut chain_sync, _misbehavior_rx) = new_chain_sync_with(
        peer_set_with_permits(0),
        MockService::build().for_unit_tests(),
    );

    let recorder = CounterRecorder::default();
    let metrics_guard = metrics::set_default_local_recorder(&recorder);

    let batch = IndexSet::from([block::Hash::from([0xE1; 32])]);
    let tips = tokio::time::timeout(Duration::from_secs(1), chain_sync.request_blocks(batch));
    assert!(tips.await.is_err(), "the block waits for network capacity");
    assert!(chain_sync.count_cancelled_network_wait());

    let genesis = tokio::time::timeout(Duration::from_secs(1), chain_sync.request_genesis_once());
    assert!(
        genesis.await.is_err(),
        "the genesis block waits for network capacity"
    );
    assert!(chain_sync.count_cancelled_network_wait());

    assert!(!chain_sync.count_cancelled_network_wait());
    drop(metrics_guard);

    assert_eq!(
        recorder.count(
            "sync.block.error.count",
            &[("kind", "network_ready_timeout")]
        ),
        2,
    );
}

/// A download that failed before a restart is counted when the restart drops the downloads the
/// syncer hasn't read yet.
#[tokio::test(start_paused = true)]
async fn finished_block_errors_are_counted_before_a_restart_drops_them() {
    use indexmap::IndexSet;

    let _init_guard = zebra_test::init();

    let mut peer_set = MockService::build().for_unit_tests();
    let mut block_verifier_router = MockService::build().for_unit_tests();
    let (mut chain_sync, _misbehavior_rx) =
        new_chain_sync_with(peer_set.clone(), block_verifier_router.clone());

    let block1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("test vector is a valid block");
    let block1_hash = block1.hash();

    chain_sync
        .request_blocks(IndexSet::from([block1_hash]))
        .await
        .expect("the peer set has capacity");
    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block1_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block1.clone(),
            None,
        ))]));
    block_verifier_router
        .expect_request(zebra_consensus::Request::Commit(block1))
        .await
        .respond(Err(zebra_consensus::BoxError::from(
            "synthetic test verifier error",
        )));
    tokio::time::sleep(Duration::from_secs(1)).await;

    let recorder = CounterRecorder::default();
    let metrics_guard = metrics::set_default_local_recorder(&recorder);
    chain_sync.count_finished_block_errors();
    drop(metrics_guard);

    assert_eq!(
        recorder.count("sync.block.error.count", &[("kind", "validation_request")]),
        1,
    );
    assert_eq!(chain_sync.downloads.in_flight(), 0);
}

/// A timeout that cancels a queued block after it got network capacity isn't counted as a wait for
/// network capacity, even when it fires before the queue call returns.
#[tokio::test(start_paused = true)]
async fn timeouts_after_network_capacity_arrives_are_not_counted_as_network_waits() {
    let _init_guard = zebra_test::init();

    let (mut chain_sync, _misbehavior_rx) = new_chain_sync_with(
        MockService::build().for_unit_tests(),
        MockService::build().for_unit_tests(),
    );

    // The peer set is ready at once, so this poll sends the download and then yields.
    let mut queueing = Box::pin(chain_sync.queue_block(block::Hash::from([0xE3; 32])));
    assert!(futures::poll!(&mut queueing).is_pending());
    drop(queueing);

    assert!(!chain_sync.count_cancelled_network_wait());
}

/// An obtain tips or sync step timeout that cancels a block's wait for network capacity restarts
/// the sync as that block's network readiness timeout, and counts it. Otherwise the timeout keeps
/// its own restart reason.
#[tokio::test(start_paused = true)]
async fn stage_timeouts_that_cancel_a_network_wait_restart_as_block_errors() {
    use indexmap::IndexSet;

    let _init_guard = zebra_test::init();

    // A peer set with no ready peers: a zero-permit limit never becomes ready.
    let (mut chain_sync, _misbehavior_rx) = new_chain_sync_with(
        peer_set_with_permits(0),
        MockService::build().for_unit_tests(),
    );

    let recorder = CounterRecorder::default();
    let metrics_guard = metrics::set_default_local_recorder(&recorder);

    let batch = IndexSet::from([block::Hash::from([0xE2; 32])]);
    let elapsed = tokio::time::timeout(Duration::from_secs(1), chain_sync.request_blocks(batch))
        .await
        .expect_err("the block waits for network capacity");
    let waiting = chain_sync.stage_timeout_error(RestartReason::SyncStepTimeout, elapsed);
    assert_eq!(waiting.reason, RestartReason::BlockError);
    assert_eq!(waiting.kind, Some("network_ready_timeout"));

    let elapsed = tokio::time::timeout(Duration::from_secs(1), std::future::pending::<()>())
        .await
        .expect_err("the future never finishes");
    let idle = chain_sync.stage_timeout_error(RestartReason::SyncStepTimeout, elapsed);
    assert_eq!(idle.reason, RestartReason::SyncStepTimeout);
    assert_eq!(idle.kind, None);
    drop(metrics_guard);

    assert_eq!(
        recorder.count(
            "sync.block.error.count",
            &[("kind", "network_ready_timeout")]
        ),
        1,
    );
}

/// A genesis attempt that times out while its queued block downloads is a genesis timeout, which
/// isn't counted, and one that times out waiting for network capacity is a network readiness
/// timeout.
#[tokio::test(start_paused = true)]
async fn genesis_timeouts_are_told_apart_from_network_readiness_timeouts() {
    let _init_guard = zebra_test::init();

    let recorder = CounterRecorder::default();
    let metrics_guard = metrics::set_default_local_recorder(&recorder);

    // The genesis download gets the only permit, and the mock peer set never answers it.
    let (mut downloading, _misbehavior_rx) = new_chain_sync_with(
        peer_set_with_permits(1),
        MockService::build().for_unit_tests(),
    );
    let genesis = tokio::time::timeout(Duration::from_secs(1), downloading.request_genesis_once());
    assert!(genesis.await.is_err(), "the genesis block never downloads");
    assert!(matches!(
        downloading.genesis_timeout(),
        BlockDownloadVerifyError::GenesisTimeout
    ));

    // A peer set with no ready peers: a zero-permit limit never becomes ready.
    let (mut waiting, _misbehavior_rx) = new_chain_sync_with(
        peer_set_with_permits(0),
        MockService::build().for_unit_tests(),
    );
    let genesis = tokio::time::timeout(Duration::from_secs(1), waiting.request_genesis_once());
    assert!(
        genesis.await.is_err(),
        "the genesis block waits for network capacity"
    );
    assert!(matches!(
        waiting.genesis_timeout(),
        BlockDownloadVerifyError::Timeout
    ));
    drop(metrics_guard);

    assert_eq!(
        recorder.count("sync.block.error.count", &[("kind", "genesis_timeout")]),
        0,
    );
    assert_eq!(
        recorder.count(
            "sync.block.error.count",
            &[("kind", "network_ready_timeout")]
        ),
        1,
    );
}

/// While a batch waits for network capacity, the published download state includes the blocks
/// queued earlier in that batch.
#[tokio::test(start_paused = true)]
async fn diagnostics_include_blocks_queued_before_a_readiness_wait() {
    use indexmap::IndexSet;

    let _init_guard = zebra_test::init();

    // The first download holds the only permit, so the second waits for network capacity.
    let (mut chain_sync, _misbehavior_rx) = new_chain_sync_with(
        peer_set_with_permits(1),
        MockService::build().for_unit_tests(),
    );
    let diagnostics = chain_sync.diagnostics.subscribe();

    let batch = IndexSet::from([block::Hash::from([0xD1; 32]), block::Hash::from([0xD2; 32])]);
    let waiting = tokio::time::timeout(Duration::from_secs(1), chain_sync.request_blocks(batch));
    assert!(
        waiting.await.is_err(),
        "the second block waits for network capacity"
    );

    let diagnostics = diagnostics.borrow();
    assert_eq!(diagnostics.in_flight, 1);
    assert!(diagnostics
        .oldest_in_flight_age(std::time::Instant::now())
        .is_some());
}

/// A genesis block download that never finishes is published as in flight, so the progress task
/// reports its age before the node has a chain tip.
#[tokio::test(start_paused = true)]
async fn diagnostics_include_a_genesis_block_that_never_downloads() {
    let _init_guard = zebra_test::init();

    // The genesis download gets the only permit, and the mock peer set never answers it.
    let (mut chain_sync, _misbehavior_rx) = new_chain_sync_with(
        peer_set_with_permits(1),
        MockService::build().for_unit_tests(),
    );
    let diagnostics = chain_sync.diagnostics.subscribe();

    let waiting = tokio::time::timeout(Duration::from_secs(1), chain_sync.request_genesis_once());
    assert!(waiting.await.is_err(), "the genesis block never downloads");

    let diagnostics = diagnostics.borrow();
    assert_eq!(diagnostics.in_flight, 1);
    assert!(diagnostics
        .oldest_in_flight_age(std::time::Instant::now())
        .is_some());
}

/// A download that finished isn't reported as in flight while the syncer hasn't read its result,
/// which a node at the tip can leave unread for several sync rounds after queueing the one block
/// it was missing.
#[tokio::test(start_paused = true)]
async fn diagnostics_leave_out_finished_downloads_the_syncer_has_not_read() {
    use indexmap::IndexSet;

    let _init_guard = zebra_test::init();

    let mut peer_set = MockService::build().for_unit_tests();
    let mut block_verifier_router = MockService::build().for_unit_tests();
    let (mut chain_sync, _misbehavior_rx) =
        new_chain_sync_with(peer_set.clone(), block_verifier_router.clone());
    let diagnostics = chain_sync.diagnostics.subscribe();

    let block1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("test vector is a valid block");
    let block1_hash = block1.hash();

    chain_sync
        .request_blocks(IndexSet::from([block1_hash]))
        .await
        .expect("the peer set has capacity");
    assert!(diagnostics
        .borrow()
        .oldest_in_flight_age(std::time::Instant::now())
        .is_some());

    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block1_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block1.clone(),
            None,
        ))]));
    block_verifier_router
        .expect_request(zebra_consensus::Request::Commit(block1))
        .await
        .respond(block1_hash);
    tokio::time::sleep(Duration::from_secs(1)).await;

    assert!(diagnostics
        .borrow()
        .oldest_in_flight_age(std::time::Instant::now())
        .is_none());
}

/// When the oldest download finishes before the syncer reads its result, the age is that of the
/// next download that is still running, even though the syncer hasn't republished since.
#[tokio::test(start_paused = true)]
async fn diagnostics_report_the_next_running_download_when_the_oldest_finishes() {
    use indexmap::IndexSet;

    let _init_guard = zebra_test::init();

    let mut peer_set = MockService::build().for_unit_tests();
    let mut block_verifier_router = MockService::build().for_unit_tests();
    let (mut chain_sync, _misbehavior_rx) =
        new_chain_sync_with(peer_set.clone(), block_verifier_router.clone());
    let diagnostics = chain_sync.diagnostics.subscribe();

    let block1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("test vector is a valid block");
    let block1_hash = block1.hash();
    // The mock peer set never answers this download.
    let stuck_hash = block::Hash::from([0xD4; 32]);

    chain_sync
        .request_blocks(IndexSet::from([block1_hash]))
        .await
        .expect("the peer set has capacity");
    let stuck_queued_after = std::time::Instant::now();
    chain_sync
        .request_blocks(IndexSet::from([stuck_hash]))
        .await
        .expect("the peer set has capacity");

    peer_set
        .expect_request(zn::Request::BlockDownload {
            hash: block1_hash,
            advertiser: None,
        })
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block1.clone(),
            None,
        ))]));
    block_verifier_router
        .expect_request(zebra_consensus::Request::Commit(block1))
        .await
        .respond(block1_hash);
    tokio::time::sleep(Duration::from_secs(1)).await;

    let now = std::time::Instant::now();
    let age = diagnostics
        .borrow()
        .oldest_in_flight_age(now)
        .expect("the stuck download is still running");
    assert!(
        age <= now.saturating_duration_since(stuck_queued_after),
        "the age is measured from the stuck download, not the finished one"
    );
}

/// Verifies fix for GHSA-qhr3-cvch-5fh2: a block that lands above the lookahead
/// height limit must NOT score the peer that served it, while consensus-invalid
/// blocks still must.
///
/// Far-ahead hashes from a malicious `FindBlocks` response carry no peer attribution,
/// so the follow-up `BlockDownload` request is routed to an independently chosen,
/// honest peer. That serving peer did not choose the height, so scoring this path bans
/// honest peers at the attacker's direction.
#[tokio::test]
async fn far_ahead_block_does_not_produce_misbehavior_score() {
    let (mut chain_sync, mut misbehavior_rx) = new_chain_sync_with_misbehavior();

    let peer: PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();

    // Positive control, and proof the channel plumbing works: a consensus-invalid
    // block with a non-zero score must still be reported.
    let router_error = RouterError::Block {
        source: Box::new(VerifyBlockError::Subsidy(SubsidyError::NoCoinbase)),
    };
    let expected_score = router_error.misbehavior_score();
    assert_ne!(
        expected_score, 0,
        "this control needs an error with a non-zero misbehavior score"
    );

    let _ = chain_sync.handle_block_response(Err(BlockDownloadVerifyError::Invalid {
        error: router_error,
        height: block::Height(60_000),
        hash: block::Hash::from([0xAB; 32]),
        advertiser_addr: Some(peer),
    }));
    let report = misbehavior_rx
        .try_recv()
        .expect("consensus-invalid blocks must still score the serving peer");
    assert_eq!(
        (report.addr, report.score),
        (peer, expected_score),
        "consensus-invalid blocks must still score the serving peer"
    );
    assert!(
        report
            .reason
            .starts_with("block failed consensus validation: ")
            && report.reason.contains("NoCoinbase"),
        "the report must explain why the peer was scored, got: {:?}",
        report.reason,
    );

    // The fix: an above-lookahead block must not produce any misbehavior score.
    let _ = chain_sync.handle_block_response(Err(
        BlockDownloadVerifyError::AboveLookaheadHeightLimit {
            height: block::Height(60_000),
            hash: block::Hash::from([0xBB; 32]),
        },
    ));
    assert_eq!(
        misbehavior_rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty),
        "GHSA-qhr3-cvch-5fh2: an above-lookahead block must not score the serving peer"
    );
}

/// Build a [`ChainSync`] wired to mock services, returning the receiver end of the misbehavior
/// channel so a test can assert whether a peer was scored.
///
/// Unlike [`setup`], this returns the `ChainSync` value itself rather than its `sync` future, so a
/// test can call response-handling methods directly.
#[allow(clippy::type_complexity)]
pub(in crate::components::sync) fn new_chain_sync_with_misbehavior() -> (
    ChainSync<
        MockService<zn::Request, zn::Response, PanicAssertion>,
        MockService<zs::Request, zs::Response, PanicAssertion>,
        MockService<zs::ReadRequest, zs::ReadResponse, PanicAssertion>,
        MockService<zebra_consensus::Request, block::Hash, PanicAssertion>,
        MockChainTip,
    >,
    tokio::sync::mpsc::Receiver<zn::MisbehaviorReport>,
) {
    let _init_guard = zebra_test::init();

    new_chain_sync_with(
        MockService::build().for_unit_tests(),
        MockService::build().for_unit_tests(),
    )
}

/// Builds a [`ChainSync`] like [`new_chain_sync_with_misbehavior`], downloading from `peer_set`
/// and verifying with `block_verifier_router`.
#[allow(clippy::type_complexity)]
fn new_chain_sync_with<ZN>(
    peer_set: ZN,
    block_verifier_router: MockService<zebra_consensus::Request, block::Hash, PanicAssertion>,
) -> (
    ChainSync<
        ZN,
        MockService<zs::Request, zs::Response, PanicAssertion>,
        MockService<zs::ReadRequest, zs::ReadResponse, PanicAssertion>,
        MockService<zebra_consensus::Request, block::Hash, PanicAssertion>,
        MockChainTip,
    >,
    tokio::sync::mpsc::Receiver<zn::MisbehaviorReport>,
)
where
    ZN: tower::Service<zn::Request, Response = zn::Response, Error = crate::BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZN::Future: Send + 'static,
{
    let config = ZebradConfig {
        consensus: ConsensusConfig::default(),
        state: StateConfig::ephemeral(),
        ..Default::default()
    };

    let (mock_chain_tip, _mock_chain_tip_sender) = MockChainTip::new();

    let (misbehavior_tx, misbehavior_rx) = tokio::sync::mpsc::channel(4);
    let (chain_sync, _sync_status) = ChainSync::new(
        &config,
        Height(0),
        peer_set,
        block_verifier_router,
        MockService::build().for_unit_tests(),
        MockService::build().for_unit_tests(),
        mock_chain_tip,
        misbehavior_tx,
    );

    (chain_sync, misbehavior_rx)
}

/// A mock peer set that never answers, and serves at most `permits` requests at a time.
///
/// With no permits, it never becomes ready.
fn peer_set_with_permits(
    permits: usize,
) -> tower::limit::ConcurrencyLimit<MockService<zn::Request, zn::Response, PanicAssertion>> {
    tower::limit::ConcurrencyLimit::new(MockService::build().for_unit_tests(), permits)
}

fn setup() -> (
    // ChainSync
    impl Future<Output = Result<(), Report>> + Send,
    SyncStatus,
    // BlockVerifierRouter
    MockService<zebra_consensus::Request, block::Hash, PanicAssertion>,
    // PeerSet
    MockService<zebra_network::Request, zebra_network::Response, PanicAssertion>,
    // StateService
    MockService<zebra_state::Request, zebra_state::Response, PanicAssertion>,
    MockChainTipSender,
) {
    let _init_guard = zebra_test::init();

    let consensus_config = ConsensusConfig::default();
    let state_config = StateConfig::ephemeral();
    let config = ZebradConfig {
        consensus: consensus_config,
        state: state_config,
        ..Default::default()
    };

    // All services are in-process mocks, so paused Tokio time cannot race real I/O.
    // Keep the full observation windows for absence assertions.
    let peer_set = MockService::build()
        .with_max_request_delay(MAX_SERVICE_REQUEST_DELAY)
        .for_unit_tests();

    let block_verifier_router = MockService::build()
        .with_max_request_delay(MAX_SERVICE_REQUEST_DELAY)
        .for_unit_tests();

    let state_service = MockService::build()
        .with_max_request_delay(MAX_SERVICE_REQUEST_DELAY)
        .for_unit_tests();

    let read_state_service: MockService<zs::ReadRequest, zs::ReadResponse, PanicAssertion> =
        MockService::build()
            .with_max_request_delay(MAX_SERVICE_REQUEST_DELAY)
            .for_unit_tests();

    let (mock_chain_tip, mock_chain_tip_sender) = MockChainTip::new();

    let (misbehavior_tx, _misbehavior_rx) = tokio::sync::mpsc::channel(1);
    let (chain_sync, sync_status) = ChainSync::new(
        &config,
        Height(0),
        peer_set.clone(),
        block_verifier_router.clone(),
        state_service.clone(),
        read_state_service,
        mock_chain_tip,
        misbehavior_tx,
    );

    let chain_sync_future = chain_sync.sync();

    (
        chain_sync_future,
        sync_status,
        block_verifier_router,
        peer_set,
        state_service,
        mock_chain_tip_sender,
    )
}

/// A hash advertised once must exhaust its retry budget across empty and singleton tip rounds.
#[tokio::test(start_paused = true)]
async fn singleton_tip_rounds_reap_completed_downloads() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let _init_guard = zebra_test::init();
    let hash = block::Hash::from([0xF1; 32]);
    let known_hash = block::Hash::from([0xF2; 32]);
    for singleton in [false, true] {
        for download_timeout in [false, true] {
            let advertised = Arc::new(AtomicBool::new(false));
            let network_advertised = advertised.clone();
            let network = tower::service_fn(move |request| {
                let response = match request {
                    zn::Request::FindBlocks { .. } => {
                        let hashes = if !network_advertised.load(Ordering::SeqCst) {
                            vec![hash]
                        } else if singleton {
                            vec![known_hash]
                        } else {
                            Vec::new()
                        };
                        Some(Ok(zn::Response::BlockHashes(hashes)))
                    }
                    zn::Request::BlockDownload {
                        hash: requested, ..
                    } => {
                        assert_eq!(requested, hash);
                        if download_timeout {
                            None
                        } else {
                            Some(Err(crate::BoxError::from("NotFoundResponse")))
                        }
                    }
                    _ => unreachable!("the syncer only requests tips and blocks"),
                };
                async move {
                    match response {
                        Some(response) => response,
                        None => futures::future::pending().await,
                    }
                }
            });
            let state = tower::service_fn(move |request| {
                futures::future::ok::<_, crate::BoxError>(match request {
                    zs::Request::BlockLocator => zs::Response::BlockLocator(vec![known_hash]),
                    zs::Request::KnownBlock(requested) => zs::Response::KnownBlock(
                        (requested == known_hash).then_some(zs::KnownBlock::BestChain),
                    ),
                    _ => unreachable!("the syncer only checks known blocks and the locator"),
                })
            });
            let (tip, _tip_sender) = MockChainTip::new();
            let (misbehavior_tx, _misbehavior_rx) = tokio::sync::mpsc::channel(4);
            let (mut chain_sync, _status) = ChainSync::new(
                &ZebradConfig::default(),
                Height(0),
                network,
                MockService::build().for_unit_tests(),
                state,
                MockService::build().for_unit_tests(),
                tip,
                misbehavior_tx,
            );
            chain_sync.full_verify_concurrency_limit = 2;
            chain_sync.try_to_sync().await.unwrap();
            assert_eq!(chain_sync.downloads.in_flight(), 1);
            advertised.store(true, Ordering::SeqCst);

            for attempt in 1..=sync::MAX_BLOCK_REOBTAIN_RETRIES + 1 {
                tokio::time::sleep(sync::BLOCK_DOWNLOAD_ROUND_TIMEOUT + sync::SYNC_RESTART_DELAY)
                    .await;
                tokio::time::timeout(sync::SYNC_RESTART_DELAY, chain_sync.try_to_sync())
                    .await
                    .expect("an empty or singleton tip round must finish")
                    .unwrap();
                assert!(chain_sync.prospective_tips.is_empty());
                let retrying = attempt <= sync::MAX_BLOCK_REOBTAIN_RETRIES;
                assert_eq!(
                    chain_sync.downloads.in_flight(),
                    usize::from(retrying),
                    "the hash must be dropped after its retry budget is exhausted"
                );
                assert_eq!(
                    chain_sync.block_reobtain_retries.get(&hash).copied(),
                    retrying.then_some(attempt),
                    "retry counts must survive the inter-round sleep"
                );
            }
            assert!(chain_sync.reobtain_hashes.is_empty());
            chain_sync.try_to_sync().await.unwrap();
            assert!(chain_sync.downloads.is_empty());
            chain_sync
                .downloads
                .download_and_verify(hash)
                .await
                .expect("reaping the exhausted download must remove its cancellation handle");
            chain_sync.downloads.cancel_all();
        }
    }
}

/// A terminal header rejection must not leave its previous retry budget behind.
#[tokio::test(start_paused = true)]
async fn terminal_header_failure_reaps_retry_counter_next_round() {
    let _init_guard = zebra_test::init();
    let hash = block::Hash::from([0xE1; 32]);
    let network = tower::service_fn(|request| {
        assert!(matches!(request, zn::Request::FindBlocks { .. }));
        futures::future::ok::<_, crate::BoxError>(zn::Response::BlockHashes(Vec::new()))
    });
    let state = tower::service_fn(move |request| {
        assert!(matches!(request, zs::Request::BlockLocator));
        futures::future::ok::<_, crate::BoxError>(zs::Response::BlockLocator(vec![hash]))
    });
    let (tip, _tip_sender) = MockChainTip::new();
    let (misbehavior_tx, _misbehavior_rx) = tokio::sync::mpsc::channel(4);
    let (mut chain_sync, _status) = ChainSync::new(
        &ZebradConfig::default(),
        Height(0),
        network,
        MockService::build().for_unit_tests(),
        state,
        MockService::build().for_unit_tests(),
        tip,
        misbehavior_tx,
    );

    chain_sync
        .handle_block_response(Err(BlockDownloadVerifyError::DownloadTimedOut { hash }))
        .unwrap();
    assert!(chain_sync.reobtain_hashes.shift_remove(&hash));
    chain_sync
        .handle_block_response(Err(BlockDownloadVerifyError::Invalid {
            error: RouterError::Block {
                source: Box::new(VerifyBlockError::Block {
                    source: zebra_consensus::BlockError::InvalidDifficulty(Height(1), hash),
                }),
            },
            height: Height(1),
            hash,
            advertiser_addr: None,
        }))
        .unwrap();
    assert!(chain_sync.block_reobtain_retries.contains_key(&hash));
    chain_sync.try_to_sync().await.unwrap();
    assert!(
        !chain_sync.block_reobtain_retries.contains_key(&hash),
        "terminal failures must release retry bookkeeping before the next tip round"
    );
}

/// A failed ancestor is retried after the inter-round sleep, even without fresh tip hashes.
#[tokio::test(start_paused = true)]
async fn inter_round_ancestor_failure_is_retried() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let _init_guard = zebra_test::init();
    let parent: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let child: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let parent_hash = parent.hash();
    let child_hash = child.hash();
    for singleton in [false, true] {
        let recovered = Arc::new(AtomicBool::new(false));
        let network_recovered = recovered.clone();
        let parent = parent.clone();
        let network_child = child.clone();
        let network = tower::service_fn(move |request| {
            let response = match request {
                zn::Request::FindBlocks { .. } => {
                    let hashes = if !network_recovered.load(Ordering::SeqCst) {
                        vec![parent_hash]
                    } else if singleton {
                        vec![child_hash]
                    } else {
                        Vec::new()
                    };
                    Ok(zn::Response::BlockHashes(hashes))
                }
                zn::Request::BlockDownload { hash, .. } if hash == child_hash => Ok(
                    zn::Response::Blocks(vec![Available((network_child.clone(), None))]),
                ),
                zn::Request::BlockDownload { hash, .. } if hash == parent_hash => {
                    if network_recovered.load(Ordering::SeqCst) {
                        Ok(zn::Response::Blocks(vec![Available((
                            parent.clone(),
                            None,
                        ))]))
                    } else {
                        Err(crate::BoxError::from("NotFoundResponse"))
                    }
                }
                _ => unreachable!("only the parent, child, and tips are requested"),
            };
            futures::future::ready(response)
        });
        let state = tower::service_fn(move |request| {
            futures::future::ok::<_, crate::BoxError>(match request {
                zs::Request::BlockLocator => zs::Response::BlockLocator(vec![parent_hash]),
                zs::Request::KnownBlock(_) => zs::Response::KnownBlock(None),
                _ => unreachable!("only known blocks and the locator are queried"),
            })
        });
        let mut verifier = MockService::build().for_unit_tests();
        let (tip, _tip_sender) = MockChainTip::new();
        let (misbehavior_tx, _misbehavior_rx) = tokio::sync::mpsc::channel(1);
        let (mut chain_sync, _status) = ChainSync::new(
            &ZebradConfig::default(),
            Height(0),
            network,
            verifier.clone(),
            state,
            MockService::build().for_unit_tests(),
            tip,
            misbehavior_tx,
        );
        chain_sync
            .downloads
            .download_and_verify(child_hash)
            .await
            .unwrap();
        let pending_child = verifier
            .expect_request(zebra_consensus::Request::Commit(child.clone()))
            .await;
        chain_sync.try_to_sync().await.unwrap();
        tokio::time::sleep(sync::SYNC_RESTART_DELAY).await;
        recovered.store(true, Ordering::SeqCst);
        chain_sync.try_to_sync().await.unwrap();
        verifier
            .expect_request_that(|request: &zebra_consensus::Request| {
                request.block().hash() == parent_hash
            })
            .await
            .respond(parent_hash);
        pending_child.respond(child_hash);
        let results = tokio::time::timeout(Duration::from_secs(1), async {
            let mut hashes = Vec::new();
            while let Some(response) = chain_sync.downloads.next().await {
                hashes.push(response.unwrap().1);
            }
            hashes
        })
        .await
        .expect("the recovered parent and retained child must both finish");
        assert!(results.contains(&parent_hash) && results.contains(&child_hash));
    }
}

/// A first responder must not fill the batch before another peer's next block is admitted.
#[tokio::test(start_paused = true)]
async fn sync_tip_responses_share_batch_capacity() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let _init_guard = zebra_test::init();
    let [honest, adversary_first, adversary_second, overlap] =
        [1, 98, 99, 97].map(|byte| block::Hash::from([byte; 32]));

    for extend in [false, true] {
        for duplicate in [false, true] {
            let response_index = Arc::new(AtomicUsize::new(0));
            let network = tower::service_fn(move |request| {
                let response = match request {
                    zn::Request::FindBlocks { .. } => {
                        let mut hashes = match response_index.fetch_add(1, Ordering::SeqCst) {
                            0 => {
                                let mut hashes = vec![adversary_first, adversary_second];
                                if duplicate {
                                    hashes.push(honest);
                                }
                                hashes
                            }
                            1 => vec![honest],
                            _ => Vec::new(),
                        };
                        if extend {
                            hashes.insert(0, overlap);
                        }
                        Some(zn::Response::BlockHashes(hashes))
                    }
                    zn::Request::BlockDownload { .. } => None,
                    _ => unreachable!("the syncer only requests tips and blocks"),
                };
                // Each tip response completes before the fanout yields to dispatch the next.
                async move {
                    match response {
                        Some(response) => Ok::<_, crate::BoxError>(response),
                        None => futures::future::pending().await,
                    }
                }
            });
            let state = tower::service_fn(|request| {
                let response = match request {
                    zs::Request::BlockLocator => {
                        zs::Response::BlockLocator(vec![block::Hash::from([0; 32])])
                    }
                    zs::Request::KnownBlock(_) => zs::Response::KnownBlock(None),
                    _ => unreachable!("the syncer only checks known blocks and the locator"),
                };
                futures::future::ok::<_, crate::BoxError>(response)
            });
            let (tip, tip_sender) = MockChainTip::new();
            tip_sender.send_best_tip_height(Height(0));
            let (misbehavior_tx, _misbehavior_rx) = tokio::sync::mpsc::channel(1);
            let (mut chain_sync, _status) = ChainSync::new(
                &ZebradConfig::default(),
                Height(0),
                network,
                MockService::build().for_unit_tests(),
                state,
                MockService::build().for_unit_tests(),
                tip,
                misbehavior_tx,
            );
            chain_sync.full_verify_concurrency_limit = 2;
            if extend {
                chain_sync.prospective_tips.insert(sync::CheckedTip {
                    tip: block::Hash::from([0; 32]),
                    expected_next: overlap,
                });
            }

            let extra = tokio::time::timeout(MAX_SERVICE_REQUEST_DELAY, async {
                if extend {
                    chain_sync.extend_tips().await
                } else {
                    chain_sync.obtain_tips().await
                }
            })
            .await
            .expect("a tip round must finish without waiting for pending block downloads")
            .unwrap();

            assert_eq!(
                extra.into_iter().collect::<Vec<_>>(),
                vec![adversary_second],
                "the honest successor must be admitted in this round"
            );
            assert_eq!(chain_sync.downloads.in_flight(), 2);
            assert!(matches!(
                chain_sync.downloads.download_and_verify(honest).await,
                Err(BlockDownloadVerifyError::DuplicateBlockQueuedForDownload { .. })
            ));
            chain_sync.downloads.cancel_all();
        }
    }
}

/// New batches use only the remaining lookahead capacity and preserve the unqueued suffix.
#[tokio::test(start_paused = true)]
async fn request_blocks_respects_remaining_capacity() {
    let (mut chain_sync, _misbehavior_rx) = new_chain_sync_with_misbehavior();
    chain_sync.full_verify_concurrency_limit = 2;
    let hashes = [1, 2, 3].map(|byte| block::Hash::from([byte; 32]));

    assert!(chain_sync
        .request_blocks([hashes[0]].into_iter().collect())
        .await
        .unwrap()
        .is_empty());
    let extra = chain_sync
        .request_blocks(hashes[1..].iter().copied().collect())
        .await
        .unwrap();
    assert_eq!(extra.iter().copied().collect::<Vec<_>>(), vec![hashes[2]]);
    assert_eq!(chain_sync.downloads.in_flight(), 2);

    let extra = chain_sync.request_blocks(extra).await.unwrap();
    assert_eq!(extra.into_iter().collect::<Vec<_>>(), vec![hashes[2]]);
    assert_eq!(chain_sync.downloads.in_flight(), 2);
    chain_sync.downloads.cancel_all();
}

/// Delayed retries retain capacity so a fresh batch cannot overfill the queue when they resume.
#[tokio::test(start_paused = true)]
async fn request_blocks_reserves_capacity_for_retries() {
    let (mut chain_sync, _misbehavior_rx) = new_chain_sync_with_misbehavior();
    chain_sync.full_verify_concurrency_limit = 2;
    let hashes = [1, 2, 3].map(|byte| block::Hash::from([byte; 32]));
    chain_sync.reobtain_hashes.insert(hashes[0]);

    let extra = chain_sync
        .request_blocks(hashes[1..].iter().copied().collect())
        .await
        .unwrap();
    assert_eq!(extra.into_iter().collect::<Vec<_>>(), vec![hashes[2]]);
    assert_eq!(chain_sync.downloads.in_flight(), 1);

    chain_sync.reobtain_missing_blocks().await;
    assert_eq!(chain_sync.downloads.in_flight(), 2);
    assert!(chain_sync.reobtain_hashes.is_empty());
    chain_sync.downloads.cancel_all();
}

/// A partially queued final checkpoint must leave room for its missing blocks.
#[tokio::test(start_paused = true)]
async fn partial_final_checkpoint_keeps_admission_capacity() {
    let _init_guard = zebra_test::init();
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .range(1..=9)
        .map(|(_, bytes)| bytes.zcash_deserialize_into().unwrap())
        .collect();
    let hashes: Vec<_> = blocks.iter().map(|block| block.hash()).collect();
    let network = tower::service_fn(move |request| {
        let zn::Request::BlockDownload { hash, .. } = request else {
            panic!("only block downloads are expected");
        };
        let block = blocks.iter().find(|block| block.hash() == hash).unwrap();
        futures::future::ok::<_, crate::BoxError>(zn::Response::Blocks(vec![Available((
            block.clone(),
            None,
        ))]))
    });
    let mut verifier = MockService::build().for_unit_tests();
    let (tip, tip_sender) = MockChainTip::new();
    tip_sender.send_best_tip_height(Height(0));
    let (misbehavior_tx, _misbehavior_rx) = tokio::sync::mpsc::channel(1);
    let mut config = ZebradConfig::default();
    config.sync.full_verify_concurrency_limit = 1;
    let (mut chain_sync, _status) = ChainSync::new(
        &config,
        Height(5),
        network,
        verifier.clone(),
        MockService::build().for_unit_tests(),
        MockService::build().for_unit_tests(),
        tip,
        misbehavior_tx,
    );

    assert!(chain_sync
        .request_blocks(hashes[..3].iter().copied().collect())
        .await
        .unwrap()
        .is_empty());
    let mut pending_checkpoint = Vec::new();
    for _ in 0..3 {
        pending_checkpoint.push(verifier.expect_request_that(|_| true).await);
    }
    let extra = chain_sync
        .request_blocks(hashes[3..].iter().copied().collect())
        .await
        .unwrap();
    assert_eq!(chain_sync.downloads.in_flight(), 6);
    assert_eq!(extra.iter().copied().collect::<Vec<_>>(), hashes[6..]);
    for _ in 0..3 {
        pending_checkpoint.push(verifier.expect_request_that(|_| true).await);
    }
    assert!(pending_checkpoint
        .iter()
        .any(|request| request.request().block().coinbase_height() == Some(Height(5))));

    // The final checkpoint and first fully verified block can now finish.
    for request in pending_checkpoint {
        request.respond_with(|request| request.block().hash());
    }
    tip_sender.send_best_tip_height(Height(6));
    let extra = tokio::time::timeout(
        MAX_SERVICE_REQUEST_DELAY,
        chain_sync.try_to_sync_once(extra),
    )
    .await
    .expect("a completed final checkpoint must not block the full-verification pipeline")
    .unwrap();
    assert_eq!(chain_sync.downloads.in_flight(), 1);
    assert_eq!(extra.into_iter().collect::<Vec<_>>(), hashes[7..]);
    chain_sync.downloads.cancel_all();
}

/// A stale height-pause flag must not make a single-slot queue wait for a nonexistent task.
#[tokio::test(start_paused = true)]
async fn high_invalid_block_does_not_pause_an_empty_queue() {
    let _init_guard = zebra_test::init();
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_347499_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let hash = block.hash();
    let next_hash = block::Hash::from([0xA7; 32]);
    let network = tower::service_fn(move |request| {
        let block = block.clone();
        async move {
            match request {
                zn::Request::BlockDownload {
                    hash: requested, ..
                } if requested == hash => Ok(zn::Response::Blocks(vec![Available((block, None))])),
                zn::Request::BlockDownload { hash, .. } if hash == next_hash => {
                    futures::future::pending::<Result<zn::Response, crate::BoxError>>().await
                }
                _ => unreachable!("only the advertised blocks are requested"),
            }
        }
    });
    let verifier = tower::service_fn(move |_| {
        futures::future::err::<block::Hash, crate::BoxError>(
            RouterError::Block {
                source: Box::new(VerifyBlockError::Block {
                    source: zebra_consensus::BlockError::InvalidDifficulty(Height(347_499), hash),
                }),
            }
            .into(),
        )
    });
    let (tip, tip_sender) = MockChainTip::new();
    tip_sender.send_best_tip_height(Height(300_000));
    let (misbehavior_tx, _misbehavior_rx) = tokio::sync::mpsc::channel(1);
    let mut config = ZebradConfig::default();
    config.sync.full_verify_concurrency_limit = 1;
    let (mut chain_sync, _status) = ChainSync::new(
        &config,
        Height(0),
        network,
        verifier,
        MockService::build().for_unit_tests(),
        MockService::build().for_unit_tests(),
        tip,
        misbehavior_tx,
    );
    chain_sync
        .request_blocks([hash].into_iter().collect())
        .await
        .unwrap();
    let extra = tokio::time::timeout(
        MAX_SERVICE_REQUEST_DELAY,
        chain_sync.try_to_sync_once([next_hash].into_iter().collect()),
    )
    .await
    .expect("a failed block must release the only download slot")
    .unwrap();
    assert!(chain_sync.past_lookahead_limit_receiver.cloned_watch_data());
    assert!(extra.is_empty());
    assert_eq!(chain_sync.downloads.in_flight(), 1);
    chain_sync.downloads.cancel_all();
}

/// One invalid block or exhausted download must not cancel an honest pending verification.
#[tokio::test(start_paused = true)]
async fn failed_block_preserves_honest_pending_verification() {
    let _init_guard = zebra_test::init();
    let honest: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let invalid: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let honest_hash = honest.hash();
    let invalid_hash = invalid.hash();
    let advertiser: PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();

    for download_failure in [false, true] {
        let blocks = [honest.clone(), invalid.clone()];
        let network = tower::service_fn(move |request| {
            let zn::Request::BlockDownload { hash, .. } = request else {
                panic!("no tip requests are needed without prospective tips");
            };
            let response = if download_failure && hash == invalid_hash {
                Err(crate::BoxError::from("connection reset"))
            } else {
                let block = blocks.iter().find(|block| block.hash() == hash).unwrap();
                Ok(zn::Response::Blocks(vec![Available((
                    block.clone(),
                    Some(advertiser),
                ))]))
            };
            futures::future::ready(response)
        });
        let mut verifier = MockService::build().for_unit_tests();
        let (tip, _tip_sender) = MockChainTip::new();
        let (misbehavior_tx, mut misbehavior_rx) = tokio::sync::mpsc::channel(4);
        let (mut chain_sync, _status) = ChainSync::new(
            &ZebradConfig::default(),
            Height(0),
            network,
            verifier.clone(),
            MockService::build().for_unit_tests(),
            MockService::build().for_unit_tests(),
            tip,
            misbehavior_tx,
        );
        chain_sync.full_verify_concurrency_limit = 2;

        chain_sync
            .request_blocks([invalid_hash].into_iter().collect())
            .await
            .unwrap();
        let invalid_request = if download_failure {
            None
        } else {
            Some(
                verifier
                    .expect_request(zebra_consensus::Request::Commit(invalid.clone()))
                    .await,
            )
        };
        chain_sync
            .request_blocks([honest_hash].into_iter().collect())
            .await
            .unwrap();
        let honest_request = verifier
            .expect_request(zebra_consensus::Request::Commit(honest.clone()))
            .await;

        let error = RouterError::Block {
            source: Box::new(VerifyBlockError::Block {
                source: zebra_consensus::BlockError::InvalidDifficulty(Height(2), invalid_hash),
            }),
        };
        let expected_score = error.misbehavior_score();
        if let Some(request) = invalid_request {
            request.respond_error(error.into());
        }
        tokio::time::timeout(
            sync::BLOCK_DOWNLOAD_TIMEOUT,
            chain_sync.try_to_sync_once(Default::default()),
        )
        .await
        .expect("handling a failed block must not wait for the honest verification")
        .expect("a failed block must not restart the sync round");

        // A failed download is re-requested while the honest block waits, until its bounded
        // retries are exhausted. An invalid header is not re-requested.
        assert_eq!(chain_sync.downloads.in_flight(), 1);
        assert!(chain_sync.reobtain_hashes.is_empty());
        assert!(chain_sync.block_reobtain_retries.is_empty());
        if download_failure {
            assert!(misbehavior_rx.try_recv().is_err());
        } else {
            let report = misbehavior_rx.try_recv().unwrap();
            assert_ne!(expected_score, 0);
            assert_eq!((report.addr, report.score), (advertiser, expected_score));
        }

        honest_request.respond(honest_hash);
        let committed =
            tokio::time::timeout(MAX_SERVICE_REQUEST_DELAY, chain_sync.downloads.next())
                .await
                .expect("the honest verification must still complete")
                .expect("the honest download must still be tracked")
                .expect("the honest download must not be cancelled");
        assert_eq!(committed, (Height(1), honest_hash));
        assert!(chain_sync.downloads.is_empty());
    }
}

/// Wrapped service, state, and shutdown errors still require a sync restart.
#[test]
fn infrastructure_verification_errors_restart_sync() {
    use zebra_consensus::{BlockError, VerifyCheckpointError};

    let hash = block::Hash::from([0xD1; 32]);
    let block_errors = [
        VerifyBlockError::Depth {
            source: "state depth failed".into(),
            hash,
        },
        VerifyBlockError::StateService {
            source: "state failed".into(),
            hash,
        },
        VerifyBlockError::Commit(zs::CommitBlockError::new_write_task_exited()),
        VerifyBlockError::Block {
            source: BlockError::Other("internal failure".into()),
        },
        VerifyBlockError::Transaction(TransactionError::InternalDowncastError(
            "unknown verifier failure".into(),
        )),
    ];
    let mut errors: Vec<_> = block_errors
        .into_iter()
        .map(|error| RouterError::Block {
            source: Box::new(error),
        })
        .collect();
    errors.extend(
        [
            VerifyCheckpointError::VerifyBlock(VerifyBlockError::Depth {
                source: "state depth failed".into(),
                hash,
            }),
            VerifyCheckpointError::CommitCheckpointVerified(
                zs::CommitCheckpointVerifiedError::from(
                    zs::CommitBlockError::new_write_task_exited(),
                )
                .into(),
            ),
            VerifyCheckpointError::CommitCheckpointVerified("state commit failed".into()),
            VerifyCheckpointError::Tip("state tip failed".into()),
            VerifyCheckpointError::CheckpointList("checkpoint list failed".into()),
            VerifyCheckpointError::Dropped,
            VerifyCheckpointError::ShuttingDown,
        ]
        .into_iter()
        .map(|error| RouterError::Checkpoint {
            source: Box::new(error),
        }),
    );

    for error in errors {
        let error = BlockDownloadVerifyError::Invalid {
            error,
            height: Height(1),
            hash,
            advertiser_addr: None,
        };
        assert!(TestChainSync::should_restart_sync(&error), "{error:?}");
    }
}
