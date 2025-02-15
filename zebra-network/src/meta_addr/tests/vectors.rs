//! Fixed test cases for MetaAddr and MetaAddrChange.

use std::time::Instant;

use chrono::Utc;

use zebra_chain::{
    parameters::Network::*,
    serialization::{DateTime32, Duration32},
};

use crate::{
    constants::{CONCURRENT_ADDRESS_CHANGE_PERIOD, MAX_PEER_ACTIVE_FOR_GOSSIP},
    protocol::types::PeerServices,
    PeerSocketAddr,
};

use super::{super::MetaAddr, check};

/// Margin of error for time-based tests.
///
/// This is a short duration to consider as error due to a test's execution time when comparing
/// [`DateTime32`]s.
const TEST_TIME_ERROR_MARGIN: Duration32 = Duration32::from_seconds(1);

/// Make sure that the sanitize function handles minimum and maximum times.
#[test]
fn sanitize_extremes() {
    let _init_guard = zebra_test::init();

    let min_time_entry = MetaAddr {
        addr: "127.0.0.1:8233".parse().unwrap(),
        services: Default::default(),
        untrusted_last_seen: Some(u32::MIN.into()),
        last_response: Some(u32::MIN.into()),
        last_attempt: None,
        last_failure: None,
        last_connection_state: Default::default(),
        misbehavior_score: Default::default(),
        is_inbound: false,
    };

    let max_time_entry = MetaAddr {
        addr: "127.0.0.1:8233".parse().unwrap(),
        services: Default::default(),
        untrusted_last_seen: Some(u32::MAX.into()),
        last_response: Some(u32::MAX.into()),
        last_attempt: None,
        last_failure: None,
        last_connection_state: Default::default(),
        misbehavior_score: Default::default(),
        is_inbound: false,
    };

    if let Some(min_sanitized) = min_time_entry.sanitize(&Mainnet) {
        check::sanitize_avoids_leaks(&min_time_entry, &min_sanitized);
    }
    if let Some(max_sanitized) = max_time_entry.sanitize(&Mainnet) {
        check::sanitize_avoids_leaks(&max_time_entry, &max_sanitized);
    }
}

/// Test if a newly created local listening address is gossipable.
///
/// The local listener [`MetaAddr`] is always considered gossipable.
#[test]
fn new_local_listener_is_gossipable() {
    let _init_guard = zebra_test::init();

    let instant_now = Instant::now();
    let chrono_now = Utc::now();
    let local_now: DateTime32 = chrono_now.try_into().expect("will succeed until 2038");

    let address = PeerSocketAddr::from(([192, 168, 180, 9], 10_000));
    let peer =
        MetaAddr::new_local_listener_change(address).into_new_meta_addr(instant_now, local_now);

    assert!(peer.is_active_for_gossip(chrono_now));
}

/// Test if recently received gossiped peer is gossipable.
#[test]
fn gossiped_peer_reportedly_to_be_seen_recently_is_gossipable() {
    let _init_guard = zebra_test::init();

    let chrono_now = Utc::now();

    let address = PeerSocketAddr::from(([192, 168, 180, 9], 10_000));

    // Report last seen within the reachable interval.
    let offset = MAX_PEER_ACTIVE_FOR_GOSSIP
        .checked_sub(TEST_TIME_ERROR_MARGIN)
        .expect("Test margin is too large");
    let last_seen = DateTime32::now()
        .checked_sub(offset)
        .expect("Offset is too large");

    let peer = MetaAddr::new_gossiped_meta_addr(address, PeerServices::NODE_NETWORK, last_seen);

    assert!(peer.is_active_for_gossip(chrono_now));
}

/// Test if received gossiped peer that was reportedly last seen in the future is gossipable.
#[test]
fn gossiped_peer_reportedly_seen_in_the_future_is_gossipable() {
    let _init_guard = zebra_test::init();

    let chrono_now = Utc::now();

    let address = PeerSocketAddr::from(([192, 168, 180, 9], 10_000));

    // Report last seen in the future
    let last_seen = DateTime32::now()
        .checked_add(MAX_PEER_ACTIVE_FOR_GOSSIP)
        .expect("Reachable peer duration is too large");

    let peer = MetaAddr::new_gossiped_meta_addr(address, PeerServices::NODE_NETWORK, last_seen);

    assert!(peer.is_active_for_gossip(chrono_now));
}

/// Test if gossiped peer that was reported last seen a long time ago is not gossipable.
#[test]
fn gossiped_peer_reportedly_seen_long_ago_is_not_gossipable() {
    let _init_guard = zebra_test::init();

    let chrono_now = Utc::now();

    let address = PeerSocketAddr::from(([192, 168, 180, 9], 10_000));

    // Report last seen just outside the reachable interval.
    let offset = MAX_PEER_ACTIVE_FOR_GOSSIP
        .checked_add(TEST_TIME_ERROR_MARGIN)
        .expect("Test margin is too large");
    let last_seen = DateTime32::now()
        .checked_sub(offset)
        .expect("Offset is too large");

    let peer = MetaAddr::new_gossiped_meta_addr(address, PeerServices::NODE_NETWORK, last_seen);

    assert!(!peer.is_active_for_gossip(chrono_now));
}

/// Test that peer that has just responded is gossipable.
#[test]
fn recently_responded_peer_is_gossipable() {
    let _init_guard = zebra_test::init();

    let instant_now = Instant::now();
    let chrono_now = Utc::now();
    let local_now: DateTime32 = chrono_now.try_into().expect("will succeed until 2038");

    let address = PeerSocketAddr::from(([192, 168, 180, 9], 10_000));
    let peer_seed = MetaAddr::new_initial_peer(address).into_new_meta_addr(instant_now, local_now);

    // Create a peer that has responded
    let peer = MetaAddr::new_responded(address)
        .apply_to_meta_addr(peer_seed, instant_now, chrono_now)
        .expect("Failed to create MetaAddr for responded peer");

    assert!(peer.is_active_for_gossip(chrono_now));
}

/// Test that peer that last responded in the reachable interval is gossipable.
#[test]
fn not_so_recently_responded_peer_is_still_gossipable() {
    let _init_guard = zebra_test::init();

    let instant_now = Instant::now();
    let chrono_now = Utc::now();
    let local_now: DateTime32 = chrono_now.try_into().expect("will succeed until 2038");

    let address = PeerSocketAddr::from(([192, 168, 180, 9], 10_000));
    let peer_seed = MetaAddr::new_initial_peer(address).into_new_meta_addr(instant_now, local_now);

    // Create a peer that has responded
    let mut peer = MetaAddr::new_responded(address)
        .apply_to_meta_addr(peer_seed, instant_now, chrono_now)
        .expect("Failed to create MetaAddr for responded peer");

    // Tweak the peer's last response time to be within the limits of the reachable duration
    let offset = MAX_PEER_ACTIVE_FOR_GOSSIP
        .checked_sub(TEST_TIME_ERROR_MARGIN)
        .expect("Test margin is too large");
    let last_response = DateTime32::now()
        .checked_sub(offset)
        .expect("Offset is too large");

    peer.set_last_response(last_response);

    assert!(peer.is_active_for_gossip(chrono_now));
}

/// Test that peer that responded long ago is not gossipable.
#[test]
fn responded_long_ago_peer_is_not_gossipable() {
    let _init_guard = zebra_test::init();

    let instant_now = Instant::now();
    let chrono_now = Utc::now();
    let local_now: DateTime32 = chrono_now.try_into().expect("will succeed until 2038");

    let address = PeerSocketAddr::from(([192, 168, 180, 9], 10_000));
    let peer_seed = MetaAddr::new_initial_peer(address).into_new_meta_addr(instant_now, local_now);

    // Create a peer that has responded
    let mut peer = MetaAddr::new_responded(address)
        .apply_to_meta_addr(peer_seed, instant_now, chrono_now)
        .expect("Failed to create MetaAddr for responded peer");

    // Tweak the peer's last response time to be outside the limits of the reachable duration
    let offset = MAX_PEER_ACTIVE_FOR_GOSSIP
        .checked_add(TEST_TIME_ERROR_MARGIN)
        .expect("Test margin is too large");
    let last_response = DateTime32::now()
        .checked_sub(offset)
        .expect("Offset is too large");

    peer.set_last_response(last_response);

    assert!(!peer.is_active_for_gossip(chrono_now));
}

/// Test that a change that is delayed for a long time is not applied to the address state.
#[test]
fn long_delayed_change_is_not_applied() {
    let _init_guard = zebra_test::init();

    let instant_now = Instant::now();
    let chrono_now = Utc::now();
    let local_now: DateTime32 = chrono_now.try_into().expect("will succeed until 2038");

    let address = PeerSocketAddr::from(([192, 168, 180, 9], 10_000));
    let peer_seed = MetaAddr::new_initial_peer(address).into_new_meta_addr(instant_now, local_now);

    // Create a peer that has responded
    let peer = MetaAddr::new_responded(address)
        .apply_to_meta_addr(peer_seed, instant_now, chrono_now)
        .expect("Failed to create MetaAddr for responded peer");

    // Create an earlier change to Failed that has been delayed a long time.
    // Failed typically comes after Responded, so it will pass the connection progress check.
    //
    // This is very unlikely in the May 2023 production code,
    // but it can happen due to getting the time, then waiting for the address book mutex.

    // Create some change times that are much earlier
    let instant_early = instant_now - (CONCURRENT_ADDRESS_CHANGE_PERIOD * 3);
    let chrono_early = chrono_now
        - chrono::Duration::from_std(CONCURRENT_ADDRESS_CHANGE_PERIOD * 3)
            .expect("constant is valid");

    let change = MetaAddr::new_errored(address, PeerServices::NODE_NETWORK);
    let outcome = change.apply_to_meta_addr(peer, instant_early, chrono_early);

    assert_eq!(
        outcome, None,
        "\n\
         unexpected application of a much earlier change to a peer:\n\
         change: {change:?}\n\
         times: {instant_early:?} {chrono_early}\n\
         peer: {peer:?}"
    );
}

/// Test that a change that happens a long time after the previous change
/// is applied to the address state, even if it is a revert.
#[test]
fn later_revert_change_is_applied() {
    let _init_guard = zebra_test::init();

    let instant_now = Instant::now();
    let chrono_now = Utc::now();
    let local_now: DateTime32 = chrono_now.try_into().expect("will succeed until 2038");

    let address = PeerSocketAddr::from(([192, 168, 180, 9], 10_000));
    let peer_seed = MetaAddr::new_initial_peer(address).into_new_meta_addr(instant_now, local_now);

    // Create a peer that has responded
    let peer = MetaAddr::new_responded(address)
        .apply_to_meta_addr(peer_seed, instant_now, chrono_now)
        .expect("Failed to create MetaAddr for responded peer");

    // Create an earlier change to AttemptPending that happens a long time later.
    // AttemptPending typically comes before Responded, so it will fail the connection progress
    // check, but that failure should be ignored because it is not concurrent.
    //
    // This is a typical reconnect in production.

    // Create some change times that are much later
    let instant_late = instant_now + (CONCURRENT_ADDRESS_CHANGE_PERIOD * 3);
    let chrono_late = chrono_now
        + chrono::Duration::from_std(CONCURRENT_ADDRESS_CHANGE_PERIOD * 3)
            .expect("constant is valid");

    let change = MetaAddr::new_reconnect(address);
    let outcome = change.apply_to_meta_addr(peer, instant_late, chrono_late);

    assert!(
        outcome.is_some(),
        "\n\
         unexpected skipped much later change to a peer:\n\
         change: {change:?}\n\
         times: {instant_late:?} {chrono_late}\n\
         peer: {peer:?}"
    );
}

/// Test that a concurrent change which reverses the connection state is not applied.
#[test]
fn concurrent_state_revert_change_is_not_applied() {
    let _init_guard = zebra_test::init();

    let instant_now = Instant::now();
    let chrono_now = Utc::now();
    let local_now: DateTime32 = chrono_now.try_into().expect("will succeed until 2038");

    let address = PeerSocketAddr::from(([192, 168, 180, 9], 10_000));
    let peer_seed = MetaAddr::new_initial_peer(address).into_new_meta_addr(instant_now, local_now);

    // Create a peer that has responded
    let peer = MetaAddr::new_responded(address)
        .apply_to_meta_addr(peer_seed, instant_now, chrono_now)
        .expect("Failed to create MetaAddr for responded peer");

    // Create a concurrent change to AttemptPending.
    // AttemptPending typically comes before Responded, so it will fail the progress check.
    //
    // This is likely to happen in production, it just requires a short delay in the earlier change.

    // Create some change times that are earlier but concurrent
    let instant_early = instant_now - (CONCURRENT_ADDRESS_CHANGE_PERIOD / 2);
    let chrono_early = chrono_now
        - chrono::Duration::from_std(CONCURRENT_ADDRESS_CHANGE_PERIOD / 2)
            .expect("constant is valid");

    let change = MetaAddr::new_reconnect(address);
    let outcome = change.apply_to_meta_addr(peer, instant_early, chrono_early);

    assert_eq!(
        outcome, None,
        "\n\
         unexpected application of an early concurrent change to a peer:\n\
         change: {change:?}\n\
         times: {instant_early:?} {chrono_early}\n\
         peer: {peer:?}"
    );

    // Create some change times that are later but concurrent
    let instant_late = instant_now + (CONCURRENT_ADDRESS_CHANGE_PERIOD / 2);
    let chrono_late = chrono_now
        + chrono::Duration::from_std(CONCURRENT_ADDRESS_CHANGE_PERIOD / 2)
            .expect("constant is valid");

    let change = MetaAddr::new_reconnect(address);
    let outcome = change.apply_to_meta_addr(peer, instant_late, chrono_late);

    assert_eq!(
        outcome, None,
        "\n\
         unexpected application of a late concurrent change to a peer:\n\
         change: {change:?}\n\
         times: {instant_late:?} {chrono_late}\n\
         peer: {peer:?}"
    );
}

/// Test that a concurrent change which progresses the connection state is applied.
#[test]
fn concurrent_state_progress_change_is_applied() {
    let _init_guard = zebra_test::init();

    let instant_now = Instant::now();
    let chrono_now = Utc::now();
    let local_now: DateTime32 = chrono_now.try_into().expect("will succeed until 2038");

    let address = PeerSocketAddr::from(([192, 168, 180, 9], 10_000));
    let peer_seed = MetaAddr::new_initial_peer(address).into_new_meta_addr(instant_now, local_now);

    // Create a peer that has responded
    let peer = MetaAddr::new_responded(address)
        .apply_to_meta_addr(peer_seed, instant_now, chrono_now)
        .expect("Failed to create MetaAddr for responded peer");

    // Create a concurrent change to Failed.
    // Failed typically comes after Responded, so it will pass the progress check.
    //
    // This is a typical update in production.

    // Create some change times that are earlier but concurrent
    let instant_early = instant_now - (CONCURRENT_ADDRESS_CHANGE_PERIOD / 2);
    let chrono_early = chrono_now
        - chrono::Duration::from_std(CONCURRENT_ADDRESS_CHANGE_PERIOD / 2)
            .expect("constant is valid");

    let change = MetaAddr::new_errored(address, None);
    let outcome = change.apply_to_meta_addr(peer, instant_early, chrono_early);

    assert!(
        outcome.is_some(),
        "\n\
         unexpected skipped early concurrent change to a peer:\n\
         change: {change:?}\n\
         times: {instant_early:?} {chrono_early}\n\
         peer: {peer:?}"
    );

    // Create some change times that are later but concurrent
    let instant_late = instant_now + (CONCURRENT_ADDRESS_CHANGE_PERIOD / 2);
    let chrono_late = chrono_now
        + chrono::Duration::from_std(CONCURRENT_ADDRESS_CHANGE_PERIOD / 2)
            .expect("constant is valid");

    let change = MetaAddr::new_errored(address, None);
    let outcome = change.apply_to_meta_addr(peer, instant_late, chrono_late);

    assert!(
        outcome.is_some(),
        "\n\
         unexpected skipped late concurrent change to a peer:\n\
         change: {change:?}\n\
         times: {instant_late:?} {chrono_late}\n\
         peer: {peer:?}"
    );
}
