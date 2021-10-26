//! zebra-network initialization tests using fixed configs.
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
    sync::Arc,
    time::Duration,
};

use futures::{channel::mpsc, FutureExt};
use tokio::task::JoinHandle;
use tower::{discover::Change, service_fn, Service};

use tracing::Span;
use zebra_chain::{chain_tip::NoChainTip, parameters::Network, serialization::DateTime32};
use zebra_test::net::random_known_port;

use crate::{
    init,
    meta_addr::MetaAddr,
    peer::{self, OutboundConnectorRequest},
    peer_set::{
        initialize::{crawl_and_dial, PeerChange},
        set::MorePeers,
        ActiveConnectionCounter, CandidateSet,
    },
    protocol::types::PeerServices,
    AddressBook, BoxError, Config, Request, Response,
};

use Network::*;

/// The amount of time we run the crawler before testing it.
const CRAWLER_TEST_DURATION: Duration = Duration::from_secs(5);

/// The number of fake crawler peers in the testing [`AddressBook`].
const CRAWLER_FAKE_PEER_COUNT: usize = 2;

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

/// Test zebra-network with a peer limit of zero peers on mainnet.
/// (Zebra does not support this mode of operation.)
///
/// This test doesn't require the multi-threaded executor, but it's a lot faster with it.
#[tokio::test(flavor = "multi_thread")]
#[should_panic]
async fn peer_limit_zero_mainnet() {
    zebra_test::init();

    // This test should not require network access, because the connection limit is zero.

    let unreachable_inbound_service =
        service_fn(|_| async { unreachable!("inbound service should never be called") });

    let address_book = init_with_peer_limit(0, unreachable_inbound_service, Mainnet).await;
    assert_eq!(
        address_book.lock().unwrap().peers().count(),
        0,
        "expected no peers in Mainnet address book, but got: {:?}",
        address_book.lock().unwrap().address_metrics()
    );
}

/// Test zebra-network with a peer limit of zero peers on testnet.
/// (Zebra does not support this mode of operation.)
///
/// This test doesn't require the multi-threaded executor, but it's a lot faster with it.
#[tokio::test(flavor = "multi_thread")]
#[should_panic]
async fn peer_limit_zero_testnet() {
    zebra_test::init();

    // This test should not require network access, because the connection limit is zero.

    let unreachable_inbound_service =
        service_fn(|_| async { unreachable!("inbound service should never be called") });

    let address_book = init_with_peer_limit(0, unreachable_inbound_service, Testnet).await;
    assert_eq!(
        address_book.lock().unwrap().peers().count(),
        0,
        "expected no peers in Testnet address book, but got: {:?}",
        address_book.lock().unwrap().address_metrics()
    );
}

/// Test zebra-network with a peer limit of one inbound and one outbound peer on mainnet.
///
/// This test doesn't require the multi-threaded executor, but it's a lot faster with it.
#[tokio::test(flavor = "multi_thread")]
async fn peer_limit_one_mainnet() {
    zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let nil_inbound_service = service_fn(|_| async { Ok(Response::Nil) });

    let _ = init_with_peer_limit(1, nil_inbound_service, Mainnet).await;

    // Let the crawler run for a while.
    tokio::time::sleep(CRAWLER_TEST_DURATION).await;

    // Any number of address book peers is valid here, because some peers might have failed.
}

/// Test zebra-network with a peer limit of one inbound and one outbound peer on testnet.
///
/// This test doesn't require the multi-threaded executor, but it's a lot faster with it.
#[tokio::test(flavor = "multi_thread")]
async fn peer_limit_one_testnet() {
    zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let nil_inbound_service = service_fn(|_| async { Ok(Response::Nil) });

    let _ = init_with_peer_limit(1, nil_inbound_service, Testnet).await;

    // Let the crawler run for a while.
    tokio::time::sleep(CRAWLER_TEST_DURATION).await;

    // Any number of address book peers is valid here, because some peers might have failed.
}

/// Test zebra-network with a peer limit of two inbound and three outbound peers on mainnet.
///
/// This test doesn't require the multi-threaded executor, but it's a lot faster with it.
#[tokio::test(flavor = "multi_thread")]
async fn peer_limit_two_mainnet() {
    zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let nil_inbound_service = service_fn(|_| async { Ok(Response::Nil) });

    let _ = init_with_peer_limit(2, nil_inbound_service, Mainnet).await;

    // Let the crawler run for a while.
    tokio::time::sleep(CRAWLER_TEST_DURATION).await;

    // Any number of address book peers is valid here, because some peers might have failed.
}

/// Test zebra-network with a peer limit of two inbound and three outbound peers on testnet.
///
/// This test doesn't require the multi-threaded executor, but it's a lot faster with it.
#[tokio::test(flavor = "multi_thread")]
async fn peer_limit_two_testnet() {
    zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let nil_inbound_service = service_fn(|_| async { Ok(Response::Nil) });

    let _ = init_with_peer_limit(2, nil_inbound_service, Testnet).await;

    // Let the crawler run for a while.
    tokio::time::sleep(CRAWLER_TEST_DURATION).await;

    // Any number of address book peers is valid here, because some peers might have failed.
}

/// Test the crawler with an outbound peer limit of zero.
///
/// This test doesn't require the multi-threaded executor, but it's a lot faster with it.
#[tokio::test(flavor = "multi_thread")]
async fn crawler_peer_limit_zero() {
    zebra_test::init();

    // This test does not require network access, because the outbound connector
    // and peer set are fake.

    let unreachable_outbound_connector = service_fn(|_| async {
        unreachable!("outbound connector should never be called with a zero peer limit")
    });

    let (crawl_task_handle, mut peerset_tx) =
        spawn_crawler_with_peer_limit(0, unreachable_outbound_connector);

    // Let the crawler run for a while.
    tokio::time::sleep(CRAWLER_TEST_DURATION).await;
    crawl_task_handle.abort();

    let peer_result = peerset_tx.try_next();
    assert!(
        // `Err(_)` means that no peers are available, and the sender has not been dropped.
        // `Ok(None)` means that no peers are available, and the sender has been dropped.
        matches!(peer_result, Err(_) | Ok(None)),
        "unexpected peer when peer limit is zero: {:?}",
        peer_result,
    );

    let crawl_result = crawl_task_handle.now_or_never();
    assert!(
        matches!(crawl_result, None)
            || matches!(crawl_result, Some(Err(ref e)) if e.is_cancelled()),
        "unexpected error or panic in peer crawler task: {:?}",
        crawl_result,
    );
}

/// Test the crawler with an outbound peer limit of one.
///
/// This test doesn't require the multi-threaded executor, but it's a lot faster with it.
#[tokio::test(flavor = "multi_thread")]
async fn crawler_peer_limit_one() {
    zebra_test::init();

    // This test does not require network access, because the outbound connector
    // and peer set are fake.

    let error_outbound_connector =
        service_fn(|_| async { Err("test outbound connector always returns errors".into()) });

    let (crawl_task_handle, mut peerset_tx) =
        spawn_crawler_with_peer_limit(1, error_outbound_connector);

    // Let the crawler run for a while.
    tokio::time::sleep(CRAWLER_TEST_DURATION).await;
    crawl_task_handle.abort();

    let peer_result = peerset_tx.try_next();
    assert!(
        // `Err(_)` means that no peers are available, and the sender has not been dropped.
        // `Ok(None)` means that no peers are available, and the sender has been dropped.
        matches!(peer_result, Err(_) | Ok(None)),
        "unexpected peer when all connections error: {:?}",
        peer_result,
    );

    let crawl_result = crawl_task_handle.now_or_never();
    assert!(
        matches!(crawl_result, None)
            || matches!(crawl_result, Some(Err(ref e)) if e.is_cancelled()),
        "unexpected error or panic in peer crawler task: {:?}",
        crawl_result,
    );
}

/// Test the crawler with an outbound peer limit of three peers.
///
/// This test doesn't require the multi-threaded executor, but it's a lot faster with it.
#[tokio::test(flavor = "multi_thread")]
async fn crawler_peer_limit_two() {
    zebra_test::init();

    // This test does not require network access, because the outbound connector
    // and peer set are fake.

    let error_outbound_connector =
        service_fn(|_| async { Err("test outbound connector always returns errors".into()) });

    let (crawl_task_handle, mut peerset_tx) =
        spawn_crawler_with_peer_limit(2, error_outbound_connector);

    // Let the crawler run for a while.
    tokio::time::sleep(CRAWLER_TEST_DURATION).await;
    crawl_task_handle.abort();

    let peer_result = peerset_tx.try_next();
    assert!(
        // `Err(_)` means that no peers are available, and the sender has not been dropped.
        // `Ok(None)` means that no peers are available, and the sender has been dropped.
        matches!(peer_result, Err(_) | Ok(None)),
        "unexpected peer when all connections error: {:?}",
        peer_result,
    );

    let crawl_result = crawl_task_handle.now_or_never();
    assert!(
        matches!(crawl_result, None)
            || matches!(crawl_result, Some(Err(ref e)) if e.is_cancelled()),
        "unexpected error or panic in peer crawler task: {:?}",
        crawl_result,
    );
}

/// Open a local listener on `listen_addr` for `network`.
/// Asserts that the local listener address works as expected.
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

/// Initialize a peer set with `peerset_initial_target_size` and `inbound_service` on `network`.
/// Returns the newly created [`AddressBook`] for testing.
async fn init_with_peer_limit<S>(
    peerset_initial_target_size: usize,
    inbound_service: S,
    network: Network,
) -> Arc<std::sync::Mutex<AddressBook>>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    // This test might fail on machines with no configured IPv4 addresses
    // (localhost should be enough).
    let unused_v4 = "0.0.0.0:0".parse().unwrap();

    let config = Config {
        peerset_initial_target_size,

        network,
        listen_addr: unused_v4,

        ..Config::default()
    };

    let (_peer_service, address_book) = init(config, inbound_service, NoChainTip).await;

    address_book
}

/// Initialize a peer crawler with `peerset_initial_target_size`.
///
/// Returns the crawler task [`JoinHandle`], and the peer set receiver.
fn spawn_crawler_with_peer_limit<C>(
    peerset_initial_target_size: usize,
    outbound_connector: C,
) -> (JoinHandle<Result<(), BoxError>>, mpsc::Receiver<PeerChange>)
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
    let config = Config {
        peerset_initial_target_size,

        ..Config::default()
    };

    // Manually initialize an address book without a timestamp tracker.
    let mut address_book = AddressBook::new(config.listen_addr, Span::current());

    // Add fake peers.
    let mut fake_peer = None;
    for address_number in 0..CRAWLER_FAKE_PEER_COUNT {
        let addr = SocketAddr::new(Ipv4Addr::new(127, 1, 1, address_number as _).into(), 1);
        let addr =
            MetaAddr::new_gossiped_meta_addr(addr, PeerServices::NODE_NETWORK, DateTime32::now());
        fake_peer = Some(addr);
        let addr = addr.new_gossiped_change();

        address_book.update(addr);
    }

    let nil_peer_set = service_fn(move |req| async move {
        let rsp = match req {
            // Return the correct response variant for Peers requests,
            // re-using one of the peers we already provided.
            Request::Peers => Response::Peers(vec![fake_peer.unwrap()]),
            _ => unreachable!("unexpected request: {:?}", req),
        };

        Ok(rsp)
    });

    let address_book = Arc::new(std::sync::Mutex::new(address_book));

    // Use the standard channel sizes, plus one, so the initializers never panic
    let (peerset_tx, peerset_rx) =
        mpsc::channel::<PeerChange>(config.peerset_total_connection_limit() + 1);
    let (mut demand_tx, demand_rx) =
        mpsc::channel::<MorePeers>(config.peerset_outbound_connection_limit() + 1);

    let candidates = CandidateSet::new(address_book, nil_peer_set);

    // In zebra_network::initialize() the counter would already have some initial peer connections,
    // but in this test we start with an empty counter.
    let active_outbound_connections = ActiveConnectionCounter::new_counter();

    // Add initial demand.
    for _ in 0..config.peerset_initial_target_size {
        let _ = demand_tx.try_send(MorePeers);
    }

    let crawl_fut = crawl_and_dial(
        config,
        demand_tx,
        demand_rx,
        candidates,
        outbound_connector,
        peerset_tx,
        active_outbound_connections,
    );
    let crawl_task_handle = tokio::spawn(crawl_fut);

    // TODO: return and test the address book, once we've turned it into a service (#1976)
    //
    // Currently, the address book mutex deadlocks if we try to access it in the tests.
    // This happens regardless of the executor type.

    (crawl_task_handle, peerset_rx)
}
