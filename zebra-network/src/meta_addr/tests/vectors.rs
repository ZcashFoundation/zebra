//! Fixed test cases for MetaAddr and MetaAddrChange.

use std::net::SocketAddr;

use chrono::Utc;

use zebra_chain::{
    parameters::Network::*,
    serialization::{DateTime32, Duration32},
};

use crate::{constants::MAX_PEER_ACTIVE_FOR_GOSSIP, protocol::types::PeerServices};

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
    };

    let max_time_entry = MetaAddr {
        addr: "127.0.0.1:8233".parse().unwrap(),
        services: Default::default(),
        untrusted_last_seen: Some(u32::MAX.into()),
        last_response: Some(u32::MAX.into()),
        last_attempt: None,
        last_failure: None,
        last_connection_state: Default::default(),
    };

    if let Some(min_sanitized) = min_time_entry.sanitize(Mainnet) {
        check::sanitize_avoids_leaks(&min_time_entry, &min_sanitized);
    }
    if let Some(max_sanitized) = max_time_entry.sanitize(Mainnet) {
        check::sanitize_avoids_leaks(&max_time_entry, &max_sanitized);
    }
}

/// Test if a newly created local listening address is gossipable.
///
/// The local listener [`MetaAddr`] is always considered gossipable.
#[test]
fn new_local_listener_is_gossipable() {
    let _init_guard = zebra_test::init();

    let chrono_now = Utc::now();

    let address = SocketAddr::from(([192, 168, 180, 9], 10_000));
    let peer = MetaAddr::new_local_listener_change(&address)
        .into_new_meta_addr()
        .expect("MetaAddrChange can't create a new MetaAddr");

    assert!(peer.is_active_for_gossip(chrono_now));
}

/// Test if a recently received alternate peer address is not gossipable.
///
/// Such [`MetaAddr`] is only considered gossipable after Zebra has tried to connect to it and
/// confirmed that the address is reachable.
#[test]
fn new_alternate_peer_address_is_not_gossipable() {
    let _init_guard = zebra_test::init();

    let chrono_now = Utc::now();

    let address = SocketAddr::from(([192, 168, 180, 9], 10_000));
    let peer = MetaAddr::new_alternate(&address, &PeerServices::NODE_NETWORK)
        .into_new_meta_addr()
        .expect("MetaAddrChange can't create a new MetaAddr");

    assert!(!peer.is_active_for_gossip(chrono_now));
}

/// Test if recently received gossiped peer is gossipable.
#[test]
fn gossiped_peer_reportedly_to_be_seen_recently_is_gossipable() {
    let _init_guard = zebra_test::init();

    let chrono_now = Utc::now();

    let address = SocketAddr::from(([192, 168, 180, 9], 10_000));

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

    let address = SocketAddr::from(([192, 168, 180, 9], 10_000));

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

    let address = SocketAddr::from(([192, 168, 180, 9], 10_000));

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

    let chrono_now = Utc::now();

    let address = SocketAddr::from(([192, 168, 180, 9], 10_000));
    let peer_seed = MetaAddr::new_alternate(&address, &PeerServices::NODE_NETWORK)
        .into_new_meta_addr()
        .expect("MetaAddrChange can't create a new MetaAddr");

    // Create a peer that has responded
    let peer = MetaAddr::new_responded(&address, &PeerServices::NODE_NETWORK)
        .apply_to_meta_addr(peer_seed)
        .expect("Failed to create MetaAddr for responded peer");

    assert!(peer.is_active_for_gossip(chrono_now));
}

/// Test that peer that last responded in the reachable interval is gossipable.
#[test]
fn not_so_recently_responded_peer_is_still_gossipable() {
    let _init_guard = zebra_test::init();

    let chrono_now = Utc::now();

    let address = SocketAddr::from(([192, 168, 180, 9], 10_000));
    let peer_seed = MetaAddr::new_alternate(&address, &PeerServices::NODE_NETWORK)
        .into_new_meta_addr()
        .expect("MetaAddrChange can't create a new MetaAddr");

    // Create a peer that has responded
    let mut peer = MetaAddr::new_responded(&address, &PeerServices::NODE_NETWORK)
        .apply_to_meta_addr(peer_seed)
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

    let chrono_now = Utc::now();

    let address = SocketAddr::from(([192, 168, 180, 9], 10_000));
    let peer_seed = MetaAddr::new_alternate(&address, &PeerServices::NODE_NETWORK)
        .into_new_meta_addr()
        .expect("MetaAddrChange can't create a new MetaAddr");

    // Create a peer that has responded
    let mut peer = MetaAddr::new_responded(&address, &PeerServices::NODE_NETWORK)
        .apply_to_meta_addr(peer_seed)
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
