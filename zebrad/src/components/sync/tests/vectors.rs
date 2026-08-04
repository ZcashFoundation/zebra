//! Fixed test vectors for the syncer.

#![allow(clippy::unwrap_in_result)]

use std::{collections::HashMap, iter, sync::Arc, time::Duration};

use color_eyre::Report;
use futures::{Future, FutureExt};

use zebra_chain::{
    block::{self, Block, Height},
    chain_tip::mock::{MockChainTip, MockChainTipSender},
    parameters::subsidy::SubsidyError,
    serialization::ZcashDeserializeInto,
};
use zebra_consensus::{Config as ConsensusConfig, RouterError, VerifyBlockError};
use zebra_network::{InventoryResponse, PeerSocketAddr};
use zebra_state::Config as StateConfig;
use zebra_test::mock_service::{MockService, PanicAssertion};

use zebra_network as zn;
use zebra_state as zs;

use crate::{
    components::{
        sync::{self, downloads::BlockDownloadVerifyError, SyncStatus},
        ChainSync,
    },
    config::ZebradConfig,
};

use InventoryResponse::*;

/// Maximum time to wait for a request to any test service.
///
/// The default [`MockService`] value can be too short for some of these tests that take a little
/// longer than expected to actually send the request.
///
/// Increasing this value causes the tests to take longer to complete, so it can't be too large.
const MAX_SERVICE_REQUEST_DELAY: Duration = Duration::from_millis(1000);

/// Test that the syncer downloads genesis, blocks 1-2 using obtain_tips, and blocks 3-4 using extend_tips.
///
/// This test also makes sure that the syncer downloads blocks in order.
#[tokio::test]
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
        .expect_request(zn::Request::BlocksByHash(iter::once(block0_hash).collect()))
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
        .expect_request(zn::Request::BlocksByHash(iter::once(block1_hash).collect()))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block1.clone(),
            None,
        ))]));
    peer_set
        .expect_request(zn::Request::BlocksByHash(iter::once(block2_hash).collect()))
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
        .expect_request(zn::Request::BlocksByHash(iter::once(block3_hash).collect()))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block3.clone(),
            None,
        ))]));
    peer_set
        .expect_request(zn::Request::BlocksByHash(iter::once(block4_hash).collect()))
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
#[tokio::test]
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
        .expect_request(zn::Request::BlocksByHash(iter::once(block0_hash).collect()))
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
        .expect_request(zn::Request::BlocksByHash(iter::once(block1_hash).collect()))
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

/// Test that the syncer downloads a singleton unknown hash returned by extend_tips.
#[tokio::test]
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
        .expect_request(zn::Request::BlocksByHash(iter::once(block0_hash).collect()))
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
        .expect_request(zn::Request::BlocksByHash(iter::once(block1_hash).collect()))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block1.clone(),
            None,
        ))]));
    peer_set
        .expect_request(zn::Request::BlocksByHash(iter::once(block2_hash).collect()))
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
        .expect_request(zn::Request::BlocksByHash(iter::once(block3_hash).collect()))
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
#[tokio::test]
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
        .expect_request(zn::Request::BlocksByHash(iter::once(block0_hash).collect()))
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
        .expect_request(zn::Request::BlocksByHash(iter::once(block1_hash).collect()))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block1.clone(),
            None,
        ))]));
    peer_set
        .expect_request(zn::Request::BlocksByHash(iter::once(block2_hash).collect()))
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
        .expect_request(zn::Request::BlocksByHash(iter::once(block3_hash).collect()))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block3.clone(),
            None,
        ))]));
    peer_set
        .expect_request(zn::Request::BlocksByHash(iter::once(block4_hash).collect()))
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
#[tokio::test]
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
        .expect_request(zn::Request::BlocksByHash(iter::once(block0_hash).collect()))
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
#[tokio::test]
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
        .expect_request(zn::Request::BlocksByHash(iter::once(block0_hash).collect()))
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
        .expect_request(zn::Request::BlocksByHash(
            iter::once(block982k_hash).collect(),
        ))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block982k.clone(),
            None,
        ))]));
    peer_set
        .expect_request(zn::Request::BlocksByHash(iter::once(block1_hash).collect()))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block1.clone(),
            None,
        ))]));
    peer_set
        .expect_request(zn::Request::BlocksByHash(iter::once(block2_hash).collect()))
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
#[tokio::test]
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
        .expect_request(zn::Request::BlocksByHash(iter::once(block0_hash).collect()))
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
        .expect_request(zn::Request::BlocksByHash(iter::once(block1_hash).collect()))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block1.clone(),
            None,
        ))]));
    peer_set
        .expect_request(zn::Request::BlocksByHash(iter::once(block2_hash).collect()))
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
        .expect_request(zn::Request::BlocksByHash(iter::once(block3_hash).collect()))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block3.clone(),
            None,
        ))]));
    peer_set
        .expect_request(zn::Request::BlocksByHash(iter::once(block4_hash).collect()))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block4.clone(),
            None,
        ))]));
    peer_set
        .expect_request(zn::Request::BlocksByHash(
            iter::once(block982k_hash).collect(),
        ))
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
        advertiser_addr: None,
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
        misbehavior_rx.try_recv().ok(),
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

/// The pre-existing `NotFound` re-request (#5709) still works, and only fires for `NotFound`.
///
/// Both cases now share one bounded re-queue, so this pins the behavior the behind-tip fix reuses.
#[tokio::test]
async fn download_failed_is_only_requeued_for_not_found() {
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

    // A download that failed for any other reason is a syncer restart, and is not re-queued.
    let failed_hash = block::Hash::from([0xEF; 32]);
    let restart = chain_sync
        .handle_block_response(Err(BlockDownloadVerifyError::DownloadFailed {
            error: std::io::Error::new(std::io::ErrorKind::ConnectionReset, "connection reset")
                .into(),
            hash: failed_hash,
        }))
        .is_err();
    assert!(
        restart,
        "a download that failed for another reason must restart the syncer"
    );

    assert!(
        !chain_sync.reobtain_hashes.contains(&failed_hash),
        "a download that failed for another reason must not be re-queued"
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

/// Verifies fix for GHSA-qhr3-cvch-5fh2: a block that lands above the lookahead
/// height limit must NOT score the peer that served it, while consensus-invalid
/// blocks still must.
///
/// Far-ahead hashes from a malicious `FindBlocks` response carry no peer attribution,
/// so the follow-up `BlocksByHash` request is routed to an independently chosen,
/// honest peer — `advertiser_addr` names the *serving* peer, not the peer that chose
/// the height. Scoring this path bans honest peers at the attacker's direction.
#[tokio::test]
async fn far_ahead_block_does_not_score_serving_peer() {
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
    assert_eq!(
        misbehavior_rx.try_recv(),
        Ok((peer, expected_score)),
        "consensus-invalid blocks must still score the serving peer"
    );

    // The fix: an above-lookahead block must not produce any misbehavior score.
    let _ = chain_sync.handle_block_response(Err(
        BlockDownloadVerifyError::AboveLookaheadHeightLimit {
            height: block::Height(60_000),
            hash: block::Hash::from([0xBB; 32]),
            advertiser_addr: Some(peer),
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
fn new_chain_sync_with_misbehavior() -> (
    ChainSync<
        MockService<zn::Request, zn::Response, PanicAssertion>,
        MockService<zs::Request, zs::Response, PanicAssertion>,
        MockService<zs::ReadRequest, zs::ReadResponse, PanicAssertion>,
        MockService<zebra_consensus::Request, block::Hash, PanicAssertion>,
        MockChainTip,
    >,
    tokio::sync::mpsc::Receiver<(PeerSocketAddr, u32)>,
) {
    let _init_guard = zebra_test::init();

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
        MockService::build().for_unit_tests(),
        MockService::build().for_unit_tests(),
        MockService::build().for_unit_tests(),
        MockService::build().for_unit_tests(),
        mock_chain_tip,
        misbehavior_tx,
    );

    (chain_sync, misbehavior_rx)
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

    // These tests run multiple tasks in parallel.
    // So machines under heavy load need a longer delay.
    // (For example, CI machines with limited cores.)
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
