//! Fixed test vectors for the syncer.

#![allow(clippy::unwrap_in_result)]

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use color_eyre::Report;
use futures::{Future, FutureExt};

use zebra_chain::{
    block::{self, Block},
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
    components::{sync::SyncStatus, ChainSync},
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

/// Scripted successful `FindBlocks` responses, keyed by the first locator
/// hash and served in order per key.
///
/// A request whose key has no scripted response left gets an empty hash list,
/// standing in for the fanout peers that return nothing. (An error response
/// would also work, but each one makes the crawl capture an `eyre` backtrace,
/// which blocks the single-threaded test runtime for long enough to trip the
/// mock service timeouts in debug builds.)
type FindBlocksScript = HashMap<block::Hash, VecDeque<Vec<block::Hash>>>;

/// Answers `total` requests to the mocked peer set: `BlocksByHash` from
/// `blocks` (single-block responses, like real peers), `FindBlocks` from
/// `script`.
///
/// The engine's block fetches run concurrently with the syncer's tip crawl,
/// so the arrival order across the two request kinds is not deterministic;
/// this responder replaces the legacy syncer tests' strictly-ordered peer
/// expectations. The causal request *count* is still deterministic, so the
/// responder serves exactly `total` requests and returns, letting the caller
/// join it to surface panics.
async fn respond_to_peer_requests(
    mut peer_set: MockService<zn::Request, zn::Response, PanicAssertion>,
    blocks: HashMap<block::Hash, Arc<Block>>,
    mut script: FindBlocksScript,
    total: usize,
) {
    for _ in 0..total {
        let responder = peer_set.expect_request_that(|_| true).await;
        let request = responder.request().clone();

        match request {
            zn::Request::BlocksByHash(hashes) => {
                let response = hashes
                    .iter()
                    .map(|hash| {
                        let block = blocks
                            .get(hash)
                            .unwrap_or_else(|| panic!("unexpected block fetch: {hash}"))
                            .clone();
                        Available((block, None))
                    })
                    .collect();

                responder.respond(zn::Response::Blocks(response));
            }

            zn::Request::FindBlocks { known_blocks, .. } => {
                let first = known_blocks.first().expect("locators are never empty");
                let hashes = script
                    .get_mut(first)
                    .and_then(VecDeque::pop_front)
                    .unwrap_or_default();

                responder.respond(zn::Response::BlockHashes(hashes));
            }

            other => panic!("unexpected request to the peer set: {other:?}"),
        }
    }
}

/// Test that the syncer downloads genesis, discovers blocks 1-2 using
/// obtain_tips and blocks 3-4 using extend_tips, and drives the IBD engine to
/// fetch and verify all of them.
#[tokio::test]
async fn sync_blocks_ok() -> Result<(), crate::BoxError> {
    // Get services
    let (
        chain_sync_future,
        _sync_status,
        mut block_verifier_router,
        peer_set,
        mut state_service,
        _mock_chain_tip_sender,
        _cache_dir,
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

    // The peer script: genesis fetch, one obtain_tips crawl from genesis, one
    // extend_tips crawl from the discovered tip (block 1), one final
    // extend_tips crawl from the new tip (block 3) that returns nothing and
    // ends the cycle, plus one engine fetch per discovered block.
    let blocks: HashMap<block::Hash, Arc<Block>> = [
        (block0_hash, block0.clone()),
        (block1_hash, block1.clone()),
        (block2_hash, block2.clone()),
        (block3_hash, block3.clone()),
        (block4_hash, block4.clone()),
    ]
    .into_iter()
    .collect();

    let script: FindBlocksScript = [
        (
            block0_hash,
            VecDeque::from([vec![
                block1_hash, // tip
                block2_hash, // expected_next
                block3_hash, // (discarded - last hash, possibly incorrect)
            ]]),
        ),
        (
            block1_hash,
            VecDeque::from([vec![
                block2_hash, // tip (discarded - already discovered)
                block3_hash, // expected_next
                block4_hash,
                block5_hash, // (discarded - last hash, possibly incorrect)
            ]]),
        ),
    ]
    .into_iter()
    .collect();

    // 1 genesis fetch + 3 crawl rounds × FANOUT requests + 4 block fetches.
    let total_peer_requests = 1 + 3 * super::super::FANOUT + 4;
    let peer_responder = tokio::spawn(respond_to_peer_requests(
        peer_set,
        blocks,
        script,
        total_peer_requests,
    ));

    // Start the syncer
    let chain_sync_task_handle = tokio::spawn(chain_sync_future);

    // ChainSync::request_genesis

    // State is checked for genesis
    state_service
        .expect_request(zs::Request::KnownBlock(block0_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    // Genesis is fetched, hash-checked against the network genesis hash, and
    // committed directly to the state: the semantic verifier's checkpoint
    // gate rejects blocks at the mandatory floor, so it is never asked.
    state_service
        .expect_request(zs::Request::CommitCheckpointVerifiedBlock(block0.into()))
        .await
        .respond(zs::Response::Committed(block0_hash));

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

    // State is checked for the first unknown block (block 1) in the
    // successful crawl response
    state_service
        .expect_request(zs::Request::KnownBlock(block1_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    // State is checked for all discovered blocks (blocks 1 & 2) in response
    // order, before they are fed to the engine
    state_service
        .expect_request(zs::Request::KnownBlock(block1_hash))
        .await
        .respond(zs::Response::KnownBlock(None));
    state_service
        .expect_request(zs::Request::KnownBlock(block2_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    // The engine fetches and verifies all four discovered blocks. Fetching,
    // verification, and the concurrent extend_tips crawl interleave, so only
    // the per-block commits are asserted, in any order.
    let mut remaining_blocks: HashMap<block::Hash, Arc<Block>> = [
        (block1_hash, block1),
        (block2_hash, block2),
        (block3_hash, block3),
        (block4_hash, block4),
    ]
    .into_iter()
    .collect();

    for _ in 1..=4 {
        block_verifier_router
            .expect_request_that(|req| remaining_blocks.remove(&req.block().hash()).is_some())
            .await
            .respond_with(|req| req.block().hash());
    }
    assert_eq!(
        remaining_blocks,
        HashMap::new(),
        "expected all discovered blocks to be verified"
    );

    // The peer responder served every scripted request without panicking.
    peer_responder.await?;

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

/// Test that the syncer discovers and verifies blocks exactly once when the
/// crawl responses contain duplicate block hashes.
#[tokio::test]
async fn sync_blocks_duplicate_hashes_ok() -> Result<(), crate::BoxError> {
    // Get services
    let (
        chain_sync_future,
        _sync_status,
        mut block_verifier_router,
        peer_set,
        mut state_service,
        _mock_chain_tip_sender,
        _cache_dir,
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

    let blocks: HashMap<block::Hash, Arc<Block>> = [
        (block0_hash, block0.clone()),
        (block1_hash, block1.clone()),
        (block2_hash, block2.clone()),
        (block3_hash, block3.clone()),
        (block4_hash, block4.clone()),
    ]
    .into_iter()
    .collect();

    // The crawl responses repeat hashes; the discovered download set must
    // still contain each block once.
    let script: FindBlocksScript = [
        (
            block0_hash,
            VecDeque::from([vec![
                block1_hash,
                block1_hash,
                block1_hash, // tip
                block2_hash, // expected_next
                block3_hash, // (discarded - last hash, possibly incorrect)
            ]]),
        ),
        (
            block1_hash,
            VecDeque::from([vec![
                block2_hash, // tip (discarded - already discovered)
                block3_hash, // expected_next
                block4_hash,
                block3_hash,
                block4_hash,
                block5_hash, // (discarded - last hash, possibly incorrect)
            ]]),
        ),
    ]
    .into_iter()
    .collect();

    // 1 genesis fetch + 3 crawl rounds × FANOUT requests + 4 block fetches:
    // the duplicates must not add any block fetches.
    let total_peer_requests = 1 + 3 * super::super::FANOUT + 4;
    let peer_responder = tokio::spawn(respond_to_peer_requests(
        peer_set,
        blocks,
        script,
        total_peer_requests,
    ));

    // Start the syncer
    let chain_sync_task_handle = tokio::spawn(chain_sync_future);

    // ChainSync::request_genesis

    // State is checked for genesis, genesis is fetched and committed, and the
    // state is checked again
    state_service
        .expect_request(zs::Request::KnownBlock(block0_hash))
        .await
        .respond(zs::Response::KnownBlock(None));
    state_service
        .expect_request(zs::Request::CommitCheckpointVerifiedBlock(block0.into()))
        .await
        .respond(zs::Response::Committed(block0_hash));
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

    // State is checked for the first unknown block (block 1) in the
    // successful crawl response
    state_service
        .expect_request(zs::Request::KnownBlock(block1_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    // State is checked for the deduplicated discovered blocks (blocks 1 & 2)
    // in response order, before they are fed to the engine
    state_service
        .expect_request(zs::Request::KnownBlock(block1_hash))
        .await
        .respond(zs::Response::KnownBlock(None));
    state_service
        .expect_request(zs::Request::KnownBlock(block2_hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    // Each block is verified exactly once: a duplicate commit fails the
    // remaining-blocks predicate and panics.
    let mut remaining_blocks: HashMap<block::Hash, Arc<Block>> = [
        (block1_hash, block1),
        (block2_hash, block2),
        (block3_hash, block3),
        (block4_hash, block4),
    ]
    .into_iter()
    .collect();

    for _ in 1..=4 {
        block_verifier_router
            .expect_request_that(|req| remaining_blocks.remove(&req.block().hash()).is_some())
            .await
            .respond_with(|req| req.block().hash());
    }
    assert_eq!(
        remaining_blocks,
        HashMap::new(),
        "expected all discovered blocks to be verified"
    );

    // The peer responder served every scripted request without panicking; in
    // particular, the duplicated hashes did not add block fetches.
    peer_responder.await?;

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
    // The engine's disk block cache directory (must outlive the syncer)
    tempfile::TempDir,
) {
    let _init_guard = zebra_test::init();

    let consensus_config = ConsensusConfig::default();
    let state_config = StateConfig::ephemeral();
    let mut config = ZebradConfig {
        consensus: consensus_config,
        state: state_config,
        ..Default::default()
    };
    // The mocked peers answer instantly, so a hedged refetch can only fire
    // spuriously on an overloaded test machine and break the deterministic
    // peer request counts; effectively disable it.
    config.sync.known_hash_gap_hedge_secs = 3600;

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

    let (_peer_set_status_tx, peer_set_status) =
        tokio::sync::watch::channel(zebra_network::PeerSetStatus::default());
    let cache_dir = tempfile::tempdir().expect("temporary directory is created successfully");

    let (chain_sync, sync_status) = ChainSync::new(
        &config,
        peer_set.clone(),
        block_verifier_router.clone(),
        state_service.clone(),
        mock_chain_tip,
        peer_set_status,
        cache_dir.path(),
    );

    let chain_sync_future = chain_sync.sync();

    (
        chain_sync_future,
        sync_status,
        block_verifier_router,
        peer_set,
        state_service,
        mock_chain_tip_sender,
        cache_dir,
    )
}
