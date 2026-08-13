//! Fixed test vectors for recent IP and subnet limits.

#![allow(clippy::unwrap_in_result)]

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use crate::peer_set::initialize::recent_by_ip::RecentByIp;

#[test]
fn old_connection_attempts_are_pruned() {
    const TEST_TIME_LIMIT: Duration = Duration::from_secs(5);

    let _init_guard = zebra_test::init();

    let mut recent_connections = RecentByIp::new(Some(TEST_TIME_LIMIT), None, None, None);
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

    let mut recent_connections = RecentByIp::new(
        Some(TEST_TIME_LIMIT),
        Some(TEST_MAX_CONNS_PER_IP),
        Some(TEST_MAX_CONNS_PER_IP),
        Some(TEST_MAX_CONNS_PER_IP),
    );

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

#[test]
fn subnet_cap_rejects_third_ipv4_peer_from_same_24() {
    const TEST_TIME_LIMIT: Duration = Duration::from_secs(5);
    const SUBNET_LIMIT: usize = 2;

    let _init_guard = zebra_test::init();

    // Per-IP limit is high (10) so the subnet limit is the binding constraint.
    let mut recent = RecentByIp::new(
        Some(TEST_TIME_LIMIT),
        Some(10),
        Some(10),
        Some(SUBNET_LIMIT),
    );

    let ip1: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10));
    let ip2: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));
    let ip3: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 30));
    // All in 192.168.1.0/24

    assert!(
        !recent.is_past_limit_or_add(ip1),
        "first peer from /24 accepted"
    );
    assert!(
        !recent.is_past_limit_or_add(ip2),
        "second peer from /24 accepted"
    );
    assert!(
        recent.is_past_limit_or_add(ip3),
        "third peer from same /24 rejected by subnet cap"
    );

    // A peer from a different /24 should still be accepted.
    let ip_other: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
    assert!(
        !recent.is_past_limit_or_add(ip_other),
        "peer from different /24 accepted"
    );
}

#[test]
fn subnet_cap_rejects_third_ipv6_peer_from_same_64() {
    const TEST_TIME_LIMIT: Duration = Duration::from_secs(5);
    const SUBNET_LIMIT: usize = 2;

    let _init_guard = zebra_test::init();

    let mut recent = RecentByIp::new(
        Some(TEST_TIME_LIMIT),
        Some(10),
        Some(SUBNET_LIMIT),
        Some(10),
    );

    let ip1: IpAddr = IpAddr::V6(Ipv6Addr::from([
        0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01,
    ]));
    let ip2: IpAddr = IpAddr::V6(Ipv6Addr::from([
        0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02,
    ]));
    let ip3: IpAddr = IpAddr::V6(Ipv6Addr::from([
        0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x03,
    ]));
    // All in 2001:db8::/64

    assert!(
        !recent.is_past_limit_or_add(ip1),
        "first peer from /64 accepted"
    );
    assert!(
        !recent.is_past_limit_or_add(ip2),
        "second peer from /64 accepted"
    );
    assert!(
        recent.is_past_limit_or_add(ip3),
        "third peer from same /64 rejected by subnet cap"
    );

    // A peer from a different /64 should still be accepted.
    let ip_other: IpAddr = IpAddr::V6(Ipv6Addr::from([
        0x20, 0x01, 0x0d, 0xb9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01,
    ]));
    assert!(
        !recent.is_past_limit_or_add(ip_other),
        "peer from different /64 accepted"
    );
}

#[test]
fn subnet_cap_allows_peers_from_different_subnets() {
    const TEST_TIME_LIMIT: Duration = Duration::from_secs(5);

    let _init_guard = zebra_test::init();

    let mut recent = RecentByIp::new(Some(TEST_TIME_LIMIT), Some(1), Some(2), Some(2));

    // 4 peers, each from a different IPv4 /24
    let ips = [
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
        IpAddr::V4(Ipv4Addr::new(10, 0, 1, 1)),
        IpAddr::V4(Ipv4Addr::new(10, 0, 2, 1)),
        IpAddr::V4(Ipv4Addr::new(10, 0, 3, 1)),
    ];

    for ip in ips {
        assert!(
            !recent.is_past_limit_or_add(ip),
            "peer from different /24 accepted"
        );
    }
}

#[test]
fn subnet_cap_prunes_with_time() {
    const TEST_TIME_LIMIT: Duration = Duration::from_secs(2);
    const SUBNET_LIMIT: usize = 2;

    let _init_guard = zebra_test::init();

    let mut recent = RecentByIp::new(
        Some(TEST_TIME_LIMIT),
        Some(10),
        Some(SUBNET_LIMIT),
        Some(SUBNET_LIMIT),
    );

    let ip1: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10));
    let ip2: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));
    let ip3: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 30));
    // All in 192.168.1.0/24

    assert!(!recent.is_past_limit_or_add(ip1), "first peer accepted");
    assert!(!recent.is_past_limit_or_add(ip2), "second peer accepted");
    assert!(
        recent.is_past_limit_or_add(ip3),
        "third peer rejected — subnet cap full"
    );

    // Wait for entries to expire.
    std::thread::sleep(TEST_TIME_LIMIT + Duration::from_millis(100));

    // After pruning, the subnet count should be 0, so a new peer is accepted.
    assert!(
        !recent.is_past_limit_or_add(ip3),
        "peer accepted after subnet entries pruned"
    );
}
