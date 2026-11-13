//! Gossip consumer regressions through the real buffered peer set.

use std::time::Duration;

use futures::StreamExt;
use zebra_network::{test_support::PeerSetHarness, types::PeerServices};

use super::{
    block_1, zn, zs, Available, DownloadAction, Downloads, Height, MockChainTip, MockService,
    MockState, MockVerifier,
};

async fn download_from_known_advertiser(lose_registry_update: bool) {
    let mut peers = PeerSetHarness::new(
        &[PeerServices::NODE_NETWORK, PeerServices::empty()],
        Some(0),
    )
    .await;
    let block = block_1();
    let hash = block.hash();
    peers.advertise(1, hash);
    if lose_registry_update {
        // Both updates arrive without yielding. The one-entry broadcast receiver loses
        // the block's real advertisement, exactly as a lagged inventory registry does.
        peers.advertise(1, zebra_chain::block::Hash([0xAB; 32]));
    }
    peers.flush().await;

    let mut verifier: MockVerifier = MockService::build().for_unit_tests();
    let mut state: MockState = MockService::build().for_unit_tests();
    let (tip, sender) = MockChainTip::new();
    sender.send_best_tip_height(Height(0));
    let mut downloads = Downloads::new(
        2,
        peers.network.clone(),
        tower::timeout::Timeout::new(
            verifier.clone(),
            crate::components::sync::BLOCK_VERIFY_TIMEOUT,
        ),
        state.clone(),
        tip,
    );
    assert!(matches!(
        downloads.download_and_verify(hash, Some(peers.addrs[1])),
        DownloadAction::AddedToQueue,
    ));
    state
        .expect_request(zs::Request::KnownBlock(hash))
        .await
        .respond(zs::Response::KnownBlock(None));

    // The known source is connected and ready; the unrelated serving client stays full.
    // Check delivery and verification, not just the request the downloader constructed.
    let request = peers.next_request(1).await;
    assert_eq!(
        request.request(),
        &zn::Request::BlockDownload {
            hash,
            advertiser: Some(peers.addrs[1]),
        },
    );
    request.respond(Ok(zn::Response::Blocks(vec![Available((
        block.clone(),
        Some(peers.addrs[1]),
    ))])));
    verifier
        .expect_request(zebra_consensus::Request::Commit(block))
        .await
        .respond(hash);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), downloads.next())
            .await
            .expect("the one-shot gossip download must complete")
            .expect("the gossip download is queued")
            .expect("the known advertiser supplies a verifiable block"),
        hash,
    );
}

/// N4: an already-known gossip source remains usable after the registry drops its update.
#[tokio::test(start_paused = true)]
async fn gossip_download_retains_advertiser_after_registry_lag() {
    let _init_guard = zebra_test::init();
    download_from_known_advertiser(true).await;
}

/// Preservation control: the same one-shot consumer succeeds when its registry update survives.
#[tokio::test(start_paused = true)]
async fn gossip_download_uses_advertiser_with_intact_registry() {
    let _init_guard = zebra_test::init();
    download_from_known_advertiser(false).await;
}
