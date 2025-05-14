//! Randomised property tests for MetaAddr and MetaAddrChange.

use std::{collections::HashMap, env, net::SocketAddr, str::FromStr, sync::Arc, time::Duration};

use chrono::Utc;
use proptest::{collection::vec, prelude::*};
use tower::service_fn;
use tracing::Span;

use zebra_chain::{parameters::Network::*, serialization::DateTime32};

use crate::{
    constants::{
        DEFAULT_MAX_CONNS_PER_IP, MAX_ADDRS_IN_ADDRESS_BOOK, MAX_RECENT_PEER_AGE,
        MIN_PEER_RECONNECTION_DELAY,
    },
    meta_addr::{
        arbitrary::{MAX_ADDR_CHANGE, MAX_META_ADDR},
        MetaAddr, MetaAddrChange,
        PeerAddrState::*,
    },
    peer_set::candidate_set::CandidateSet,
    protocol::{external::canonical_peer_addr, types::PeerServices},
    AddressBook, PeerSocketAddr,
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
        let _init_guard = zebra_test::init();

        if let Some(sanitized) = addr.sanitize(&Mainnet) {
            // check that all sanitized addresses are valid for outbound
            prop_assert!(addr.last_known_info_is_valid_for_outbound(&Mainnet));
            // also check the address, port, and services individually
            prop_assert!(!addr.addr.ip().is_unspecified());
            prop_assert_ne!(addr.addr.port(), 0);
            prop_assert_eq!(addr.misbehavior(), 0);
            prop_assert!(!addr.is_inbound());

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
        let _init_guard = zebra_test::init();

        let instant_now = std::time::Instant::now();
        let chrono_now = Utc::now();
        let local_now: DateTime32 = chrono_now.try_into().expect("will succeed until 2038");

        for change in changes {
            if let Some(changed_addr) = change.apply_to_meta_addr(addr, instant_now, chrono_now) {
                // untrusted last seen times:
                // check that we replace None with Some, but leave Some unchanged
                if addr.untrusted_last_seen.is_some() {
                    prop_assert_eq!(changed_addr.untrusted_last_seen, addr.untrusted_last_seen);
                } else {
                    prop_assert_eq!(
                        changed_addr.untrusted_last_seen,
                        change.untrusted_last_seen(local_now)
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
        let _init_guard = zebra_test::init();

        let instant_now = std::time::Instant::now();
        let chrono_now = Utc::now();

        let mut attempt_count: usize = 0;

        for change in changes {
            while addr.is_ready_for_connection_attempt(instant_now, chrono_now, &Mainnet) {
                // Simulate an attempt
                addr = if let Some(addr) = MetaAddr::new_reconnect(addr.addr)
                    .apply_to_meta_addr(addr, instant_now, chrono_now) {
                        attempt_count += 1;
                        // Assume that this test doesn't last longer than MIN_PEER_RECONNECTION_DELAY
                        prop_assert!(attempt_count <= 1);
                        addr
                    } else {
                        // Stop updating when an attempt comes too soon after a failure.
                        // In production these are prevented by the dialer code.
                        break;
                    }
            }

            // If `change` is invalid for the current MetaAddr state, skip it.
            if let Some(changed_addr) = change.apply_to_meta_addr(addr, instant_now, chrono_now) {
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
        let _init_guard = zebra_test::init();

        let chrono_now = Utc::now();

        let address_book = AddressBook::new_with_addrs(
            local_listener,
            &Mainnet,
            DEFAULT_MAX_CONNS_PER_IP,
            MAX_ADDRS_IN_ADDRESS_BOOK,
            Span::none(),
            address_book_addrs
        );
        let sanitized_addrs = address_book.sanitized(chrono_now);

        let expected_local_listener = address_book.local_listener_meta_addr(chrono_now);
        let canonical_local_listener = canonical_peer_addr(local_listener);
        let book_sanitized_local_listener = sanitized_addrs
            .iter()
            .find(|meta_addr| meta_addr.addr == canonical_local_listener);

        // invalid addresses should be removed by sanitization,
        // regardless of where they have come from
        prop_assert_eq!(
            book_sanitized_local_listener.cloned(),
            expected_local_listener.sanitize(&Mainnet),
            "address book: {:?}, sanitized_addrs: {:?}, canonical_local_listener: {:?}",
            address_book,
            sanitized_addrs,
            canonical_local_listener,
        );
    }

    /// Make sure that [`MetaAddrChange`]s are correctly applied
    /// when there is no [`MetaAddr`] in the address book.
    ///
    /// TODO: Make sure that [`MetaAddrChange`]s are correctly applied,
    ///       regardless of the [`MetaAddr`] that is currently in the address book.
    #[test]
    fn new_meta_addr_from_meta_addr_change(
        (addr, changes) in MetaAddrChange::addr_changes_strategy(MAX_ADDR_CHANGE)
    ) {
        let _init_guard = zebra_test::init();

        let local_listener = "0.0.0.0:0".parse().expect("unexpected invalid SocketAddr");

        let instant_now = std::time::Instant::now();
        let chrono_now = Utc::now();

        for change in changes {
            // Check direct application
            let new_addr = change.apply_to_meta_addr(None, instant_now, chrono_now);

            prop_assert!(
                new_addr.is_some(),
                "applying a change to `None` should always result in a new address,\n \
                 change: {:?}",
                change,
            );

            let new_addr = new_addr.expect("just checked is_some");
            prop_assert_eq!(new_addr.addr, addr.addr);

            // Check address book update - return value
            let mut address_book = AddressBook::new_with_addrs(
                local_listener,
                &Mainnet,
                DEFAULT_MAX_CONNS_PER_IP,
                1,
                Span::none(),
                Vec::new(),
            );

            let expected_result = new_addr;
            let book_result = address_book.update(change);
            let book_contents: Vec<MetaAddr> = address_book.peers().collect();

            // Ignore the same addresses that the address book ignores
            let expected_result = if !expected_result.address_is_valid_for_outbound(&Mainnet)
                || ( !expected_result.last_known_info_is_valid_for_outbound(&Mainnet)
                      && expected_result.last_connection_state.is_never_attempted())
            {
               None
            } else {
                Some(expected_result)
            };

            prop_assert_eq!(
                book_result.is_some(),
                expected_result.is_some(),
                "applying a change to an empty address book should return a new address,\n \
                 unless its info is invalid,\n \
                 change: {:?},\n \
                 address book returned: {:?},\n \
                 expected result: {:?}",
                change,
                book_result,
                expected_result,
            );

            if let Some(book_result) = book_result {
                prop_assert_eq!(book_result.addr, addr.addr);
                // TODO: pass times to MetaAddrChange::apply_to_meta_addr and AddressBook::update,
                //       so the times are equal
                // prop_assert_eq!(new_addr, book_result);
            }

            // Check address book update - address book contents
            prop_assert_eq!(
                !book_contents.is_empty(), expected_result.is_some(),
                "applying a change to an empty address book should add a new address,\n \
                 unless its info is invalid,\n \
                 change: {:?},\n \
                 address book contains: {:?},\n \
                 expected result: {:?}",
                change,
                book_contents,
                expected_result,
            );

            if let Some(book_contents) = book_contents.first() {
                prop_assert_eq!(book_contents.addr, addr.addr);
                // TODO: pass times to MetaAddrChange::apply_to_meta_addr and AddressBook::update,
                //       so the times are equal
                //prop_assert_eq!(new_addr, *book_contents);
            }

            prop_assert_eq!(book_result.as_ref(), book_contents.first());

            // TODO: do we need to check each field is calculated correctly as well?
        }
    }
}

proptest! {
    // These tests can produce a lot of debug output,
    // so we let developers configure them with a smaller number of cases.
    //
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
        let (runtime, _init_guard) = zebra_test::init_async();
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
        let addrs = if addr.last_known_info_is_valid_for_outbound(&Mainnet) {
            Some(addr)
        } else {
            None
        };

        let address_book = Arc::new(std::sync::Mutex::new(AddressBook::new_with_addrs(
            SocketAddr::from_str("0.0.0.0:0").unwrap(),
            &Mainnet,
            DEFAULT_MAX_CONNS_PER_IP,
            MAX_ADDRS_IN_ADDRESS_BOOK,
            Span::none(),
            addrs,
        )));
        let peer_service = service_fn(|_| async { unreachable!("Service should not be called") });
        let mut candidate_set = CandidateSet::new(address_book.clone(), peer_service);

        runtime.block_on(async move {
            tokio::time::pause();

            // The earliest time we can have a valid next attempt for this peer
            let earliest_next_attempt = tokio::time::Instant::now() + MIN_PEER_RECONNECTION_DELAY;

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
                        tokio::time::Instant::now(),
                        earliest_next_attempt,
                        attempt_count,
                        LIVE_PEER_INTERVALS,
                        overall_test_time,
                        peer_change_interval,
                        addr.last_known_info_is_valid_for_outbound(&Mainnet),
                    );
                }

                // If `change` is invalid for the current MetaAddr state,
                // multiple intervals will elapse between actual changes to
                // the MetaAddr in the AddressBook.
                address_book.clone().lock().unwrap().update(change);

                tokio::time::advance(peer_change_interval).await;
                if tokio::time::Instant::now() >= earliest_next_attempt {
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
        let (runtime, _init_guard) = zebra_test::init_async();
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
            let mut attempt_counts: HashMap<PeerSocketAddr, u32> = HashMap::new();

            // The most recent address info for each peer
            let mut addrs: HashMap<PeerSocketAddr, MetaAddr> = HashMap::new();

            for change_index in 0..MAX_ADDR_CHANGE {
                for (addr, changes) in addr_changes_lists.iter() {
                    let addr = addrs.entry(addr.addr).or_insert(*addr);
                    let change = changes.get(change_index);

                    while addr.is_ready_for_connection_attempt(instant_now, chrono_now, &Mainnet) {
                        // Simulate an attempt
                        *addr = if let Some(addr) = MetaAddr::new_reconnect(addr.addr)
                            .apply_to_meta_addr(*addr, instant_now, chrono_now) {
                                *attempt_counts.entry(addr.addr).or_default() += 1;
                                prop_assert!(
                                    *attempt_counts.get(&addr.addr).unwrap() <= LIVE_PEER_INTERVALS + 1
                                );
                                addr
                            } else {
                                // Stop updating when an attempt comes too soon after a failure.
                                // In production these are prevented by the dialer code.
                                break;
                            }
                    }

                    // If `change` is invalid for the current MetaAddr state, skip it.
                    // If we've run out of changes for this addr, do nothing.
                    if let Some(changed_addr) = change.and_then(|change| change.apply_to_meta_addr(*addr, instant_now, chrono_now))
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
