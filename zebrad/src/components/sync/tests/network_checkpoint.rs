//! Consumer regressions using ChainSync's real download deadline, retries, hedge and peer clients.

use std::{sync::Arc, time::Duration};

use futures::StreamExt;
use tokio::time::{sleep, sleep_until, timeout, Instant};
use zebra_chain::{
    block::{self, Block, Height},
    chain_tip::mock::MockChainTip,
    serialization::ZcashDeserializeInto,
};
use zebra_network::{
    self as zn,
    constants::REQUEST_TIMEOUT,
    test_support::PeerSetHarness,
    types::{InventoryHash, PeerServices},
    InventoryResponse::Available,
    NetworkService,
};
use zebra_state as zs;
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::{components::sync::ChainSync, config::ZebradConfig};

const REQUEST_BOUND: Duration = Duration::from_secs(1);
const SERVING_AND_FALLBACK: [PeerServices; 2] = [PeerServices::NODE_NETWORK, PeerServices::empty()];

type Verifier = MockService<zebra_consensus::Request, block::Hash, PanicAssertion>;
type TestSync = ChainSync<
    NetworkService,
    MockService<zs::Request, zs::Response, PanicAssertion>,
    MockService<zs::ReadRequest, zs::ReadResponse, PanicAssertion>,
    Verifier,
    MockChainTip,
>;

fn new_sync(network: NetworkService) -> (TestSync, Verifier) {
    let verifier = MockService::build()
        .with_max_request_delay(REQUEST_BOUND)
        .for_unit_tests();
    let (tip, sender) = MockChainTip::new();
    sender.send_best_tip_height(Height(0));
    let (misbehavior, _reports) = tokio::sync::mpsc::channel(1);
    let (sync, _) = ChainSync::new(
        &ZebradConfig::default(),
        Height(1_000_000),
        network,
        verifier.clone(),
        MockService::build().for_unit_tests(),
        MockService::build().for_unit_tests(),
        tip,
        misbehavior,
    );
    (sync, verifier)
}

fn block_1() -> Arc<Block> {
    zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("the block 1 vector is valid")
}

fn block_2() -> Arc<Block> {
    zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .expect("the block 2 vector is valid")
}

fn missing(hash: block::Hash) -> zn::SharedPeerError {
    zn::PeerError::NotFoundResponse(vec![InventoryHash::Block(hash)]).into()
}

async fn finish_block(sync: &mut TestSync, verifier: &mut Verifier, block: Arc<Block>) {
    let hash = block.hash();
    verifier
        .expect_request(zebra_consensus::Request::Commit(block.clone()))
        .await
        .respond(hash);
    assert_eq!(
        timeout(REQUEST_BOUND, sync.downloads.next())
            .await
            .expect("verification must finish promptly")
            .expect("the download is present")
            .expect("the supplied block must reach verification"),
        (block.coinbase_height().unwrap(), hash),
    );
}

async fn recover_busy_source(immediate_misses: usize) {
    let mut services = vec![PeerServices::NODE_NETWORK; immediate_misses + 1];
    services.push(PeerServices::empty());
    let mut peers = PeerSetHarness::new(&services, Some(0)).await;
    let (mut sync, mut verifier) = new_sync(peers.network.clone());
    let block = block_1();
    let hash = block.hash();
    let started = Instant::now();
    let download = tokio::spawn(async move {
        sync.request_blocks([hash].into_iter().collect())
            .await
            .unwrap();
        while let Some(result) = sync.downloads.next().await {
            if let Ok(block) = result {
                return Some(block);
            }
            sync.handle_block_response(result).unwrap();
            sync.reobtain_missing_blocks().await;
        }
        None
    });

    let fast_peers: Vec<_> = (1..=immediate_misses).collect();
    for _ in 0..immediate_misses {
        let (_, request) = peers.next_request_from(&fast_peers).await;
        assert_eq!(
            request.request(),
            &zn::Request::BlockDownload {
                hash,
                advertiser: None,
            },
        );
        request.respond(Err(missing(hash)));
    }

    // Recovery is strictly within the advertised peer-response bound, not after it.
    sleep_until(started + REQUEST_TIMEOUT - Duration::from_millis(100)).await;
    assert!(
        !download.is_finished(),
        "immediate notfound replies must not exhaust capacity waits before the source can recover",
    );
    peers.release_busy_peer(0).await;
    let request = peers.next_request(0).await;
    assert_eq!(
        request.request(),
        &zn::Request::BlockDownload {
            hash,
            advertiser: None,
        },
    );
    // Queuing must not consume the peer's wire-response budget after dispatch.
    sleep(REQUEST_TIMEOUT - Duration::from_millis(100)).await;
    assert!(
        !request.is_canceled(),
        "the recovered source still has its full response window",
    );
    request.respond(Ok(zn::Response::Blocks(vec![Available((
        block.clone(),
        Some(peers.addrs[0]),
    ))])));
    verifier
        .expect_request(zebra_consensus::Request::Commit(block))
        .await
        .respond(hash);
    assert_eq!(
        timeout(REQUEST_BOUND, download).await.unwrap().unwrap(),
        Some((Height(1), hash)),
    );
}

/// N3: three real client misses leave too few local capacity waits for a live source.
#[tokio::test(start_paused = true)]
async fn mixed_notfound_attempts_preserve_live_source() {
    let _init_guard = zebra_test::init();
    recover_busy_source(3).await;
}

/// Preservation control: the existing all-waits schedule survives the same recovery time.
#[tokio::test(start_paused = true)]
async fn capacity_waits_without_immediate_misses_preserve_live_source() {
    let _init_guard = zebra_test::init();
    recover_busy_source(0).await;
}

/// N13: reset cancels unrelated in-flight bodies without changing source preference.
#[tokio::test(start_paused = true)]
async fn sync_reset_does_not_fail_unrelated_peers() {
    let _init_guard = zebra_test::init();
    let mut peers = PeerSetHarness::new(&SERVING_AND_FALLBACK, None).await;
    let (mut sync, mut verifier) = new_sync(peers.network.clone());
    let block = block_1();
    sync.downloads
        .download_and_verify(block.hash())
        .await
        .unwrap();
    let first = peers.next_request(0).await;
    sync.downloads
        .download_and_verify(block_2().hash())
        .await
        .unwrap();
    let unrelated = peers.next_request(0).await;
    sync.downloads.cancel_all();
    sleep(Duration::from_millis(1)).await;
    assert!(first.is_canceled() && unrelated.is_canceled());
    peers.flush().await;

    sync.downloads
        .download_and_verify(block.hash())
        .await
        .unwrap();
    let retry = peers.next_request(0).await;
    assert_eq!(retry.request(), first.request());
    retry.respond(Ok(zn::Response::Blocks(vec![Available((
        block.clone(),
        Some(peers.addrs[0]),
    ))])));
    finish_block(&mut sync, &mut verifier, block).await;
}
