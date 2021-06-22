//! Specific configs used for zebra-network initialization tests.
//!
//! ### Note on port conflict
//!
//! If the test has a port conflict with another test, or another process, then it will fail.
//! If these conflicts cause test failures, run the tests in an isolated environment.

use std::{collections::HashSet, net::SocketAddr};

use tower::service_fn;

use zebra_chain::parameters::Network;
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

    local_listener_port_with("0.0.0.0:0".parse().unwrap(), Mainnet).await;
    local_listener_port_with("0.0.0.0:0".parse().unwrap(), Testnet).await;

    // these tests might fail on machines without IPv6
    local_listener_port_with("[::]:0".parse().unwrap(), Mainnet).await;
    local_listener_port_with("[::]:0".parse().unwrap(), Testnet).await;
}

/// Test that zebra-network discovers dynamic localhost listener ports,
/// and sends them to the `AddressBook`.
#[tokio::test]
async fn local_listener_unspecified_port_localhost_addr() {
    zebra_test::init();

    // these tests might fail on machines with unusual IPv4 localhost configs
    local_listener_port_with("127.0.0.1:0".parse().unwrap(), Mainnet).await;
    local_listener_port_with("127.0.0.1:0".parse().unwrap(), Testnet).await;

    // these tests might fail on machines without IPv6
    local_listener_port_with("[::1]:0".parse().unwrap(), Mainnet).await;
    local_listener_port_with("[::1]:0".parse().unwrap(), Testnet).await;
}

/// Test that zebra-network propagates fixed localhost listener ports to the `AddressBook`.
#[tokio::test]
async fn local_listener_fixed_port_localhost_addr() {
    zebra_test::init();

    let localhost_v4 = "127.0.0.1".parse().unwrap();
    let localhost_v6 = "::1".parse().unwrap();

    // these tests might fail on machines with unusual IPv4 localhost configs
    local_listener_port_with(SocketAddr::new(localhost_v4, random_known_port()), Mainnet).await;
    local_listener_port_with(SocketAddr::new(localhost_v4, random_known_port()), Testnet).await;

    // these tests might fail on machines without IPv6
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

    let (_peer_service, address_book) = init(config, inbound_service).await;
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
