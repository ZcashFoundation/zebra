//! Randomised property tests for candidate peer selection.

use std::{
    env,
    net::SocketAddr,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use proptest::{
    collection::{hash_map, vec},
    prelude::*,
};
use tokio::time::{sleep, timeout};
use tracing::Span;

use zebra_chain::{parameters::Network::*, serialization::DateTime32};

use crate::{
    canonical_peer_addr,
    constants::{DEFAULT_MAX_CONNS_PER_IP, MIN_OUTBOUND_PEER_CONNECTION_INTERVAL},
    meta_addr::{MetaAddr, MetaAddrChange},
    protocol::types::PeerServices,
    AddressBook, BoxError, Request, Response,
};

use super::super::{validate_addrs, CandidateSet};

/// The maximum number of candidates for a "next peer" test.
const MAX_TEST_CANDIDATES: u32 = 4;

/// The number of random test addresses for each test.
const TEST_ADDRESSES: usize = 2 * MAX_TEST_CANDIDATES as usize;

/// The default number of proptest cases for each test that includes sleeps.
const DEFAULT_SLEEP_TEST_PROPTEST_CASES: u32 = 16;

/// The largest extra delay we allow after every test sleep.
///
/// This makes the tests more reliable on machines with high CPU load.
const MAX_SLEEP_EXTRA_DELAY: Duration = Duration::from_secs(1);

proptest! {
    /// Test that validated gossiped peers never have a `last_seen` time that's in the future.
    #[test]
    fn no_last_seen_times_are_in_the_future(
        gossiped_peers in vec(MetaAddr::gossiped_strategy(), 1..TEST_ADDRESSES),
        last_seen_limit in any::<DateTime32>(),
    ) {
        let _init_guard = zebra_test::init();

        let validated_peers = validate_addrs(gossiped_peers, last_seen_limit);

        for peer in validated_peers {
            prop_assert!(peer.untrusted_last_seen().unwrap() <= last_seen_limit);
        }
    }

    /// Test that the outbound peer connection rate limit is only applied when
    /// a peer address is actually returned.
    ///
    /// TODO: after PR #2275 merges, add a similar proptest,
    ///       using a "not ready for attempt" peer generation strategy
    #[test]
    fn skipping_outbound_peer_connection_skips_rate_limit(next_peer_attempts in 0..TEST_ADDRESSES) {
        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();

        let peer_service = tower::service_fn(|_| async {
            unreachable!("Mock peer service is never used");
        });

        // Since the address book is empty, there won't be any available peers
        let address_book = AddressBook::new(SocketAddr::from_str("0.0.0.0:0").unwrap(), Mainnet, DEFAULT_MAX_CONNS_PER_IP, Span::none());

        let mut candidate_set = CandidateSet::new(Arc::new(std::sync::Mutex::new(address_book)), peer_service);

        // Make sure that the rate-limit is never triggered, even after multiple calls
        for _ in 0..next_peer_attempts {
            // An empty address book immediately returns "no next peer".
            //
            // Check that it takes less than the peer set candidate delay,
            // and hope that is enough time for test machines with high CPU load.
            let less_than_min_interval = MIN_OUTBOUND_PEER_CONNECTION_INTERVAL - Duration::from_millis(1);
            assert_eq!(runtime.block_on(timeout(less_than_min_interval, candidate_set.next())), Ok(None));
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
        peers in hash_map(MetaAddrChange::ready_outbound_strategy_ip(), MetaAddrChange::ready_outbound_strategy_port(), TEST_ADDRESSES),
        initial_candidates in 0..MAX_TEST_CANDIDATES,
        extra_candidates in 0..MAX_TEST_CANDIDATES,
    ) {
        let (runtime, _init_guard) = zebra_test::init_async();
        let _guard = runtime.enter();

        let peers = peers.into_iter().map(|(ip, port)| {
            MetaAddr::new_alternate(canonical_peer_addr(SocketAddr::new(ip, port)), &PeerServices::NODE_NETWORK)
        }).collect::<Vec<_>>();

        let peer_service = tower::service_fn(|_| async {
            unreachable!("Mock peer service is never used");
        });

        let mut address_book = AddressBook::new(SocketAddr::from_str("0.0.0.0:0").unwrap(), Mainnet, DEFAULT_MAX_CONNS_PER_IP, Span::none());
        address_book.extend(peers);

        let mut candidate_set = CandidateSet::new(Arc::new(std::sync::Mutex::new(address_book)), peer_service);

        let checks = async move {
            // Check rate limiting for initial peers
            check_candidates_rate_limiting(&mut candidate_set, initial_candidates).await;
            // Sleep more than the rate limiting delay
            sleep(MAX_TEST_CANDIDATES * MIN_OUTBOUND_PEER_CONNECTION_INTERVAL).await;
            // Check that the next peers are still respecting the rate limiting, without causing a
            // burst of reconnections
            check_candidates_rate_limiting(&mut candidate_set, extra_candidates).await;
        };

        // Allow enough time for the maximum number of candidates,
        // plus some extra time for test machines with high CPU load.
        // (The max delay asserts usually fail before hitting this timeout.)
        let max_rate_limit_sleep = 3 * MAX_TEST_CANDIDATES * MIN_OUTBOUND_PEER_CONNECTION_INTERVAL;
        let max_extra_delay = (2 * MAX_TEST_CANDIDATES + 1) * MAX_SLEEP_EXTRA_DELAY;
        assert!(runtime.block_on(timeout(max_rate_limit_sleep + max_extra_delay, checks)).is_ok());
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
    S: tower::Service<Request, Response = Response, Error = BoxError> + Send,
    S::Future: Send + 'static,
{
    let mut now = Instant::now();
    let mut minimum_reconnect_instant = now;
    // Allow extra time for test machines with high CPU load
    let mut maximum_reconnect_instant = now + MAX_SLEEP_EXTRA_DELAY;

    for _ in 0..candidates {
        assert!(
            candidate_set.next().await.is_some(),
            "there are enough available candidates"
        );

        now = Instant::now();
        assert!(
            now >= minimum_reconnect_instant,
            "all candidates should obey the minimum rate-limit: now: {now:?} min: {minimum_reconnect_instant:?}",
        );
        assert!(
            now <= maximum_reconnect_instant,
            "rate-limited candidates should not be delayed too long: now: {now:?} max: {maximum_reconnect_instant:?}. Hint: is the test machine overloaded?",
        );

        minimum_reconnect_instant = now + MIN_OUTBOUND_PEER_CONNECTION_INTERVAL;
        maximum_reconnect_instant =
            now + MIN_OUTBOUND_PEER_CONNECTION_INTERVAL + MAX_SLEEP_EXTRA_DELAY;
    }
}
