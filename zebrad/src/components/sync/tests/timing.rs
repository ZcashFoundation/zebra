use futures::future;
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};
use tokio::{
    runtime::Runtime,
    time::{timeout, Duration},
};

use super::super::*;
use crate::config::ZebradConfig;

/// Make sure the timeout values are consistent with each other.
#[test]
fn ensure_timeouts_consistent() {
    zebra_test::init();

    // This constraint clears the download pipeline during a restart
    assert!(
        SYNC_RESTART_DELAY.as_secs() > 2 * BLOCK_DOWNLOAD_TIMEOUT.as_secs(),
        "Sync restart should allow for pending and buffered requests to complete"
    );

    // This constraint avoids spurious failures due to block retries timing out.
    // We multiply by 2, because the Hedge can wait up to BLOCK_DOWNLOAD_TIMEOUT
    // seconds before retrying.
    const BLOCK_DOWNLOAD_HEDGE_TIMEOUT: u64 =
        2 * BLOCK_DOWNLOAD_RETRY_LIMIT as u64 * BLOCK_DOWNLOAD_TIMEOUT.as_secs();
    assert!(
        SYNC_RESTART_DELAY.as_secs() > BLOCK_DOWNLOAD_HEDGE_TIMEOUT,
        "Sync restart should allow for block downloads to time out on every retry"
    );

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
        GENESIS_TIMEOUT_RETRY.as_secs() > zebra_network::constants::HANDSHAKE_TIMEOUT.as_secs()
            && GENESIS_TIMEOUT_RETRY.as_secs() < BLOCK_DOWNLOAD_TIMEOUT.as_secs(),
        "Genesis retries should wait for new peers, but they shouldn't wait too long"
    );
}

/// Test that calls to [`ChainSync::request_genesis`] are rate limited.
#[test]
fn request_genesis_is_rate_limited() {
    zebra_test::init();

    // The number of calls to `request_genesis()` we are going to be testing for
    const RETRIES_TO_RUN: u8 = 3;

    // create some counters that will be updated inside async blocks
    let peer_requests_counter = Arc::new(AtomicU8::new(0));
    let peer_requests_counter_in_service = Arc::clone(&peer_requests_counter);
    let state_requests_counter = Arc::new(AtomicU8::new(0));
    let state_requests_counter_in_service = Arc::clone(&state_requests_counter);

    let runtime = Runtime::new().expect("Failed to create Tokio runtime");
    let _guard = runtime.enter();

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
            zebra_state::Request::Depth(_) => {
                // Track the call
                state_requests_counter_in_service.fetch_add(1, Ordering::SeqCst);
                // Respond with `None`
                future::ok(zebra_state::Response::Depth(None))
            }
            _ => unreachable!("no other request is allowed"),
        }
    });

    // create a verifier service that will always panic as it will never be called
    let verifier_service =
        tower::service_fn(
            move |_| async move { unreachable!("no request to this service is allowed") },
        );

    // start the sync
    let mut chain_sync = ChainSync::new(
        &ZebradConfig::default(),
        peer_service,
        state_service,
        verifier_service,
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
