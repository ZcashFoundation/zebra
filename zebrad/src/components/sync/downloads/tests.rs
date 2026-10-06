//! Fixed test vectors for the syncer's block download stream.

use std::{iter, sync::Arc, time::Duration};

use futures::stream::StreamExt;
use tokio::sync::watch;

use zebra_chain::{
    block::{self, Block, Height},
    chain_tip::mock::MockChainTip,
    serialization::ZcashDeserializeInto,
};
use zebra_network::{InventoryResponse, PeerSocketAddr};
use zebra_state::MAX_BLOCK_REORG_HEIGHT;
use zebra_test::mock_service::{MockService, PanicAssertion};

use zebra_network as zn;
use zebra_state as zs;

use crate::components::sync::MIN_CHECKPOINT_CONCURRENCY_LIMIT;

use super::{BlockDownloadVerifyError, Downloads};

use InventoryResponse::*;

/// Maximum time to wait for a request to any test service.
///
/// These tests run the download task in parallel with the test, so machines under heavy load need a
/// longer delay.
const MAX_SERVICE_REQUEST_DELAY: Duration = Duration::from_millis(1000);

/// A peer address used to check whether the supplying peer can be scored.
const ADVERTISER: &str = "127.0.0.1:8233";

type MockNetwork = MockService<zn::Request, zn::Response, PanicAssertion>;
type MockVerifier = MockService<zebra_consensus::Request, block::Hash, PanicAssertion>;
type MockState = MockService<zs::ReadRequest, zs::ReadResponse, PanicAssertion>;

/// Build a downloader whose state tip is at `tip_height`, returning the mock network, verifier, and
/// state services so a test can drive and assert on them.
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

    let (past_lookahead_limit_sender, _past_lookahead_limit_receiver) = watch::channel(false);

    // `max_checkpoint_height` is far above the test blocks, so the final-checkpoint verify timeout
    // workaround does not apply.
    let downloads = Downloads::new(
        network.clone(),
        verifier.clone(),
        state.clone(),
        latest_chain_tip,
        past_lookahead_limit_sender,
        MIN_CHECKPOINT_CONCURRENCY_LIMIT,
        Height(1_000_000),
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

/// Regression test for GHSA-g95h-hw6g-pvgv: a body whose claimed height contradicts the parent we
/// already hold is attributed to the peer that supplied it, and never reaches consensus.
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

    downloads
        .download_and_verify(hash)
        .await
        .expect("download is queued");

    network
        .expect_request(zn::Request::BlocksByHash(iter::once(hash).collect()))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    // We hold the real parent, block 1, so the body's real height is 2, not the 1 it claims.
    state
        .expect_request(zs::ReadRequest::BlockHeader(
            block.header.previous_block_hash.into(),
        ))
        .await
        .respond(zs::ReadResponse::BlockHeader {
            header: block_1.header.clone(),
            hash: block_1.hash(),
            height: Height(1),
            next_block_hash: Some(hash),
        });

    let error = downloads
        .next()
        .await
        .expect("downloads is non-empty")
        .expect_err("block behind the reorg limit is dropped");

    assert!(
        matches!(
            error,
            BlockDownloadVerifyError::BehindTipHeightLimit {
                height: Height(1),
                hash: error_hash,
                advertiser_addr: Some(error_advertiser),
            } if error_hash == hash && error_advertiser == advertiser
        ),
        "a contradicted behind-tip height must report the height, hash, and supplying peer, \
         but was: {error:?}"
    );

    // The rewritten body is dropped before consensus validation, which is why the peer must be
    // scored on this path instead.
    verifier.expect_no_requests().await;
}

/// A peer that serves a genuinely old block is not attributed, so it cannot be scored.
///
/// The syncer tolerates an out-of-order first hash in `FindBlocks` responses, so a requested hash
/// can resolve to an old block that an honest peer still holds. Its height agrees with its parent's,
/// so there is no proof of misbehaviour and the drop must stay anonymous.
#[tokio::test]
async fn genuinely_old_block_is_dropped_without_attribution() {
    let _init_guard = zebra_test::init();

    let (mut downloads, mut network, mut verifier, mut state) =
        mock_downloads(Height(2 * MAX_BLOCK_REORG_HEIGHT));

    let block = block_1();
    let hash = block.hash();
    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    downloads
        .download_and_verify(hash)
        .await
        .expect("download is queued");

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
        .expect_request(zs::ReadRequest::BlockHeader(
            block.header.previous_block_hash.into(),
        ))
        .await
        .respond(zs::ReadResponse::BlockHeader {
            header: genesis.header.clone(),
            hash: genesis.hash(),
            height: Height(0),
            next_block_hash: Some(hash),
        });

    let error = downloads
        .next()
        .await
        .expect("downloads is non-empty")
        .expect_err("block behind the reorg limit is dropped");

    assert!(
        matches!(
            error,
            BlockDownloadVerifyError::BehindTipHeightLimit {
                advertiser_addr: None,
                ..
            }
        ),
        "an authentic old block must not be attributed to its peer, but was: {error:?}"
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

    downloads
        .download_and_verify(hash)
        .await
        .expect("download is queued");

    network
        .expect_request(zn::Request::BlocksByHash(iter::once(hash).collect()))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    state
        .expect_request(zs::ReadRequest::BlockHeader(
            block.header.previous_block_hash.into(),
        ))
        .await
        .respond(Err(zn::BoxError::from("block not found in any chain")));

    let error = downloads
        .next()
        .await
        .expect("downloads is non-empty")
        .expect_err("block behind the reorg limit is dropped");

    assert!(
        matches!(
            error,
            BlockDownloadVerifyError::BehindTipHeightLimit {
                advertiser_addr: None,
                ..
            }
        ),
        "a block whose parent we do not hold must not be attributed, but was: {error:?}"
    );

    verifier.expect_no_requests().await;
}

/// The parent lookup is bounded: when the state does not answer, the block is still dropped and
/// the peer is still not attributed.
///
/// Paused time lets the runtime advance past `PARENT_LOOKUP_TIMEOUT` as soon as the download task
/// is the only thing waiting, so the test does not sleep for real.
#[tokio::test(start_paused = true)]
async fn behind_tip_parent_lookup_timeout_is_not_attributed() {
    let _init_guard = zebra_test::init();

    let (mut downloads, mut network, mut verifier, _state) =
        mock_downloads(Height(2 * MAX_BLOCK_REORG_HEIGHT));

    let block = block_1();
    let hash = block.hash();
    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    downloads
        .download_and_verify(hash)
        .await
        .expect("download is queued");

    network
        .expect_request(zn::Request::BlocksByHash(iter::once(hash).collect()))
        .await
        .respond(zn::Response::Blocks(vec![Available((
            block.clone(),
            Some(advertiser),
        ))]));

    // The state service is deliberately never answered, so the lookup can only end by timing out.
    let error = downloads
        .next()
        .await
        .expect("downloads is non-empty")
        .expect_err("block behind the reorg limit is dropped");

    assert!(
        matches!(
            error,
            BlockDownloadVerifyError::BehindTipHeightLimit {
                advertiser_addr: None,
                ..
            }
        ),
        "a timed-out parent lookup must not attribute the drop, but was: {error:?}"
    );

    verifier.expect_no_requests().await;
}

/// A block at the oldest height the syncer can legitimately be served is verified, not dropped.
///
/// This is the guard against dropping and attributing honest near-boundary blocks:
/// `min_accepted_height` is the boundary, and the behind-tip comparison is strict, so a block
/// exactly on it is a normal download and the state is never consulted.
#[tokio::test]
async fn block_at_reorg_boundary_is_verified_not_dropped() {
    let _init_guard = zebra_test::init();

    // `min_accepted_height` is `Height(1)`, so the height-1 block sits exactly on the boundary.
    let (mut downloads, mut network, mut verifier, mut state) =
        mock_downloads(Height(MAX_BLOCK_REORG_HEIGHT + 1));

    let block = block_1();
    let hash = block.hash();
    let advertiser: PeerSocketAddr = ADVERTISER.parse().expect("hard-coded address is valid");

    downloads
        .download_and_verify(hash)
        .await
        .expect("download is queued");

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
        (Height(1), hash),
        "a block at min_accepted_height must be verified, not dropped as behind the tip"
    );

    state.expect_no_requests().await;
}
