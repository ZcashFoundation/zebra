use std::{
    env,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use proptest::{collection::vec, prelude::*};
use tokio::{
    runtime::Runtime,
    time::{sleep, timeout},
};
use tracing::Span;

use zebra_chain::serialization::DateTime32;

use super::super::{validate_addrs, CandidateSet};
use crate::{
    constants::MIN_PEER_CONNECTION_INTERVAL, types::MetaAddr, AddressBook, BoxError, Config,
    Request, Response,
};

/// The maximum number of candidates for a "next peer" test.
const MAX_TEST_CANDIDATES: u32 = 4;

/// The number of random test addresses for each test.
const TEST_ADDRESSES: usize = 2 * MAX_TEST_CANDIDATES as usize;

/// The default number of proptest cases for each test that includes sleeps.
const DEFAULT_SLEEP_TEST_PROPTEST_CASES: u32 = 16;

proptest! {
    /// Test that validated gossiped peers never have a `last_seen` time that's in the future.
    #[test]
    fn no_last_seen_times_are_in_the_future(
        gossiped_peers in vec(MetaAddr::gossiped_strategy(), 1..TEST_ADDRESSES),
        last_seen_limit in any::<DateTime32>(),
    ) {
        zebra_test::init();

        let validated_peers = validate_addrs(gossiped_peers, last_seen_limit);

        for peer in validated_peers {
            prop_assert![peer.get_last_seen() <= last_seen_limit];
        }
    }
}

proptest! {
    // These tests contain sleeps, so we use a small number of cases by default.
    // Set the PROPTEST_CASES env var to override this default.
    #![proptest_config(proptest::test_runner::Config::with_cases(env::var("PROPTEST_CASES")
                                          .ok()
                                          .and_then(|v| v.parse().ok())
                                          .unwrap_or(DEFAULT_SLEEP_TEST_PROPTEST_CASES)))]

    /// Test that new outbound peer connections are rate-limited.
    #[test]
    fn new_outbound_peer_connections_are_rate_limited(
        peers in vec(MetaAddr::alternate_node_strategy(), TEST_ADDRESSES),
        initial_candidates in 0..MAX_TEST_CANDIDATES,
        extra_candidates in 0..MAX_TEST_CANDIDATES,
    ) {
        zebra_test::init();

        let runtime = Runtime::new().expect("Failed to create Tokio runtime");
        let _guard = runtime.enter();

        let peer_service = tower::service_fn(|_| async {
            unreachable!("Mock peer service is never used");
        });

        let mut address_book = AddressBook::new(&Config::default(), Span::none());
        address_book.extend(peers);

        let mut candidate_set = CandidateSet::new(Arc::new(Mutex::new(address_book)), peer_service);

        let checks = async move {
            // Check rate limiting for initial peers
            check_candidates_rate_limiting(&mut candidate_set, initial_candidates).await;
            // Sleep more than the rate limiting delay
            sleep(MAX_TEST_CANDIDATES * MIN_PEER_CONNECTION_INTERVAL).await;
            // Check that the next peers are still respecting the rate limiting, without causing a
            // burst of reconnections
            check_candidates_rate_limiting(&mut candidate_set, extra_candidates).await;
        };

        // Allow enough time for the maximum number of candidates,
        // plus some extra time for test machines with high CPU load
        let max_sleep = 3 * MAX_TEST_CANDIDATES * MIN_PEER_CONNECTION_INTERVAL;
        assert!(runtime.block_on(timeout(max_sleep + Duration::from_secs(5), checks)).is_ok());
    }
}

/// Check if obtaining a certain number of reconnection peers is rate limited.
///
/// # Panics
///
/// Will panic if:
/// - a connection peer is returned too quickly,
/// - a connection peer is returned too slowly, or
/// - if no reconnection peer is returned at all.
async fn check_candidates_rate_limiting<S>(candidate_set: &mut CandidateSet<S>, candidates: u32)
where
    S: tower::Service<Request, Response = Response, Error = BoxError>,
    S::Future: Send + 'static,
{
    let mut minimum_reconnect_instant = Instant::now();
    // Allow any delay within a peer connection interval of the minimum.
    // This allows a small amount of extra time for test machines with high CPU load.
    // Note: the maximum time check might still be unreliable on loaded VMs
    let mut maximum_reconnect_instant = Instant::now() + MIN_PEER_CONNECTION_INTERVAL;

    for _ in 0..candidates {
        assert!(candidate_set.next().await.is_some());
        assert!(Instant::now() >= minimum_reconnect_instant);
        assert!(Instant::now() <= maximum_reconnect_instant);

        minimum_reconnect_instant += MIN_PEER_CONNECTION_INTERVAL;
        maximum_reconnect_instant += MIN_PEER_CONNECTION_INTERVAL;
    }
}
