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
    DEFAULT_CRAWL_NEW_PEER_INTERVAL, HANDSHAKE_TIMEOUT, INVENTORY_ROTATION_INTERVAL,
};
use zebra_state::ChainTipSender;

use crate::{
    components::sync::{
        ChainSync, BLOCK_DOWNLOAD_RETRY_LIMIT, BLOCK_DOWNLOAD_TIMEOUT, BLOCK_VERIFY_TIMEOUT,
        GENESIS_TIMEOUT_RETRY, SYNC_RESTART_DELAY,
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

    // We multiply by 2, because the Hedge can wait up to BLOCK_DOWNLOAD_TIMEOUT
    // seconds before retrying.
    const BLOCK_DOWNLOAD_HEDGE_TIMEOUT: u64 =
        2 * BLOCK_DOWNLOAD_RETRY_LIMIT as u64 * BLOCK_DOWNLOAD_TIMEOUT.as_secs();

    // This constraint avoids spurious failures due to block download timeouts
    assert!(
        BLOCK_VERIFY_TIMEOUT.as_secs()
            > SYNC_RESTART_DELAY.as_secs()
                + BLOCK_DOWNLOAD_HEDGE_TIMEOUT
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
#[test]
fn request_genesis_is_rate_limited() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    // The number of calls to `request_genesis()` we are going to be testing for
    const RETRIES_TO_RUN: u8 = 3;

    // create some counters that will be updated inside async blocks
    let peer_requests_counter = Arc::new(AtomicU8::new(0));
    let peer_requests_counter_in_service = Arc::clone(&peer_requests_counter);
    let state_requests_counter = Arc::new(AtomicU8::new(0));
    let state_requests_counter_in_service = Arc::clone(&state_requests_counter);

    // create a fake peer service that respond with `Error` to `BlocksByHash` or
    // panic in any other type of request.
    let peer_service = tower::service_fn(move |request| {
        match request {
            zebra_network::Request::BlocksByHash(_) => {
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
        latest_chain_tip,
        misbehavior_tx,
    );

    // run `request_genesis()` with a timeout of 13 seconds
    runtime.block_on(async move {
        // allow extra wall clock time for tests on CPU-bound machines
        let retries_timeout = (RETRIES_TO_RUN - 1) as u64 * GENESIS_TIMEOUT_RETRY.as_secs()
            + GENESIS_TIMEOUT_RETRY.as_secs() / 2;
        let _ = timeout(
            Duration::from_secs(retries_timeout),
            chain_sync.request_genesis(),
        )
        .await;
    });

    let peer_requests_counter = peer_requests_counter.load(Ordering::SeqCst);
    assert!(peer_requests_counter >= RETRIES_TO_RUN);
    assert!(peer_requests_counter <= RETRIES_TO_RUN * (BLOCK_DOWNLOAD_RETRY_LIMIT as u8) * 2);
    assert_eq!(
        state_requests_counter.load(Ordering::SeqCst),
        RETRIES_TO_RUN
    );
}
