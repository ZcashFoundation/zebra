//! Specific configs used for zebra-network initialization tests.
//!
//! ## Failures due to Port Conflicts
//!
//! If the test has a port conflict with another test, or another process, then it will fail.
//! If these conflicts cause test failures, run the tests in an isolated environment.
//!
//! ## Failures due to Configured Network Interfaces
//!
//! If your test environment does not have any IPv6 interfaces configured, skip IPv6 tests
//! by setting the `ZEBRA_SKIP_IPV6_TESTS` environmental variable.
//!
//! If it does not have any IPv4 interfaces, or IPv4 localhost is not on `127.0.0.1`,
//! skip all the network tests by setting the `ZEBRA_SKIP_NETWORK_TESTS` environmental variable.

use std::{collections::HashSet, net::SocketAddr};

use tower::service_fn;

use zebra_chain::{best_tip_height::BestTipHeight, parameters::Network};
use zebra_test::net::random_known_port;

use crate::Config;

use super::super::init;

use Network::*;

/// Test that zebra-network discovers dynamic bind-to-all-interfaces listener ports,
/// and sends them to the `AddressBook`.
///
/// Note: This test doesn't cover local interface or public IP address discovery.
#[tokio::test]
async fn local_listener_unspecified_port_unspecified_addr() {
    zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    // these tests might fail on machines with no configured IPv4 addresses
    // (localhost should be enough)
    local_listener_port_with("0.0.0.0:0".parse().unwrap(), Mainnet).await;
    local_listener_port_with("0.0.0.0:0".parse().unwrap(), Testnet).await;

    if zebra_test::net::zebra_skip_ipv6_tests() {
        return;
    }

    // these tests might fail on machines with no configured IPv6 addresses
    local_listener_port_with("[::]:0".parse().unwrap(), Mainnet).await;
    local_listener_port_with("[::]:0".parse().unwrap(), Testnet).await;
}

/// Test that zebra-network discovers dynamic localhost listener ports,
/// and sends them to the `AddressBook`.
#[tokio::test]
async fn local_listener_unspecified_port_localhost_addr() {
    zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    // these tests might fail on machines with unusual IPv4 localhost configs
    local_listener_port_with("127.0.0.1:0".parse().unwrap(), Mainnet).await;
    local_listener_port_with("127.0.0.1:0".parse().unwrap(), Testnet).await;

    if zebra_test::net::zebra_skip_ipv6_tests() {
        return;
    }

    // these tests might fail on machines with no configured IPv6 addresses
    local_listener_port_with("[::1]:0".parse().unwrap(), Mainnet).await;
    local_listener_port_with("[::1]:0".parse().unwrap(), Testnet).await;
}

/// Test that zebra-network propagates fixed localhost listener ports to the `AddressBook`.
#[tokio::test]
async fn local_listener_fixed_port_localhost_addr() {
    zebra_test::init();

    let localhost_v4 = "127.0.0.1".parse().unwrap();
    let localhost_v6 = "::1".parse().unwrap();

    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    local_listener_port_with(SocketAddr::new(localhost_v4, random_known_port()), Mainnet).await;
    local_listener_port_with(SocketAddr::new(localhost_v4, random_known_port()), Testnet).await;

    if zebra_test::net::zebra_skip_ipv6_tests() {
        return;
    }

    local_listener_port_with(SocketAddr::new(localhost_v6, random_known_port()), Mainnet).await;
    local_listener_port_with(SocketAddr::new(localhost_v6, random_known_port()), Testnet).await;
}

async fn local_listener_port_with(listen_addr: SocketAddr, network: Network) {
    let config = Config {
        listen_addr,
        network,
        // Stop Zebra making outbound connections
        initial_mainnet_peers: HashSet::new(),
        initial_testnet_peers: HashSet::new(),
        ..Config::default()
    };
    let inbound_service =
        service_fn(|_| async { unreachable!("inbound service should never be called") });
    let (best_tip_height, _, _) = BestTipHeight::new();

    let (_peer_service, address_book) = init(config, inbound_service, best_tip_height).await;
    let local_listener = address_book.lock().unwrap().local_listener_meta_addr();

    if listen_addr.port() == 0 {
        assert_ne!(
            local_listener.addr.port(),
            0,
            "dynamic ports are replaced with OS-assigned ports"
        );
    } else {
        assert_eq!(
            local_listener.addr.port(),
            listen_addr.port(),
            "fixed ports are correctly propagated"
        );
    }

    assert_eq!(
        local_listener.addr.ip(),
        listen_addr.ip(),
        "IP addresses are correctly propagated"
    );
}
