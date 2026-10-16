//! Fixed test vectors for the inbound gossip block download stream.
//!
//! These are the inbound-gossip siblings of the sync-path tests in
//! `crate::components::sync::downloads::tests`: they cover GHSA-4f6v-mj46-gxg3, where a peer
//! answers a `BlocksByHash` request with a canonical header and a rewritten coinbase height. The
//! coinbase scriptSig is excluded from the V5 transaction ID (ZIP-244), so the block hash is
//! unchanged and the response passes the hash check, but the forged height is dropped as too far
//! ahead of the tip, or behind the finalized tip, before consensus validation. The supplying peer
//! must be scored only when the parent header Zebra holds proves the height was rewritten.

use std::{iter, sync::Arc, time::Duration};

use futures::stream::StreamExt;

use zebra_chain::{
    block::{Block, Height},
    chain_tip::mock::MockChainTip,
    serialization::ZcashDeserializeInto,
};
use zebra_network::{InventoryResponse, PeerSocketAddr};
use zebra_state::MAX_BLOCK_REORG_HEIGHT;
use zebra_test::mock_service::{MockService, PanicAssertion};

use zebra_network as zn;
use zebra_state as zs;

use super::{DownloadAction, Downloads, HeightLimitError, MIN_CONCURRENCY_LIMIT};

use InventoryResponse::*;

/// Maximum time to wait for a request to any test service.
///
/// These tests run the download task in parallel with the test, so machines under heavy load need a
/// longer delay.
const MAX_SERVICE_REQUEST_DELAY: Duration = Duration::from_millis(1000);

/// A peer address used to check whether the supplying peer can be scored.
const ADVERTISER: &str = "127.0.0.1:8233";

type MockNetwork = MockService<zn::Request, zn::Response, PanicAssertion>;
type MockVerifier = MockService<zebra_consensus::Request, zebra_chain::block::Hash, PanicAssertion>;
type MockState = MockService<zs::Request, zs::Response, PanicAssertion>;

/// Build an inbound gossip downloader whose state tip is at `tip_height`, returning the mock
/// network, verifier, and state services so a test can drive and assert on them.
#[allow(clippy::type_complexity)]
fn mock_downloads(
    tip_height: Height,
) -> (
    Downloads<MockNetwork, MockVerifier, MockState, MockChainTip>,
    MockNetwork,
    MockVerifier,
    MockState,
) {
    let network: MockNetwork = MockService::build()
        .with_max_request_delay(MAX_SERVICE_REQUEST_DELAY)
        .for_unit_tests();

    let verifier: MockVerifier = MockService::build()
        .with_max_request_delay(MAX_SERVICE_REQUEST_DELAY)
        .for_unit_tests();

    let state: MockState = MockService::build()
        .with_max_request_delay(MAX_SERVICE_REQUEST_DELAY)
        .for_unit_tests();

    let (latest_chain_tip, chain_tip_sender) = MockChainTip::new();
    chain_tip_sender.send_best_tip_height(tip_height);
    // Dropping the sender is fine: `watch::Receiver::borrow` keeps returning the height we just
    // sent, and no test changes the tip mid-run.

    let downloads = Downloads::new(
        MIN_CONCURRENCY_LIMIT,
        network.clone(),
        verifier.clone(),
        state.clone(),
        latest_chain_tip,
    );

    (downloads, network, verifier, state)
}

/// Load the mainnet block 1 vector, which these tests use as a low-height block body.
fn block_1() -> Arc<Block> {
    zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("hard-coded block vector deserializes")
}

/// Load the mainnet block 2 vector.
fn block_2() -> Arc<Block> {
    zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .expect("hard-coded block vector deserializes")
}

/// Load the mainnet genesis block vector.
fn genesis() -> Arc<Block> {
    zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .expect("hard-coded block vector deserializes")
}

/// Load the mainnet block 10 vector, which the far-ahead tests use as a high-height block body.
fn block_10() -> Arc<Block> {
    zebra_test::vectors::BLOCK_MAINNET_10_BYTES
        .zcash_deserialize_into()
        .expect("hard-coded block vector deserializes")
}

/// The tip height that puts block 2 exactly on the lookahead limit.
///
/// The downloader accepts blocks up to `MIN_CONCURRENCY_LIMIT` above the tip, so block 10 is far
/// ahead of this tip, and block 2 is the highest block it still verifies.
fn lookahead_boundary_tip() -> Height {
    Height(2 - u32::try_from(MIN_CONCURRENCY_LIMIT).expect("small constant fits in u32"))
}

/// Regression test for GHSA-4f6v-mj46-gxg3: a gossiped body whose claimed height contradicts the
/// parent we already hold is attributed to the peer that supplied it, and never reaches consensus.
///
/// A peer can answer with a canonical header and rewritten coinbase height because the initial hash
/// check does not recompute the header's commitment to the body's authorizing data. The parent is
/// the proof: a block's height is one more than its parent's.
#[tokio::test]
async fn contradicted_behind_tip_height_is_attributed_and_never_verified() {
    let _init_guard = zebra_test::init();

    // The tip is far enough ahead that a claimed height of 1 is behind the reorg limit.
    let (mut downloads, mut network, mut verifier, mut state) =
        mock_downloads(Height(2 * MAX_BLOCK_REORG_HEIGHT));

    // Block 2's header with block 1's body: the hash and parent are block 2's, because the hash
    // covers only the header, but the coinbase now claims height 1 instead of 2. This is the shape
    // of the reported attack, without reproducing the hash-preserving rewrite itself.
    let block_1 = block_1();
    let block_2 = block_2();
    let block = Arc::new(Block {
        header: block_2.header.clone(),
        transactions: block_1.transactions.clone(),
    });
    let hash = block.hash();
    assert_eq!(
        hash,
        block_2.hash(),
        "the block hash covers only the header"
    );
    assert_eq!(
        block.coinbase_height(),
        Some(Height(1)),
        "the body must claim the height the downloader reads"
    );

    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    assert!(
        matches!(
            downloads.download_and_verify(hash, Some(advertiser)),
            DownloadAction::AddedToQueue
        ),
        "download is queued"
    );

    // The block is not already in the state.
    state
        .expect_request(zs::Request::KnownBlock(hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    network
        .expect_request(zn::Request::BlocksByHash(iter::once(hash).collect()))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    // We hold the real parent, block 1, so the body's real height is 2, not the 1 it claims.
    state
        .expect_request(zs::Request::BlockHeader(
            block.header.previous_block_hash.into(),
        ))
        .await
        .respond(zs::Response::BlockHeader {
            header: block_1.header.clone(),
            hash: block_1.hash(),
            height: Height(1),
            next_block_hash: Some(hash),
        });

    let (error, advertiser_addr) = downloads
        .next()
        .await
        .expect("downloads is non-empty")
        .expect_err("block behind the reorg limit is dropped");

    assert!(
        matches!(
            error.downcast_ref::<HeightLimitError>(),
            Some(HeightLimitError::BehindTip { .. })
        ),
        "a contradicted behind-tip height must be a scoreable typed error, but was: {error:?}"
    );
    assert_eq!(
        advertiser_addr,
        Some(advertiser),
        "a contradicted behind-tip height must attribute the drop to the supplying peer"
    );

    // The rewritten body is dropped before consensus validation, which is why the peer must be
    // scored on this path instead.
    verifier.expect_no_requests().await;
}

/// A peer that serves a genuinely old block is not attributed, so it cannot be scored.
///
/// Its height agrees with its parent's, so there is no proof of misbehaviour and the drop must stay
/// anonymous.
#[tokio::test]
async fn genuinely_old_block_is_dropped_without_attribution() {
    let _init_guard = zebra_test::init();

    let (mut downloads, mut network, mut verifier, mut state) =
        mock_downloads(Height(2 * MAX_BLOCK_REORG_HEIGHT));

    let block = block_1();
    let hash = block.hash();
    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    assert!(
        matches!(
            downloads.download_and_verify(hash, Some(advertiser)),
            DownloadAction::AddedToQueue
        ),
        "download is queued"
    );

    state
        .expect_request(zs::Request::KnownBlock(hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    network
        .expect_request(zn::Request::BlocksByHash(iter::once(hash).collect()))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    // The parent is genesis, one below the height the body claims: the body is authentic.
    let genesis = genesis();
    assert_eq!(
        block.header.previous_block_hash,
        genesis.hash(),
        "block 1's parent is genesis"
    );
    state
        .expect_request(zs::Request::BlockHeader(
            block.header.previous_block_hash.into(),
        ))
        .await
        .respond(zs::Response::BlockHeader {
            header: genesis.header.clone(),
            hash: genesis.hash(),
            height: Height(0),
            next_block_hash: Some(hash),
        });

    let (error, advertiser_addr) = downloads
        .next()
        .await
        .expect("downloads is non-empty")
        .expect_err("block behind the reorg limit is dropped");

    assert!(
        matches!(
            error.downcast_ref::<HeightLimitError>(),
            Some(HeightLimitError::BehindTip { .. })
        ),
        "an old block must still be a typed behind-tip error, but was: {error:?}"
    );
    assert_eq!(
        advertiser_addr, None,
        "an authentic old block must not be attributed to its peer"
    );

    verifier.expect_no_requests().await;
}

/// A peer whose old block has a parent we do not hold is not attributed either: without the parent
/// there is no proof the height was rewritten.
#[tokio::test]
async fn behind_tip_block_with_unknown_parent_is_not_attributed() {
    let _init_guard = zebra_test::init();

    let (mut downloads, mut network, mut verifier, mut state) =
        mock_downloads(Height(2 * MAX_BLOCK_REORG_HEIGHT));

    let block = block_1();
    let hash = block.hash();
    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    assert!(
        matches!(
            downloads.download_and_verify(hash, Some(advertiser)),
            DownloadAction::AddedToQueue
        ),
        "download is queued"
    );

    state
        .expect_request(zs::Request::KnownBlock(hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    network
        .expect_request(zn::Request::BlocksByHash(iter::once(hash).collect()))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    state
        .expect_request(zs::Request::BlockHeader(
            block.header.previous_block_hash.into(),
        ))
        .await
        .respond(Err(zn::BoxError::from("block not found in any chain")));

    let (error, advertiser_addr) = downloads
        .next()
        .await
        .expect("downloads is non-empty")
        .expect_err("block behind the reorg limit is dropped");

    assert!(
        matches!(
            error.downcast_ref::<HeightLimitError>(),
            Some(HeightLimitError::BehindTip { .. })
        ),
        "a behind-tip drop must be a typed error, but was: {error:?}"
    );
    assert_eq!(
        advertiser_addr, None,
        "a block whose parent we do not hold must not be attributed"
    );

    verifier.expect_no_requests().await;
}

/// The parent lookup is bounded: when the state does not answer, the block is still dropped and the
/// peer is still not attributed.
///
/// Paused time lets the runtime advance past `PARENT_LOOKUP_TIMEOUT` as soon as the download task is
/// the only thing waiting, so the test does not sleep for real.
#[tokio::test(start_paused = true)]
async fn behind_tip_parent_lookup_timeout_is_not_attributed() {
    let _init_guard = zebra_test::init();

    let (mut downloads, mut network, mut verifier, mut state) =
        mock_downloads(Height(2 * MAX_BLOCK_REORG_HEIGHT));

    let block = block_1();
    let hash = block.hash();
    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    assert!(
        matches!(
            downloads.download_and_verify(hash, Some(advertiser)),
            DownloadAction::AddedToQueue
        ),
        "download is queued"
    );

    state
        .expect_request(zs::Request::KnownBlock(hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    network
        .expect_request(zn::Request::BlocksByHash(iter::once(hash).collect()))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    // The parent lookup is deliberately never answered, so it can only end by timing out.
    let (error, advertiser_addr) = downloads
        .next()
        .await
        .expect("downloads is non-empty")
        .expect_err("block behind the reorg limit is dropped");

    assert!(
        matches!(
            error.downcast_ref::<HeightLimitError>(),
            Some(HeightLimitError::BehindTip { .. })
        ),
        "a timed-out lookup must still drop with a typed error, but was: {error:?}"
    );
    assert_eq!(
        advertiser_addr, None,
        "a timed-out parent lookup must not attribute the drop"
    );

    verifier.expect_no_requests().await;
}

/// A block at the oldest height that is still within the reorg limit is verified, not dropped.
///
/// This is the guard against dropping and attributing honest near-boundary blocks:
/// `min_accepted_height` is the boundary, and the behind-tip comparison is strict, so a block
/// exactly on it is a normal download and the parent is never consulted.
#[tokio::test]
async fn block_at_reorg_boundary_is_verified_not_dropped() {
    let _init_guard = zebra_test::init();

    // `min_accepted_height` is `Height(1)`, so the height-1 block sits exactly on the boundary.
    let (mut downloads, mut network, mut verifier, mut state) =
        mock_downloads(Height(MAX_BLOCK_REORG_HEIGHT + 1));

    let block = block_1();
    let hash = block.hash();
    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    assert!(
        matches!(
            downloads.download_and_verify(hash, Some(advertiser)),
            DownloadAction::AddedToQueue
        ),
        "download is queued"
    );

    state
        .expect_request(zs::Request::KnownBlock(hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    network
        .expect_request(zn::Request::BlocksByHash(iter::once(hash).collect()))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    verifier
        .expect_request(zebra_consensus::Request::Commit(block))
        .await
        .respond(hash);

    assert_eq!(
        downloads
            .next()
            .await
            .expect("downloads is non-empty")
            .expect("block on the reorg boundary is verified"),
        hash,
        "a block at min_accepted_height must be verified, not dropped as behind the tip"
    );

    // The parent lookup only runs on the behind-tip drop path, so a boundary block must not trigger
    // one.
    state.expect_no_requests().await;
}

/// The far-ahead sibling of the regression test for GHSA-4f6v-mj46-gxg3: a gossiped body whose
/// claimed height is above the lookahead limit, and contradicts the parent we already hold, is
/// attributed to the peer that supplied it, and never reaches consensus.
///
/// The sync path deliberately leaves its far-ahead drops unscored (GHSA-qhr3-cvch-5fh2), because
/// the serving peer did not choose the height of a genuine block. Here the parent is the proof, so
/// scoring is safe: a genuine block whose parent we hold is at most one above the tip, so its real
/// height is never far ahead.
#[tokio::test]
async fn contradicted_far_ahead_height_is_attributed_and_never_verified() {
    let _init_guard = zebra_test::init();

    let (mut downloads, mut network, mut verifier, mut state) =
        mock_downloads(lookahead_boundary_tip());

    // Block 2's header with block 10's body: the hash and parent are block 2's, because the hash
    // covers only the header, but the coinbase now claims height 10 instead of 2.
    let block_2 = block_2();
    let block_10 = block_10();
    let block = Arc::new(Block {
        header: block_2.header.clone(),
        transactions: block_10.transactions.clone(),
    });
    let hash = block.hash();
    assert_eq!(
        hash,
        block_2.hash(),
        "the block hash covers only the header"
    );
    assert_eq!(
        block.coinbase_height(),
        Some(Height(10)),
        "the body must claim the height the downloader reads"
    );

    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    assert!(
        matches!(
            downloads.download_and_verify(hash, Some(advertiser)),
            DownloadAction::AddedToQueue
        ),
        "download is queued"
    );

    state
        .expect_request(zs::Request::KnownBlock(hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    network
        .expect_request(zn::Request::BlocksByHash(iter::once(hash).collect()))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    // We hold the real parent, block 1, so the body's real height is 2, not the 10 it claims.
    let block_1 = block_1();
    state
        .expect_request(zs::Request::BlockHeader(
            block.header.previous_block_hash.into(),
        ))
        .await
        .respond(zs::Response::BlockHeader {
            header: block_1.header.clone(),
            hash: block_1.hash(),
            height: Height(1),
            next_block_hash: Some(hash),
        });

    let (error, advertiser_addr) = downloads
        .next()
        .await
        .expect("downloads is non-empty")
        .expect_err("block above the lookahead limit is dropped");

    assert!(
        matches!(
            error.downcast_ref::<HeightLimitError>(),
            Some(HeightLimitError::AboveLookahead { .. })
        ),
        "a contradicted far-ahead height must be a scoreable typed error, but was: {error:?}"
    );
    assert_eq!(
        advertiser_addr,
        Some(advertiser),
        "a contradicted far-ahead height must attribute the drop to the supplying peer"
    );

    verifier.expect_no_requests().await;
}

/// A peer that serves a genuinely far-ahead block is not attributed, so it cannot be scored.
///
/// This is the common case while Zebra is catching up: the block's parent is not in the state yet,
/// so there is no proof of misbehaviour and the drop must stay anonymous.
#[tokio::test]
async fn genuinely_far_ahead_block_is_dropped_without_attribution() {
    let _init_guard = zebra_test::init();

    let (mut downloads, mut network, mut verifier, mut state) =
        mock_downloads(lookahead_boundary_tip());

    let block = block_10();
    let hash = block.hash();
    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    assert!(
        matches!(
            downloads.download_and_verify(hash, Some(advertiser)),
            DownloadAction::AddedToQueue
        ),
        "download is queued"
    );

    state
        .expect_request(zs::Request::KnownBlock(hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    network
        .expect_request(zn::Request::BlocksByHash(iter::once(hash).collect()))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    // The parent, block 9, is far ahead too, so we do not hold it.
    state
        .expect_request(zs::Request::BlockHeader(
            block.header.previous_block_hash.into(),
        ))
        .await
        .respond(Err(zn::BoxError::from("block hash or height not found")));

    let (error, advertiser_addr) = downloads
        .next()
        .await
        .expect("downloads is non-empty")
        .expect_err("block above the lookahead limit is dropped");

    assert!(
        matches!(
            error.downcast_ref::<HeightLimitError>(),
            Some(HeightLimitError::AboveLookahead { .. })
        ),
        "a far-ahead drop must be a typed error, but was: {error:?}"
    );
    assert_eq!(
        advertiser_addr, None,
        "a genuinely far-ahead block must not be attributed to its peer"
    );

    verifier.expect_no_requests().await;
}

/// A block at the highest height that is still within the lookahead limit is verified, not dropped.
///
/// This is the guard against dropping and attributing honest near-boundary blocks:
/// `max_lookahead_height` is the boundary, and the far-ahead comparison is strict, so a block exactly
/// on it is a normal download and the parent is never consulted.
#[tokio::test]
async fn block_at_lookahead_boundary_is_verified_not_dropped() {
    let _init_guard = zebra_test::init();

    let (mut downloads, mut network, mut verifier, mut state) =
        mock_downloads(lookahead_boundary_tip());

    let block = block_2();
    let hash = block.hash();
    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    assert!(
        matches!(
            downloads.download_and_verify(hash, Some(advertiser)),
            DownloadAction::AddedToQueue
        ),
        "download is queued"
    );

    state
        .expect_request(zs::Request::KnownBlock(hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    network
        .expect_request(zn::Request::BlocksByHash(iter::once(hash).collect()))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    verifier
        .expect_request(zebra_consensus::Request::Commit(block))
        .await
        .respond(hash);

    assert_eq!(
        downloads
            .next()
            .await
            .expect("downloads is non-empty")
            .expect("block on the lookahead boundary is verified"),
        hash,
        "a block at max_lookahead_height must be verified, not dropped as far ahead"
    );

    // The parent lookup only runs on the height limit drop paths, so a boundary block must not
    // trigger one.
    state.expect_no_requests().await;
}
