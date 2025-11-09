//! Fixed test vectors for the syncer.

#![allow(clippy::unwrap_in_result)]

use std::{collections::HashMap, iter, sync::Arc, time::Duration};

use color_eyre::Report;
use futures::{Future, FutureExt};

use zebra_chain::{
    block::{self, Block, Height},
    chain_tip::mock::{MockChainTip, MockChainTipSender},
    serialization::ZcashDeserializeInto,
};
use zebra_consensus::Config as ConsensusConfig;
use zebra_network::InventoryResponse;
use zebra_state::Config as StateConfig;
use zebra_test::mock_service::{MockService, PanicAssertion};

use zebra_network as zn;
use zebra_state as zs;

use crate::{
    components::{
        sync::{self, SyncStatus},
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

    let block5: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_5_BYTES.zcash_deserialize_into()?;
    let block5_hash = block5.hash();

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
            block3_hash, // (discarded - last hash, possibly incorrect)
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
            block5_hash, // (discarded - last hash, possibly incorrect)
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

    let block5: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_5_BYTES.zcash_deserialize_into()?;
    let block5_hash = block5.hash();

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
            block3_hash, // (discarded - last hash, possibly incorrect)
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
            block5_hash, // (discarded - last hash, possibly incorrect)
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

    let block3: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_3_BYTES.zcash_deserialize_into()?;
    let block3_hash = block3.hash();

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
            block3_hash, // (discarded - last hash, possibly incorrect)
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

    let block5: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_5_BYTES.zcash_deserialize_into()?;
    let block5_hash = block5.hash();

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
            block3_hash, // (discarded - last hash, possibly incorrect)
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
            block5_hash, // (discarded - last hash, possibly incorrect)
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

    let (mock_chain_tip, mock_chain_tip_sender) = MockChainTip::new();

    let (misbehavior_tx, _misbehavior_rx) = tokio::sync::mpsc::channel(1);
    let (chain_sync, sync_status) = ChainSync::new(
        &config,
        Height(0),
        peer_set.clone(),
        block_verifier_router.clone(),
        state_service.clone(),
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
