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
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::Utc;
use futures::{channel::mpsc, FutureExt, StreamExt};
use indexmap::IndexSet;
use tokio::{net::TcpStream, task::JoinHandle};
use tower::{service_fn, Service};
use tracing::Span;

use zebra_chain::{chain_tip::NoChainTip, parameters::Network, serialization::DateTime32};
use zebra_test::net::random_known_port;

use crate::{
    address_book_updater::AddressBookUpdater,
    constants, init,
    meta_addr::MetaAddr,
    peer::{self, ClientTestHarness, HandshakeRequest, OutboundConnectorRequest},
    peer_set::{
        initialize::{
            accept_inbound_connections, add_initial_peers, crawl_and_dial, open_listener,
            DiscoveredPeer,
        },
        set::MorePeers,
        ActiveConnectionCounter, CandidateSet,
    },
    protocol::types::PeerServices,
    AddressBook, BoxError, Config, Request, Response,
};

use Network::*;

/// The amount of time to run the crawler, before testing what it has done.
///
/// Using a very short time can make the crawler not run at all.
const CRAWLER_TEST_DURATION: Duration = Duration::from_secs(10);

/// The amount of time to run the listener, before testing what it has done.
///
/// Using a very short time can make the listener not run at all.
const LISTENER_TEST_DURATION: Duration = Duration::from_secs(10);

/// Test that zebra-network discovers dynamic bind-to-all-interfaces listener ports,
/// and sends them to the `AddressBook`.
///
/// Note: This test doesn't cover local interface or public IP address discovery.
#[tokio::test]
async fn local_listener_unspecified_port_unspecified_addr_v4() {
    let _init_guard = zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    // these tests might fail on machines with no configured IPv4 addresses
    // (localhost should be enough)
    local_listener_port_with("0.0.0.0:0".parse().unwrap(), Mainnet).await;
    local_listener_port_with("0.0.0.0:0".parse().unwrap(), Testnet).await;
}

/// Test that zebra-network discovers dynamic bind-to-all-interfaces listener ports,
/// and sends them to the `AddressBook`.
///
/// Note: This test doesn't cover local interface or public IP address discovery.
#[tokio::test]
async fn local_listener_unspecified_port_unspecified_addr_v6() {
    let _init_guard = zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

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
async fn local_listener_unspecified_port_localhost_addr_v4() {
    let _init_guard = zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    // these tests might fail on machines with unusual IPv4 localhost configs
    local_listener_port_with("127.0.0.1:0".parse().unwrap(), Mainnet).await;
    local_listener_port_with("127.0.0.1:0".parse().unwrap(), Testnet).await;
}

/// Test that zebra-network discovers dynamic localhost listener ports,
/// and sends them to the `AddressBook`.
#[tokio::test]
async fn local_listener_unspecified_port_localhost_addr_v6() {
    let _init_guard = zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    if zebra_test::net::zebra_skip_ipv6_tests() {
        return;
    }

    // these tests might fail on machines with no configured IPv6 addresses
    local_listener_port_with("[::1]:0".parse().unwrap(), Mainnet).await;
    local_listener_port_with("[::1]:0".parse().unwrap(), Testnet).await;
}

/// Test that zebra-network propagates fixed localhost listener ports to the `AddressBook`.
#[tokio::test]
async fn local_listener_fixed_port_localhost_addr_v4() {
    let _init_guard = zebra_test::init();

    let localhost_v4 = "127.0.0.1".parse().unwrap();

    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    local_listener_port_with(SocketAddr::new(localhost_v4, random_known_port()), Mainnet).await;
    local_listener_port_with(SocketAddr::new(localhost_v4, random_known_port()), Testnet).await;
}

/// Test that zebra-network propagates fixed localhost listener ports to the `AddressBook`.
#[tokio::test]
async fn local_listener_fixed_port_localhost_addr_v6() {
    let _init_guard = zebra_test::init();

    let localhost_v6 = "::1".parse().unwrap();

    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    if zebra_test::net::zebra_skip_ipv6_tests() {
        return;
    }

    local_listener_port_with(SocketAddr::new(localhost_v6, random_known_port()), Mainnet).await;
    local_listener_port_with(SocketAddr::new(localhost_v6, random_known_port()), Testnet).await;
}

/// Test zebra-network with a peer limit of zero peers on mainnet.
/// (Zebra does not support this mode of operation.)
#[tokio::test]
#[should_panic]
async fn peer_limit_zero_mainnet() {
    let _init_guard = zebra_test::init();

    // This test should not require network access, because the connection limit is zero.

    let unreachable_inbound_service =
        service_fn(|_| async { unreachable!("inbound service should never be called") });

    let address_book = init_with_peer_limit(0, unreachable_inbound_service, Mainnet).await;
    assert_eq!(
        address_book.lock().unwrap().peers().count(),
        0,
        "expected no peers in Mainnet address book, but got: {:?}",
        address_book.lock().unwrap().address_metrics(Utc::now())
    );
}

/// Test zebra-network with a peer limit of zero peers on testnet.
/// (Zebra does not support this mode of operation.)
#[tokio::test]
#[should_panic]
async fn peer_limit_zero_testnet() {
    let _init_guard = zebra_test::init();

    // This test should not require network access, because the connection limit is zero.

    let unreachable_inbound_service =
        service_fn(|_| async { unreachable!("inbound service should never be called") });

    let address_book = init_with_peer_limit(0, unreachable_inbound_service, Testnet).await;
    assert_eq!(
        address_book.lock().unwrap().peers().count(),
        0,
        "expected no peers in Testnet address book, but got: {:?}",
        address_book.lock().unwrap().address_metrics(Utc::now())
    );
}

/// Test zebra-network with a peer limit of one inbound and one outbound peer on mainnet.
#[tokio::test]
async fn peer_limit_one_mainnet() {
    let _init_guard = zebra_test::init();

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
#[tokio::test]
async fn peer_limit_one_testnet() {
    let _init_guard = zebra_test::init();

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
#[tokio::test]
async fn peer_limit_two_mainnet() {
    let _init_guard = zebra_test::init();

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
#[tokio::test]
async fn peer_limit_two_testnet() {
    let _init_guard = zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let nil_inbound_service = service_fn(|_| async { Ok(Response::Nil) });

    let _ = init_with_peer_limit(2, nil_inbound_service, Testnet).await;

    // Let the crawler run for a while.
    tokio::time::sleep(CRAWLER_TEST_DURATION).await;

    // Any number of address book peers is valid here, because some peers might have failed.
}

/// Test the crawler with an outbound peer limit of zero peers, and a connector that panics.
#[tokio::test]
async fn crawler_peer_limit_zero_connect_panic() {
    let _init_guard = zebra_test::init();

    // This test does not require network access, because the outbound connector
    // and peer set are fake.

    let unreachable_outbound_connector = service_fn(|_| async {
        unreachable!("outbound connector should never be called with a zero peer limit")
    });

    let (_config, mut peerset_rx) =
        spawn_crawler_with_peer_limit(0, unreachable_outbound_connector).await;

    let peer_result = peerset_rx.try_next();
    assert!(
        // `Err(_)` means that no peers are available, and the sender has not been dropped.
        // `Ok(None)` means that no peers are available, and the sender has been dropped.
        matches!(peer_result, Err(_) | Ok(None)),
        "unexpected peer when outbound limit is zero: {:?}",
        peer_result,
    );
}

/// Test the crawler with an outbound peer limit of one peer, and a connector that always errors.
#[tokio::test]
async fn crawler_peer_limit_one_connect_error() {
    let _init_guard = zebra_test::init();

    // This test does not require network access, because the outbound connector
    // and peer set are fake.

    let error_outbound_connector =
        service_fn(|_| async { Err("test outbound connector always returns errors".into()) });

    let (_config, mut peerset_rx) =
        spawn_crawler_with_peer_limit(1, error_outbound_connector).await;

    let peer_result = peerset_rx.try_next();
    assert!(
        // `Err(_)` means that no peers are available, and the sender has not been dropped.
        // `Ok(None)` means that no peers are available, and the sender has been dropped.
        matches!(peer_result, Err(_) | Ok(None)),
        "unexpected peer when all connections error: {:?}",
        peer_result,
    );
}

/// Test the crawler with an outbound peer limit of one peer,
/// and a connector that returns success then disconnects the peer.
#[tokio::test]
async fn crawler_peer_limit_one_connect_ok_then_drop() {
    let _init_guard = zebra_test::init();

    // This test does not require network access, because the outbound connector
    // and peer set are fake.

    let success_disconnect_outbound_connector =
        service_fn(|req: OutboundConnectorRequest| async move {
            let OutboundConnectorRequest {
                addr,
                connection_tracker,
            } = req;

            let (fake_client, _harness) = ClientTestHarness::build().finish();

            // Fake the connection closing.
            std::mem::drop(connection_tracker);

            // Give the crawler time to get the message.
            tokio::task::yield_now().await;

            Ok((addr, fake_client))
        });

    let (config, mut peerset_rx) =
        spawn_crawler_with_peer_limit(1, success_disconnect_outbound_connector).await;

    let mut peer_count: usize = 0;
    loop {
        let peer_result = peerset_rx.try_next();
        match peer_result {
            // A peer handshake succeeded.
            Ok(Some(peer_result)) => {
                assert!(
                    matches!(peer_result, Ok((_, _))),
                    "unexpected connection error: {:?}\n\
                     {} previous peers succeeded",
                    peer_result,
                    peer_count,
                );
                peer_count += 1;
            }

            // The channel is closed and there are no messages left in the channel.
            Ok(None) => break,
            // The channel is still open, but there are no messages left in the channel.
            Err(_) => break,
        }
    }

    assert!(
        peer_count > config.peerset_outbound_connection_limit(),
        "unexpected number of peer connections {}, should be at least the limit of {}",
        peer_count,
        config.peerset_outbound_connection_limit(),
    );
}

/// Test the crawler with an outbound peer limit of one peer,
/// and a connector that returns success then holds the peer open.
#[tokio::test]
async fn crawler_peer_limit_one_connect_ok_stay_open() {
    let _init_guard = zebra_test::init();

    // This test does not require network access, because the outbound connector
    // and peer set are fake.

    let (peer_tracker_tx, mut peer_tracker_rx) = mpsc::unbounded();

    let success_stay_open_outbound_connector = service_fn(move |req: OutboundConnectorRequest| {
        let peer_tracker_tx = peer_tracker_tx.clone();
        async move {
            let OutboundConnectorRequest {
                addr,
                connection_tracker,
            } = req;

            let (fake_client, _harness) = ClientTestHarness::build().finish();

            // Make the connection staying open.
            peer_tracker_tx
                .unbounded_send(connection_tracker)
                .expect("unexpected error sending to unbounded channel");

            Ok((addr, fake_client))
        }
    });

    let (config, mut peerset_rx) =
        spawn_crawler_with_peer_limit(1, success_stay_open_outbound_connector).await;

    let mut peer_change_count: usize = 0;
    loop {
        let peer_change_result = peerset_rx.try_next();
        match peer_change_result {
            // A peer handshake succeeded.
            Ok(Some(peer_change_result)) => {
                assert!(
                    matches!(peer_change_result, Ok((_, _))),
                    "unexpected connection error: {:?}\n\
                     {} previous peers succeeded",
                    peer_change_result,
                    peer_change_count,
                );
                peer_change_count += 1;
            }

            // The channel is closed and there are no messages left in the channel.
            Ok(None) => break,
            // The channel is still open, but there are no messages left in the channel.
            Err(_) => break,
        }
    }

    let mut peer_tracker_count: usize = 0;
    loop {
        let peer_tracker_result = peer_tracker_rx.try_next();
        match peer_tracker_result {
            // We held this peer tracker open until now.
            Ok(Some(peer_tracker)) => {
                std::mem::drop(peer_tracker);
                peer_tracker_count += 1;
            }

            // The channel is closed and there are no messages left in the channel.
            Ok(None) => break,
            // The channel is still open, but there are no messages left in the channel.
            Err(_) => break,
        }
    }

    assert!(
        peer_change_count <= config.peerset_outbound_connection_limit(),
        "unexpected number of peer changes {}, over limit of {}, had {} peer trackers",
        peer_change_count,
        config.peerset_outbound_connection_limit(),
        peer_tracker_count,
    );

    assert!(
        peer_tracker_count <= config.peerset_outbound_connection_limit(),
        "unexpected number of peer trackers {}, over limit of {}, had {} peer changes",
        peer_tracker_count,
        config.peerset_outbound_connection_limit(),
        peer_change_count,
    );
}

/// Test the crawler with the default outbound peer limit, and a connector that always errors.
#[tokio::test]
async fn crawler_peer_limit_default_connect_error() {
    let _init_guard = zebra_test::init();

    // This test does not require network access, because the outbound connector
    // and peer set are fake.

    let error_outbound_connector =
        service_fn(|_| async { Err("test outbound connector always returns errors".into()) });

    let (_config, mut peerset_rx) =
        spawn_crawler_with_peer_limit(None, error_outbound_connector).await;

    let peer_result = peerset_rx.try_next();
    assert!(
        // `Err(_)` means that no peers are available, and the sender has not been dropped.
        // `Ok(None)` means that no peers are available, and the sender has been dropped.
        matches!(peer_result, Err(_) | Ok(None)),
        "unexpected peer when all connections error: {:?}",
        peer_result,
    );
}

/// Test the crawler with the default outbound peer limit,
/// and a connector that returns success then disconnects the peer.
#[tokio::test]
async fn crawler_peer_limit_default_connect_ok_then_drop() {
    let _init_guard = zebra_test::init();

    // This test does not require network access, because the outbound connector
    // and peer set are fake.

    let success_disconnect_outbound_connector =
        service_fn(|req: OutboundConnectorRequest| async move {
            let OutboundConnectorRequest {
                addr,
                connection_tracker,
            } = req;

            let (fake_client, _harness) = ClientTestHarness::build().finish();

            // Fake the connection closing.
            std::mem::drop(connection_tracker);

            // Give the crawler time to get the message.
            tokio::task::yield_now().await;

            Ok((addr, fake_client))
        });

    // TODO: tweak the crawler timeouts and rate-limits so we get over the actual limit
    //       (currently, getting over the limit can take 30 seconds or more)
    let (config, mut peerset_rx) =
        spawn_crawler_with_peer_limit(15, success_disconnect_outbound_connector).await;

    let mut peer_count: usize = 0;
    loop {
        let peer_result = peerset_rx.try_next();
        match peer_result {
            // A peer handshake succeeded.
            Ok(Some(peer_result)) => {
                assert!(
                    matches!(peer_result, Ok((_, _))),
                    "unexpected connection error: {:?}\n\
                     {} previous peers succeeded",
                    peer_result,
                    peer_count,
                );
                peer_count += 1;
            }

            // The channel is closed and there are no messages left in the channel.
            Ok(None) => break,
            // The channel is still open, but there are no messages left in the channel.
            Err(_) => break,
        }
    }

    assert!(
        peer_count > config.peerset_outbound_connection_limit(),
        "unexpected number of peer connections {}, should be over the limit of {}",
        peer_count,
        config.peerset_outbound_connection_limit(),
    );
}

/// Test the crawler with the default outbound peer limit,
/// and a connector that returns success then holds the peer open.
#[tokio::test]
async fn crawler_peer_limit_default_connect_ok_stay_open() {
    let _init_guard = zebra_test::init();

    // This test does not require network access, because the outbound connector
    // and peer set are fake.

    let (peer_tracker_tx, mut peer_tracker_rx) = mpsc::unbounded();

    let success_stay_open_outbound_connector = service_fn(move |req: OutboundConnectorRequest| {
        let peer_tracker_tx = peer_tracker_tx.clone();
        async move {
            let OutboundConnectorRequest {
                addr,
                connection_tracker,
            } = req;

            let (fake_client, _harness) = ClientTestHarness::build().finish();

            // Make the connection staying open.
            peer_tracker_tx
                .unbounded_send(connection_tracker)
                .expect("unexpected error sending to unbounded channel");

            Ok((addr, fake_client))
        }
    });

    // The initial target size is multiplied by 1.5 to give the actual limit.
    let (config, mut peerset_rx) =
        spawn_crawler_with_peer_limit(None, success_stay_open_outbound_connector).await;

    let mut peer_change_count: usize = 0;
    loop {
        let peer_change_result = peerset_rx.try_next();
        match peer_change_result {
            // A peer handshake succeeded.
            Ok(Some(peer_change_result)) => {
                assert!(
                    matches!(peer_change_result, Ok((_, _))),
                    "unexpected connection error: {:?}\n\
                     {} previous peers succeeded",
                    peer_change_result,
                    peer_change_count,
                );
                peer_change_count += 1;
            }

            // The channel is closed and there are no messages left in the channel.
            Ok(None) => break,
            // The channel is still open, but there are no messages left in the channel.
            Err(_) => break,
        }
    }

    let mut peer_tracker_count: usize = 0;
    loop {
        let peer_tracker_result = peer_tracker_rx.try_next();
        match peer_tracker_result {
            // We held this peer tracker open until now.
            Ok(Some(peer_tracker)) => {
                std::mem::drop(peer_tracker);
                peer_tracker_count += 1;
            }

            // The channel is closed and there are no messages left in the channel.
            Ok(None) => break,
            // The channel is still open, but there are no messages left in the channel.
            Err(_) => break,
        }
    }

    assert!(
        peer_change_count <= config.peerset_outbound_connection_limit(),
        "unexpected number of peer changes {}, over limit of {}, had {} peer trackers",
        peer_change_count,
        config.peerset_outbound_connection_limit(),
        peer_tracker_count,
    );

    assert!(
        peer_tracker_count <= config.peerset_outbound_connection_limit(),
        "unexpected number of peer trackers {}, over limit of {}, had {} peer changes",
        peer_tracker_count,
        config.peerset_outbound_connection_limit(),
        peer_change_count,
    );
}

/// Test the listener with an inbound peer limit of zero peers, and a handshaker that panics.
#[tokio::test]
async fn listener_peer_limit_zero_handshake_panic() {
    let _init_guard = zebra_test::init();

    // This test requires an IPv4 network stack with 127.0.0.1 as localhost.
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let unreachable_inbound_handshaker = service_fn(|_| async {
        unreachable!("inbound handshaker should never be called with a zero peer limit")
    });

    let (_config, mut peerset_rx) =
        spawn_inbound_listener_with_peer_limit(0, unreachable_inbound_handshaker).await;

    let peer_result = peerset_rx.try_next();
    assert!(
        // `Err(_)` means that no peers are available, and the sender has not been dropped.
        // `Ok(None)` means that no peers are available, and the sender has been dropped.
        matches!(peer_result, Err(_) | Ok(None)),
        "unexpected peer when inbound limit is zero: {:?}",
        peer_result,
    );
}

/// Test the listener with an inbound peer limit of one peer, and a handshaker that always errors.
#[tokio::test]
async fn listener_peer_limit_one_handshake_error() {
    let _init_guard = zebra_test::init();

    // This test requires an IPv4 network stack with 127.0.0.1 as localhost.
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let error_inbound_handshaker =
        service_fn(|_| async { Err("test inbound handshaker always returns errors".into()) });

    let (_config, mut peerset_rx) =
        spawn_inbound_listener_with_peer_limit(1, error_inbound_handshaker).await;

    let peer_result = peerset_rx.try_next();
    assert!(
        // `Err(_)` means that no peers are available, and the sender has not been dropped.
        // `Ok(None)` means that no peers are available, and the sender has been dropped.
        matches!(peer_result, Err(_) | Ok(None)),
        "unexpected peer when all handshakes error: {:?}",
        peer_result,
    );
}

/// Test the listener with an inbound peer limit of one peer,
/// and a handshaker that returns success then disconnects the peer.
#[tokio::test]
async fn listener_peer_limit_one_handshake_ok_then_drop() {
    let _init_guard = zebra_test::init();

    // This test requires an IPv4 network stack with 127.0.0.1 as localhost.
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let success_disconnect_inbound_handshaker =
        service_fn(|req: HandshakeRequest<TcpStream>| async move {
            let HandshakeRequest {
                data_stream: tcp_stream,
                connected_addr: _,
                connection_tracker,
            } = req;

            let (fake_client, _harness) = ClientTestHarness::build().finish();

            // Actually close the connection.
            std::mem::drop(connection_tracker);
            std::mem::drop(tcp_stream);

            // Give the crawler time to get the message.
            tokio::task::yield_now().await;

            Ok(fake_client)
        });

    let (config, mut peerset_rx) =
        spawn_inbound_listener_with_peer_limit(1, success_disconnect_inbound_handshaker).await;

    let mut peer_count: usize = 0;
    loop {
        let peer_result = peerset_rx.try_next();
        match peer_result {
            // A peer handshake succeeded.
            Ok(Some(peer_result)) => {
                assert!(
                    matches!(peer_result, Ok((_, _))),
                    "unexpected connection error: {:?}\n\
                     {} previous peers succeeded",
                    peer_result,
                    peer_count,
                );
                peer_count += 1;
            }

            // The channel is closed and there are no messages left in the channel.
            Ok(None) => break,
            // The channel is still open, but there are no messages left in the channel.
            Err(_) => break,
        }
    }

    assert!(
        peer_count > config.peerset_inbound_connection_limit(),
        "unexpected number of peer connections {}, should be over the limit of {}",
        peer_count,
        config.peerset_inbound_connection_limit(),
    );
}

/// Test the listener with an inbound peer limit of one peer,
/// and a handshaker that returns success then holds the peer open.
#[tokio::test]
async fn listener_peer_limit_one_handshake_ok_stay_open() {
    let _init_guard = zebra_test::init();

    // This test requires an IPv4 network stack with 127.0.0.1 as localhost.
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let (peer_tracker_tx, mut peer_tracker_rx) = mpsc::unbounded();

    let success_stay_open_inbound_handshaker =
        service_fn(move |req: HandshakeRequest<TcpStream>| {
            let peer_tracker_tx = peer_tracker_tx.clone();
            async move {
                let HandshakeRequest {
                    data_stream: tcp_stream,
                    connected_addr: _,
                    connection_tracker,
                } = req;

                let (fake_client, _harness) = ClientTestHarness::build().finish();

                // Make the connection staying open.
                peer_tracker_tx
                    .unbounded_send((tcp_stream, connection_tracker))
                    .expect("unexpected error sending to unbounded channel");

                Ok(fake_client)
            }
        });

    let (config, mut peerset_rx) =
        spawn_inbound_listener_with_peer_limit(1, success_stay_open_inbound_handshaker).await;

    let mut peer_change_count: usize = 0;
    loop {
        let peer_change_result = peerset_rx.try_next();
        match peer_change_result {
            // A peer handshake succeeded.
            Ok(Some(peer_change_result)) => {
                assert!(
                    matches!(peer_change_result, Ok((_, _))),
                    "unexpected connection error: {:?}\n\
                     {} previous peers succeeded",
                    peer_change_result,
                    peer_change_count,
                );
                peer_change_count += 1;
            }

            // The channel is closed and there are no messages left in the channel.
            Ok(None) => break,
            // The channel is still open, but there are no messages left in the channel.
            Err(_) => break,
        }
    }

    let mut peer_tracker_count: usize = 0;
    loop {
        let peer_tracker_result = peer_tracker_rx.try_next();
        match peer_tracker_result {
            // We held this peer connection and tracker open until now.
            Ok(Some((peer_connection, peer_tracker))) => {
                std::mem::drop(peer_connection);
                std::mem::drop(peer_tracker);
                peer_tracker_count += 1;
            }

            // The channel is closed and there are no messages left in the channel.
            Ok(None) => break,
            // The channel is still open, but there are no messages left in the channel.
            Err(_) => break,
        }
    }

    assert!(
        peer_change_count <= config.peerset_inbound_connection_limit(),
        "unexpected number of peer changes {}, over limit of {}, had {} peer trackers",
        peer_change_count,
        config.peerset_inbound_connection_limit(),
        peer_tracker_count,
    );

    assert!(
        peer_tracker_count <= config.peerset_inbound_connection_limit(),
        "unexpected number of peer trackers {}, over limit of {}, had {} peer changes",
        peer_tracker_count,
        config.peerset_inbound_connection_limit(),
        peer_change_count,
    );
}

/// Test the listener with the default inbound peer limit, and a handshaker that always errors.
#[tokio::test]
async fn listener_peer_limit_default_handshake_error() {
    let _init_guard = zebra_test::init();

    // This test requires an IPv4 network stack with 127.0.0.1 as localhost.
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let error_inbound_handshaker =
        service_fn(|_| async { Err("test inbound handshaker always returns errors".into()) });

    let (_config, mut peerset_rx) =
        spawn_inbound_listener_with_peer_limit(None, error_inbound_handshaker).await;

    let peer_result = peerset_rx.try_next();
    assert!(
        // `Err(_)` means that no peers are available, and the sender has not been dropped.
        // `Ok(None)` means that no peers are available, and the sender has been dropped.
        matches!(peer_result, Err(_) | Ok(None)),
        "unexpected peer when all handshakes error: {:?}",
        peer_result,
    );
}

/// Test the listener with the default inbound peer limit,
/// and a handshaker that returns success then disconnects the peer.
///
/// TODO: tweak the crawler timeouts and rate-limits so we get over the actual limit on macOS
///       (currently, getting over the limit can take 30 seconds or more)
#[cfg(not(target_os = "macos"))]
#[tokio::test]
async fn listener_peer_limit_default_handshake_ok_then_drop() {
    let _init_guard = zebra_test::init();

    // This test requires an IPv4 network stack with 127.0.0.1 as localhost.
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let success_disconnect_inbound_handshaker =
        service_fn(|req: HandshakeRequest<TcpStream>| async move {
            let HandshakeRequest {
                data_stream: tcp_stream,
                connected_addr: _,
                connection_tracker,
            } = req;

            let (fake_client, _harness) = ClientTestHarness::build().finish();

            // Actually close the connection.
            std::mem::drop(connection_tracker);
            std::mem::drop(tcp_stream);

            // Give the crawler time to get the message.
            tokio::task::yield_now().await;

            Ok(fake_client)
        });

    let (config, mut peerset_rx) =
        spawn_inbound_listener_with_peer_limit(None, success_disconnect_inbound_handshaker).await;

    let mut peer_count: usize = 0;
    loop {
        let peer_result = peerset_rx.try_next();
        match peer_result {
            // A peer handshake succeeded.
            Ok(Some(peer_result)) => {
                assert!(
                    matches!(peer_result, Ok((_, _))),
                    "unexpected connection error: {:?}\n\
                     {} previous peers succeeded",
                    peer_result,
                    peer_count,
                );
                peer_count += 1;
            }

            // The channel is closed and there are no messages left in the channel.
            Ok(None) => break,
            // The channel is still open, but there are no messages left in the channel.
            Err(_) => break,
        }
    }

    assert!(
        peer_count > config.peerset_inbound_connection_limit(),
        "unexpected number of peer connections {}, should be over the limit of {}",
        peer_count,
        config.peerset_inbound_connection_limit(),
    );
}

/// Test the listener with the default inbound peer limit,
/// and a handshaker that returns success then holds the peer open.
#[tokio::test]
async fn listener_peer_limit_default_handshake_ok_stay_open() {
    let _init_guard = zebra_test::init();

    // This test requires an IPv4 network stack with 127.0.0.1 as localhost.
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let (peer_tracker_tx, mut peer_tracker_rx) = mpsc::unbounded();

    let success_stay_open_inbound_handshaker =
        service_fn(move |req: HandshakeRequest<TcpStream>| {
            let peer_tracker_tx = peer_tracker_tx.clone();
            async move {
                let HandshakeRequest {
                    data_stream: tcp_stream,
                    connected_addr: _,
                    connection_tracker,
                } = req;

                let (fake_client, _harness) = ClientTestHarness::build().finish();

                // Make the connection staying open.
                peer_tracker_tx
                    .unbounded_send((tcp_stream, connection_tracker))
                    .expect("unexpected error sending to unbounded channel");

                Ok(fake_client)
            }
        });

    let (config, mut peerset_rx) =
        spawn_inbound_listener_with_peer_limit(None, success_stay_open_inbound_handshaker).await;

    let mut peer_change_count: usize = 0;
    loop {
        let peer_change_result = peerset_rx.try_next();
        match peer_change_result {
            // A peer handshake succeeded.
            Ok(Some(peer_change_result)) => {
                assert!(
                    matches!(peer_change_result, Ok((_, _))),
                    "unexpected connection error: {:?}\n\
                     {} previous peers succeeded",
                    peer_change_result,
                    peer_change_count,
                );
                peer_change_count += 1;
            }

            // The channel is closed and there are no messages left in the channel.
            Ok(None) => break,
            // The channel is still open, but there are no messages left in the channel.
            Err(_) => break,
        }
    }

    let mut peer_tracker_count: usize = 0;
    loop {
        let peer_tracker_result = peer_tracker_rx.try_next();
        match peer_tracker_result {
            // We held this peer connection and tracker open until now.
            Ok(Some((peer_connection, peer_tracker))) => {
                std::mem::drop(peer_connection);
                std::mem::drop(peer_tracker);
                peer_tracker_count += 1;
            }

            // The channel is closed and there are no messages left in the channel.
            Ok(None) => break,
            // The channel is still open, but there are no messages left in the channel.
            Err(_) => break,
        }
    }

    assert!(
        peer_change_count <= config.peerset_inbound_connection_limit(),
        "unexpected number of peer changes {}, over limit of {}, had {} peer trackers",
        peer_change_count,
        config.peerset_inbound_connection_limit(),
        peer_tracker_count,
    );

    assert!(
        peer_tracker_count <= config.peerset_inbound_connection_limit(),
        "unexpected number of peer trackers {}, over limit of {}, had {} peer changes",
        peer_tracker_count,
        config.peerset_inbound_connection_limit(),
        peer_change_count,
    );
}

/// Test if the initial seed peer connections is rate-limited.
#[tokio::test]
async fn add_initial_peers_is_rate_limited() {
    let _init_guard = zebra_test::init();

    // This test should not require network access.

    // We don't need to actually connect to the peers; we only need to check
    // if the connection attempts is rate-limited. Therefore, just return an error.
    let outbound_connector =
        service_fn(|_| async { Err("test outbound connector always returns errors".into()) });

    const PEER_COUNT: usize = 10;

    let before = Instant::now();

    let (initial_peers_task_handle, peerset_rx, address_book_updater_task_handle) =
        spawn_add_initial_peers(PEER_COUNT, outbound_connector).await;
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

    // Check for panics or errors in the address book updater task.
    let updater_result = address_book_updater_task_handle.now_or_never();
    assert!(
        matches!(updater_result, None)
            || matches!(updater_result, Some(Err(ref join_error)) if join_error.is_cancelled())
            // The task method only returns one kind of error.
            // We can't check for error equality due to type erasure,
            // and we can't downcast due to ownership.
            || matches!(updater_result, Some(Ok(Err(ref _all_senders_closed)))),
        "unexpected error or panic in address book updater task: {:?}",
        updater_result,
    );
}

/// Test that [`init`] does not deadlock in `add_initial_peers`,
/// even if the seeders return a lot of peers.
#[tokio::test]
async fn add_initial_peers_deadlock() {
    // The `PEER_COUNT` is the amount of initial seed peers. The value is set so
    // that the peers fill up `PEERSET_INITIAL_TARGET_SIZE`, fill up the channel
    // for sending unused peers to the `AddressBook`, and so that there are
    // still some extra peers left.
    const PEER_COUNT: usize = 200;
    const PEERSET_INITIAL_TARGET_SIZE: usize = 2;
    const TIME_LIMIT: Duration = Duration::from_secs(10);

    let _init_guard = zebra_test::init();

    // This test requires an IPv4 network stack. Localhost should be enough.
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    // Create a list of dummy IPs, and initialize a config using them as the
    // initial peers. The amount of these peers will overflow
    // `PEERSET_INITIAL_TARGET_SIZE`.
    let mut peers = IndexSet::new();
    for address_number in 0..PEER_COUNT {
        peers.insert(
            SocketAddr::new(Ipv4Addr::new(127, 1, 1, address_number as _).into(), 1).to_string(),
        );
    }

    // This test might fail on machines with no configured IPv4 addresses
    // (localhost should be enough).
    let unused_v4 = "0.0.0.0:0".parse().unwrap();

    let config = Config {
        initial_mainnet_peers: peers,
        peerset_initial_target_size: PEERSET_INITIAL_TARGET_SIZE,

        network: Network::Mainnet,
        listen_addr: unused_v4,

        ..Config::default()
    };

    let nil_inbound_service = service_fn(|_| async { Ok(Response::Nil) });

    let init_future = init(config, nil_inbound_service, NoChainTip);

    assert!(tokio::time::timeout(TIME_LIMIT, init_future).await.is_ok());
}

/// Open a local listener on `listen_addr` for `network`.
/// Asserts that the local listener address works as expected.
async fn local_listener_port_with(listen_addr: SocketAddr, network: Network) {
    let config = Config {
        listen_addr,
        network,

        // Stop Zebra making outbound connections
        initial_mainnet_peers: IndexSet::new(),
        initial_testnet_peers: IndexSet::new(),

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
///
/// Binds the network listener to an unused port on all network interfaces.
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

/// Run a peer crawler with `peerset_initial_target_size` and `outbound_connector`.
///
/// Uses the default values for all other config fields.
/// Does not bind a local listener.
///
/// Returns the generated [`Config`], and the peer set receiver.
async fn spawn_crawler_with_peer_limit<C>(
    peerset_initial_target_size: impl Into<Option<usize>>,
    outbound_connector: C,
) -> (Config, mpsc::Receiver<DiscoveredPeer>)
where
    C: Service<OutboundConnectorRequest, Response = (SocketAddr, peer::Client), Error = BoxError>
        + Clone
        + Send
        + 'static,
    C::Future: Send + 'static,
{
    // Create a test config.
    let mut config = Config::default();
    if let Some(peerset_initial_target_size) = peerset_initial_target_size.into() {
        config.peerset_initial_target_size = peerset_initial_target_size;
    }

    // Manually initialize an address book without a timestamp tracker.
    let mut address_book = AddressBook::new(config.listen_addr, config.network, Span::current());

    // Add enough fake peers to go over the limit, even if the limit is zero.
    let over_limit_peers = config.peerset_outbound_connection_limit() * 2 + 1;
    let mut fake_peer = None;
    for address_number in 0..over_limit_peers {
        let addr = SocketAddr::new(Ipv4Addr::new(127, 1, 1, address_number as _).into(), 1);
        let addr =
            MetaAddr::new_gossiped_meta_addr(addr, PeerServices::NODE_NETWORK, DateTime32::now());
        fake_peer = Some(addr);
        let addr = addr
            .new_gossiped_change()
            .expect("created MetaAddr contains enough information to represent a gossiped address");

        address_book.update(addr);
    }

    // Create a fake peer set.
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

    // Make the channels large enough to hold all the peers.
    let (peerset_tx, peerset_rx) = mpsc::channel::<DiscoveredPeer>(over_limit_peers);
    let (mut demand_tx, demand_rx) = mpsc::channel::<MorePeers>(over_limit_peers);

    let candidates = CandidateSet::new(address_book.clone(), nil_peer_set);

    // In zebra_network::initialize() the counter would already have some initial peer connections,
    // but in this test we start with an empty counter.
    let active_outbound_connections = ActiveConnectionCounter::new_counter();

    // Add fake demand over the limit.
    for _ in 0..over_limit_peers {
        let _ = demand_tx.try_send(MorePeers);
    }

    // Start the crawler.
    let crawl_fut = crawl_and_dial(
        config.clone(),
        demand_tx,
        demand_rx,
        candidates,
        outbound_connector,
        peerset_tx,
        active_outbound_connections,
    );
    let crawl_task_handle = tokio::spawn(crawl_fut);

    // Let the crawler run for a while.
    tokio::time::sleep(CRAWLER_TEST_DURATION).await;

    // Stop the crawler and let it finish.
    crawl_task_handle.abort();
    tokio::task::yield_now().await;

    // Check for panics or errors in the crawler.
    let crawl_result = crawl_task_handle.now_or_never();
    assert!(
        matches!(crawl_result, None)
            || matches!(crawl_result, Some(Err(ref e)) if e.is_cancelled()),
        "unexpected error or panic in peer crawler task: {:?}",
        crawl_result,
    );

    // Check the final address book contents.
    assert_eq!(
        address_book.lock().unwrap().peers().count(),
        over_limit_peers,
        "expected {} peers in Mainnet address book, but got: {:?}",
        over_limit_peers,
        address_book.lock().unwrap().address_metrics(Utc::now())
    );

    (config, peerset_rx)
}

/// Run an inbound peer listener with `peerset_initial_target_size` and `handshaker`.
///
/// Binds the local listener to an unused localhost port.
/// Uses the default values for all other config fields.
///
/// Returns the generated [`Config`], and the peer set receiver.
async fn spawn_inbound_listener_with_peer_limit<S>(
    peerset_initial_target_size: impl Into<Option<usize>>,
    listen_handshaker: S,
) -> (Config, mpsc::Receiver<DiscoveredPeer>)
where
    S: Service<peer::HandshakeRequest<TcpStream>, Response = peer::Client, Error = BoxError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    // Create a test config that listens on an unused port.
    let listen_addr = "127.0.0.1:0".parse().unwrap();
    let mut config = Config {
        listen_addr,
        ..Config::default()
    };

    if let Some(peerset_initial_target_size) = peerset_initial_target_size.into() {
        config.peerset_initial_target_size = peerset_initial_target_size;
    }

    // Open the listener port.
    let (tcp_listener, listen_addr) = open_listener(&config.clone()).await;

    // Make enough inbound connections to go over the limit, even if the limit is zero.
    // Make the channels large enough to hold all the connections.
    let over_limit_connections = config.peerset_inbound_connection_limit() * 2 + 1;
    let (peerset_tx, peerset_rx) = mpsc::channel::<DiscoveredPeer>(over_limit_connections);

    // Start listening for connections.
    let listen_fut = accept_inbound_connections(
        config.clone(),
        tcp_listener,
        listen_handshaker,
        peerset_tx.clone(),
    );
    let listen_task_handle = tokio::spawn(listen_fut);

    // Open inbound connections.
    let mut outbound_task_handles = Vec::new();
    for _ in 0..over_limit_connections {
        let outbound_fut = async move {
            let outbound_result = TcpStream::connect(listen_addr).await;
            // Let other tasks run before we block on reading.
            tokio::task::yield_now().await;

            if let Ok(outbound_stream) = outbound_result {
                // Wait until the listener closes the connection.
                // The handshaker is fake, so it never sends any data.
                let readable_result = outbound_stream.readable().await;
                info!(
                    ?readable_result,
                    "outbound connection became readable or errored: \
                     closing connection to test inbound listener"
                );
            } else {
                // If the connection is closed quickly, we might get errors here.
                debug!(
                    ?outbound_result,
                    "outbound connection error in inbound listener test"
                );
            }
        };

        let outbound_task_handle = tokio::spawn(outbound_fut);
        outbound_task_handles.push(outbound_task_handle);
    }

    // Let the listener run for a while.
    tokio::time::sleep(LISTENER_TEST_DURATION).await;

    // Stop the listener and outbound tasks, and let them finish.
    listen_task_handle.abort();
    for outbound_task_handle in outbound_task_handles {
        outbound_task_handle.abort();
    }
    tokio::task::yield_now().await;

    // Check for panics or errors in the listener.
    let listen_result = listen_task_handle.now_or_never();
    assert!(
        matches!(listen_result, None)
            || matches!(listen_result, Some(Err(ref e)) if e.is_cancelled()),
        "unexpected error or panic in inbound peer listener task: {:?}",
        listen_result,
    );

    (config, peerset_rx)
}

/// Initialize a task that connects to `peer_count` initial peers using the
/// given connector.
///
/// Connects to IP addresses in the IPv4 localhost range.
/// Does not open a local listener port.
///
/// Returns the initial peers task [`JoinHandle`], the peer set receiver,
/// and the address book updater task join handle.
async fn spawn_add_initial_peers<C>(
    peer_count: usize,
    outbound_connector: C,
) -> (
    JoinHandle<Result<ActiveConnectionCounter, BoxError>>,
    mpsc::Receiver<DiscoveredPeer>,
    JoinHandle<Result<(), BoxError>>,
)
where
    C: Service<OutboundConnectorRequest, Response = (SocketAddr, peer::Client), Error = BoxError>
        + Clone
        + Send
        + 'static,
    C::Future: Send + 'static,
{
    // Create a list of dummy IPs and initialize a config using them as the
    // initial peers.
    let mut peers = IndexSet::new();
    for address_number in 0..peer_count {
        peers.insert(
            SocketAddr::new(Ipv4Addr::new(127, 1, 1, address_number as _).into(), 1).to_string(),
        );
    }

    // This address isn't actually bound - it just gets passed to the address book.
    let unused_v4 = "0.0.0.0:0".parse().unwrap();

    let config = Config {
        initial_mainnet_peers: peers,

        network: Network::Mainnet,
        listen_addr: unused_v4,

        ..Config::default()
    };

    let (peerset_tx, peerset_rx) = mpsc::channel::<DiscoveredPeer>(peer_count + 1);

    let (_address_book, address_book_updater, _address_metrics, address_book_updater_guard) =
        AddressBookUpdater::spawn(&config, unused_v4);

    let add_fut = add_initial_peers(config, outbound_connector, peerset_tx, address_book_updater);
    let add_task_handle = tokio::spawn(add_fut);

    (add_task_handle, peerset_rx, address_book_updater_guard)
}
