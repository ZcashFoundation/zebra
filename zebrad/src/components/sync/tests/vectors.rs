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
}

/// Test that calls to [`ChainSync::request_genesis`] are rate limited.
#[test]
fn request_genesis_is_rate_limited() {
    // create some counters that will be updated inside async blocks
    let peer_requests_counter = Arc::new(AtomicU8::new(0));
    let peer_requests_counter_clone = Arc::clone(&peer_requests_counter);
    let state_requests_counter = Arc::new(AtomicU8::new(0));
    let state_requests_counter_clone = Arc::clone(&state_requests_counter);
    let verifier_requests_counter = Arc::new(AtomicU8::new(0));
    let verifier_requests_counter_clone = Arc::clone(&verifier_requests_counter);

    let runtime = Runtime::new().expect("Failed to create Tokio runtime");
    let _guard = runtime.enter();

    // create a fake peer service that respond with `Nil` to `BlocksByHash` or
    // panic in any other type of request.
    let peer_service = tower::service_fn(move |request| {
        match request {
            zebra_network::Request::BlocksByHash(_) => {
                // Track the call
                peer_requests_counter_clone.fetch_add(1, Ordering::SeqCst);
                // Respond with `Nil`
                future::ok(zebra_network::Response::Nil)
            }
            _ => panic!("no other request is allowed"),
        }
    });

    // create a state service that respond with `None` to `Depth` or
    // panic in any other type of request.
    let state_service = tower::service_fn(move |request| {
        match request {
            zebra_state::Request::Depth(_) => {
                // Track the call
                state_requests_counter_clone.fetch_add(1, Ordering::SeqCst);
                // Respond with `None`
                future::ok(zebra_state::Response::Depth(None))
            }
            _ => panic!("no other request is allowed"),
        }
    });

    // create a verifier service that respond always with `GENESIS_PREVIOUS_BLOCK_HASH`
    let verifier_service = tower::service_fn(move |_| {
        // Track the call
        verifier_requests_counter_clone.fetch_add(1, Ordering::SeqCst);
        // Respond with `GENESIS_PREVIOUS_BLOCK_HASH`
        future::ok(zebra_chain::parameters::GENESIS_PREVIOUS_BLOCK_HASH)
    });

    // start the sync
    let mut chain_sync = ChainSync::new(
        &ZebradConfig::default(),
        peer_service,
        state_service,
        verifier_service,
    );

    // run `request_genesis()` with a timeout of 15 seconds
    runtime.block_on(async move {
        let _ = timeout(Duration::from_secs(15), chain_sync.request_genesis()).await;
    });

    // In 15 seconds and as we have a rate limit of 5 seconds in `request_genesis()` ...

    // we should have 3 calls to the peer service
    assert_eq!(peer_requests_counter.load(Ordering::SeqCst), 3);
    // we should have 3 calls to the state service
    assert_eq!(state_requests_counter.load(Ordering::SeqCst), 3);
    // we should have 3 calls to the verifier
    assert_eq!(verifier_requests_counter.load(Ordering::SeqCst), 0);
}
