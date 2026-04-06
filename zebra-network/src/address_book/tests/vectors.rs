//! Fixed test vectors for the address book.

use std::time::Instant;

use chrono::Utc;
use tracing::Span;

use zebra_chain::{
    parameters::Network::*,
    serialization::{DateTime32, Duration32},
};

use crate::{
    constants::{
        DEFAULT_MAX_CONNS_PER_IP, MAX_ADDRS_IN_ADDRESS_BOOK, TIMESTAMP_TRUNCATION_SECONDS,
    },
    meta_addr::{MetaAddr, MetaAddrChange},
    protocol::external::types::PeerServices,
    AddressBook,
};

/// Make sure an empty address book is actually empty.
#[test]
fn address_book_empty() {
    let address_book = AddressBook::new(
        "0.0.0.0:0".parse().unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        Span::current(),
    );

    assert_eq!(
        address_book
            .reconnection_peers(Instant::now(), Utc::now())
            .next(),
        None
    );
    assert_eq!(address_book.len(), 0);
}

/// Make sure peers are attempted in priority order.
#[test]
fn address_book_peer_order() {
    let addr1 = "127.0.0.1:1".parse().unwrap();
    let addr2 = "127.0.0.2:2".parse().unwrap();

    let mut meta_addr1 =
        MetaAddr::new_gossiped_meta_addr(addr1, PeerServices::NODE_NETWORK, DateTime32::MIN);
    let mut meta_addr2 = MetaAddr::new_gossiped_meta_addr(
        addr2,
        PeerServices::NODE_NETWORK,
        DateTime32::MIN.saturating_add(Duration32::from_seconds(1)),
    );

    // Regardless of the order of insertion, the most recent address should be chosen first
    let addrs = vec![meta_addr1, meta_addr2];
    let address_book = AddressBook::new_with_addrs(
        "0.0.0.0:0".parse().unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        MAX_ADDRS_IN_ADDRESS_BOOK,
        Span::current(),
        addrs,
    );
    assert_eq!(
        address_book
            .reconnection_peers(Instant::now(), Utc::now())
            .next(),
        Some(meta_addr2),
    );

    // Reverse the order, check that we get the same result
    let addrs = vec![meta_addr2, meta_addr1];
    let address_book = AddressBook::new_with_addrs(
        "0.0.0.0:0".parse().unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        MAX_ADDRS_IN_ADDRESS_BOOK,
        Span::current(),
        addrs,
    );
    assert_eq!(
        address_book
            .reconnection_peers(Instant::now(), Utc::now())
            .next(),
        Some(meta_addr2),
    );

    // Now check that the order depends on the time, not the address
    meta_addr1.addr = addr2;
    meta_addr2.addr = addr1;

    let addrs = vec![meta_addr1, meta_addr2];
    let address_book = AddressBook::new_with_addrs(
        "0.0.0.0:0".parse().unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        MAX_ADDRS_IN_ADDRESS_BOOK,
        Span::current(),
        addrs,
    );
    assert_eq!(
        address_book
            .reconnection_peers(Instant::now(), Utc::now())
            .next(),
        Some(meta_addr2),
    );

    // Reverse the order, check that we get the same result
    let addrs = vec![meta_addr2, meta_addr1];
    let address_book = AddressBook::new_with_addrs(
        "0.0.0.0:0".parse().unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        MAX_ADDRS_IN_ADDRESS_BOOK,
        Span::current(),
        addrs,
    );
    assert_eq!(
        address_book
            .reconnection_peers(Instant::now(), Utc::now())
            .next(),
        Some(meta_addr2),
    );
}

/// Check that `reconnection_peers` skips addresses with IPs for which
/// Zebra already has recently updated outbound peers.
#[test]
fn reconnection_peers_skips_recently_updated_ip() {
    // tests that reconnection_peers() skips addresses where there's a connection at that IP with a recent:
    // - `last_response`
    test_reconnection_peers_skips_recently_updated_ip(true, |addr| {
        MetaAddr::new_responded(addr, None)
    });

    // tests that reconnection_peers() *does not* skip addresses where there's a connection at that IP with a recent:
    // - `last_attempt`
    test_reconnection_peers_skips_recently_updated_ip(false, MetaAddr::new_reconnect);
    // - `last_failure`
    test_reconnection_peers_skips_recently_updated_ip(false, |addr| {
        MetaAddr::new_errored(addr, PeerServices::NODE_NETWORK)
    });
}

fn test_reconnection_peers_skips_recently_updated_ip<
    M: Fn(crate::PeerSocketAddr) -> crate::meta_addr::MetaAddrChange,
>(
    should_skip_ip: bool,
    make_meta_addr_change: M,
) {
    let addr1 = "127.0.0.1:1".parse().unwrap();
    let addr2 = "127.0.0.1:2".parse().unwrap();

    let meta_addr1 = make_meta_addr_change(addr1).into_new_meta_addr(
        Instant::now(),
        Utc::now().try_into().expect("will succeed until 2038"),
    );
    let meta_addr2 = MetaAddr::new_gossiped_meta_addr(
        addr2,
        PeerServices::NODE_NETWORK,
        DateTime32::MIN.saturating_add(Duration32::from_seconds(1)),
    );

    // The second address should be skipped because the first address has a
    // recent `last_response` time and the two addresses have the same IP.
    let addrs = vec![meta_addr1, meta_addr2];
    let address_book = AddressBook::new_with_addrs(
        "0.0.0.0:0".parse().unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        MAX_ADDRS_IN_ADDRESS_BOOK,
        Span::current(),
        addrs,
    );

    let next_reconnection_peer = address_book
        .reconnection_peers(Instant::now(), Utc::now())
        .next();

    if should_skip_ip {
        assert_eq!(next_reconnection_peer, None,);
    } else {
        assert_ne!(next_reconnection_peer, None,);
    }
}

/// A gossiped address with port 0 is not valid for outbound connections.
///
/// Port 0 is the unspecified port and cannot be dialled.
#[test]
fn port_zero_address_not_valid_for_outbound() {
    let addr = MetaAddr::new_gossiped_meta_addr(
        "1.2.3.4:0".parse().unwrap(),
        PeerServices::NODE_NETWORK,
        DateTime32::MIN,
    );
    assert!(
        !addr.address_is_valid_for_outbound(&Mainnet),
        "port 0 must be rejected as an outbound target"
    );
}

/// A gossiped address with the unspecified IP (0.0.0.0) is not valid for outbound connections.
#[test]
fn unspecified_ip_not_valid_for_outbound() {
    let addr = MetaAddr::new_gossiped_meta_addr(
        "0.0.0.0:8233".parse().unwrap(),
        PeerServices::NODE_NETWORK,
        DateTime32::MIN,
    );
    assert!(
        !addr.address_is_valid_for_outbound(&Mainnet),
        "unspecified IP must be rejected as an outbound target"
    );
}

/// RFC-1918 private addresses are currently accepted for outbound connections.
///
/// See TODO <https://github.com/ZcashFoundation/zebra/issues/3117>: once that issue is resolved
/// this test should be inverted (or removed) to confirm filtering is in place.
#[test]
fn rfc1918_address_accepted_for_outbound_documents_missing_filter() {
    let addr = MetaAddr::new_gossiped_meta_addr(
        "192.168.1.1:8233".parse().unwrap(),
        PeerServices::NODE_NETWORK,
        DateTime32::MIN,
    );
    // RFC-1918 addresses are NOT filtered today. This assertion captures current behavior.
    // If this test starts failing, private-address filtering has been implemented — update it.
    assert!(
        addr.address_is_valid_for_outbound(&Mainnet),
        "RFC-1918 address should pass the current (unfiltered) outbound check"
    );
}

/// IPv4-mapped IPv6 representation of an RFC-1918 address is also unfiltered.
///
/// `::ffff:192.168.1.1` is canonicalized to `192.168.1.1` by `canonical_peer_addr`,
/// so it is subject to the same (absent) filtering as plain RFC-1918 IPv4.
/// See TODO <https://github.com/ZcashFoundation/zebra/issues/3117>.
#[test]
fn ipv4_mapped_ipv6_rfc1918_accepted_for_outbound_documents_missing_filter() {
    // Rust parses this as a V6 address; canonical_peer_addr converts it back to V4.
    let addr = MetaAddr::new_gossiped_meta_addr(
        "[::ffff:192.168.1.1]:8233".parse().unwrap(),
        PeerServices::NODE_NETWORK,
        DateTime32::MIN,
    );
    assert!(
        addr.address_is_valid_for_outbound(&Mainnet),
        "IPv4-mapped IPv6 RFC-1918 address should pass the current (unfiltered) outbound check"
    );
}

/// IPv4 loopback (127.0.0.1) is currently accepted for outbound connections.
///
/// See TODO <https://github.com/ZcashFoundation/zebra/issues/3117>.
#[test]
fn loopback_address_accepted_for_outbound_documents_missing_filter() {
    let addr = MetaAddr::new_gossiped_meta_addr(
        "127.0.0.1:8233".parse().unwrap(),
        PeerServices::NODE_NETWORK,
        DateTime32::MIN,
    );
    assert!(
        addr.address_is_valid_for_outbound(&Mainnet),
        "loopback address should pass the current (unfiltered) outbound check"
    );
}

/// IPv6 loopback (::1) is currently accepted for outbound connections.
///
/// See TODO <https://github.com/ZcashFoundation/zebra/issues/3117>.
#[test]
fn ipv6_loopback_accepted_for_outbound_documents_missing_filter() {
    let addr = MetaAddr::new_gossiped_meta_addr(
        "[::1]:8233".parse().unwrap(),
        PeerServices::NODE_NETWORK,
        DateTime32::MIN,
    );
    assert!(
        addr.address_is_valid_for_outbound(&Mainnet),
        "IPv6 loopback address should pass the current (unfiltered) outbound check"
    );
}

/// Addresses from inbound connections are not included in `GetAddr` responses.
///
/// Gossiping an inbound peer's address could let an attacker map the internal network
/// or cause amplification: the attacker connects inbound, then other peers try to reach it.
#[test]
fn inbound_address_is_not_sanitized() {
    let now_ts: DateTime32 = Utc::now().try_into().expect("will succeed until 2038");
    let inbound = MetaAddr::new_connected(
        "1.2.3.4:8233".parse().unwrap(),
        &PeerServices::NODE_NETWORK,
        true, // is_inbound
    )
    .into_new_meta_addr(Instant::now(), now_ts);

    assert!(
        inbound.sanitize(&Mainnet).is_none(),
        "inbound peer addresses must be suppressed from GetAddr responses"
    );
}

/// Addresses of misbehaving peers are not included in `GetAddr` responses.
///
/// Gossiping misbehaving peers could waste other peers' connection slots.
#[test]
fn misbehaving_peer_is_not_sanitized() {
    let now_ts: DateTime32 = Utc::now().try_into().expect("will succeed until 2038");

    // Start with a valid gossiped entry.
    let gossiped = MetaAddr::new_gossiped_meta_addr(
        "1.2.3.4:8233".parse().unwrap(),
        PeerServices::NODE_NETWORK,
        now_ts,
    );
    // Apply a misbehavior increment.
    let misbehaving = MetaAddrChange::UpdateMisbehavior {
        addr: "1.2.3.4:8233".parse().unwrap(),
        score_increment: 1,
    }
    .apply_to_meta_addr(Some(gossiped), Instant::now(), Utc::now())
    .expect("applying UpdateMisbehavior to a gossiped MetaAddr should succeed");

    assert!(
        misbehaving.sanitize(&Mainnet).is_none(),
        "misbehaving peers must be suppressed from GetAddr responses"
    );
}

/// A gossiped address whose timestamp is from year 2000 is too old to be gossiped.
///
/// `AddressBook::sanitized` filters out peers not seen within `MAX_PEER_ACTIVE_FOR_GOSSIP`
/// (3 hours).  A timestamp 25+ years in the past must fail this check.
#[test]
fn timestamp_far_past_not_active_for_gossip() {
    // January 1, 2000 = Unix timestamp 946_684_800.
    let year_2000 = DateTime32::from(946_684_800_u32);
    let addr = MetaAddr::new_gossiped_meta_addr(
        "1.2.3.4:8233".parse().unwrap(),
        PeerServices::NODE_NETWORK,
        year_2000,
    );
    assert!(
        !addr.is_active_for_gossip(Utc::now()),
        "a peer last seen in year 2000 must not be active for gossip"
    );
}

/// A gossiped address with a far-future timestamp (year 2100) is sanitized without panic,
/// and the resulting timestamp is truncated to the nearest `TIMESTAMP_TRUNCATION_SECONDS` boundary.
///
/// Future timestamps are treated as "recently seen" (elapsed = 0), so the peer remains
/// gossipiable; however, the raw future timestamp must still be truncated before sending.
#[test]
fn timestamp_far_future_is_sanitized_and_truncated() {
    // ~January 1, 2100 = Unix timestamp 4_102_444_800 (fits in u32).
    let year_2100 = DateTime32::from(4_102_444_800_u32);
    let addr = MetaAddr::new_gossiped_meta_addr(
        "1.2.3.4:8233".parse().unwrap(),
        PeerServices::NODE_NETWORK,
        year_2100,
    );

    // sanitize must not panic on a future timestamp.
    let sanitized = addr
        .sanitize(&Mainnet)
        .expect("far-future timestamp should sanitize without panic");

    let ts = sanitized
        .last_seen()
        .expect("sanitized MetaAddr must have a last_seen timestamp")
        .timestamp();

    assert_eq!(
        ts % TIMESTAMP_TRUNCATION_SECONDS,
        0,
        "sanitized timestamp must be aligned to a {TIMESTAMP_TRUNCATION_SECONDS}-second boundary"
    );
}
