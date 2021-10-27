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

use std::{
    collections::HashSet,
    net::{Ipv4Addr, SocketAddr},
    time::Instant,
};

use futures::{channel::mpsc, StreamExt};
use tokio::task::JoinHandle;
use tower::{discover::Change, service_fn, Service};

use zebra_chain::{chain_tip::NoChainTip, parameters::Network};
use zebra_test::net::random_known_port;

use crate::{
    constants,
    peer::{self, OutboundConnectorRequest},
    peer_set::{initialize::PeerChange, ActiveConnectionCounter},
    BoxError, Config,
};

use super::super::{add_initial_peers, init};

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

    let (_peer_service, address_book) = init(config, inbound_service, NoChainTip).await;
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

/// Test if the initial seed peer connections is rate-limited.
#[tokio::test]
async fn add_initial_peers_is_rate_limited() {
    zebra_test::init();

    // We don't need to actually connect to the peers; we only need to check
    // if the connection attempts is rate-limited. Therefore, just return an error.
    let outbound_connector =
        service_fn(|_| async { Err("test outbound connector always returns errors".into()) });

    const PEER_COUNT: usize = 10;

    let before = Instant::now();

    let (initial_peers_task_handle, peerset_rx) =
        spawn_add_initial_peers(PEER_COUNT, outbound_connector);
    let connections = peerset_rx.take(PEER_COUNT).collect::<Vec<_>>().await;

    let elapsed = Instant::now() - before;

    assert_eq!(connections.len(), PEER_COUNT);
    // Make sure the rate limiting worked by checking if it took long enough
    assert!(
        elapsed > constants::MIN_PEER_CONNECTION_INTERVAL.saturating_mul((PEER_COUNT - 1) as u32),
        "elapsed only {:?}",
        elapsed
    );

    let initial_peers_result = initial_peers_task_handle.await;
    assert!(
        matches!(initial_peers_result, Ok(Ok(_))),
        "unexpected error or panic in add_initial_peers task: {:?}",
        initial_peers_result,
    );
}

/// Initialize a task that connects to `peer_count` initial peers using the
/// given connector.
///
/// Dummy IPs are used.
///
/// Returns the task [`JoinHandle`], and the peer set receiver.
fn spawn_add_initial_peers<C>(
    peer_count: usize,
    outbound_connector: C,
) -> (
    JoinHandle<Result<ActiveConnectionCounter, BoxError>>,
    mpsc::Receiver<PeerChange>,
)
where
    C: Service<
            OutboundConnectorRequest,
            Response = Change<SocketAddr, peer::Client>,
            Error = BoxError,
        > + Clone
        + Send
        + 'static,
    C::Future: Send + 'static,
{
    // Create a list of dummy IPs and initialize a config using them as the
    // initial peers.
    let mut peers = HashSet::new();
    for address_number in 0..peer_count {
        peers.insert(
            SocketAddr::new(Ipv4Addr::new(127, 1, 1, address_number as _).into(), 1).to_string(),
        );
    }
    let config = Config {
        initial_mainnet_peers: peers,
        network: Network::Mainnet,
        ..Config::default()
    };

    let (peerset_tx, peerset_rx) = mpsc::channel::<PeerChange>(peer_count + 1);

    let add_fut = add_initial_peers(config, outbound_connector, peerset_tx);
    let add_task_handle = tokio::spawn(add_fut);

    (add_task_handle, peerset_rx)
}
