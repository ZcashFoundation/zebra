//! Randomised property tests for MetaAddr.

use std::{
    convert::{TryFrom, TryInto},
    env,
    sync::Arc,
    time::Duration,
};

use proptest::prelude::*;
use tokio::{runtime::Runtime, time::Instant};
use tower::service_fn;
use tracing::Span;

use zebra_chain::serialization::{ZcashDeserialize, ZcashSerialize};

use super::check;
use crate::{
    constants::LIVE_PEER_DURATION,
    meta_addr::{arbitrary::MAX_ADDR_CHANGE, MetaAddr, MetaAddrChange, PeerAddrState::*},
    peer_set::candidate_set::CandidateSet,
    AddressBook, Config,
};

/// The number of test cases to use for proptest that have verbose failures.
///
/// Set this to the default number of proptest cases, unless you're debugging a
/// failure.
const DEFAULT_VERBOSE_TEST_PROPTEST_CASES: u32 = 256;

proptest! {
    /// Make sure that the sanitize function reduces time and state metadata
    /// leaks.
    #[test]
    fn sanitize_avoids_leaks(addr in MetaAddr::arbitrary()) {
        zebra_test::init();

        if let Some(sanitized) = addr.sanitize() {
            check::sanitize_avoids_leaks(&addr, &sanitized);
        }
    }

    /// Test round-trip serialization for gossiped MetaAddrs
    #[test]
    fn gossiped_roundtrip(
        gossiped_addr in MetaAddr::gossiped_strategy()
    ) {
        zebra_test::init();

        // We require sanitization before serialization
        let gossiped_addr = gossiped_addr.sanitize();
        prop_assume!(gossiped_addr.is_some());
        let gossiped_addr = gossiped_addr.unwrap();

        // Check that malicious peers can't make Zebra's serialization fail
        let addr_bytes = gossiped_addr.zcash_serialize_to_vec();
        prop_assert!(
            addr_bytes.is_ok(),
            "unexpected serialization error: {:?}, addr: {:?}",
            addr_bytes,
            gossiped_addr
        );
        let addr_bytes = addr_bytes.unwrap();

        // Assume other implementations deserialize like Zebra
        let deserialized_addr = MetaAddr::zcash_deserialize(addr_bytes.as_slice());
        prop_assert!(
            deserialized_addr.is_ok(),
            "unexpected deserialization error: {:?}, addr: {:?}, bytes: {:?}",
            deserialized_addr,
            gossiped_addr,
            hex::encode(addr_bytes),
        );
        let deserialized_addr = deserialized_addr.unwrap();

        // Check that the addrs are equal
        prop_assert_eq!(
            gossiped_addr,
            deserialized_addr,
            "unexpected round-trip mismatch with bytes: {:?}",
            hex::encode(addr_bytes),
        );

        // Now check that the re-serialized bytes are equal
        // (`impl PartialEq for MetaAddr` might not match serialization equality)
        let addr_bytes2 = deserialized_addr.zcash_serialize_to_vec();
        prop_assert!(
            addr_bytes2.is_ok(),
            "unexpected serialization error after round-trip: {:?}, original addr: {:?}, bytes: {:?}, deserialized addr: {:?}",
            addr_bytes2,
            gossiped_addr,
            hex::encode(addr_bytes),
            deserialized_addr,
        );
        let addr_bytes2 = addr_bytes2.unwrap();

        prop_assert_eq!(
            &addr_bytes,
            &addr_bytes2,
            "unexpected round-trip bytes mismatch: original addr: {:?}, bytes: {:?}, deserialized addr: {:?}, bytes: {:?}",
            gossiped_addr,
            hex::encode(&addr_bytes),
            deserialized_addr,
            hex::encode(&addr_bytes2),
        );

    }

    /// Test round-trip serialization for all MetaAddr variants after sanitization
    #[test]
    fn sanitized_roundtrip(
        addr in any::<MetaAddr>()
    ) {
        zebra_test::init();

        // We require sanitization before serialization,
        // but we also need the original address for this test
        let sanitized_addr = addr.sanitize();
        prop_assume!(sanitized_addr.is_some());
        let sanitized_addr = sanitized_addr.unwrap();

        // Make sure sanitization avoids leaks on this address, to avoid spurious errors
        check::sanitize_avoids_leaks(&addr, &sanitized_addr);

        // Check that sanitization doesn't make Zebra's serialization fail
        let addr_bytes = sanitized_addr.zcash_serialize_to_vec();
        prop_assert!(
            addr_bytes.is_ok(),
            "unexpected serialization error: {:?}, addr: {:?}",
            addr_bytes,
            sanitized_addr
        );
        let addr_bytes = addr_bytes.unwrap();

        // Assume other implementations deserialize like Zebra
        let deserialized_addr = MetaAddr::zcash_deserialize(addr_bytes.as_slice());
        prop_assert!(
            deserialized_addr.is_ok(),
            "unexpected deserialization error: {:?}, addr: {:?}, bytes: {:?}",
            deserialized_addr,
            sanitized_addr,
            hex::encode(addr_bytes),
        );
        let deserialized_addr = deserialized_addr.unwrap();

        // Check that the addrs are equal
        prop_assert_eq!(
            sanitized_addr,
            deserialized_addr,
            "unexpected round-trip mismatch with bytes: {:?}",
            hex::encode(addr_bytes),
        );

        // Check that serialization hasn't de-sanitized anything
        check::sanitize_avoids_leaks(&addr, &deserialized_addr);

        // Now check that the re-serialized bytes are equal
        // (`impl PartialEq for MetaAddr` might not match serialization equality)
        let addr_bytes2 = deserialized_addr.zcash_serialize_to_vec();
        prop_assert!(
            addr_bytes2.is_ok(),
            "unexpected serialization error after round-trip: {:?}, original addr: {:?}, bytes: {:?}, deserialized addr: {:?}",
            addr_bytes2,
            sanitized_addr,
            hex::encode(addr_bytes),
            deserialized_addr,
        );
        let addr_bytes2 = addr_bytes2.unwrap();

        prop_assert_eq!(
            &addr_bytes,
            &addr_bytes2,
            "unexpected double-serialization round-trip mismatch with original addr: {:?}, bytes: {:?}, deserialized addr: {:?}, bytes: {:?}",
            sanitized_addr,
            hex::encode(&addr_bytes),
            deserialized_addr,
            hex::encode(&addr_bytes2),
        );

    }

    /// Make sure that [`MetaAddrChange`]s:
    /// - do not modify the last seen time, unless it was None, and
    /// - only modify the services after a response or failure.
    #[test]
    fn preserve_initial_untrusted_values((mut addr, changes) in MetaAddrChange::addr_changes_strategy(MAX_ADDR_CHANGE)) {
        zebra_test::init();

        for change in changes {
            if let Some(changed_addr) = change.apply_to_meta_addr(addr) {
                // untrusted last seen times:
                // check that we replace None with Some, but leave Some unchanged
                if addr.untrusted_last_seen.is_some() {
                    prop_assert_eq!(changed_addr.untrusted_last_seen, addr.untrusted_last_seen);
                } else {
                    prop_assert_eq!(changed_addr.untrusted_last_seen, change.untrusted_last_seen());
                }

                // services:
                // check that we only change if there was a handshake
                if changed_addr.last_connection_state.is_never_attempted()
                    || changed_addr.last_connection_state == AttemptPending
                    || change.untrusted_services().is_none() {
                    prop_assert_eq!(changed_addr.services, addr.services);
                }

                addr = changed_addr;
            }
        }
    }

    /// Make sure that [`MetaAddr`]s do not get retried more than once per
    /// [`LIVE_PEER_DURATION`], regardless of the [`MetaAddrChange`]s that are
    /// applied to them.
    ///
    /// This is the simple version of the test, which checks [`MetaAddr`]s by
    /// themselves. It detects bugs in [`MetaAddr`]s which have compensating bugs
    /// in the [`CandidateSet`] or [`AddressBook`].
    #[test]
    fn individual_peer_retry_limit_meta_addr(
        (mut addr, changes) in MetaAddrChange::addr_changes_strategy(MAX_ADDR_CHANGE)
    ) {
        zebra_test::init();

        let mut attempt_count: usize = 0;

        for change in changes {
            while addr.is_valid_for_outbound() && addr.is_ready_for_attempt() {
                attempt_count += 1;
                // Assume that this test doesn't last longer than LIVE_PEER_DURATION
                prop_assert!(attempt_count <= 1);

                // Simulate an attempt
                addr = MetaAddr::new_reconnect(&addr.addr)
                    .apply_to_meta_addr(addr)
                    .expect("unexpected invalid attempt");
            }

            if let Some(changed_addr) = change.apply_to_meta_addr(addr) {
                addr = changed_addr;
            }
        }
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
    /// [`LIVE_PEER_DURATION`], regardless of the [`MetaAddrChange`]s that are
    /// applied to a single peer's entries in the [`AddressBook`].
    ///
    /// This is the complex version of the test, which checks [`MetaAddr`],
    /// [`CandidateSet`] and [`AddressBook`] together.
    #[test]
    fn individual_peer_retry_limit_candidate_set(
        (addr, changes) in MetaAddrChange::addr_changes_strategy(MAX_ADDR_CHANGE)
    ) {
        zebra_test::init();

        // Run the test for this many simulated live peer durations
        const LIVE_PEER_INTERVALS: u32 = 3;
        // Run the test for this much simulated time
        let overall_test_time: Duration = LIVE_PEER_DURATION * LIVE_PEER_INTERVALS;
        // Advance the clock by this much for every peer change
        let peer_change_interval: Duration = overall_test_time / MAX_ADDR_CHANGE.try_into().unwrap();

        assert!(
            u32::try_from(MAX_ADDR_CHANGE).unwrap() >= 3 * LIVE_PEER_INTERVALS,
            "there are enough changes for good test coverage",
        );

        let runtime = Runtime::new().expect("Failed to create Tokio runtime");
        let _guard = runtime.enter();

        // Only put valid addresses in the address book.
        // This means some tests will start with an empty address book.
        let addrs = if addr.is_valid_for_outbound() {
            Some(addr)
        } else {
            None
        };

        let address_book = Arc::new(std::sync::Mutex::new(AddressBook::new_with_addrs(&Config::default(), Span::none(), addrs)));
        let peer_service = service_fn(|_| async { unreachable!("Service should not be called") });
        let mut candidate_set = CandidateSet::new(address_book.clone(), peer_service);

        let mut attempt_count: usize = 0;

        runtime.block_on(async move {
            tokio::time::pause();

            // The earliest time we can have a valid next attempt for this peer
            let earliest_next_attempt = Instant::now() + LIVE_PEER_DURATION;

            for (i, change) in changes.into_iter().enumerate() {
                while let Some(candidate_addr) = candidate_set.next().await {
                    assert_eq!(candidate_addr.addr, addr.addr);
                    attempt_count += 1;
                    assert!(
                        attempt_count <= 1,
                        "candidate: {:?}, change: {}, now: {:?}, earliest next attempt: {:?}, attempts: {}, live peer interval limit: {}, test time limit: {:?}, peer change interval: {:?}, original addr was in address book: {}",
                        candidate_addr,
                        i,
                        Instant::now(),
                        earliest_next_attempt,
                        attempt_count,
                        LIVE_PEER_INTERVALS,
                        overall_test_time,
                        peer_change_interval,
                        addr.is_valid_for_outbound(),
                    );
                }

                // Changes can be invalid for the current MetaAddr state,
                // so multiple intervals can elapse between actual changes to
                // the MetaAddr in the AddressBook
                address_book.clone().lock().unwrap().update(change);

                tokio::time::advance(peer_change_interval).await;
                if Instant::now() >= earliest_next_attempt {
                    attempt_count = 0;
                }
            }
        });
    }

    // TODO: Make sure that [`MetaAddr`]s:
    // - all disconnected [`MetaAddr`]s in a particular state are retried once,
    //   before any are retried twice.
}
