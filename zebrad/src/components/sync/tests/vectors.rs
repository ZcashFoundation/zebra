use futures::future;
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};
use tokio::{
    runtime::Runtime,
    time::{timeout, Duration},
};

use super::super::ChainSync;
use crate::config::ZebradConfig;

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
    let verifier_service = tower::service_fn(move |request| {
        match request {
            _ => {
                // Track the call
                verifier_requests_counter_clone.fetch_add(1, Ordering::SeqCst);
                // Respond with `GENESIS_PREVIOUS_BLOCK_HASH`
                future::ok(zebra_chain::parameters::GENESIS_PREVIOUS_BLOCK_HASH)
            }
        }
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
