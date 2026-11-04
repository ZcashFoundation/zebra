//! Check the relationship between various sync timeouts and delays.

#![allow(clippy::unwrap_in_result)]

use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};

use futures::future;
use tokio::time::{timeout, Duration};

use zebra_chain::{
    block::Height,
    parameters::{Network, POST_BLOSSOM_POW_TARGET_SPACING},
};
use zebra_network::constants::{
    DEFAULT_CRAWL_NEW_PEER_INTERVAL, FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT, HANDSHAKE_TIMEOUT,
    INVENTORY_ROTATION_INTERVAL, REQUEST_TIMEOUT,
};
use zebra_state::ChainTipSender;

use crate::{
    components::sync::{
        ChainSync, BLOCK_DOWNLOAD_RETRY_LIMIT, BLOCK_DOWNLOAD_ROUND_TIMEOUT,
        BLOCK_DOWNLOAD_TIMEOUT, BLOCK_VERIFY_TIMEOUT, GENESIS_TIMEOUT_RETRY, SYNC_RESTART_DELAY,
        TIPS_RESPONSE_TIMEOUT,
    },
    config::ZebradConfig,
};

/// Make sure the timeout values are consistent with each other.
#[test]
fn ensure_timeouts_consistent() {
    let _init_guard = zebra_test::init();

    // This constraint clears the download pipeline during a restart
    assert!(
        SYNC_RESTART_DELAY.as_secs() > 2 * BLOCK_DOWNLOAD_TIMEOUT.as_secs(),
        "Sync restart should allow for pending and buffered requests to complete"
    );

    // This constraint avoids spurious failures due to block download timeouts
    assert!(
        BLOCK_VERIFY_TIMEOUT.as_secs()
            > SYNC_RESTART_DELAY.as_secs()
                + BLOCK_DOWNLOAD_ROUND_TIMEOUT.as_secs()
                + BLOCK_DOWNLOAD_TIMEOUT.as_secs(),
        "Block verify should allow for a block timeout, a sync restart, and some block fetches"
    );

    // The minimum recommended network speed for Zebra, in bytes per second.
    const MIN_NETWORK_SPEED_BYTES_PER_SEC: u64 = 10 * 1024 * 1024 / 8;

    // This constraint avoids spurious failures when restarting large checkpoints
    assert!(
       BLOCK_VERIFY_TIMEOUT.as_secs() > SYNC_RESTART_DELAY.as_secs() + 2 * zebra_consensus::MAX_CHECKPOINT_BYTE_COUNT / MIN_NETWORK_SPEED_BYTES_PER_SEC,
       "Block verify should allow for a full checkpoint download, a sync restart, then a full checkpoint re-download"
    );

    // This constraint avoids spurious failures after checkpointing has finished
    assert!(
        BLOCK_VERIFY_TIMEOUT.as_secs()
            > 2 * zebra_chain::parameters::NetworkUpgrade::Blossom
                .target_spacing()
                .num_seconds() as u64,
        "Block verify should allow for at least one new block to be generated and distributed"
    );

    // This constraint makes genesis retries more likely to succeed
    assert!(
        GENESIS_TIMEOUT_RETRY.as_secs() > HANDSHAKE_TIMEOUT.as_secs()
            && GENESIS_TIMEOUT_RETRY.as_secs() < BLOCK_DOWNLOAD_TIMEOUT.as_secs(),
        "Genesis retries should wait for new peers, but they shouldn't wait too long"
    );

    assert!(
        SYNC_RESTART_DELAY.as_secs() < POST_BLOSSOM_POW_TARGET_SPACING.into(),
        "a syncer tip crawl should complete before most new blocks"
    );

    // This is a compromise between two failure modes:
    // - some peers have the inventory, but they weren't ready last time we checked,
    //   so we want to retry soon
    // - all peers are missing the inventory, so we want to wait for a while before retrying
    assert!(
        INVENTORY_ROTATION_INTERVAL < SYNC_RESTART_DELAY,
        "we should expire some inventory every time the syncer resets"
    );
    assert!(
        SYNC_RESTART_DELAY < 2 * INVENTORY_ROTATION_INTERVAL,
        "we should give the syncer at least one retry attempt, \
         before we expire all inventory"
    );

    // A busy peer can finish its current wire response before the queued download is dispatched.
    assert!(
        BLOCK_DOWNLOAD_ROUND_TIMEOUT > REQUEST_TIMEOUT + BLOCK_DOWNLOAD_TIMEOUT,
        "the round deadline must leave time for both readiness and the block response"
    );

    // A find request waits for a busy serving peer, then is sent to any ready peer, all within the
    // syncer's tips response timeout. That peer needs most of the timeout to answer.
    assert!(
        FIND_BUSY_SERVING_PEER_WAIT_TIMEOUT < TIPS_RESPONSE_TIMEOUT / 3,
        "waiting for a busy serving peer should leave most of the tips response timeout \
         for the peer's answer, because timed out tip extensions are not retried"
    );

    // The default peer crawler interval should be at least
    // `HANDSHAKE_TIMEOUT` lower than all other crawler intervals.
    //
    // See `DEFAULT_CRAWL_NEW_PEER_INTERVAL` for details.
    assert!(
        DEFAULT_CRAWL_NEW_PEER_INTERVAL.as_secs() + HANDSHAKE_TIMEOUT.as_secs()
            < SYNC_RESTART_DELAY.as_secs(),
        "an address crawl and peer connections should complete before most syncer tips crawls"
    );
}

/// Test that calls to [`ChainSync::request_genesis`] are rate limited.
#[tokio::test(start_paused = true)]
async fn request_genesis_is_rate_limited() {
    let _init_guard = zebra_test::init();

    // The number of calls to `request_genesis()` we are going to be testing for
    const RETRIES_TO_RUN: u8 = 3;

    // create some counters that will be updated inside async blocks
    let peer_requests_counter = Arc::new(AtomicU8::new(0));
    let peer_requests_counter_in_service = Arc::clone(&peer_requests_counter);
    let state_requests_counter = Arc::new(AtomicU8::new(0));
    let state_requests_counter_in_service = Arc::clone(&state_requests_counter);

    // create a fake peer service that responds with `Error` to `BlockDownload` or
    // panic in any other type of request.
    let peer_service = tower::service_fn(move |request| {
        match request {
            zebra_network::Request::BlockDownload { .. } => {
                // Track the call
                peer_requests_counter_in_service.fetch_add(1, Ordering::SeqCst);
                // Respond with `Error`
                future::err("block not found".into())
            }
            _ => unreachable!("no other request is allowed"),
        }
    });

    // create a state service that respond with `None` to `Depth` or
    // panic in any other type of request.
    let state_service = tower::service_fn(move |request| {
        match request {
            zebra_state::Request::KnownBlock(_) => {
                // Track the call
                state_requests_counter_in_service.fetch_add(1, Ordering::SeqCst);
                // Respond with `None`
                future::ok(zebra_state::Response::KnownBlock(None))
            }
            _ => unreachable!("no other request is allowed"),
        }
    });

    // create a read-only state service that is never called: the genesis download fails before any
    // block body is available to check
    let read_state_service = tower::service_fn(move |_request| async move {
        unreachable!("no request to this service is allowed")
    });

    // create an empty latest chain tip
    let (_sender, latest_chain_tip, _change) = ChainTipSender::new(None, &Network::Mainnet);

    // create a verifier service that will always panic as it will never be called
    let verifier_service =
        tower::service_fn(
            move |_| async move { unreachable!("no request to this service is allowed") },
        );

    // start the sync
    let (misbehavior_tx, _misbehavior_rx) = tokio::sync::mpsc::channel(1);
    let (mut chain_sync, _) = ChainSync::new(
        &ZebradConfig::default(),
        Height(0),
        peer_service,
        verifier_service,
        state_service,
        read_state_service,
        latest_chain_tip,
        misbehavior_tx,
    );

    // Observe three genesis attempts, stopping halfway to the fourth retry.
    let retries_timeout = (RETRIES_TO_RUN - 1) as u64 * GENESIS_TIMEOUT_RETRY.as_secs()
        + GENESIS_TIMEOUT_RETRY.as_secs() / 2;
    let _ = timeout(
        Duration::from_secs(retries_timeout),
        chain_sync.request_genesis(),
    )
    .await;

    let peer_requests_counter = peer_requests_counter.load(Ordering::SeqCst);
    assert!(peer_requests_counter >= RETRIES_TO_RUN);
    // The retry limit is the constant 3, so conversion to u8 is lossless.
    assert!(peer_requests_counter <= RETRIES_TO_RUN * (BLOCK_DOWNLOAD_RETRY_LIMIT as u8) * 2);
    assert_eq!(
        state_requests_counter.load(Ordering::SeqCst),
        RETRIES_TO_RUN
    );
}

/// Repeated failed extensions back off without cancelling an unrelated live verification.
#[tokio::test(start_paused = true)]
async fn failed_tip_extensions_have_a_no_progress_deadline() {
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    use futures::StreamExt;
    use zebra_chain::{
        block::{self, Block},
        chain_tip::mock::MockChainTip,
        serialization::ZcashDeserializeInto,
    };
    use zebra_test::mock_service::MockService;

    use crate::components::sync::FANOUT;

    let _init_guard = zebra_test::init();
    // Keep the pending verification outside the short post-checkpoint timeout window.
    let honest: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_202_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let honest_hash = honest.hash();
    let genesis_hash = Network::Mainnet.genesis_hash();
    let a = block::Hash::from([0xA1; 32]);
    let b = block::Hash::from([0xB1; 32]);
    let started = tokio::time::Instant::now();
    let injected = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(AtomicUsize::new(0));
    let network_requests = requests.clone();
    let network = tower::service_fn(move |request| {
        let block = honest.clone();
        let injected = injected.clone();
        let requests = network_requests.clone();
        async move {
            match request {
                zebra_network::Request::FindBlocks { known_blocks, .. } => {
                    requests.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    let hashes = if known_blocks == vec![genesis_hash] {
                        vec![a, b]
                    } else if started.elapsed() >= BLOCK_VERIFY_TIMEOUT / 2
                        && !injected.swap(true, Ordering::SeqCst)
                    {
                        // Keep the repeated tip (a, b), but also admit an honest block late
                        // enough that its verification is still live when the round ends.
                        vec![b, honest_hash, a, b]
                    } else {
                        vec![b, a, b]
                    };
                    Ok(zebra_network::Response::BlockHashes(hashes))
                }
                zebra_network::Request::BlockDownload { hash, .. } if hash == honest_hash => {
                    Ok(zebra_network::Response::Blocks(vec![
                        zebra_network::InventoryResponse::Available((block, None)),
                    ]))
                }
                zebra_network::Request::BlockDownload { .. } => {
                    Err(crate::BoxError::from("connection reset"))
                }
                _ => unreachable!("only tip and block requests are expected"),
            }
        }
    });
    let state = tower::service_fn(move |request| {
        future::ok::<_, crate::BoxError>(match request {
            zebra_state::Request::BlockLocator => {
                zebra_state::Response::BlockLocator(vec![genesis_hash])
            }
            zebra_state::Request::KnownBlock(_) => zebra_state::Response::KnownBlock(None),
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
    timeout(
        BLOCK_VERIFY_TIMEOUT + Duration::from_secs(2),
        chain_sync.try_to_sync(),
    )
    .await
    .expect("repeated failing extensions must not run forever")
    .expect("the no-progress deadline must preserve unrelated pending downloads");
    assert!(started.elapsed() >= BLOCK_VERIFY_TIMEOUT);
    assert!(requests.load(Ordering::SeqCst) > 5 * FANOUT);

    verifier
        .expect_request_that(|request: &zebra_consensus::Request| {
            request.block().hash() == honest_hash
        })
        .await
        .respond(honest_hash);
    timeout(Duration::from_secs(1), async {
        loop {
            match chain_sync.downloads.next().await {
                Some(Ok((_, hash))) if hash == honest_hash => break,
                Some(result) => chain_sync.handle_block_response(result).unwrap(),
                None => panic!("the live honest verification must not be cancelled"),
            }
        }
    })
    .await
    .expect("the honest verification must finish after the round backs off");
    chain_sync.downloads.cancel_all();
}

/// A tip advance renews the deadline without cancelling a pending tip extension.
#[tokio::test(start_paused = true)]
async fn tip_progress_renews_an_in_progress_sync_step() {
    use zebra_chain::{block, chain_tip::mock::MockChainTip};
    use zebra_test::mock_service::MockService;

    use crate::components::sync::RestartReason;

    let _init_guard = zebra_test::init();
    let genesis_hash = Network::Mainnet.genesis_hash();
    let a = block::Hash::from([0xA1; 32]);
    let b = block::Hash::from([0xB1; 32]);
    let started = tokio::time::Instant::now();
    let network = tower::service_fn(move |request| async move {
        match request {
            zebra_network::Request::FindBlocks { known_blocks, .. } => {
                let hashes = if known_blocks == vec![genesis_hash] {
                    vec![a, b]
                } else {
                    tokio::time::sleep(Duration::from_secs(5) - Duration::from_millis(1)).await;
                    if started.elapsed() > BLOCK_VERIFY_TIMEOUT {
                        Vec::new()
                    } else {
                        vec![b, a, b]
                    }
                };
                Ok(zebra_network::Response::BlockHashes(hashes))
            }
            zebra_network::Request::BlockDownload { .. } => {
                Err(crate::BoxError::from("connection reset"))
            }
            _ => unreachable!("only tip and block requests are expected"),
        }
    });
    let state = tower::service_fn(move |request| {
        future::ok::<_, crate::BoxError>(match request {
            zebra_state::Request::BlockLocator => {
                zebra_state::Response::BlockLocator(vec![genesis_hash])
            }
            zebra_state::Request::KnownBlock(_) => zebra_state::Response::KnownBlock(None),
            _ => unreachable!("only known blocks and the locator are queried"),
        })
    });
    let (tip, tip_sender) = MockChainTip::new();
    let advance_tip = tokio::spawn(async move {
        tokio::time::sleep(BLOCK_VERIFY_TIMEOUT - Duration::from_millis(1)).await;
        tip_sender.send_best_tip_height(Height(1));
        tip_sender
    });
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
    let reason = timeout(
        BLOCK_VERIFY_TIMEOUT + Duration::from_secs(6),
        chain_sync.try_to_sync(),
    )
    .await
    .expect("the pending extension must finish within its original response timeout")
    .unwrap();
    assert_eq!(
        reason,
        RestartReason::ExhaustedTips,
        "recent chain progress must prevent the old deadline from cancelling the extension"
    );
    assert!(started.elapsed() > BLOCK_VERIFY_TIMEOUT);
    let _tip_sender = advance_tip.await.unwrap();
    chain_sync.downloads.cancel_all();
}

