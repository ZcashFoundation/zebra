//! Test vectors for MetaAddr.

use std::net::SocketAddr;

use zebra_chain::serialization::Duration32;

use super::{super::MetaAddr, check};
use crate::protocol::types::PeerServices;

/// Make sure that the sanitize function handles minimum and maximum times.
#[test]
fn sanitize_extremes() {
    zebra_test::init();

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

    if let Some(min_sanitized) = min_time_entry.sanitize() {
        check::sanitize_avoids_leaks(&min_time_entry, &min_sanitized);
    }
    if let Some(max_sanitized) = max_time_entry.sanitize() {
        check::sanitize_avoids_leaks(&max_time_entry, &max_sanitized);
    }
}

/// Test if a newly created local listening address is gossipable.
///
/// The local listener [`MetaAddr`] is always considered gossipable.
#[test]
fn new_local_listener_is_gossipable() {
    zebra_test::init();

    let address = SocketAddr::from(([192, 168, 180, 9], 10_000));
    let peer = MetaAddr::new_local_listener_change(&address)
        .into_new_meta_addr()
        .expect("MetaAddrChange can't create a new MetaAddr");

    assert!(peer.is_active_for_gossip());
}

/// Test if a recently received alternate peer address is not gossipable.
///
/// Such [`MetaAddr`] is only considered gossipable after Zebra has tried to connect to it and
/// confirmed that the address is reachable.
#[test]
fn new_alternate_peer_address_is_not_gossipable() {
    zebra_test::init();

    let address = SocketAddr::from(([192, 168, 180, 9], 10_000));
    let peer = MetaAddr::new_alternate(&address, &PeerServices::NODE_NETWORK)
        .into_new_meta_addr()
        .expect("MetaAddrChange can't create a new MetaAddr");

    assert!(!peer.is_active_for_gossip());
}
