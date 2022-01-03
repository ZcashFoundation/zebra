//! Randomised property tests for MetaAddr.

use std::{
    collections::HashMap, convert::TryFrom, env, net::SocketAddr, str::FromStr, sync::Arc,
    time::Duration,
};

use chrono::Utc;
use proptest::{collection::vec, prelude::*};
use tokio::time::Instant;
use tower::service_fn;
use tracing::Span;

use zebra_chain::serialization::DateTime32;

use crate::{
    constants::{MAX_ADDRS_IN_ADDRESS_BOOK, MAX_RECENT_PEER_AGE, MIN_PEER_RECONNECTION_DELAY},
    meta_addr::{
        arbitrary::{MAX_ADDR_CHANGE, MAX_META_ADDR},
        MetaAddr, MetaAddrChange,
        PeerAddrState::*,
    },
    peer_set::candidate_set::CandidateSet,
    protocol::{external::canonical_socket_addr, types::PeerServices},
    AddressBook,
};

use super::check;

/// The number of test cases to use for proptest that have verbose failures.
///
/// Set this to the default number of proptest cases, unless you're debugging a
/// failure.
const DEFAULT_VERBOSE_TEST_PROPTEST_CASES: u32 = 256;

proptest! {
    /// Make sure that the sanitize function reduces time and state metadata
    /// leaks for valid addresses.
    ///
    /// Make sure that the sanitize function skips invalid IP addresses, ports,
    /// and client services.
    #[test]
    fn sanitize_avoids_leaks(addr in MetaAddr::arbitrary()) {
        zebra_test::init();

        if let Some(sanitized) = addr.sanitize() {
            // check that all sanitized addresses are valid for outbound
            prop_assert!(addr.last_known_info_is_valid_for_outbound());
            // also check the address, port, and services individually
            prop_assert!(!addr.addr.ip().is_unspecified());
            prop_assert_ne!(addr.addr.port(), 0);

            if let Some(services) = addr.services {
                prop_assert!(services.contains(PeerServices::NODE_NETWORK));
            }

            check::sanitize_avoids_leaks(&addr, &sanitized);
        }
    }

    /// Make sure that [`MetaAddrChange`]s:
    /// - do not modify the last seen time, unless it was None, and
    /// - only modify the services after a response or failure.
    #[test]
    fn preserve_initial_untrusted_values(
        (mut addr, changes) in MetaAddrChange::addr_changes_strategy(MAX_ADDR_CHANGE),
    ) {
        zebra_test::init();

        for change in changes {
            if let Some(changed_addr) = change.apply_to_meta_addr(addr) {
                // untrusted last seen times:
                // check that we replace None with Some, but leave Some unchanged
                if addr.untrusted_last_seen.is_some() {
                    prop_assert_eq!(changed_addr.untrusted_last_seen, addr.untrusted_last_seen);
                } else {
                    prop_assert_eq!(
                        changed_addr.untrusted_last_seen,
                        change.untrusted_last_seen()
                    );
                }

                // services:
                // check that we only change if there was a handshake
                if addr.services.is_some()
                    && (changed_addr.last_connection_state.is_never_attempted()
                        || changed_addr.last_connection_state == AttemptPending
                        || change.untrusted_services().is_none())
                {
                    prop_assert_eq!(changed_addr.services, addr.services);
                }

                addr = changed_addr;
            }
        }
    }

    /// Make sure that [`MetaAddr`]s do not get retried more than once per
    /// [`MIN_PEER_RECONNECTION_DELAY`], regardless of the [`MetaAddrChange`]s that are
    /// applied to them.
    ///
    /// This is the simple version of the test, which checks [`MetaAddr`]s by
    /// themselves. It detects bugs in [`MetaAddr`]s, even if there are
    /// compensating bugs in the [`CandidateSet`] or [`AddressBook`].
    #[test]
    fn individual_peer_retry_limit_meta_addr(
        (mut addr, changes) in MetaAddrChange::addr_changes_strategy(MAX_ADDR_CHANGE)
    ) {
        zebra_test::init();

        let instant_now = std::time::Instant::now();
        let chrono_now = Utc::now();

        let mut attempt_count: usize = 0;

        for change in changes {
            while addr.is_ready_for_connection_attempt(instant_now, chrono_now) {
                attempt_count += 1;
                // Assume that this test doesn't last longer than MIN_PEER_RECONNECTION_DELAY
                prop_assert!(attempt_count <= 1);

                // Simulate an attempt
                addr = MetaAddr::new_reconnect(&addr.addr)
                    .apply_to_meta_addr(addr)
                    .expect("unexpected invalid attempt");
            }

            // If `change` is invalid for the current MetaAddr state, skip it.
            if let Some(changed_addr) = change.apply_to_meta_addr(addr) {
                prop_assert_eq!(changed_addr.addr, addr.addr);
                addr = changed_addr;
            }
        }
    }

    /// Make sure that a sanitized [`AddressBook`] contains the local listener
    /// [`MetaAddr`], regardless of the previous contents of the address book.
    ///
    /// If Zebra gossips its own listener address to peers, and gets it back,
    /// its address book will contain its local listener address. This address
    /// will likely be in [`PeerAddrState::Failed`], due to failed
    /// self-connection attempts.
    #[test]
    fn sanitized_address_book_contains_local_listener(
        local_listener in any::<SocketAddr>(),
        address_book_addrs in vec(any::<MetaAddr>(), 0..MAX_META_ADDR),
    ) {
        zebra_test::init();

        let chrono_now = Utc::now();

        let address_book = AddressBook::new_with_addrs(
            local_listener,
            MAX_ADDRS_IN_ADDRESS_BOOK,
            Span::none(),
            address_book_addrs
        );
        let sanitized_addrs = address_book.sanitized(chrono_now);

        let expected_local_listener = address_book.local_listener_meta_addr();
        let canonical_local_listener = canonical_socket_addr(local_listener);
        let book_sanitized_local_listener = sanitized_addrs
            .iter()
            .find(|meta_addr| meta_addr.addr == canonical_local_listener);

        // invalid addresses should be removed by sanitization,
        // regardless of where they have come from
        prop_assert_eq!(
            book_sanitized_local_listener.cloned(),
            expected_local_listener.sanitize(),
            "address book: {:?}, sanitized_addrs: {:?}, canonical_local_listener: {:?}",
            address_book,
            sanitized_addrs,
            canonical_local_listener,
        );
    }
}

proptest! {
    // These tests can produce a lot of debug output, so we use a smaller number of cases by default.
    // Set the PROPTEST_CASES env var to override this default.
    #![proptest_config(proptest::test_runner::Config::with_cases(env::var("PROPTEST_CASES")
                                          .ok()
                                          .and_then(|v| v.parse().ok())
                                          .unwrap_or(DEFAULT_VERBOSE_TEST_PROPTEST_CASES)))]

    /// Make sure that [`MetaAddr`]s do not get retried more than once per
    /// [`MIN_PEER_RECONNECTION_DELAY`], regardless of the [`MetaAddrChange`]s that are
    /// applied to a single peer's entries in the [`AddressBook`].
    ///
    /// This is the complex version of the test, which checks [`MetaAddr`],
    /// [`CandidateSet`] and [`AddressBook`] together.
    #[test]
    fn individual_peer_retry_limit_candidate_set(
        (addr, changes) in MetaAddrChange::addr_changes_strategy(MAX_ADDR_CHANGE)
    ) {
        let runtime = zebra_test::init_async();
        let _guard = runtime.enter();

        // Run the test for this many simulated live peer durations
        const LIVE_PEER_INTERVALS: u32 = 3;
        // Run the test for this much simulated time
        let overall_test_time: Duration = MIN_PEER_RECONNECTION_DELAY * LIVE_PEER_INTERVALS;
        // Advance the clock by this much for every peer change
        let peer_change_interval: Duration =
            overall_test_time / u32::try_from(MAX_ADDR_CHANGE).unwrap();

        prop_assert!(
            u32::try_from(MAX_ADDR_CHANGE).unwrap() >= 3 * LIVE_PEER_INTERVALS,
            "there are enough changes for good test coverage",
        );

        // Only put valid addresses in the address book.
        // This means some tests will start with an empty address book.
        let addrs = if addr.last_known_info_is_valid_for_outbound() {
            Some(addr)
        } else {
            None
        };

        let address_book = Arc::new(std::sync::Mutex::new(AddressBook::new_with_addrs(
            SocketAddr::from_str("0.0.0.0:0").unwrap(),
            MAX_ADDRS_IN_ADDRESS_BOOK,
            Span::none(),
            addrs,
        )));
        let peer_service = service_fn(|_| async { unreachable!("Service should not be called") });
        let mut candidate_set = CandidateSet::new(address_book.clone(), peer_service);

        runtime.block_on(async move {
            tokio::time::pause();

            // The earliest time we can have a valid next attempt for this peer
            let earliest_next_attempt = Instant::now() + MIN_PEER_RECONNECTION_DELAY;

            // The number of attempts for this peer in the last MIN_PEER_RECONNECTION_DELAY
            let mut attempt_count: usize = 0;

            for (i, change) in changes.into_iter().enumerate() {
                while let Some(candidate_addr) = candidate_set.next().await {
                    prop_assert_eq!(candidate_addr.addr, addr.addr);

                    attempt_count += 1;
                    prop_assert!(
                        attempt_count <= 1,
                        "candidate: {:?},\n \
                         change: {},\n \
                         now: {:?},\n \
                         earliest next attempt: {:?},\n \
                         attempts: {}, live peer interval limit: {},\n \
                         test time limit: {:?}, peer change interval: {:?},\n \
                         original addr was in address book: {}\n",
                        candidate_addr,
                        i,
                        Instant::now(),
                        earliest_next_attempt,
                        attempt_count,
                        LIVE_PEER_INTERVALS,
                        overall_test_time,
                        peer_change_interval,
                        addr.last_known_info_is_valid_for_outbound(),
                    );
                }

                // If `change` is invalid for the current MetaAddr state,
                // multiple intervals will elapse between actual changes to
                // the MetaAddr in the AddressBook.
                address_book.clone().lock().unwrap().update(change);

                tokio::time::advance(peer_change_interval).await;
                if Instant::now() >= earliest_next_attempt {
                    attempt_count = 0;
                }
            }

            Ok(())
        })?;
    }

    /// Make sure that all disconnected [`MetaAddr`]s are retried once, before
    /// any are retried twice.
    ///
    /// This is the simple version of the test, which checks [`MetaAddr`]s by
    /// themselves. It detects bugs in [`MetaAddr`]s, even if there are
    /// compensating bugs in the [`CandidateSet`] or [`AddressBook`].
    //
    // TODO: write a similar test using the AddressBook and CandidateSet
    #[test]
    fn multiple_peer_retry_order_meta_addr(
        addr_changes_lists in vec(
            MetaAddrChange::addr_changes_strategy(MAX_ADDR_CHANGE),
            2..MAX_ADDR_CHANGE
        ),
    ) {
        let runtime = zebra_test::init_async();
        let _guard = runtime.enter();

        let instant_now = std::time::Instant::now();
        let chrono_now = Utc::now();

        // Run the test for this many simulated live peer durations
        const LIVE_PEER_INTERVALS: u32 = 3;
        // Run the test for this much simulated time
        let overall_test_time: Duration = MIN_PEER_RECONNECTION_DELAY * LIVE_PEER_INTERVALS;
        // Advance the clock by this much for every peer change
        let peer_change_interval: Duration =
            overall_test_time / u32::try_from(MAX_ADDR_CHANGE).unwrap();

        prop_assert!(
            u32::try_from(MAX_ADDR_CHANGE).unwrap() >= 3 * LIVE_PEER_INTERVALS,
            "there are enough changes for good test coverage",
        );

        let attempt_counts = runtime.block_on(async move {
            tokio::time::pause();

            // The current attempt counts for each peer in this interval
            let mut attempt_counts: HashMap<SocketAddr, u32> = HashMap::new();

            // The most recent address info for each peer
            let mut addrs: HashMap<SocketAddr, MetaAddr> = HashMap::new();

            for change_index in 0..MAX_ADDR_CHANGE {
                for (addr, changes) in addr_changes_lists.iter() {
                    let addr = addrs.entry(addr.addr).or_insert(*addr);
                    let change = changes.get(change_index);

                    while addr.is_ready_for_connection_attempt(instant_now, chrono_now) {
                        *attempt_counts.entry(addr.addr).or_default() += 1;
                        prop_assert!(
                            *attempt_counts.get(&addr.addr).unwrap() <= LIVE_PEER_INTERVALS + 1
                        );

                        // Simulate an attempt
                        *addr = MetaAddr::new_reconnect(&addr.addr)
                            .apply_to_meta_addr(*addr)
                            .expect("unexpected invalid attempt");
                    }

                    // If `change` is invalid for the current MetaAddr state, skip it.
                    // If we've run out of changes for this addr, do nothing.
                    if let Some(changed_addr) = change.and_then(|change| change.apply_to_meta_addr(*addr))
                    {
                        prop_assert_eq!(changed_addr.addr, addr.addr);
                        *addr = changed_addr;
                    }
                }

                tokio::time::advance(peer_change_interval).await;
            }

            Ok(attempt_counts)
        })?;

        let min_attempts = attempt_counts.values().min();
        let max_attempts = attempt_counts.values().max();
        if let (Some(&min_attempts), Some(&max_attempts)) = (min_attempts, max_attempts) {
            prop_assert!(max_attempts >= min_attempts);
            prop_assert!(max_attempts - min_attempts <= 1);
        }
    }

    /// Make sure check if a peer was recently seen is correct.
    #[test]
    fn last_seen_is_recent_is_correct(peer in any::<MetaAddr>()) {
        let chrono_now = Utc::now();

        let time_since_last_seen = peer
            .last_seen()
            .map(|last_seen| last_seen.saturating_elapsed(chrono_now));

        let recently_seen = time_since_last_seen
            .map(|elapsed| elapsed <= MAX_RECENT_PEER_AGE)
            .unwrap_or(false);

        prop_assert_eq!(
            peer.last_seen_is_recent(chrono_now),
            recently_seen,
            "last seen: {:?}, now: {:?}",
            peer.last_seen(),
            DateTime32::now(),
        );
    }

    /// Make sure a peer is correctly determined to be probably reachable.
    #[test]
    fn probably_rechable_is_determined_correctly(peer in any::<MetaAddr>()) {

        let chrono_now = Utc::now();

        let last_attempt_failed = peer.last_connection_state == Failed;
        let not_recently_seen = !peer.last_seen_is_recent(chrono_now);

        let probably_unreachable = last_attempt_failed && not_recently_seen;

        prop_assert_eq!(
            peer.is_probably_reachable(chrono_now),
            !probably_unreachable,
            "last_connection_state: {:?}, last_seen: {:?}",
            peer.last_connection_state,
            peer.last_seen()
        );
    }
}
