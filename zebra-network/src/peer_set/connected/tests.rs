//! Tests for the dialers' view of the peer set's connected keys.

use std::{collections::HashSet, net::SocketAddr, sync::Arc};

use tokio::sync::watch;

use crate::PeerSocketAddr;

use super::ConnectedPeers;

/// Returns the peer set address for `addr`.
fn peer(addr: &str) -> PeerSocketAddr {
    addr.parse::<SocketAddr>()
        .expect("test addresses are valid")
        .into()
}

/// Returns a [`ConnectedPeers`] connected to `keys`, with `max_conns_per_ip`.
fn connected_to(keys: &[&str], max_conns_per_ip: usize) -> ConnectedPeers {
    let keys: HashSet<PeerSocketAddr> = keys.iter().map(|key| peer(key)).collect();
    let (_keys_tx, keys_rx) = watch::channel(Arc::new(keys));
    ConnectedPeers::new(keys_rx, max_conns_per_ip)
}

#[test]
fn blocks_a_connected_address() {
    let connected = connected_to(&["203.0.113.1:8233"], 10);

    assert_eq!(
        connected.blocks_dial(peer("203.0.113.1:8233")),
        Some("already connected")
    );
    assert_eq!(connected.blocks_dial(peer("203.0.113.2:8233")), None);
}

#[test]
fn blocks_an_ipv4_address_at_the_per_ip_limit() {
    let connected = connected_to(&["203.0.113.1:50000"], 1);

    assert_eq!(
        connected.blocks_dial(peer("203.0.113.1:8233")),
        Some("per-IP limit")
    );
    // The neighbouring IPv4 address is its own group.
    assert_eq!(connected.blocks_dial(peer("203.0.113.2:8233")), None);
}

#[test]
fn groups_ipv6_addresses_by_slash_64() {
    let connected = connected_to(&["[2001:db8:1:2::1]:50000"], 1);

    assert_eq!(
        connected.blocks_dial(peer("[2001:db8:1:2:ffff::9]:8233")),
        Some("per-IP limit"),
        "an address in the same /64 is at the limit"
    );
    assert_eq!(
        connected.blocks_dial(peer("[2001:db8:1:3::1]:8233")),
        None,
        "an address in the next /64 is not"
    );
}

#[test]
fn allows_dials_below_a_per_ip_limit_over_one() {
    let connected = connected_to(&["203.0.113.1:50000", "203.0.113.1:50001"], 3);

    assert_eq!(connected.blocks_dial(peer("203.0.113.1:8233")), None);

    let connected = connected_to(
        &[
            "203.0.113.1:50000",
            "203.0.113.1:50001",
            "203.0.113.1:50002",
        ],
        3,
    );
    assert_eq!(
        connected.blocks_dial(peer("203.0.113.1:8233")),
        Some("per-IP limit")
    );
}

#[test]
fn follows_the_watch() {
    let (keys_tx, keys_rx) = watch::channel(Arc::new(HashSet::new()));
    let connected = ConnectedPeers::new(keys_rx, 10);
    let addr = peer("203.0.113.1:8233");

    assert_eq!(connected.blocks_dial(addr), None);
    keys_tx.send_replace(Arc::new([addr].into_iter().collect()));
    assert_eq!(connected.blocks_dial(addr), Some("already connected"));
    keys_tx.send_replace(Arc::new(HashSet::new()));
    assert_eq!(connected.blocks_dial(addr), None);
}
