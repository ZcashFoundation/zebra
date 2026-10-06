//! Fixed test vectors for recent IP limits.

#![allow(clippy::unwrap_in_result)]

use std::net::IpAddr;
use std::time::Duration;

use crate::peer_set::initialize::recent_by_ip::RecentByIp;

#[test]
fn old_connection_attempts_are_pruned() {
    const TEST_TIME_LIMIT: Duration = Duration::from_secs(5);

    let _init_guard = zebra_test::init();

    let mut recent_connections = RecentByIp::new(Some(TEST_TIME_LIMIT), None);
    let ip = "127.0.0.1".parse().expect("should parse");

    assert!(
        !recent_connections.is_past_limit_or_add(ip),
        "should not be past limit"
    );
    assert!(
        recent_connections.is_past_limit_or_add(ip),
        "should be past max_connections_per_ip limit"
    );

    std::thread::sleep(TEST_TIME_LIMIT / 3);

    assert!(
        recent_connections.is_past_limit_or_add(ip),
        "should still contain entry after a third of the time limit"
    );

    std::thread::sleep(3 * TEST_TIME_LIMIT / 4);

    assert!(
        !recent_connections.is_past_limit_or_add(ip),
        "should prune entry after 13/12 * time_limit"
    );

    const TEST_MAX_CONNS_PER_IP: usize = 3;

    let mut recent_connections =
        RecentByIp::new(Some(TEST_TIME_LIMIT), Some(TEST_MAX_CONNS_PER_IP));

    for _ in 0..TEST_MAX_CONNS_PER_IP {
        assert!(
            !recent_connections.is_past_limit_or_add(ip),
            "should not be past limit"
        );
    }

    assert!(
        recent_connections.is_past_limit_or_add(ip),
        "should be past max_connections_per_ip limit"
    );

    std::thread::sleep(TEST_TIME_LIMIT / 3);

    assert!(
        recent_connections.is_past_limit_or_add(ip),
        "should still be past limit after a third of the reconnection delay"
    );

    std::thread::sleep(3 * TEST_TIME_LIMIT / 4);

    assert!(
        !recent_connections.is_past_limit_or_add(ip),
        "should prune entry after 13/12 * time_limit"
    );
}

/// Connection attempts from different addresses in the same IPv6 `/64` share
/// one limit slot: a single machine with a `/64` allocation can present many
/// distinct addresses, and each must count against the same cap.
#[test]
fn ipv6_addresses_in_same_64_share_the_limit() {
    const TEST_TIME_LIMIT: Duration = Duration::from_secs(5);

    let _init_guard = zebra_test::init();

    let mut recent_connections = RecentByIp::new(Some(TEST_TIME_LIMIT), Some(1));

    let first: IpAddr = "2001:db8::a".parse().expect("valid IPv6 address");
    let second: IpAddr = "2001:db8::b".parse().expect("valid IPv6 address");

    assert!(
        !recent_connections.is_past_limit_or_add(first),
        "the first address in a /64 should be accepted"
    );
    assert!(
        recent_connections.is_past_limit_or_add(second),
        "a second address in the same /64 should be past the limit"
    );

    // An address in a different /64 is unaffected.
    let other_subnet: IpAddr = "2001:db8:1::a".parse().expect("valid IPv6 address");
    assert!(
        !recent_connections.is_past_limit_or_add(other_subnet),
        "an address in a different /64 should be accepted"
    );
}

/// IPv4 addresses are limited per address, not per subnet: unlike an IPv6
/// `/64`, every extra IPv4 address costs an attacker a separate allocation,
/// and grouping them would let one peer block Zebra from its neighbours.
#[test]
fn ipv4_addresses_in_same_24_are_limited_separately() {
    const TEST_TIME_LIMIT: Duration = Duration::from_secs(5);

    let _init_guard = zebra_test::init();

    let mut recent_connections = RecentByIp::new(Some(TEST_TIME_LIMIT), Some(1));

    let first: IpAddr = "192.0.2.10".parse().expect("valid IPv4 address");
    let second: IpAddr = "192.0.2.20".parse().expect("valid IPv4 address");

    assert!(
        !recent_connections.is_past_limit_or_add(first),
        "the first address in a /24 should be accepted"
    );
    assert!(
        !recent_connections.is_past_limit_or_add(second),
        "a different address in the same /24 should also be accepted"
    );

    // The same address is still limited.
    assert!(
        recent_connections.is_past_limit_or_add(first),
        "a repeat of the first address should be past the limit"
    );
}

/// IPv4-mapped IPv6 spellings of an IPv4 address must share the IPv4 bucket,
/// so a peer cannot evade the limit by reconnecting via the other spelling.
#[test]
fn ipv4_mapped_ipv6_shares_the_ipv4_limit() {
    const TEST_TIME_LIMIT: Duration = Duration::from_secs(5);

    let _init_guard = zebra_test::init();

    let mut recent_connections = RecentByIp::new(Some(TEST_TIME_LIMIT), Some(1));

    let ipv4: IpAddr = "192.0.2.10".parse().expect("valid IPv4 address");
    let mapped: IpAddr = "::ffff:192.0.2.10"
        .parse()
        .expect("valid IPv4-mapped address");

    assert!(
        !recent_connections.is_past_limit_or_add(ipv4),
        "the plain IPv4 address should be accepted"
    );
    assert!(
        recent_connections.is_past_limit_or_add(mapped),
        "the IPv4-mapped spelling of the same address should be past the limit"
    );
}
