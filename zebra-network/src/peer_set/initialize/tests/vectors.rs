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
//! by setting the `SKIP_IPV6_TESTS` environmental variable.
//!
//! If it does not have any IPv4 interfaces, or IPv4 localhost is not on `127.0.0.1`,
//! skip all the network tests by setting the `SKIP_NETWORK_TESTS` environmental variable.

use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use chrono::Utc;
use futures::{channel::mpsc, FutureExt, StreamExt};
use indexmap::IndexSet;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpSocket, TcpStream},
    sync::{broadcast, watch},
    task::JoinHandle,
};
use tower::{service_fn, Layer, Service, ServiceExt};

use zebra_chain::{
    chain_tip::NoChainTip,
    parameters::Network,
    serialization::{DateTime32, ZcashDeserialize},
};

use crate::{
    address_book_updater::{change_channel, AddressBookUpdater},
    config::CacheDir,
    connect_isolated_tcp_direct_with_inbound, constants, init,
    meta_addr::{MetaAddr, PeerAddrState},
    peer::{
        self,
        v2::{handshake::LocalIdentity, rate::TokenBucket},
        ClientTestHarness, ConnectedAddr, HandshakeRequest, OutboundConnectorRequest,
    },
    peer_set::{
        crawler_services,
        initialize::{
            accept_inbound_connections, add_initial_peers, batch_misbehavior_updates,
            crawl_and_dial, limit_initial_peers, open_listener, v2, DiscoveredPeer,
        },
        set::MorePeers,
        ActiveConnectionCounter, ConnectedPeers, ConnectionTracker,
    },
    protocol::types::PeerServices,
    AddressBook, BoxError, Config, MisbehaviorReport, PeerSocketAddr, Request, Response,
};

use Network::*;

/// The amount of time to run the crawler, before testing what it has done.
///
/// Using a very short time can make the crawler not run at all.
const CRAWLER_TEST_DURATION: Duration = Duration::from_secs(10);

/// The crawl timer interval for crawler tests.
///
/// Much shorter than [`CRAWLER_TEST_DURATION`], so the timer refills freed outbound slots many
/// times during each test, rather than only on its first tick.
const CRAWLER_TEST_CRAWL_NEW_PEER_INTERVAL: Duration = Duration::from_millis(100);

/// The deadline for every test connection to be handshaken or rejected by the listener.
const LISTENER_TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The amount of time to run zcashd-compat listener tests.
const ZCASHD_COMPAT_LISTENER_TEST_DURATION: Duration = Duration::from_millis(500);

/// The amount of time to make the inbound connection acceptor wait between peer connections.
const MIN_INBOUND_PEER_CONNECTION_INTERVAL_FOR_TESTS: Duration = Duration::from_millis(25);

/// The amount of time to wait for the peer set to become ready, or stay unready.
const PEER_SET_BAN_TEST_TIMEOUT: Duration = Duration::from_secs(5);

/// The amount of time a single poll waits for the peer set to route a request, when the test
/// expects the peer set to have no peer left.
///
/// [`PeerSet::poll_ready()`](crate::peer_set::PeerSet) documents that "if there are no ready
/// peers, and no new peers, network requests will pause", so a request that can't be routed
/// hangs rather than failing. A banned peer is dropped on the same poll that sees the ban, so
/// once the ban is visible in the address book this only has to outlast one poll.
const PEER_SET_UNREADY_POLL_TIMEOUT: Duration = Duration::from_millis(500);

/// The number of consecutive unanswered polls that mean the peer set has no peer left.
///
/// Kept below [`constants::PEERSET_BUFFER_SIZE`]: an abandoned request holds its `Buffer` permit
/// until the worker handles it, and the worker can't make progress while there is no peer. The
/// test asserts on the number of abandoned requests, so exceeding the buffer fails rather than
/// hanging.
const PEER_SET_UNREADY_POLLS: usize = 2;

/// The amount of time to wait for a misbehaviour update to be flushed and turned into a ban.
///
/// Misbehaviour updates are batched behind [`constants::MISBEHAVIOR_FLUSH_INTERVAL`] in
/// `initialize()`, so this is derived from that interval rather than hard-coded.
const MISBEHAVIOR_FLUSH_TIMEOUT: Duration =
    constants::MISBEHAVIOR_FLUSH_INTERVAL.saturating_add(Duration::from_secs(5));

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
    for network in Network::iter() {
        local_listener_port_with("0.0.0.0:0".parse().unwrap(), network).await;
    }
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
    for network in Network::iter() {
        local_listener_port_with("[::]:0".parse().unwrap(), network).await;
    }
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
    for network in Network::iter() {
        local_listener_port_with("127.0.0.1:0".parse().unwrap(), network).await;
    }
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
    for network in Network::iter() {
        local_listener_port_with("[::1]:0".parse().unwrap(), network).await;
    }
}

/// Test that zebra-network propagates a bound localhost listener port to the `AddressBook`.
#[tokio::test]
async fn local_listener_bound_port_localhost_addr_v4() {
    let _init_guard = zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    for network in Network::iter() {
        local_listener_bound_port_with("127.0.0.1:0".parse().unwrap(), network).await;
    }
}

/// Test that zebra-network propagates a bound localhost listener port to the `AddressBook`.
#[tokio::test]
async fn local_listener_bound_port_localhost_addr_v6() {
    let _init_guard = zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    if zebra_test::net::zebra_skip_ipv6_tests() {
        return;
    }

    for network in Network::iter() {
        local_listener_bound_port_with("[::1]:0".parse().unwrap(), network).await;
    }
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

    let (_network, address_book) =
        init_with_peer_limit(0, unreachable_inbound_service, Mainnet, None, None).await;
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

    let (_network, address_book) = init_with_peer_limit(
        0,
        unreachable_inbound_service,
        Network::new_default_testnet(),
        None,
        None,
    )
    .await;

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

    let _network = init_with_peer_limit(1, nil_inbound_service, Mainnet, None, None).await;

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

    let _network = init_with_peer_limit(
        1,
        nil_inbound_service,
        Network::new_default_testnet(),
        None,
        None,
    )
    .await;

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

    let _network = init_with_peer_limit(2, nil_inbound_service, Mainnet, None, None).await;

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

    let _network = init_with_peer_limit(
        2,
        nil_inbound_service,
        Network::new_default_testnet(),
        None,
        None,
    )
    .await;

    // Let the crawler run for a while.
    tokio::time::sleep(CRAWLER_TEST_DURATION).await;

    // Any number of address book peers is valid here, because some peers might have failed.
}

/// Test that startup loads exactly the written cache into its connection candidates and address book.
#[tokio::test]
async fn written_peer_cache_is_automatically_read_on_startup() {
    let _init_guard = zebra_test::init();

    let peer_cache_dir =
        tempfile::tempdir().expect("creating a temporary cache directory should succeed");
    let config = Config {
        cache_dir: CacheDir::custom_path(peer_cache_dir.path()),
        // The disk cache must be the only source of initial peers.
        initial_mainnet_peers: IndexSet::new(),
        initial_testnet_peers: IndexSet::new(),
        ..Config::default()
    };
    let expected_peers: HashSet<PeerSocketAddr> = ["127.1.1.1:8233", "127.1.1.2:8233"]
        .into_iter()
        .map(|addr| addr.parse().expect("test peer addresses are valid"))
        .collect();

    // The persistent_mode_peer_cache integration test covers the background writer with a
    // real local peer. Here, write the cache directly: DNS seeds have no last-seen timestamp,
    // so finding seeds alone does not guarantee any cacheable peers.
    config
        .update_peer_cache(expected_peers.clone())
        .await
        .expect("writing the temporary peer cache should succeed");

    let (
        address_book,
        _bans_receiver,
        _address_book_updater,
        address_book_service,
        _address_metrics,
        _address_book_updater_task_handle,
    ) = AddressBookUpdater::spawn(&config, config.listen_addr);

    // Exercise the startup loader without opening sockets or handshaking with public peers.
    let initial_peers = limit_initial_peers(&config, &address_book_service).await;
    assert_eq!(initial_peers, expected_peers);

    // The loader adds the peers to the address book before returning.
    let loaded_peers: HashSet<_> = address_book
        .lock()
        .expect("previous thread panicked while holding address book lock")
        .peers()
        .map(|peer| peer.addr)
        .collect();
    assert_eq!(loaded_peers, expected_peers);
}

/// Test the crawler with an outbound peer limit of zero peers, and a connector that panics.
#[tokio::test(start_paused = true)]
async fn crawler_peer_limit_zero_connect_panic() {
    let _init_guard = zebra_test::init();

    // This test does not require network access, because the outbound connector
    // and peer set are fake.

    let unreachable_outbound_connector = service_fn(|_| async {
        unreachable!("outbound connector should never be called with a zero peer limit")
    });

    let (_config, mut peerset_rx) =
        spawn_crawler_with_peer_limit(0, unreachable_outbound_connector).await;

    let peer_result = peerset_rx.try_recv();
    assert!(
        // `Err(TryRecvError::Empty)` means no peers are available and the channel is still open.
        // `Err(TryRecvError::Closed)` means the channel is closed.
        peer_result.is_err(),
        "unexpected peer when outbound limit is zero: {peer_result:?}",
    );
}

/// Test the crawler with an outbound peer limit of one peer, and a connector that always errors.
#[tokio::test(start_paused = true)]
async fn crawler_peer_limit_one_connect_error() {
    let _init_guard = zebra_test::init();

    // This test does not require network access, because the outbound connector
    // and peer set are fake.

    let error_outbound_connector =
        service_fn(|_| async { Err("test outbound connector always returns errors".into()) });

    let (_config, mut peerset_rx) =
        spawn_crawler_with_peer_limit(1, error_outbound_connector).await;

    let peer_result = peerset_rx.try_recv();
    assert!(
        // `Err(TryRecvError::Empty)` means no peers are available and the channel is still open.
        // `Err(TryRecvError::Closed)` means the channel is closed.
        peer_result.is_err(),
        "unexpected peer when all connections error: {peer_result:?}",
    );
}

/// Test the crawler with an outbound peer limit of one peer,
/// and a connector that returns success then disconnects the peer.
#[tokio::test(start_paused = true)]
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
        let peer_result = peerset_rx.try_recv();
        match peer_result {
            // A peer handshake succeeded.
            Ok(_peer_change) => peer_count += 1,
            // The channel is closed or there are no messages left in the channel.
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
#[tokio::test(start_paused = true)]
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
        let peer_change_result = peerset_rx.try_recv();
        match peer_change_result {
            // A peer handshake succeeded.
            Ok(_peer_change) => peer_change_count += 1,
            // The channel is closed or there are no messages left in the channel.
            Err(_) => break,
        }
    }

    let mut peer_tracker_count: usize = 0;
    loop {
        let peer_tracker_result = peer_tracker_rx.try_recv();
        match peer_tracker_result {
            // We held this peer tracker open until now.
            Ok(peer_tracker) => {
                std::mem::drop(peer_tracker);
                peer_tracker_count += 1;
            }

            // The channel is closed or there are no messages left in the channel.
            Err(_) => break,
        }
    }

    assert!(
        peer_change_count > 0,
        "expected successful peer connections"
    );
    assert_eq!(
        peer_tracker_count, peer_change_count,
        "expected one held tracker per successful peer connection",
    );

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
#[tokio::test(start_paused = true)]
async fn crawler_peer_limit_default_connect_error() {
    let _init_guard = zebra_test::init();

    // This test does not require network access, because the outbound connector
    // and peer set are fake.

    let error_outbound_connector =
        service_fn(|_| async { Err("test outbound connector always returns errors".into()) });

    let (_config, mut peerset_rx) =
        spawn_crawler_with_peer_limit(None, error_outbound_connector).await;

    let peer_result = peerset_rx.try_recv();
    assert!(
        // `Err(TryRecvError::Empty)` means no peers are available and the channel is still open.
        // `Err(TryRecvError::Closed)` means the channel is closed.
        peer_result.is_err(),
        "unexpected peer when all connections error: {peer_result:?}",
    );
}

/// Test the crawler with the default outbound peer limit,
/// and a connector that returns success then disconnects the peer.
#[tokio::test(start_paused = true)]
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
        let peer_result = peerset_rx.try_recv();
        match peer_result {
            // A peer handshake succeeded.
            Ok(_peer_change) => peer_count += 1,
            // The channel is closed or there are no messages left in the channel.
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
#[tokio::test(start_paused = true)]
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
        let peer_change_result = peerset_rx.try_recv();
        match peer_change_result {
            // A peer handshake succeeded.
            Ok(_peer_change) => peer_change_count += 1,
            // The channel is closed or there are no messages left in the channel.
            Err(_) => break,
        }
    }

    let mut peer_tracker_count: usize = 0;
    loop {
        let peer_tracker_result = peer_tracker_rx.try_recv();
        match peer_tracker_result {
            // We held this peer tracker open until now.
            Ok(peer_tracker) => {
                std::mem::drop(peer_tracker);
                peer_tracker_count += 1;
            }

            // The channel is closed or there are no messages left in the channel.
            Err(_) => break,
        }
    }

    assert!(
        peer_change_count > 0,
        "expected successful peer connections"
    );
    assert_eq!(
        peer_tracker_count, peer_change_count,
        "expected one held tracker per successful peer connection",
    );

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

/// Test that the crawler timer fills the peer set up to the outbound connection limit
/// using existing address book candidates, without any demand signals, and even when
/// crawls return no new addresses. Then test that it replaces dropped connections,
/// and doesn't dial over the limit once it is reached.
#[tokio::test(start_paused = true)]
async fn crawler_refills_spare_outbound_capacity_on_timer() {
    let _init_guard = zebra_test::init();

    // This test does not require network access, because the outbound connector
    // and peer set are fake.

    let config = Config {
        peerset_initial_target_size: 1,
        // Tick often enough that the crawler gets multiple chances to fill spare capacity.
        crawl_new_peer_interval: Duration::from_millis(200),
        ..Config::default()
    };

    let outbound_limit = config.peerset_outbound_connection_limit();

    let (
        address_book,
        _bans_receiver,
        address_book_updater,
        address_book_service,
        _address_metrics,
        _address_book_updater_guard,
    ) = AddressBookUpdater::spawn(&config, config.listen_addr);

    // Add enough candidates for the initial fill and the replacement dials.
    let candidate_count = outbound_limit * 2 + 1;
    for address_number in 0..candidate_count {
        let addr = SocketAddr::new(Ipv4Addr::new(127, 1, 1, address_number as _).into(), 1);
        let addr = MetaAddr::new_gossiped_meta_addr(
            addr.into(),
            PeerServices::NODE_NETWORK,
            DateTime32::now(),
        )
        .new_gossiped_change()
        .expect("created MetaAddr contains enough information to represent a gossiped address");

        address_book
            .lock()
            .expect("panic in previous thread while accessing the address book")
            .update(addr);
    }

    // A peer set whose crawl responses never contain any addresses,
    // so the crawler can only dial existing address book candidates.
    let empty_peer_set = service_fn(|req| async move {
        let rsp = match req {
            Request::Peers => Response::Peers(Vec::new()),
            _ => unreachable!("unexpected request: {:?}", req),
        };

        Ok(rsp)
    });

    let (peer_tracker_tx, mut peer_tracker_rx) = mpsc::unbounded();

    let success_stay_open_outbound_connector = service_fn(move |req: OutboundConnectorRequest| {
        let peer_tracker_tx = peer_tracker_tx.clone();
        async move {
            let OutboundConnectorRequest {
                addr,
                connection_tracker,
            } = req;

            let (fake_client, _harness) = ClientTestHarness::build().finish();

            // Make the connection stay open.
            peer_tracker_tx
                .unbounded_send(connection_tracker)
                .expect("unexpected error sending to unbounded channel");

            Ok((addr, fake_client))
        }
    });

    let (peerset_tx, mut peerset_rx) = mpsc::channel::<DiscoveredPeer>(candidate_count);
    // The demand channel starts empty: all dials must come from the crawler timer.
    let (demand_tx, demand_rx) = mpsc::channel::<MorePeers>(candidate_count);

    let (next_peer_service, crawl_service) =
        crawler_services(address_book_service.clone(), empty_peer_set);
    let active_outbound_connections = ActiveConnectionCounter::new_counter();

    let crawl_task_handle = tokio::spawn(crawl_and_dial(
        config.clone(),
        demand_tx,
        demand_rx,
        next_peer_service,
        crawl_service,
        address_book_service,
        success_stay_open_outbound_connector,
        peerset_tx,
        active_outbound_connections,
        address_book_updater,
        ConnectedPeers::unconnected(),
    ));

    // Let the crawler fill the peer set from the timer.
    tokio::time::sleep(CRAWLER_TEST_DURATION / 2).await;

    let mut initial_peer_count: usize = 0;
    while peerset_rx.try_recv().is_ok() {
        initial_peer_count += 1;
    }

    assert_eq!(
        initial_peer_count, outbound_limit,
        "expected the timer to fill the peer set to the outbound limit without demand",
    );

    // Hold the initial connections open, then drop some to make spare capacity.
    let mut peer_trackers: Vec<ConnectionTracker> = Vec::new();
    while let Ok(peer_tracker) = peer_tracker_rx.try_recv() {
        peer_trackers.push(peer_tracker);
    }
    assert_eq!(
        peer_trackers.len(),
        outbound_limit,
        "expected one held connection tracker per successful handshake",
    );

    let dropped_count = outbound_limit.div_ceil(2);
    for peer_tracker in peer_trackers.drain(..dropped_count) {
        std::mem::drop(peer_tracker);
    }

    // Let the crawler replace the dropped connections.
    tokio::time::sleep(CRAWLER_TEST_DURATION / 2).await;

    let mut replacement_peer_count: usize = 0;
    while peerset_rx.try_recv().is_ok() {
        replacement_peer_count += 1;
    }

    assert_eq!(
        replacement_peer_count, dropped_count,
        "expected the crawler to replace exactly the dropped connections, \
         without dialing over the limit",
    );

    stop_crawler(crawl_task_handle).await;
}

/// Test that a timer crawl which discovers a candidate does not also refresh initial peers.
#[tokio::test]
async fn crawler_does_not_readd_initial_peers_after_finding_candidates() {
    let _init_guard = zebra_test::init();

    let initial_addr: PeerSocketAddr = "127.1.1.1:8233".parse().unwrap();
    let discovered_addr: PeerSocketAddr = "127.1.1.2:8233".parse().unwrap();
    let config = Config {
        initial_mainnet_peers: [initial_addr.remove_socket_addr_privacy().to_string()].into(),
        cache_dir: CacheDir::disabled(),
        // Only the first timer tick should run during this test.
        crawl_new_peer_interval: CRAWLER_TEST_DURATION * 2,
        peerset_initial_target_size: 1,
        ..Config::default()
    };

    let (
        address_book,
        _bans_receiver,
        address_book_updater,
        address_book_service,
        _address_metrics,
        _address_book_updater_guard,
    ) = AddressBookUpdater::spawn(&config, config.listen_addr);

    let peer_set = service_fn(move |req| async move {
        match req {
            Request::Peers => Ok(Response::Peers(vec![MetaAddr::new_gossiped_meta_addr(
                discovered_addr,
                PeerServices::NODE_NETWORK,
                DateTime32::now(),
            )])),
            _ => unreachable!("unexpected request: {req:?}"),
        }
    });
    let (next_peer_service, crawl_service) =
        crawler_services(address_book_service.clone(), peer_set);
    let (outbound_connector, _dial_count, _peer_tracker_rx) = counting_connector(0);
    let channel_size = config.peerset_outbound_connection_limit() * 2;
    let (peerset_tx, mut peerset_rx) = mpsc::channel(channel_size);
    let (demand_tx, demand_rx) = mpsc::channel(channel_size);

    let crawl_task_handle = tokio::spawn(crawl_and_dial(
        config,
        demand_tx,
        demand_rx,
        next_peer_service,
        crawl_service,
        address_book_service,
        outbound_connector,
        peerset_tx,
        ActiveConnectionCounter::new_counter(),
        address_book_updater,
        ConnectedPeers::unconnected(),
    ));

    let (connected_addr, _client) = tokio::time::timeout(CRAWLER_TEST_DURATION, peerset_rx.next())
        .await
        .expect("the crawl discovers a peer and queues connection demand")
        .expect("the crawler holds the peer set sender");
    assert_eq!(connected_addr, discovered_addr);

    // Allow an incorrectly spawned refresh to finish resolving the literal initial address.
    tokio::time::sleep(CRAWLER_TEST_DURATION / 10).await;
    stop_crawler(crawl_task_handle).await;

    assert!(
        address_book
            .lock()
            .expect("no task panicked while holding the address book")
            .get(initial_addr)
            .is_none(),
        "a successful crawl should not trigger an initial peer refresh",
    );
}

/// Test that a crawler with no peers adds its initial peers to the address book,
/// and dials them.
#[tokio::test(start_paused = true)]
async fn crawler_dials_readded_initial_peers() {
    let _init_guard = zebra_test::init();

    // This test does not require network access, because the initial peers are IP addresses,
    // and the outbound connector and peer set are fake.

    const PEER_COUNT: usize = 2;

    let (outbound_connector, _dial_count, _peer_tracker_rx) = counting_connector(0);
    // Start with an empty address book, so the crawler can only dial re-added peers.
    // The crawl timer only ticks once during the test, so the dials must come from the
    // demand that the re-add queues.
    let (_address_book, mut peerset_rx, crawl_task_handle) = spawn_crawler_with_initial_peers(
        PEER_COUNT,
        false,
        CRAWLER_TEST_DURATION * 2,
        outbound_connector,
    )
    .await;

    let connected_peer = tokio::time::timeout(CRAWLER_TEST_DURATION, peerset_rx.next())
        .await
        .expect("crawler connects to a re-added initial peer before the timeout")
        .expect("crawler holds the peer set sender while it is running");

    assert!(
        initial_peer_addrs(PEER_COUNT).contains(&connected_peer.0),
        "expected a connection to an initial peer, got {:?}",
        connected_peer.0,
    );

    stop_crawler(crawl_task_handle).await;
}

/// Test that a crawler whose initial peers all failed adds them again, keeping their
/// reconnection delay, at most once every `MIN_INITIAL_PEER_RECOVERY_INTERVAL`.
#[tokio::test(start_paused = true)]
async fn crawler_readds_failed_initial_peers_at_most_once_per_interval() {
    let _init_guard = zebra_test::init();

    // This test does not require network access, because the initial peers are IP addresses,
    // and the outbound connector and peer set are fake.

    const PEER_COUNT: usize = 2;

    let (outbound_connector, dial_count, _peer_tracker_rx) = counting_connector(usize::MAX);
    let (address_book, _peerset_rx, crawl_task_handle) = spawn_crawler_with_initial_peers(
        PEER_COUNT,
        true,
        CRAWLER_TEST_CRAWL_NEW_PEER_INTERVAL,
        outbound_connector,
    )
    .await;

    // The address book uses `std::time::Instant`, which doesn't advance while tokio time is
    // paused, so the re-added peers stay within their reconnection delay for the whole test.
    while dial_count.load(Ordering::SeqCst) < PEER_COUNT {
        tokio::time::sleep(CRAWLER_TEST_CRAWL_NEW_PEER_INTERVAL).await;
    }
    tokio::time::sleep(CRAWLER_TEST_CRAWL_NEW_PEER_INTERVAL * 5).await;

    for peer in initial_peers_in_address_book(&address_book, PEER_COUNT) {
        let peer = peer.expect("initial peers stay in the address book");
        assert_eq!(
            peer.last_connection_state,
            PeerAddrState::NeverAttemptedGossiped,
            "expected a failed initial peer to be added again",
        );
        assert!(
            peer.last_failure().is_some(),
            "expected a re-added peer to keep its last failure time",
        );
    }

    // Fail the peers again, then check they are only added again after the interval.
    for addr in initial_peer_addrs(PEER_COUNT) {
        address_book
            .lock()
            .expect("panic in previous thread while accessing the address book")
            .update(MetaAddr::new_errored(addr, None));
    }

    tokio::time::sleep(constants::MIN_INITIAL_PEER_RECOVERY_INTERVAL / 2).await;
    for peer in initial_peers_in_address_book(&address_book, PEER_COUNT) {
        assert_eq!(
            peer.expect("initial peers stay in the address book")
                .last_connection_state,
            PeerAddrState::Failed,
            "expected no re-add before the interval",
        );
    }

    tokio::time::sleep(constants::MIN_INITIAL_PEER_RECOVERY_INTERVAL / 2).await;
    for peer in initial_peers_in_address_book(&address_book, PEER_COUNT) {
        assert_eq!(
            peer.expect("initial peers stay in the address book")
                .last_connection_state,
            PeerAddrState::NeverAttemptedGossiped,
            "expected a re-add after the interval",
        );
    }

    assert_eq!(
        dial_count.load(Ordering::SeqCst),
        PEER_COUNT,
        "expected no dials within the reconnection delay",
    );

    stop_crawler(crawl_task_handle).await;
}

/// Test that the crawler doesn't add the initial peers again while it has an open
/// outbound connection, even if it has no other candidates.
#[tokio::test(start_paused = true)]
async fn crawler_does_not_readd_initial_peers_while_connected() {
    let _init_guard = zebra_test::init();

    // This test does not require network access, because the initial peers are IP addresses,
    // and the outbound connector and peer set are fake.

    const PEER_COUNT: usize = 2;

    // The first dial fails, so the crawler is left with one connection and one failed peer.
    let (outbound_connector, dial_count, _peer_tracker_rx) = counting_connector(1);
    let (address_book, mut peerset_rx, crawl_task_handle) = spawn_crawler_with_initial_peers(
        PEER_COUNT,
        true,
        CRAWLER_TEST_CRAWL_NEW_PEER_INTERVAL,
        outbound_connector,
    )
    .await;

    tokio::time::sleep(constants::MIN_INITIAL_PEER_RECOVERY_INTERVAL * 2).await;

    assert_eq!(dial_count.load(Ordering::SeqCst), PEER_COUNT);
    assert!(
        peerset_rx.try_recv().is_ok(),
        "expected one peer connection"
    );
    let failed_peers = initial_peers_in_address_book(&address_book, PEER_COUNT)
        .into_iter()
        .flatten()
        .filter(|peer| peer.last_connection_state == PeerAddrState::Failed)
        .count();
    assert_eq!(
        failed_peers, 1,
        "expected the failed initial peer to stay failed while an outbound connection is open",
    );

    stop_crawler(crawl_task_handle).await;
}

/// Test that re-adding the initial peers doesn't add a banned initial peer.
#[tokio::test(start_paused = true)]
async fn crawler_does_not_readd_banned_initial_peers() {
    let _init_guard = zebra_test::init();

    // This test does not require network access, because the initial peers are IP addresses,
    // and the outbound connector and peer set are fake.

    const PEER_COUNT: usize = 1;

    let (outbound_connector, dial_count, _peer_tracker_rx) = counting_connector(usize::MAX);
    let (address_book, _peerset_rx, crawl_task_handle) = spawn_crawler_with_initial_peers(
        PEER_COUNT,
        true,
        CRAWLER_TEST_CRAWL_NEW_PEER_INTERVAL,
        outbound_connector,
    )
    .await;

    // Wait for the first dial to fail, then ban the peer.
    while dial_count.load(Ordering::SeqCst) < PEER_COUNT {
        tokio::time::sleep(CRAWLER_TEST_CRAWL_NEW_PEER_INTERVAL).await;
    }
    for addr in initial_peer_addrs(PEER_COUNT) {
        address_book
            .lock()
            .expect("panic in previous thread while accessing the address book")
            .update(MetaAddr::new_misbehavior(
                addr,
                constants::MAX_PEER_MISBEHAVIOR_SCORE,
                "test ban",
            ));
    }

    tokio::time::sleep(constants::MIN_INITIAL_PEER_RECOVERY_INTERVAL * 2).await;

    assert!(
        initial_peers_in_address_book(&address_book, PEER_COUNT)
            .iter()
            .all(Option::is_none),
        "expected a banned initial peer to stay out of the address book",
    );

    stop_crawler(crawl_task_handle).await;
}

/// Returns a fake outbound connector that fails its first `fail_first` dials, then connects
/// and keeps each connection open, the dial count, and a receiver for the open connections.
fn counting_connector(
    fail_first: usize,
) -> (
    impl Service<
            OutboundConnectorRequest,
            Response = (PeerSocketAddr, peer::Client),
            Error = BoxError,
            Future: Send,
        > + Clone
        + Send
        + 'static,
    Arc<AtomicUsize>,
    mpsc::UnboundedReceiver<ConnectionTracker>,
) {
    let dial_count = Arc::new(AtomicUsize::new(0));
    let (peer_tracker_tx, peer_tracker_rx) = mpsc::unbounded();

    let outbound_connector = {
        let dial_count = dial_count.clone();
        service_fn(move |req: OutboundConnectorRequest| {
            let previous_dials = dial_count.fetch_add(1, Ordering::SeqCst);
            let peer_tracker_tx = peer_tracker_tx.clone();
            async move {
                if previous_dials < fail_first {
                    return Err("test outbound connector fails the first dials".into());
                }

                let (fake_client, _harness) = ClientTestHarness::build().finish();

                // Make the connection stay open.
                peer_tracker_tx
                    .unbounded_send(req.connection_tracker)
                    .expect("unexpected error sending to unbounded channel");

                Ok((req.addr, fake_client))
            }
        })
    };

    (outbound_connector, dial_count, peer_tracker_rx)
}

/// Returns the initial peer addresses used by [`spawn_crawler_with_initial_peers`].
fn initial_peer_addrs(peer_count: usize) -> HashSet<PeerSocketAddr> {
    (0..peer_count)
        .map(|address_number| {
            let address_number = u8::try_from(address_number).expect("tests use a few peers");
            SocketAddr::new(Ipv4Addr::new(127, 1, 1, address_number).into(), 1).into()
        })
        .collect()
}

/// Returns the address book entries for the addresses in [`initial_peer_addrs`].
fn initial_peers_in_address_book(
    address_book: &std::sync::Mutex<AddressBook>,
    peer_count: usize,
) -> Vec<Option<MetaAddr>> {
    let mut address_book = address_book
        .lock()
        .expect("panic in previous thread while accessing the address book");

    initial_peer_addrs(peer_count)
        .into_iter()
        .map(|addr| address_book.get(addr))
        .collect()
}

/// Starts a crawler configured with `peer_count` initial peers and no other peers,
/// using `crawl_new_peer_interval`.
///
/// If `add_to_address_book` is true, adds the initial peers to the address book like startup
/// does, so the crawler dials them as new candidates before any re-add.
///
/// Returns the address book, the peer set receiver, and the crawler task.
async fn spawn_crawler_with_initial_peers<C>(
    peer_count: usize,
    add_to_address_book: bool,
    crawl_new_peer_interval: Duration,
    outbound_connector: C,
) -> (
    Arc<std::sync::Mutex<AddressBook>>,
    mpsc::Receiver<DiscoveredPeer>,
    JoinHandle<Result<(), BoxError>>,
)
where
    C: Service<
            OutboundConnectorRequest,
            Response = (PeerSocketAddr, peer::Client),
            Error = BoxError,
        > + Clone
        + Send
        + 'static,
    C::Future: Send + 'static,
{
    // This address isn't actually bound - it just gets passed to the address book.
    let unused_v4 = "0.0.0.0:0".parse().unwrap();

    let config = Config {
        initial_mainnet_peers: initial_peer_addrs(peer_count)
            .iter()
            .map(|addr| addr.remove_socket_addr_privacy().to_string())
            .collect(),
        // We want exactly the above list of peers, without any cached peers.
        cache_dir: CacheDir::disabled(),
        network: Network::Mainnet,
        listen_addr: unused_v4,
        peerset_initial_target_size: peer_count,
        crawl_new_peer_interval,
        ..Config::default()
    };

    let (
        address_book,
        _bans_receiver,
        address_book_updater,
        address_book_service,
        _address_metrics,
        _address_book_updater_guard,
    ) = AddressBookUpdater::spawn(&config, unused_v4);

    if add_to_address_book {
        let initial_peers = limit_initial_peers(&config, &address_book_service).await;
        assert_eq!(initial_peers.len(), peer_count);
    }

    // A peer set with no peers to crawl. Any crawl response, even an empty one, queues
    // demand, so fail the crawl like a peer set with no ready peers does.
    let empty_peer_set = service_fn(|req| async move {
        match req {
            Request::Peers => Err::<Response, BoxError>("test peer set has no peers".into()),
            _ => unreachable!("unexpected request: {:?}", req),
        }
    });

    // Leave room for every connection and failed dial demand signal.
    let channel_size = config.peerset_outbound_connection_limit() * 2;
    let (peerset_tx, peerset_rx) = mpsc::channel::<DiscoveredPeer>(channel_size);
    let (demand_tx, demand_rx) = mpsc::channel::<MorePeers>(channel_size);

    let (next_peer_service, crawl_service) =
        crawler_services(address_book_service.clone(), empty_peer_set);

    let crawl_task_handle = tokio::spawn(crawl_and_dial(
        config,
        demand_tx,
        demand_rx,
        next_peer_service,
        crawl_service,
        address_book_service,
        outbound_connector,
        peerset_tx,
        ActiveConnectionCounter::new_counter(),
        address_book_updater,
        ConnectedPeers::unconnected(),
    ));

    (address_book, peerset_rx, crawl_task_handle)
}

/// Stops the crawler task, and checks it didn't panic or return an error.
async fn stop_crawler(crawl_task_handle: JoinHandle<Result<(), BoxError>>) {
    crawl_task_handle.abort();
    tokio::task::yield_now().await;

    let crawl_result = crawl_task_handle.now_or_never();
    assert!(
        crawl_result.is_none() || matches!(crawl_result, Some(Err(ref e)) if e.is_cancelled()),
        "unexpected error or panic in peer crawler task: {crawl_result:?}",
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

    // The listener can stop before a panicking handshake task reports its panic, so also record
    // whether the handshaker was called.
    let handshaker_called = Arc::new(AtomicBool::new(false));
    let called = handshaker_called.clone();
    let unreachable_inbound_handshaker = service_fn(move |_| {
        called.store(true, Ordering::SeqCst);
        async { unreachable!("inbound handshaker should never be called with a zero peer limit") }
    });

    let (_config, mut peerset_rx) =
        spawn_inbound_listener_with_peer_limit(0, None, unreachable_inbound_handshaker).await;

    let peer_result = peerset_rx.try_recv();
    assert!(
        // `Err(TryRecvError::Empty)` means no peers are available and the channel is still open.
        // `Err(TryRecvError::Closed)` means the channel is closed.
        peer_result.is_err(),
        "unexpected peer when inbound limit is zero: {peer_result:?}",
    );
    assert!(
        !handshaker_called.load(Ordering::SeqCst),
        "inbound handshaker was called with a zero peer limit",
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
        spawn_inbound_listener_with_peer_limit(1, None, error_inbound_handshaker).await;

    let peer_result = peerset_rx.try_recv();
    assert!(
        // `Err(TryRecvError::Empty)` means no peers are available and the channel is still open.
        // `Err(TryRecvError::Closed)` means the channel is closed.
        peer_result.is_err(),
        "unexpected peer when all handshakes error: {peer_result:?}",
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

            Ok(fake_client)
        });

    let (config, mut peerset_rx) = spawn_inbound_listener_with_peer_limit(
        1,
        usize::MAX,
        success_disconnect_inbound_handshaker,
    )
    .await;

    let mut peer_count: usize = 0;
    loop {
        let peer_result = peerset_rx.try_recv();
        match peer_result {
            // A peer handshake succeeded.
            Ok(_peer_change) => peer_count += 1,
            // The channel is closed or there are no messages left in the channel.
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
                    data_stream: mut tcp_stream,
                    connected_addr: _,
                    connection_tracker,
                } = req;

                let (fake_client, _harness) = ClientTestHarness::build().finish();

                // Signal acceptance without closing the connection.
                tcp_stream.write_all(&[0]).await.expect("test peer is open");

                // Make the connection staying open.
                peer_tracker_tx
                    .unbounded_send((tcp_stream, connection_tracker))
                    .expect("unexpected error sending to unbounded channel");

                Ok(fake_client)
            }
        });

    let (config, mut peerset_rx) =
        spawn_inbound_listener_with_peer_limit(1, None, success_stay_open_inbound_handshaker).await;

    let mut peer_change_count: usize = 0;
    loop {
        let peer_change_result = peerset_rx.try_recv();
        match peer_change_result {
            // A peer handshake succeeded.
            Ok(_peer_change) => peer_change_count += 1,
            // The channel is closed or there are no messages left in the channel.
            Err(_) => break,
        }
    }

    let mut peer_tracker_count: usize = 0;
    loop {
        let peer_tracker_result = peer_tracker_rx.try_recv();
        match peer_tracker_result {
            // We held this peer connection and tracker open until now.
            Ok((peer_connection, peer_tracker)) => {
                std::mem::drop(peer_connection);
                std::mem::drop(peer_tracker);
                peer_tracker_count += 1;
            }

            // The channel is closed or there are no messages left in the channel.
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

/// Test the listener reserves one inbound slot for a configured zcashd-compat peer,
/// matching native and IPv4-mapped address representations canonically.
#[tokio::test]
async fn listener_reserves_one_zcashd_compat_inbound_slot() {
    let _init_guard = zebra_test::init();

    // This test requires an IPv4 network stack with 127.0.0.1 as localhost.
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let public_ip = Ipv4Addr::LOCALHOST;
    let zcashd_compat_ip = Ipv4Addr::new(127, 0, 0, 2);
    let (peer_tracker_tx, mut peer_tracker_rx) = mpsc::unbounded();

    let success_stay_open_inbound_handshaker =
        service_fn(move |req: HandshakeRequest<TcpStream>| {
            let peer_tracker_tx = peer_tracker_tx.clone();
            async move {
                let HandshakeRequest {
                    data_stream: tcp_stream,
                    connected_addr,
                    connection_tracker,
                } = req;

                let ConnectedAddr::InboundDirect { addr } = connected_addr else {
                    unreachable!("inbound listener handshakes use inbound direct addresses");
                };

                let (fake_client, _harness) = ClientTestHarness::build()
                    .with_connected_addr(connected_addr)
                    .finish();

                peer_tracker_tx
                    .unbounded_send((addr.ip(), tcp_stream, connection_tracker))
                    .expect("unexpected error sending to unbounded channel");

                Ok(fake_client)
            }
        });

    let mut config = Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        peerset_initial_target_size: 1,
        max_connections_per_ip: usize::MAX,
        ..Config::default()
    };
    let (tcp_listener, listen_addr) = open_listener(&config.clone()).await;
    config.listen_addr = listen_addr;

    // One inbound slot is reserved for the zcashd-compat peer, the rest are public.
    let inbound_limit = config.peerset_inbound_connection_limit();
    let public_limit = inbound_limit - 1;

    let (peerset_tx, mut peerset_rx) = mpsc::channel::<DiscoveredPeer>(inbound_limit * 2);
    let (_bans_tx, bans_rx) = tokio::sync::watch::channel(Default::default());

    let listen_fut = accept_inbound_connections(
        config.clone(),
        tcp_listener,
        MIN_INBOUND_PEER_CONNECTION_INTERVAL_FOR_TESTS,
        success_stay_open_inbound_handshaker,
        peerset_tx,
        bans_rx,
        vec![IpAddr::V6(zcashd_compat_ip.to_ipv6_mapped())],
    );
    let listen_task_handle = tokio::spawn(listen_fut);

    // Open one more public connection than the public slots, and two zcashd-compat
    // connections for the single reserved slot.
    let mut connections = Vec::new();
    for _ in 0..=public_limit {
        connections.push(connect_from(public_ip, listen_addr).await);
    }
    connections.push(connect_from(zcashd_compat_ip, listen_addr).await);
    connections.push(connect_from(zcashd_compat_ip, listen_addr).await);

    tokio::time::sleep(ZCASHD_COMPAT_LISTENER_TEST_DURATION).await;

    listen_task_handle.abort();
    tokio::task::yield_now().await;
    assert_listener_task_cancelled(listen_task_handle);

    let accepted_ips = drain_accepted_inbound_ips(&mut peer_tracker_rx);
    let peer_change_count = drain_discovered_peers(&mut peerset_rx);

    assert_eq!(
        peer_change_count, inbound_limit,
        "accepted connections must stay within the total inbound limit"
    );
    assert_eq!(
        accepted_ips.iter().filter(|ip| **ip == public_ip).count(),
        public_limit,
        "ordinary inbound peers should only use the public slots"
    );
    assert_eq!(
        accepted_ips
            .iter()
            .filter(|ip| **ip == zcashd_compat_ip)
            .count(),
        1,
        "the canonically matched zcashd-compat peer should use the reserved slot"
    );

    std::mem::drop(connections);
}

/// Test the listener does not add zcashd-compat peers to the recent-IP throttle.
#[tokio::test]
async fn listener_zcashd_compat_reconnect_bypasses_recent_ip_limit() {
    let _init_guard = zebra_test::init();

    // This test requires an IPv4 network stack with 127.0.0.1 as localhost.
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let zcashd_compat_ip = Ipv4Addr::new(127, 0, 0, 2);
    let success_disconnect_inbound_handshaker =
        service_fn(|req: HandshakeRequest<TcpStream>| async move {
            let HandshakeRequest {
                data_stream: tcp_stream,
                connected_addr,
                connection_tracker,
            } = req;

            let (fake_client, _harness) = ClientTestHarness::build()
                .with_connected_addr(connected_addr)
                .finish();

            std::mem::drop(connection_tracker);
            std::mem::drop(tcp_stream);
            tokio::task::yield_now().await;

            Ok(fake_client)
        });

    let mut config = Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        peerset_initial_target_size: 1,
        max_connections_per_ip: 1,
        ..Config::default()
    };
    let (tcp_listener, listen_addr) = open_listener(&config.clone()).await;
    config.listen_addr = listen_addr;

    let (peerset_tx, mut peerset_rx) = mpsc::channel::<DiscoveredPeer>(2);
    let (_bans_tx, bans_rx) = tokio::sync::watch::channel(Default::default());

    let listen_fut = accept_inbound_connections(
        config,
        tcp_listener,
        MIN_INBOUND_PEER_CONNECTION_INTERVAL_FOR_TESTS,
        success_disconnect_inbound_handshaker,
        peerset_tx,
        bans_rx,
        vec![zcashd_compat_ip.into()],
    );
    let listen_task_handle = tokio::spawn(listen_fut);

    let first_connection = connect_from(zcashd_compat_ip, listen_addr).await;
    tokio::time::sleep(MIN_INBOUND_PEER_CONNECTION_INTERVAL_FOR_TESTS * 2).await;
    let second_connection = connect_from(zcashd_compat_ip, listen_addr).await;

    tokio::time::sleep(ZCASHD_COMPAT_LISTENER_TEST_DURATION).await;

    listen_task_handle.abort();
    tokio::task::yield_now().await;
    assert_listener_task_cancelled(listen_task_handle);

    assert_eq!(
        drain_discovered_peers(&mut peerset_rx),
        2,
        "rapid zcashd-compat reconnects should bypass the recent-IP throttle"
    );

    std::mem::drop(first_connection);
    std::mem::drop(second_connection);
}

/// Test bans are applied before the zcashd-compat reserved slot.
#[tokio::test]
async fn listener_bans_zcashd_compat_peer_before_reserved_slot() {
    let _init_guard = zebra_test::init();

    // This test requires an IPv4 network stack with 127.0.0.1 as localhost.
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let zcashd_compat_ip = Ipv4Addr::new(127, 0, 0, 2);
    let unreachable_inbound_handshaker = service_fn(|_| async {
        unreachable!("banned zcashd-compat peer should never reach the handshaker")
    });

    let mut config = Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        peerset_initial_target_size: 1,
        ..Config::default()
    };
    let (tcp_listener, listen_addr) = open_listener(&config.clone()).await;
    config.listen_addr = listen_addr;

    let (peerset_tx, mut peerset_rx) = mpsc::channel::<DiscoveredPeer>(1);
    let mut bans = crate::BanList::default();
    bans.ban(zcashd_compat_ip.into());
    let (bans_tx, bans_rx) = tokio::sync::watch::channel(bans);

    let listen_fut = accept_inbound_connections(
        config,
        tcp_listener,
        MIN_INBOUND_PEER_CONNECTION_INTERVAL_FOR_TESTS,
        unreachable_inbound_handshaker,
        peerset_tx,
        bans_rx,
        vec![zcashd_compat_ip.into()],
    );
    let listen_task_handle = tokio::spawn(listen_fut);

    let banned_connection = connect_from(zcashd_compat_ip, listen_addr).await;

    tokio::time::sleep(ZCASHD_COMPAT_LISTENER_TEST_DURATION).await;

    listen_task_handle.abort();
    tokio::task::yield_now().await;
    assert_listener_task_cancelled(listen_task_handle);

    assert_eq!(
        drain_discovered_peers(&mut peerset_rx),
        0,
        "banned zcashd-compat peers must not be admitted"
    );

    std::mem::drop(bans_tx);
    std::mem::drop(banned_connection);
}

/// Test that inbound connections accepted on a dual-stack listener are canonicalized,
/// so that IPv4 peers arriving as IPv4-mapped IPv6 addresses are keyed the same way as
/// the ban set, the recent-IP limiter, and the peer set.
#[tokio::test]
async fn listener_canonicalizes_ipv4_mapped_inbound_addrs() {
    let _init_guard = zebra_test::init();

    // This test requires an IPv4 network stack with 127.0.0.1 as localhost.
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    // And a dual-stack IPv6 listener.
    if zebra_test::net::zebra_skip_ipv6_tests() {
        return;
    }

    let peer_ip = Ipv4Addr::LOCALHOST;
    let (peer_tracker_tx, mut peer_tracker_rx) = mpsc::unbounded();

    let success_stay_open_inbound_handshaker =
        service_fn(move |req: HandshakeRequest<TcpStream>| {
            let peer_tracker_tx = peer_tracker_tx.clone();
            async move {
                let HandshakeRequest {
                    data_stream: tcp_stream,
                    connected_addr,
                    connection_tracker,
                } = req;

                let ConnectedAddr::InboundDirect { addr } = connected_addr else {
                    unreachable!("inbound listener handshakes use inbound direct addresses");
                };

                let (fake_client, _harness) = ClientTestHarness::build()
                    .with_connected_addr(connected_addr)
                    .finish();

                peer_tracker_tx
                    .unbounded_send((addr.ip(), tcp_stream, connection_tracker))
                    .expect("unexpected error sending to unbounded channel");

                Ok(fake_client)
            }
        });

    let mut config = Config {
        listen_addr: "[::]:0".parse().unwrap(),
        peerset_initial_target_size: 1,
        max_connections_per_ip: usize::MAX,
        ..Config::default()
    };
    let (tcp_listener, listen_addr) = open_listener(&config.clone()).await;
    config.listen_addr = listen_addr;

    let (peerset_tx, mut peerset_rx) = mpsc::channel::<DiscoveredPeer>(2);
    let (_bans_tx, bans_rx) = tokio::sync::watch::channel(Default::default());

    let listen_fut = accept_inbound_connections(
        config,
        tcp_listener,
        MIN_INBOUND_PEER_CONNECTION_INTERVAL_FOR_TESTS,
        success_stay_open_inbound_handshaker,
        peerset_tx,
        bans_rx,
        Vec::new(),
    );
    let listen_task_handle = tokio::spawn(listen_fut);

    // Connect over IPv4 to the dual-stack listener, so the accepted address is
    // the IPv4-mapped IPv6 form `::ffff:127.0.0.1`.
    let ipv4_listen_addr = SocketAddr::new(peer_ip.into(), listen_addr.port());
    let connection = connect_from(peer_ip, ipv4_listen_addr).await;

    tokio::time::sleep(ZCASHD_COMPAT_LISTENER_TEST_DURATION).await;

    listen_task_handle.abort();
    tokio::task::yield_now().await;
    assert_listener_task_cancelled(listen_task_handle);

    let mut accepted_ips = Vec::new();
    while let Ok((ip, peer_connection, peer_tracker)) = peer_tracker_rx.try_recv() {
        accepted_ips.push(ip);
        std::mem::drop(peer_connection);
        std::mem::drop(peer_tracker);
    }

    let mut peer_set_ips = Vec::new();
    while let Ok((addr, _client)) = peerset_rx.try_recv() {
        peer_set_ips.push(addr.ip());
    }

    assert_eq!(
        accepted_ips,
        vec![IpAddr::V4(peer_ip)],
        "inbound addresses must be canonicalized before they are used as keys"
    );
    assert_eq!(
        peer_set_ips,
        vec![IpAddr::V4(peer_ip)],
        "peer set keys must be canonical, so that ban and per-IP checks match"
    );

    std::mem::drop(connection);
}

/// Test that a ban on an IPv4 address is enforced against an inbound connection that
/// presents the IPv4-mapped IPv6 form of that address on a dual-stack listener.
#[tokio::test]
async fn listener_bans_ipv4_mapped_inbound_connection() {
    let _init_guard = zebra_test::init();

    // This test requires an IPv4 network stack with 127.0.0.1 as localhost.
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    // And a dual-stack IPv6 listener.
    if zebra_test::net::zebra_skip_ipv6_tests() {
        return;
    }

    let banned_ip = Ipv4Addr::LOCALHOST;
    let unreachable_inbound_handshaker = service_fn(|_| async {
        unreachable!("banned inbound peers should never reach the handshaker")
    });

    let mut config = Config {
        listen_addr: "[::]:0".parse().unwrap(),
        peerset_initial_target_size: 1,
        ..Config::default()
    };
    let (tcp_listener, listen_addr) = open_listener(&config.clone()).await;
    config.listen_addr = listen_addr;

    let (peerset_tx, mut peerset_rx) = mpsc::channel::<DiscoveredPeer>(1);
    // The ban is stored in the canonical IPv4 form, like `MetaAddr::new_misbehavior`
    // stores it.
    let mut bans = crate::BanList::default();
    bans.ban(banned_ip.into());
    let (bans_tx, bans_rx) = tokio::sync::watch::channel(bans);

    let listen_fut = accept_inbound_connections(
        config,
        tcp_listener,
        MIN_INBOUND_PEER_CONNECTION_INTERVAL_FOR_TESTS,
        unreachable_inbound_handshaker,
        peerset_tx,
        bans_rx,
        Vec::new(),
    );
    let listen_task_handle = tokio::spawn(listen_fut);

    // The banned peer connects over IPv4, so the listener accepts it as `::ffff:127.0.0.1`.
    let ipv4_listen_addr = SocketAddr::new(banned_ip.into(), listen_addr.port());
    let banned_connection = connect_from(banned_ip, ipv4_listen_addr).await;

    tokio::time::sleep(ZCASHD_COMPAT_LISTENER_TEST_DURATION).await;

    listen_task_handle.abort();
    tokio::task::yield_now().await;
    assert_listener_task_cancelled(listen_task_handle);

    assert_eq!(
        drain_discovered_peers(&mut peerset_rx),
        0,
        "banned IPv4 peers must not be admitted as IPv4-mapped IPv6 peers"
    );

    std::mem::drop(bans_tx);
    std::mem::drop(banned_connection);
}

/// End-to-end test: a peer that is banned while it is connected must be dropped from the
/// peer set, including when it connected to a dual-stack listener as IPv4-mapped IPv6.
///
/// Uses the real listener, a real TCP connection, a real handshake, and the real
/// misbehaviour channel returned by [`init()`].
#[tokio::test]
async fn banned_connected_inbound_peer_is_dropped_from_peer_set() {
    let _init_guard = zebra_test::init();

    // This test requires an IPv4 network stack with 127.0.0.1 as localhost.
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    // And a dual-stack IPv6 listener.
    if zebra_test::net::zebra_skip_ipv6_tests() {
        return;
    }

    let peers_inbound_service =
        service_fn(|_req| async move { Ok::<Response, BoxError>(Response::Peers(Vec::new())) });

    let config = Config {
        listen_addr: "[::]:0".parse().unwrap(),
        peerset_initial_target_size: 1,

        // Stop Zebra making outbound connections
        initial_mainnet_peers: IndexSet::new(),
        initial_testnet_peers: IndexSet::new(),
        cache_dir: CacheDir::disabled(),

        ..Config::default()
    };

    let (mut peer_set, address_book, misbehavior_tx, _advertiser) = init(
        config,
        peers_inbound_service,
        NoChainTip,
        "Test user agent".to_string(),
    )
    .await;

    let listen_addr = address_book.lock().unwrap().local_listener_socket_addr();
    let peer_ip = Ipv4Addr::LOCALHOST;

    // Connect over IPv4, so the listener accepts `::ffff:127.0.0.1`.
    let ipv4_listen_addr = SocketAddr::new(peer_ip.into(), listen_addr.port());
    let _connected_peer = connect_isolated_tcp_direct_with_inbound(
        &Mainnet,
        ipv4_listen_addr,
        "Test peer user agent".to_string(),
        peers_inbound_service,
    )
    .await
    .expect("local listener connection succeeds");

    // The peer set routes requests to the connected peer before it is banned.
    //
    // `peer_set` is a `Buffer`, so its `poll_ready` only checks the buffer's capacity: the
    // only way to tell whether the peer set has a usable peer is to send it a request.
    let response = tokio::time::timeout(
        PEER_SET_BAN_TEST_TIMEOUT,
        peer_set
            .ready()
            .await
            .expect("peer set buffer is ready")
            .call(Request::Peers),
    )
    .await;

    assert!(
        response.is_ok(),
        "the connected peer should answer requests before it is banned"
    );

    // Ban the peer, using the same channel that block and transaction verification uses.
    misbehavior_tx
        .send(MisbehaviorReport::new(
            PeerSocketAddr::from(SocketAddr::new(peer_ip.into(), ipv4_listen_addr.port())),
            constants::MAX_PEER_MISBEHAVIOR_SCORE,
            "test misbehavior",
        ))
        .await
        .expect("misbehavior channel is open");

    // # Correctness
    //
    // Misbehaviour updates are batched behind a flush timer, so the ban does not exist yet.
    // Wait for it to actually appear in the address book before checking the peer set,
    // otherwise this test passes whenever the timer hasn't fired, whatever the peer set does.
    let ban_deadline = Instant::now() + MISBEHAVIOR_FLUSH_TIMEOUT;
    let banned_ip: IpAddr = peer_ip.into();
    loop {
        if address_book.lock().unwrap().bans().is_banned(banned_ip) {
            break;
        }

        assert!(
            Instant::now() < ban_deadline,
            "the misbehaviour update should be flushed and banned within {MISBEHAVIOR_FLUSH_TIMEOUT:?}"
        );

        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // The banned peer must be dropped, so there is no peer left to route a request to.
    //
    // # Correctness
    //
    // Poll until the peer set stops answering, rather than waiting out a single long timeout.
    // This keeps the test fast when the peer is dropped promptly, but still gives a loaded
    // runner the full `PEER_SET_BAN_TEST_TIMEOUT` before the test fails.
    //
    // A single unanswered poll isn't enough: on a loaded runner it can just be scheduling
    // jitter, and treating that as success would make this test pass while the peer is still
    // connected. So the peer set has to miss `PEER_SET_UNREADY_POLLS` polls in a row.
    //
    // These polls must stay few: each request that is abandoned at the timeout keeps its
    // `Buffer` permit until the worker handles it, and the worker is blocked precisely because
    // there is no peer. `PEERSET_BUFFER_SIZE` is 3, so `peer_set.ready()` - which is outside
    // the timeout - would block forever if we ever abandoned that many requests.
    let drop_deadline = Instant::now() + PEER_SET_BAN_TEST_TIMEOUT;
    let mut unanswered_polls = 0;
    let mut abandoned_requests = 0;
    while unanswered_polls < PEER_SET_UNREADY_POLLS {
        let response = tokio::time::timeout(
            PEER_SET_UNREADY_POLL_TIMEOUT,
            peer_set
                .ready()
                .await
                .expect("peer set buffer is ready")
                .call(Request::Peers),
        )
        .await;

        if response.is_err() {
            unanswered_polls += 1;
            abandoned_requests += 1;

            // Fail rather than hang if the buffer is about to fill up, because
            // `peer_set.ready()` is outside the timeout above.
            assert!(
                abandoned_requests < constants::PEERSET_BUFFER_SIZE,
                "a banned peer must be dropped from the peer set, so its requests can't be routed"
            );

            continue;
        }

        unanswered_polls = 0;

        assert!(
            Instant::now() < drop_deadline,
            "a banned peer must be dropped from the peer set, so its requests can't be routed"
        );

        tokio::time::sleep(Duration::from_millis(50)).await;
    }
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
        spawn_inbound_listener_with_peer_limit(None, None, error_inbound_handshaker).await;

    let peer_result = peerset_rx.try_recv();
    assert!(
        // `Err(TryRecvError::Empty)` means no peers are available and the channel is still open.
        // `Err(TryRecvError::Closed)` means the channel is closed.
        peer_result.is_err(),
        "unexpected peer when all handshakes error: {peer_result:?}",
    );
}

/// Test the listener with the default inbound peer limit,
/// and a handshaker that returns success then disconnects the peer.
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

            Ok(fake_client)
        });

    let (config, mut peerset_rx) = spawn_inbound_listener_with_peer_limit(
        None,
        usize::MAX,
        success_disconnect_inbound_handshaker,
    )
    .await;

    let mut peer_count: usize = 0;
    loop {
        let peer_result = peerset_rx.try_recv();
        match peer_result {
            // A peer handshake succeeded.
            Ok(_peer_change) => peer_count += 1,
            // The channel is closed or there are no messages left in the channel.
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
                    data_stream: mut tcp_stream,
                    connected_addr: _,
                    connection_tracker,
                } = req;

                let (fake_client, _harness) = ClientTestHarness::build().finish();

                // Signal acceptance without closing the connection.
                tcp_stream.write_all(&[0]).await.expect("test peer is open");

                // Make the connection staying open.
                peer_tracker_tx
                    .unbounded_send((tcp_stream, connection_tracker))
                    .expect("unexpected error sending to unbounded channel");

                Ok(fake_client)
            }
        });

    let (config, mut peerset_rx) =
        spawn_inbound_listener_with_peer_limit(None, None, success_stay_open_inbound_handshaker)
            .await;

    let mut peer_change_count: usize = 0;
    loop {
        let peer_change_result = peerset_rx.try_recv();
        match peer_change_result {
            // A peer handshake succeeded.
            Ok(_peer_change) => peer_change_count += 1,
            // The channel is closed or there are no messages left in the channel.
            Err(_) => break,
        }
    }

    let mut peer_tracker_count: usize = 0;
    loop {
        let peer_tracker_result = peer_tracker_rx.try_recv();
        match peer_tracker_result {
            // We held this peer connection and tracker open until now.
            Ok((peer_connection, peer_tracker)) => {
                std::mem::drop(peer_connection);
                std::mem::drop(peer_tracker);
                peer_tracker_count += 1;
            }

            // The channel is closed or there are no messages left in the channel.
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

/// Test that Zebra sets `TCP_NODELAY` on the TCP sockets of inbound and outbound peer connections.
#[tokio::test]
async fn peer_tcp_connections_disable_nagle() {
    let _init_guard = zebra_test::init();

    // This test requires an IPv4 network stack with 127.0.0.1 as localhost.
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let (inbound_nodelay_tx, mut inbound_nodelay_rx) = mpsc::unbounded();

    // The listener has already called `set_tcp_nodelay()` on the stream when it calls the
    // handshaker, so this reads the option the real inbound handshake would see.
    let nodelay_inbound_handshaker = service_fn(move |req: HandshakeRequest<TcpStream>| {
        let inbound_nodelay_tx = inbound_nodelay_tx.clone();
        async move {
            inbound_nodelay_tx
                .unbounded_send(req.data_stream.nodelay())
                .expect("unexpected error sending to unbounded channel");

            let (fake_client, _harness) = ClientTestHarness::build().finish();

            Ok(fake_client)
        }
    });

    let config = Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        ..Config::default()
    };
    let (tcp_listener, listen_addr) = open_listener(&config.clone()).await;

    let (peerset_tx, _peerset_rx) = mpsc::channel::<DiscoveredPeer>(1);
    let (_bans_tx, bans_rx) = tokio::sync::watch::channel(Default::default());

    let listen_fut = accept_inbound_connections(
        config,
        tcp_listener,
        MIN_INBOUND_PEER_CONNECTION_INTERVAL_FOR_TESTS,
        nodelay_inbound_handshaker,
        peerset_tx,
        bans_rx,
        Vec::new(),
    );
    let listen_task_handle = tokio::spawn(listen_fut);

    // Outbound side: the socket from the connect helper that the outbound connector uses.
    let outbound_stream = peer::connect_tcp_nodelay(listen_addr)
        .await
        .expect("connecting to a local listener should succeed");
    assert!(
        outbound_stream
            .nodelay()
            .expect("reading TCP_NODELAY on an open socket should succeed"),
        "outbound peer connections should disable Nagle's algorithm",
    );

    // Inbound side: the result from the fake handshaker, for the same connection.
    let inbound_nodelay = tokio::time::timeout(LISTENER_TEST_TIMEOUT, inbound_nodelay_rx.next())
        .await
        .expect("the listener should hand the inbound connection to the handshaker")
        .expect("the handshaker keeps its sender until it is dropped with the listener");
    assert!(
        inbound_nodelay.expect("reading TCP_NODELAY on an open socket should succeed"),
        "inbound peer connections should disable Nagle's algorithm",
    );

    listen_task_handle.abort();
    tokio::task::yield_now().await;
    assert_listener_task_cancelled(listen_task_handle);

    std::mem::drop(outbound_stream);
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

    // Errors are ignored, so we don't expect any peers here
    assert_eq!(connections.len(), 0);
    // Make sure the rate limiting worked by checking if it took long enough
    assert!(
        elapsed
            > constants::MIN_OUTBOUND_PEER_CONNECTION_INTERVAL
                .saturating_mul((PEER_COUNT - 1) as u32),
        "elapsed only {elapsed:?}"
    );

    let initial_peers_result = initial_peers_task_handle.await;
    assert!(
        matches!(initial_peers_result, Ok(Ok(_))),
        "unexpected error or panic in add_initial_peers task: {initial_peers_result:?}",
    );

    // Check for panics or errors in the address book updater task.
    let updater_result = address_book_updater_task_handle.now_or_never();
    assert!(
        updater_result.is_none()
            || matches!(updater_result, Some(Err(ref join_error)) if join_error.is_cancelled())
            // The task method only returns one kind of error.
            // We can't check for error equality due to type erasure,
            // and we can't downcast due to ownership.
            || matches!(updater_result, Some(Ok(Err(ref _all_senders_closed)))),
        "unexpected error or panic in address book updater task: {updater_result:?}",
    );
}

/// Test that self-connections fail.
//
// TODO:
// - add a unit test that makes sure the error is a nonce reuse error
// - add a unit test that makes sure connections that replay nonces also get rejected
#[tokio::test]
async fn self_connections_should_fail() {
    let _init_guard = zebra_test::init();

    // This test requires an IPv4 network stack with 127.0.0.1 as localhost.
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    const TEST_PEERSET_INITIAL_TARGET_SIZE: usize = 3;
    const TEST_CRAWL_NEW_PEER_INTERVAL: Duration = Duration::from_secs(1);

    // If we get an inbound request from a peer, the test has a bug,
    // because self-connections should fail at the handshake stage.
    let unreachable_inbound_service =
        service_fn(|_| async { unreachable!("inbound service should never be called") });

    let force_listen_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();

    let no_initial_peers_config = Config {
        crawl_new_peer_interval: TEST_CRAWL_NEW_PEER_INTERVAL,

        initial_mainnet_peers: IndexSet::new(),
        initial_testnet_peers: IndexSet::new(),
        cache_dir: CacheDir::disabled(),

        ..Config::default()
    };

    let (_network, address_book) = init_with_peer_limit(
        TEST_PEERSET_INITIAL_TARGET_SIZE,
        unreachable_inbound_service,
        Mainnet,
        force_listen_addr,
        no_initial_peers_config,
    )
    .await;

    // Insert our own address into the address book, and make sure it works
    let (real_self_listener, updated_addr) = {
        let mut unlocked_address_book = address_book
            .lock()
            .expect("unexpected panic in address book");

        let real_self_listener = unlocked_address_book.local_listener_meta_addr(Utc::now());

        // Set a fake listener to get past the check for adding our own address
        unlocked_address_book.set_local_listener("192.168.0.0:1".parse().unwrap());

        let updated_addr = unlocked_address_book.update(
            real_self_listener
                .clone()
                .new_gossiped_change()
                .expect("change is valid"),
        );

        std::mem::drop(unlocked_address_book);

        (real_self_listener, updated_addr)
    };

    // Make sure we modified the address book correctly
    let updated_addr =
        updated_addr.expect("inserting our own address into the address book failed");
    assert_eq!(
        updated_addr.addr(),
        real_self_listener.addr(),
        "wrong address inserted into address book"
    );
    assert_ne!(
        updated_addr.addr().ip(),
        Ipv4Addr::UNSPECIFIED,
        "invalid address inserted into address book: ip must be valid for inbound connections"
    );
    assert_ne!(
        updated_addr.addr().port(),
        0,
        "invalid address inserted into address book: port must be valid for inbound connections"
    );

    // Wait for the crawler's failed self-connection to reach the address book.
    let self_connection_status = tokio::time::timeout(CRAWLER_TEST_DURATION, async {
        loop {
            let status = address_book
                .lock()
                .expect("unexpected panic in address book")
                .get(real_self_listener.addr())
                .expect("unexpected dropped listener address in address book");
            if status.last_connection_state == PeerAddrState::Failed {
                break status;
            }

            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the crawler should finish a self-connection attempt");

    // Make sure we fetched from the address book correctly
    assert_eq!(
        self_connection_status.addr(),
        real_self_listener.addr(),
        "wrong address fetched from address book"
    );

    // Make sure the self-connection failed
    assert_eq!(
        self_connection_status.last_connection_state,
        PeerAddrState::Failed,
        "self-connection should have failed"
    );
}

/// Test that the number of nonces is limited when peers send an invalid response or
/// if handshakes time out and are dropped.
#[tokio::test]
async fn remnant_nonces_from_outbound_connections_are_limited() {
    use tower::timeout::TimeoutLayer;

    let _init_guard = zebra_test::init();

    // This test requires an IPv4 network stack with 127.0.0.1 as localhost.
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    const TEST_PEERSET_INITIAL_TARGET_SIZE: usize = 3;

    // Create a test config that listens on an unused port.
    let listen_addr = "127.0.0.1:0".parse().unwrap();
    let config = Config {
        listen_addr,
        peerset_initial_target_size: TEST_PEERSET_INITIAL_TARGET_SIZE,
        ..Config::default()
    };

    let hs_timeout = TimeoutLayer::new(constants::HANDSHAKE_TIMEOUT);
    let nil_inbound_service =
        tower::service_fn(|_req| async move { Ok::<Response, BoxError>(Response::Nil) });

    let hs = peer::Handshake::builder()
        .with_config(config.clone())
        .with_inbound_service(nil_inbound_service)
        .with_user_agent(("Test user agent").to_string())
        .with_latest_chain_tip(NoChainTip)
        .want_transactions(true)
        .finish()
        .expect("configured all required parameters");

    let mut outbound_connector = hs_timeout.layer(peer::Connector::new(hs.clone()));

    let mut active_outbound_connections = ActiveConnectionCounter::new_counter();

    let expected_max_nonces = config.peerset_total_connection_limit();
    let num_connection_attempts = 2 * expected_max_nonces;

    for i in 1..num_connection_attempts {
        let expected_nonce_count = expected_max_nonces.min(i);

        let (tcp_listener, addr) = open_listener(&config.clone()).await;

        tokio::spawn(async move {
            let (mut tcp_stream, _addr) = tcp_listener
                .accept()
                .await
                .expect("client connection should succeed");

            tcp_stream
                .shutdown()
                .await
                .expect("shutdown should succeed");
        });

        let outbound_connector = outbound_connector
            .ready()
            .await
            .expect("outbound connector never errors");

        let connection_tracker = active_outbound_connections.track_connection();

        let req = OutboundConnectorRequest {
            addr: addr.into(),
            connection_tracker,
        };

        outbound_connector
            .call(req)
            .await
            .expect_err("connection attempt should fail");

        let nonce_count = hs.nonce_count().await;

        assert!(
            expected_max_nonces >= nonce_count,
            "number of nonces should be limited to `peerset_total_connection_limit`"
        );

        assert!(
            expected_nonce_count == nonce_count,
            "number of nonces should be the lesser of the number of closed connections and `peerset_total_connection_limit`"
        )
    }
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
    const TIME_LIMIT: Duration = Duration::from_secs(20);

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
        // Only use the configured dummy peers, not addresses from the default peer cache.
        cache_dir: CacheDir::disabled(),

        network: Network::Mainnet,
        listen_addr: unused_v4,

        ..Config::default()
    };

    let nil_inbound_service = service_fn(|_| async { Ok(Response::Nil) });

    let init_future = init(
        config,
        nil_inbound_service,
        NoChainTip,
        "Test user agent".to_string(),
    );

    tokio::time::timeout(TIME_LIMIT, init_future)
        .await
        .expect("should not timeout");
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
        cache_dir: CacheDir::disabled(),

        ..Config::default()
    };
    let inbound_service =
        service_fn(|_| async { unreachable!("inbound service should never be called") });

    let (_peer_service, address_book, _, _advertiser) = init(
        config,
        inbound_service,
        NoChainTip,
        "Test user agent".to_string(),
    )
    .await;
    let local_listener = address_book
        .lock()
        .unwrap()
        .local_listener_meta_addr(Utc::now());

    assert_ne!(
        local_listener.addr.port(),
        0,
        "dynamic ports are replaced with OS-assigned ports"
    );

    assert_eq!(
        local_listener.addr.ip(),
        listen_addr.ip(),
        "IP addresses are correctly propagated"
    );
}

/// Checks that the address book advertises the actual port while its listener is held open.
async fn local_listener_bound_port_with(listen_addr: SocketAddr, network: Network) {
    let config = Config {
        listen_addr,
        network,
        ..Config::default()
    };
    let (listener, bound_addr) = open_listener(&config).await;
    let (address_book, _, _, _, _, updater_task) = AddressBookUpdater::spawn(&config, bound_addr);
    let local_listener = address_book
        .lock()
        .unwrap()
        .local_listener_meta_addr(Utc::now());

    assert_eq!(
        local_listener.addr.port(),
        listener
            .local_addr()
            .expect("the listener has bound its address")
            .port(),
        "bound ports are correctly propagated"
    );
    assert_eq!(
        local_listener.addr.ip(),
        listen_addr.ip(),
        "IP addresses are correctly propagated"
    );
    updater_task.abort();
}

/// Initialize a peer set with `peerset_initial_target_size`, `inbound_service`, and `network`.
///
/// If `force_listen_addr` is set, binds the network listener to that address.
/// Otherwise, binds the network listener to an unused port on all network interfaces.
/// Uses `default_config` or Zebra's defaults for the rest of the configuration.
///
/// Returns the network owner and newly created [`AddressBook`] for testing.
async fn init_with_peer_limit<S>(
    peerset_initial_target_size: usize,
    inbound_service: S,
    network: Network,
    force_listen_addr: impl Into<Option<SocketAddr>>,
    default_config: impl Into<Option<Config>>,
) -> (crate::NetworkService, Arc<std::sync::Mutex<AddressBook>>)
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + Sync + 'static,
    S::Future: Send + 'static,
{
    // This test might fail on machines with no configured IPv4 addresses
    // (localhost should be enough).
    let unused_v4 = "0.0.0.0:0".parse().unwrap();

    let default_config = default_config.into().unwrap_or_default();

    let config = Config {
        peerset_initial_target_size,

        network,
        listen_addr: force_listen_addr.into().unwrap_or(unused_v4),

        ..default_config
    };

    let (peer_service, address_book, _, _advertiser) = init(
        config,
        inbound_service,
        NoChainTip,
        "Test user agent".to_string(),
    )
    .await;

    (peer_service, address_book)
}

/// Run a peer crawler with `peerset_initial_target_size` and `outbound_connector`.
///
/// Uses the default values for all other config fields, except the initial peers and disk cache,
/// which are empty.
/// Does not bind a local listener.
///
/// Returns the generated [`Config`], and the peer set receiver.
async fn spawn_crawler_with_peer_limit<C>(
    peerset_initial_target_size: impl Into<Option<usize>>,
    outbound_connector: C,
) -> (Config, mpsc::Receiver<DiscoveredPeer>)
where
    C: Service<
            OutboundConnectorRequest,
            Response = (PeerSocketAddr, peer::Client),
            Error = BoxError,
        > + Clone
        + Send
        + 'static,
    C::Future: Send + 'static,
{
    // Create a test config.
    let mut config = Config {
        crawl_new_peer_interval: CRAWLER_TEST_CRAWL_NEW_PEER_INTERVAL,
        // Without initial peers or a disk cache, a crawler with no connections has no initial
        // peers to add again, which would add DNS seeder or cached peers to the address book.
        initial_mainnet_peers: IndexSet::new(),
        initial_testnet_peers: IndexSet::new(),
        cache_dir: CacheDir::disabled(),
        ..Config::default()
    };
    if let Some(peerset_initial_target_size) = peerset_initial_target_size.into() {
        config.peerset_initial_target_size = peerset_initial_target_size;
    }

    let (
        address_book,
        _bans_receiver,
        address_book_updater,
        address_book_service,
        _address_metrics,
        _address_book_updater_guard,
    ) = AddressBookUpdater::spawn(&config, config.listen_addr);

    // Add enough fake peers to go over the limit, even if the limit is zero.
    let over_limit_peers = config.peerset_outbound_connection_limit() * 2 + 1;
    let mut fake_peer = None;
    for address_number in 0..over_limit_peers {
        let addr = SocketAddr::new(Ipv4Addr::new(127, 1, 1, address_number as _).into(), 1);
        let addr = MetaAddr::new_gossiped_meta_addr(
            addr.into(),
            PeerServices::NODE_NETWORK,
            DateTime32::now(),
        );
        fake_peer = Some(addr.clone());
        let addr = addr
            .new_gossiped_change()
            .expect("created MetaAddr contains enough information to represent a gossiped address");

        address_book
            .lock()
            .expect("panic in previous thread while accessing the address book")
            .update(addr);
    }

    // Create a fake peer set.
    let fake_peer = fake_peer.expect("there is at least one fake peer");
    let nil_peer_set = service_fn(move |req| {
        let fake_peer = fake_peer.clone();
        async move {
            let rsp = match req {
                // Return the correct response variant for Peers requests,
                // reusing one of the peers we already provided.
                Request::Peers => Response::Peers(vec![fake_peer]),
                _ => unreachable!("unexpected request: {:?}", req),
            };

            Ok(rsp)
        }
    });

    // Make the channels large enough to hold all the peers.
    let (peerset_tx, peerset_rx) = mpsc::channel::<DiscoveredPeer>(over_limit_peers);
    let (mut demand_tx, demand_rx) = mpsc::channel::<MorePeers>(over_limit_peers);

    let (next_peer_service, crawl_service) =
        crawler_services(address_book_service.clone(), nil_peer_set);

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
        next_peer_service,
        crawl_service,
        address_book_service,
        outbound_connector,
        peerset_tx,
        active_outbound_connections,
        address_book_updater,
        ConnectedPeers::unconnected(),
    );
    let crawl_task_handle = tokio::spawn(crawl_fut);

    // Let the crawler run for a while.
    tokio::time::sleep(CRAWLER_TEST_DURATION).await;

    stop_crawler(crawl_task_handle).await;

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

/// Connects to `listen_addr` from a specific loopback `source_ip`.
async fn connect_from(source_ip: Ipv4Addr, listen_addr: SocketAddr) -> TcpStream {
    let socket = TcpSocket::new_v4().expect("test should create an IPv4 TCP socket");
    socket
        .bind(SocketAddr::new(source_ip.into(), 0))
        .expect("test should bind to a loopback source address");
    socket
        .connect(listen_addr)
        .await
        .expect("test should connect to the inbound listener")
}

/// Drains the accepted inbound peer tracker channel, returning the accepted peer IPs.
fn drain_accepted_inbound_ips(
    peer_tracker_rx: &mut mpsc::UnboundedReceiver<(IpAddr, TcpStream, ConnectionTracker)>,
) -> Vec<Ipv4Addr> {
    let mut accepted_ips = Vec::new();

    while let Ok((ip, peer_connection, peer_tracker)) = peer_tracker_rx.try_recv() {
        let IpAddr::V4(ip) = ip else {
            panic!("zcashd-compat listener tests only use IPv4 peers");
        };

        accepted_ips.push(ip);
        std::mem::drop(peer_connection);
        std::mem::drop(peer_tracker);
    }

    accepted_ips
}

/// Drains the discovered peer channel, returning the number of discovered peers.
fn drain_discovered_peers(peerset_rx: &mut mpsc::Receiver<DiscoveredPeer>) -> usize {
    let mut peer_count = 0;

    while peerset_rx.try_recv().is_ok() {
        peer_count += 1;
    }

    peer_count
}

/// Asserts that an aborted inbound listener task was cancelled without errors or panics.
fn assert_listener_task_cancelled(listen_task_handle: JoinHandle<Result<(), BoxError>>) {
    let listen_result = listen_task_handle.now_or_never();
    assert!(
        listen_result.is_none() || matches!(listen_result, Some(Err(ref e)) if e.is_cancelled()),
        "unexpected error or panic in inbound peer listener task: {listen_result:?}",
    );
}

/// Run an inbound peer listener with `peerset_initial_target_size` and `handshaker`.
///
/// Binds the local listener to an unused localhost port.
/// Uses the default values for all other config fields.
/// Fake handshakers that keep connections open must send one byte to signal completion.
///
/// Returns the generated [`Config`], and the peer set receiver.
async fn spawn_inbound_listener_with_peer_limit<S>(
    peerset_initial_target_size: impl Into<Option<usize>>,
    max_connections_per_ip: impl Into<Option<usize>>,
    listen_handshaker: S,
) -> (Config, mpsc::Receiver<DiscoveredPeer>)
where
    S: Service<peer::HandshakeRequest<TcpStream>, Response = peer::Client, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    S::Future: Send + 'static,
{
    // Create a test config that listens on an unused port.
    let listen_addr = "127.0.0.1:0".parse().unwrap();
    let mut config = Config {
        listen_addr,
        max_connections_per_ip: max_connections_per_ip
            .into()
            .unwrap_or(constants::DEFAULT_MAX_CONNS_PER_IP),
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

    let (_bans_tx, bans_rx) = tokio::sync::watch::channel(Default::default());

    // Start listening for connections.
    let listen_fut = accept_inbound_connections(
        config.clone(),
        tcp_listener,
        MIN_INBOUND_PEER_CONNECTION_INTERVAL_FOR_TESTS,
        listen_handshaker,
        peerset_tx.clone(),
        bans_rx,
        Vec::new(),
    );
    let listen_task_handle = tokio::spawn(listen_fut);

    // Open inbound connections.
    let mut outbound_task_handles = Vec::new();
    for _ in 0..over_limit_connections {
        let outbound_fut = async move {
            let outbound_result = TcpStream::connect(listen_addr).await;
            // Let other tasks run before we block on reading.
            tokio::task::yield_now().await;

            if let Ok(mut outbound_stream) = outbound_result {
                // A held-open handshake sends one byte; rejection or disconnection sends EOF.
                let read_result = outbound_stream.read_exact(&mut [0]).await;
                debug!(?read_result, "inbound listener processed test connection");
                Some(outbound_stream)
            } else {
                // If the connection is closed quickly, we might get errors here.
                debug!(
                    ?outbound_result,
                    "outbound connection error in inbound listener test"
                );
                None
            }
        };

        let outbound_task_handle = tokio::spawn(outbound_fut);
        outbound_task_handles.push(outbound_task_handle);
    }

    // Keep accepted connections open until all attempts have reached the listener.
    let _outbound_connections = tokio::time::timeout(LISTENER_TEST_TIMEOUT, async {
        let mut connections = Vec::new();
        for outbound_task_handle in outbound_task_handles {
            connections.push(
                outbound_task_handle
                    .await
                    .expect("outbound connection task should not panic"),
            );
        }
        connections
    })
    .await
    .expect("the listener should handshake or reject every test connection");

    // Stop the listener and check that it did not panic or fail.
    listen_task_handle.abort();
    let listen_result = listen_task_handle.await;
    assert!(
        matches!(&listen_result, Err(e) if e.is_cancelled()),
        "unexpected error or panic in inbound peer listener task: {listen_result:?}",
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
    C: Service<
            OutboundConnectorRequest,
            Response = (PeerSocketAddr, peer::Client),
            Error = BoxError,
        > + Clone
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
        // We want exactly the above list of peers, without any cached peers.
        cache_dir: CacheDir::disabled(),

        network: Network::Mainnet,
        listen_addr: unused_v4,

        ..Config::default()
    };

    let (peerset_tx, peerset_rx) = mpsc::channel::<DiscoveredPeer>(peer_count + 1);

    let (
        _address_book,
        _bans_receiver,
        _address_book_updater,
        address_book_service,
        _address_metrics,
        address_book_updater_guard,
    ) = AddressBookUpdater::spawn(&config, unused_v4);

    let add_fut = add_initial_peers(config, outbound_connector, peerset_tx, address_book_service);
    let add_task_handle = tokio::spawn(add_fut);

    (add_task_handle, peerset_rx, address_book_updater_guard)
}

/// Check that misbehaviour reports sent while a flush is waiting on the address book
/// updater are received, rather than filling up the channel and being dropped.
#[tokio::test]
async fn misbehavior_reports_are_received_during_flush() {
    let _init_guard = zebra_test::init();
    tokio::time::pause();

    const CHANNEL_SIZE: usize = 4;
    const FLUSH_INTERVAL: Duration = Duration::from_millis(100);

    let (misbehavior_tx, misbehavior_rx) = tokio::sync::mpsc::channel(CHANNEL_SIZE);

    // A full address book channel, which nothing reads until the end of the test,
    // makes the flush wait.
    let (address_book_updater, mut address_book_rx) = change_channel(1);

    let batch_task = tokio::spawn(batch_misbehavior_updates(
        misbehavior_rx,
        address_book_updater,
        FLUSH_INTERVAL,
    ));

    let report = |port| {
        MisbehaviorReport::new(
            PeerSocketAddr::from(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port)),
            1,
            "test",
        )
    };

    // The first flush sends one of these updates, then waits to send the other one.
    misbehavior_tx.send(report(1)).await.unwrap();
    misbehavior_tx.send(report(2)).await.unwrap();
    tokio::time::sleep(FLUSH_INTERVAL * 2).await;

    // Verification code uses `try_send()`, which fails if the channel is full.
    for _ in 0..CHANNEL_SIZE * 3 {
        misbehavior_tx
            .try_send(report(3))
            .expect("the batch task should keep receiving while it waits to flush");
        tokio::task::yield_now().await;
    }

    // Unblock the flush, then wait for the next one, which has all the reports for peer 3.
    let mut updates = 0;
    while tokio::time::timeout(FLUSH_INTERVAL * 3, address_book_rx.recv())
        .await
        .is_ok_and(|update| update.is_some())
    {
        updates += 1;
    }

    assert_eq!(updates, 3, "expected one address book update for each peer");

    drop(misbehavior_tx);
    batch_task.await.expect("batch task should not panic");
}

/// A peer set with a version 2 initial peer and a legacy inbound peer serves
/// requests over both protocols, and routes the mempool snapshot request,
/// which version 2 connections cannot answer yet, to the legacy peer only.
#[tokio::test]
async fn mixed_legacy_and_v2_peers_route_by_capability() {
    let _init_guard = zebra_test::init();
    let network = Mainnet;

    // The responder's QUIC port must be known before the dialer starts; probe a free UDP port.
    let responder_v2_addr = {
        let probe = std::net::UdpSocket::bind("127.0.0.1:0").expect("a UDP port is free");
        probe.local_addr().expect("bound")
    };

    // The version 2 responder answers headers, but has no mempool answer (like a v2 connection).
    let responder_config = Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        network: network.clone(),
        initial_mainnet_peers: IndexSet::new(),
        initial_testnet_peers: IndexSet::new(),
        cache_dir: CacheDir::disabled(),
        v2_listen_addr: Some(responder_v2_addr),
        ..Config::default()
    };
    let responder_inbound = service_fn(move |request: Request| async move {
        match request {
            Request::FindHeaders { .. } => Ok(v2_headers_answer()),
            _ => Ok::<Response, BoxError>(Response::Nil),
        }
    });
    let (_responder_service, _responder_book, _, _advertiser) = init(
        responder_config,
        responder_inbound,
        NoChainTip,
        "v2 responder".to_string(),
    )
    .await;

    // The node under test dials the v2 responder and listens for legacy peers.
    let config = Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        network: network.clone(),
        initial_mainnet_peers: IndexSet::new(),
        initial_testnet_peers: IndexSet::new(),
        cache_dir: CacheDir::disabled(),
        v2_listen_addr: Some("127.0.0.1:0".parse().unwrap()),
        initial_v2_peers: [responder_v2_addr.to_string()].into_iter().collect(),
        // Both test peers are on 127.0.0.1; the peer set caps peers per IP across both protocols.
        max_connections_per_ip: 2,
        ..Config::default()
    };
    let inbound = service_fn(|_| async { Ok::<Response, BoxError>(Response::Nil) });
    let (mut peer_set, address_book, _, _advertiser) =
        init(config, inbound, NoChainTip, "mixed node".to_string()).await;

    // The version 2 peer is dialled in the background and is the only peer that can answer yet,
    // so the first answer proves the version 2 connection was established.
    assert_eq!(
        first_response(&mut peer_set, v2_headers_request()).await,
        v2_headers_answer(),
    );

    // A legacy peer connects inbound over TCP; its mempool answer is recognisable.
    let legacy_tx_id =
        zebra_chain::transaction::UnminedTxId::Legacy(zebra_chain::transaction::Hash([0x42; 32]));
    let legacy_gossiped: PeerSocketAddr = "203.0.113.6:8233".parse::<SocketAddr>().unwrap().into();
    let legacy_inbound = service_fn(move |request: Request| async move {
        match request {
            Request::MempoolTransactionIds => Ok(Response::TransactionIds(vec![legacy_tx_id])),
            // A legacy peer that does not answer `getaddr` makes the requester time out.
            Request::Peers => Ok(Response::Peers(vec![MetaAddr::new_gossiped_meta_addr(
                legacy_gossiped,
                PeerServices::NODE_NETWORK,
                DateTime32::now(),
            )])),
            _ => Ok::<Response, BoxError>(Response::Nil),
        }
    });
    let listen_addr = address_book.lock().unwrap().local_listener_socket_addr();
    let _legacy_peer = connect_isolated_tcp_direct_with_inbound(
        &network,
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), listen_addr.port()),
        "legacy peer".to_string(),
        legacy_inbound,
    )
    .await
    .expect("the legacy peer connects");

    // The mempool snapshot request must always reach the legacy peer: a v2 peer would answer
    // with an empty list. Repeat enough times that random routing would have hit v2 by now.
    let expected = Response::TransactionIds(vec![legacy_tx_id]);
    assert_eq!(
        first_response(&mut peer_set, Request::MempoolTransactionIds).await,
        expected
    );
    for _ in 0..19 {
        let response = tokio::time::timeout(Duration::from_secs(30), async {
            peer_set.ready().await.expect("buffer is ready");
            peer_set.call(Request::MempoolTransactionIds).await
        })
        .await
        .expect("answered in time")
        .expect("a legacy peer is ready");
        assert_eq!(response, expected);
    }
}

/// An IPv4 version 2 endpoint skips IPv6 initial v2 peers, which it could
/// never reach, instead of giving them one of its outbound slots: with a
/// single slot, the IPv4 peer listed after an IPv6 one is still dialled.
#[tokio::test]
async fn ipv4_v2_endpoint_skips_ipv6_initial_peers() {
    let _init_guard = zebra_test::init();
    let network = Mainnet;

    // The responder's QUIC port must be known before the dialer starts; probe a free UDP port.
    let responder_v2_addr = {
        let probe = std::net::UdpSocket::bind("127.0.0.1:0").expect("a UDP port is free");
        probe.local_addr().expect("bound")
    };

    let responder_config = Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        network: network.clone(),
        initial_mainnet_peers: IndexSet::new(),
        initial_testnet_peers: IndexSet::new(),
        cache_dir: CacheDir::disabled(),
        v2_listen_addr: Some(responder_v2_addr),
        ..Config::default()
    };
    let responder_inbound = service_fn(move |request: Request| async move {
        match request {
            Request::FindHeaders { .. } => Ok(v2_headers_answer()),
            _ => Ok::<Response, BoxError>(Response::Nil),
        }
    });
    let (_responder_service, _responder_book, _, _advertiser) = init(
        responder_config,
        responder_inbound,
        NoChainTip,
        "v2 responder".to_string(),
    )
    .await;

    // The IPv6 peer comes first, so without the skip it would take the only outbound v2 slot.
    let ipv6_peer = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), responder_v2_addr.port());
    let config = Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        network: network.clone(),
        initial_mainnet_peers: IndexSet::new(),
        initial_testnet_peers: IndexSet::new(),
        cache_dir: CacheDir::disabled(),
        v2_listen_addr: Some("127.0.0.1:0".parse().unwrap()),
        initial_v2_peers: [ipv6_peer.to_string(), responder_v2_addr.to_string()]
            .into_iter()
            .collect(),
        // One legacy target peer gives the version 2 dialer a single outbound slot.
        peerset_initial_target_size: 1,
        ..Config::default()
    };
    let inbound = service_fn(|_| async { Ok::<Response, BoxError>(Response::Nil) });
    let (mut peer_set, _address_book, _, _advertiser) =
        init(config, inbound, NoChainTip, "ipv4 node".to_string()).await;

    assert_eq!(
        first_response(&mut peer_set, v2_headers_request()).await,
        v2_headers_answer(),
    );
}

/// A request that the version 2 test responders answer with [`v2_headers_answer`]: the headers
/// after the Mainnet genesis block.
fn v2_headers_request() -> Request {
    Request::FindHeaders {
        known_blocks: vec![Mainnet.genesis_hash()],
        stop: None,
    }
}

/// The version 2 test responders' answer to [`v2_headers_request`]: the Mainnet block 1 header.
fn v2_headers_answer() -> Response {
    let block_1 = zebra_chain::block::Block::zcash_deserialize(
        &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
    )
    .expect("the test vector is a valid block");
    Response::BlockHeaders(vec![zebra_chain::block::CountedHeader {
        header: block_1.header,
    }])
}

/// Polls `peer_set` with `request` until a peer answers it: peers connect in
/// the background, so the first calls fail with no ready peer.
async fn first_response<S>(peer_set: &mut S, request: Request) -> Response
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let attempt = async {
                peer_set.ready().await?;
                peer_set.call(request.clone()).await
            }
            .await;
            match attempt {
                Ok(response) => break response,
                Err(error) => {
                    tracing::debug!(%error, ?request, "waiting for a peer that can answer");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    })
    .await
    .expect("a peer answers within the timeout")
}

/// The reconnection delay of the version 2 dialer tests: short, so they can watch several
/// redials.
const V2_TEST_RECONNECTION_DELAY: Duration = Duration::from_millis(300);

/// How long version 2 tests wait for a connection that should be made.
const V2_TEST_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// How long version 2 tests watch for connections that should not be made: several redials.
const V2_TEST_QUIET_PERIOD: Duration = Duration::from_secs(3);

/// The jittered reconnection delay stays within a quarter of the base delay above it.
#[test]
fn v2_reconnection_jitter_is_bounded() {
    let _init_guard = zebra_test::init();

    let delay = constants::MIN_PEER_RECONNECTION_DELAY;
    for _ in 0..1000 {
        let jittered = v2::with_jitter(delay);
        assert!(jittered >= delay && jittered <= delay + delay / 4);
    }
}

/// The version 2 listener refuses connections from banned IPs, and accepts them otherwise.
#[tokio::test]
async fn v2_listener_refuses_banned_ip() {
    let _init_guard = zebra_test::init();
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    for banned in [true, false] {
        let mut bans = crate::BanList::default();
        if banned {
            bans.ban(Ipv4Addr::LOCALHOST.into());
        }
        let (_bans_tx, bans_rx) = watch::channel(bans);

        let listener_addr = free_udp_addr();
        let (mut listener_rx, listener_task) =
            spawn_v2_node(v2_test_config(listener_addr, &[]), bans_rx);
        let (_dialer_rx, dialer_task) =
            spawn_v2_node(v2_test_config(any_udp_addr(), &[listener_addr]), no_bans());

        if banned {
            tokio::time::sleep(V2_TEST_QUIET_PERIOD).await;
            assert_eq!(
                drain_discovered_peers(&mut listener_rx),
                0,
                "a banned IP must not be accepted"
            );
        } else {
            tokio::time::timeout(V2_TEST_CONNECT_TIMEOUT, listener_rx.next())
                .await
                .expect("an unbanned IP is accepted")
                .expect("the listener is running");
        }

        listener_task.abort();
        dialer_task.abort();
    }
}

/// The version 2 dialer does not dial a configured peer whose IP is banned, so a misbehaving
/// configured peer is not reconnected, and dials it once the ban is lifted.
#[tokio::test]
async fn v2_dialer_skips_banned_initial_peers() {
    let _init_guard = zebra_test::init();
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let mut bans = crate::BanList::default();
    bans.ban(Ipv4Addr::LOCALHOST.into());
    let (bans_tx, bans_rx) = watch::channel(bans);

    let listener_addr = free_udp_addr();
    let (mut listener_rx, listener_task) =
        spawn_v2_node(v2_test_config(listener_addr, &[]), no_bans());
    let (_dialer_rx, dialer_task) =
        spawn_v2_node(v2_test_config(any_udp_addr(), &[listener_addr]), bans_rx);

    // Several reconnection delays, so the dialer has retried while the peer is banned.
    tokio::time::sleep(V2_TEST_QUIET_PERIOD).await;
    assert_eq!(
        drain_discovered_peers(&mut listener_rx),
        0,
        "a banned configured peer must not be dialled"
    );

    bans_tx.send_replace(Default::default());
    tokio::time::timeout(V2_TEST_CONNECT_TIMEOUT, listener_rx.next())
        .await
        .expect("the configured peer is dialled once it is no longer banned")
        .expect("the listener is running");

    listener_task.abort();
    dialer_task.abort();
}

/// A banned configured peer takes no outbound slot while it waits for the reconnection delay:
/// with one slot, the live peer listed after it is dialled at once, and only the peer listed
/// after that one is logged as waiting for a slot: not the banned peer, which is deferred, and
/// not the live peer, which holds the slot.
#[tokio::test]
#[tracing_test::traced_test]
async fn v2_dialer_gives_banned_initial_peer_no_slot() {
    // No `zebra_test::init()`: `traced_test` installs the subscriber that records the logs.
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    // The banned peer is on another loopback address, so the ban does not cover the live peer.
    // It is never dialled, so nothing needs to listen there, nor at the waiting peer's address,
    // which never gets the slot.
    let banned_ip = Ipv4Addr::new(127, 0, 0, 2);
    let [live_addr, waiting_addr] = free_udp_addrs();
    let banned_addr = SocketAddr::new(banned_ip.into(), live_addr.port());
    let mut bans = crate::BanList::default();
    bans.ban(banned_ip.into());
    let (_bans_tx, bans_rx) = watch::channel(bans);

    let (mut live_rx, live_task) = spawn_v2_node(v2_test_config(live_addr, &[]), no_bans());

    // The banned peer is listed first, so it is offered the only slot first. With the production
    // reconnection delay, a slot held while the banned peer waits would not free in this test.
    let dialer_config = Config {
        peerset_initial_target_size: 1,
        ..v2_test_config(any_udp_addr(), &[banned_addr, live_addr, waiting_addr])
    };
    assert_eq!(dialer_config.v2_outbound_connection_limit(), 1);
    let (_dialer_rx, dialer_task) = spawn_v2_node_with_delay(
        dialer_config,
        bans_rx,
        constants::MIN_PEER_RECONNECTION_DELAY,
    );

    tokio::time::timeout(V2_TEST_CONNECT_TIMEOUT, live_rx.next())
        .await
        .expect("the live configured peer is dialled while the banned one waits")
        .expect("the listener is running");

    // Once a round of dials ends, the dialer logs each host with addresses that wait for a slot,
    // in the configured order, so the waiting peer's log comes after any for the two before it.
    let waiting_log = |host: &SocketAddr| {
        format!("wait for a connection to fail or end before they are dialled host={host} ")
    };
    tokio::time::timeout(V2_TEST_CONNECT_TIMEOUT, async {
        while !logs_contain(&waiting_log(&waiting_addr)) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the peer that has no slot is logged as waiting for one");
    assert!(
        !logs_contain(&waiting_log(&banned_addr)),
        "the banned peer is deferred, so it does not wait for a slot"
    );
    assert!(
        !logs_contain(&waiting_log(&live_addr)),
        "the live peer holds the slot"
    );

    live_task.abort();
    dialer_task.abort();
}

/// A dialer with no outbound slot warns that its configured peers are never dialled, and stops,
/// instead of logging that they are dialled when a connection fails or ends, which never happens.
///
/// `Config::v2_outbound_connection_limit` is at least one when the endpoint is configured, so this
/// checks the dialer's guard against a change there.
#[tokio::test]
#[tracing_test::traced_test]
async fn v2_dialer_stops_with_a_warning_when_it_has_no_outbound_slot() {
    // No `zebra_test::init()`: `traced_test` installs the subscriber that records the logs.
    let config = v2_test_config(any_udp_addr(), &[*test_v2_addr(1)]);
    let endpoint = crate::quic::bind(&config.network, any_udp_addr()).expect("a UDP port is free");
    let identity = LocalIdentity {
        user_agent: "v2 test node".to_string(),
        services: PeerServices::NODE_NETWORK,
        relay: true,
    };
    let (inv_collector, _) = broadcast::channel(16);
    let (misbehavior_tx, _) = tokio::sync::mpsc::channel(1);
    let (shared, _open_inbound_tx) = v2::Shared::new(
        &config,
        endpoint,
        identity,
        service_fn(|_| async { Ok::<Response, BoxError>(Response::Nil) }),
        inv_collector,
        NoChainTip,
        misbehavior_tx,
        no_bans(),
        V2_TEST_RECONNECTION_DELAY,
        MIN_INBOUND_PEER_CONNECTION_INTERVAL_FOR_TESTS,
        crate::v2_address_book::V2BookHandle::new(tokio::sync::mpsc::channel(1).0),
        ConnectedPeers::unconnected(),
    );
    let (peerset_tx, _peerset_rx) = mpsc::channel(1);

    tokio::time::timeout(
        V2_TEST_CONNECT_TIMEOUT,
        v2::keep_initial_peers_connected(
            shared,
            config.initial_v2_peers,
            0,
            no_bans(),
            peerset_tx,
            watch::channel(Default::default()).0,
        ),
    )
    .await
    .expect("the dialer stops when it has no slot");

    assert!(logs_contain("the outbound v2 connection limit is zero"));
    assert!(!logs_contain("wait for a connection to fail or end"));
}

/// The version 2 dialer tells the legacy crawler to leave the resolved configured version 2
/// peers to it, so a legacy connection the crawler makes first cannot keep it from them.
#[tokio::test]
async fn v2_dialer_keeps_the_legacy_crawler_from_its_configured_peers() {
    let _init_guard = zebra_test::init();
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let configured = test_v2_addr(1);
    let config = v2_test_config(any_udp_addr(), &[*configured]);
    let endpoint = crate::quic::bind(&config.network, any_udp_addr()).expect("a UDP port is free");
    let identity = LocalIdentity {
        user_agent: "v2 test node".to_string(),
        services: PeerServices::NODE_NETWORK,
        relay: true,
    };
    let (inv_collector, _) = broadcast::channel(16);
    let (misbehavior_tx, _) = tokio::sync::mpsc::channel(1);
    let connected_peers = ConnectedPeers::unconnected();
    let legacy_addr: PeerSocketAddr = SocketAddr::new(configured.ip(), 8233).into();
    assert_eq!(connected_peers.blocks_legacy_dial(legacy_addr), None);

    let (shared, _open_inbound_tx) = v2::Shared::new(
        &config,
        endpoint,
        identity,
        service_fn(|_| async { Ok::<Response, BoxError>(Response::Nil) }),
        inv_collector,
        NoChainTip,
        misbehavior_tx,
        no_bans(),
        V2_TEST_RECONNECTION_DELAY,
        MIN_INBOUND_PEER_CONNECTION_INTERVAL_FOR_TESTS,
        crate::v2_address_book::V2BookHandle::new(tokio::sync::mpsc::channel(1).0),
        connected_peers.clone(),
    );
    let (peerset_tx, _peerset_rx) = mpsc::channel(1);
    let dialer_task = tokio::spawn(v2::keep_initial_peers_connected(
        shared,
        config.initial_v2_peers,
        1,
        no_bans(),
        peerset_tx,
        watch::channel(Default::default()).0,
    ));

    tokio::time::timeout(V2_TEST_CONNECT_TIMEOUT, async {
        while connected_peers.blocks_legacy_dial(legacy_addr) != Some("configured v2 peer") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the dialer reserves its resolved configured peers");

    dialer_task.abort();
}

/// The version 2 listener refuses connections over its share of the inbound limit.
#[tokio::test]
async fn v2_listener_refuses_connections_over_inbound_limit() {
    let _init_guard = zebra_test::init();
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    // A zero legacy target makes every legacy limit zero, so the version 2 inbound limit is its
    // minimum of one connection.
    let listener_config = Config {
        peerset_initial_target_size: 0,
        ..v2_test_config(free_udp_addr(), &[])
    };
    assert_eq!(listener_config.v2_inbound_connection_limit(), 1);

    assert_v2_listener_accepts_one_of_two_dialers(listener_config).await;
}

/// The listener's accept rate is a burst, then one token per interval, and taking a token never
/// waits.
#[test]
fn v2_accept_rate_is_a_burst_then_one_per_interval() {
    let interval = Duration::from_secs(1);
    let start = tokio::time::Instant::now();
    let mut rate = TokenBucket::new(interval, 3, start);

    for _ in 0..3 {
        assert!(rate.try_take(start), "the burst is available at once");
    }
    assert!(!rate.is_available(start));
    assert!(!rate.try_take(start + interval / 2));

    // One token per interval, counted from when the bucket was last full.
    assert!(rate.try_take(start + interval));
    assert!(!rate.try_take(start + interval + interval / 2));
    assert!(rate.try_take(start + 2 * interval));

    // A long quiet time refills no more than the burst.
    let later = start + 100 * interval;
    for _ in 0..3 {
        assert!(rate.try_take(later));
    }
    assert!(!rate.try_take(later));

    // An empty bucket's next token is one interval after the last one started to refill.
    assert_eq!(rate.next_token_at(later), later + interval);
    assert_eq!(rate.next_token_at(later + interval / 4), later + interval);
    assert_eq!(rate.next_token_at(later + interval), later + interval);
    assert!(rate.try_take(later + interval));

    // A zero interval never limits.
    let mut unlimited = TokenBucket::new(Duration::ZERO, 1, start);
    assert!(unlimited.try_take(start));
    assert!(unlimited.try_take(start));
}

/// The version 2 listener answers every connection attempt at once, whatever its accept rate:
/// the attempts within the burst are accepted, and the next one is refused rather than left
/// without a Retry until the dialer's connect timeout.
#[tokio::test]
async fn v2_listener_answers_attempts_beyond_its_accept_rate_at_once() {
    let _init_guard = zebra_test::init();
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let listener_addr = free_udp_addr();
    let config = v2_test_config(listener_addr, &[]);
    let (peerset_tx, _peerset_rx) = mpsc::channel(config.peerset_total_connection_limit());
    let (inv_collector, _) = broadcast::channel(16);
    let (misbehavior_tx, _) = tokio::sync::mpsc::channel(1);
    // An accept interval far longer than the test, so the rate never refills.
    let listener_task = v2::start_with_intervals(
        config,
        service_fn(|_| async { Ok::<Response, BoxError>(Response::Nil) }),
        inv_collector,
        NoChainTip,
        "v2 test node".to_string(),
        no_bans(),
        misbehavior_tx,
        peerset_tx,
        ConnectedPeers::unconnected(),
        V2_TEST_RECONNECTION_DELAY,
        Duration::from_secs(3600),
    )
    .expect("version 2 test configs listen");

    let dialer = crate::quic::bind(&Mainnet, any_udp_addr()).expect("a UDP port is free");
    let mut accepted = Vec::new();
    for attempt in 0..v2::INBOUND_ACCEPT_BURST {
        let connection = crate::quic::connect(&dialer, listener_addr)
            .await
            .unwrap_or_else(|error| panic!("attempt {attempt} is within the burst: {error:?}"));
        accepted.push(connection);
    }

    let started = Instant::now();
    let error = crate::quic::connect(&dialer, listener_addr)
        .await
        .expect_err("an attempt beyond the accept rate is refused");
    assert!(
        matches!(
            error,
            crate::quic::ConnectError::Handshake(quinn::ConnectionError::ConnectionClosed(_))
        ),
        "the attempt is refused, not left to time out, got {error:?}"
    );
    assert!(
        started.elapsed() < constants::HANDSHAKE_TIMEOUT,
        "the refusal arrives before the connect timeout"
    );

    listener_task.abort();
}

/// The version 2 listener's per-IP limits count an IPv6 address by its `/48`, so one site's
/// allocation cannot take a slot for each of its 65,536 `/64`s, and an IPv4 address by itself.
#[test]
fn v2_inbound_limits_group_ipv6_by_48() {
    let ip = |addr: &str| addr.parse::<IpAddr>().expect("a valid IP address");

    let group = ip("2001:db8:1::");
    for same_group in [
        "2001:db8:1::1",
        "2001:db8:1:1::1",
        "2001:db8:1:ffff:ffff:ffff:ffff:ffff",
    ] {
        assert_eq!(v2::inbound_group(ip(same_group)), group, "{same_group}");
    }
    assert_ne!(v2::inbound_group(ip("2001:db8:2::1")), group);
    // IPv4 addresses are their own groups, whichever way they are written.
    assert_eq!(v2::inbound_group(ip("192.0.2.1")), ip("192.0.2.1"));
    assert_eq!(v2::inbound_group(ip("::ffff:192.0.2.1")), ip("192.0.2.1"));
    assert_ne!(v2::inbound_group(ip("192.0.2.2")), ip("192.0.2.1"));

    // An attempt is only recent for an instant here, so only the open connections limit.
    let mut groups = v2::InboundGroups::new(1, Some(Duration::ZERO));
    let first = ip("2001:db8:1:1::1");
    let other_64 = ip("2001:db8:1:2::1");
    let other_48 = ip("2001:db8:2:1::1");
    assert!(!groups.is_full(first));
    let opened = groups.opened(first);
    assert_eq!(opened, group);
    assert!(
        groups.is_full(other_64),
        "another /64 of the same /48 shares its connection limit"
    );
    assert!(!groups.is_full(other_48));
    assert!(!groups.is_full(ip("192.0.2.1")));
    groups.closed(opened);
    assert!(!groups.is_full(other_64));

    // Recent attempts are counted by the same group.
    let mut groups = v2::InboundGroups::new(1, None);
    assert!(!groups.is_past_recent_limit_or_add(first));
    assert!(
        groups.is_past_recent_limit_or_add(other_64),
        "another /64 of the same /48 shares its rate limit"
    );
    assert!(!groups.is_past_recent_limit_or_add(other_48));
    assert!(!groups.is_past_recent_limit_or_add(ip("192.0.2.1")));
    assert!(groups.is_past_recent_limit_or_add(ip("::ffff:192.0.2.1")));
}

/// The version 2 listener refuses a second connection from the same IP, like the legacy one.
#[tokio::test]
async fn v2_listener_refuses_connections_past_per_ip_limit() {
    let _init_guard = zebra_test::init();
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let listener_config = Config {
        max_connections_per_ip: 1,
        ..v2_test_config(free_udp_addr(), &[])
    };
    assert!(listener_config.v2_inbound_connection_limit() > 1);

    assert_v2_listener_accepts_one_of_two_dialers(listener_config).await;
}

/// With one outbound version 2 slot, a configured peer whose connection stays up keeps the slot,
/// so the next configured peer waits for it to free, instead of a live connection being ended.
#[tokio::test]
async fn v2_dialer_keeps_live_initial_peer_slot() {
    let _init_guard = zebra_test::init();
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let [first_addr, second_addr] = free_udp_addrs();
    let (mut first_rx, first_task) = spawn_v2_node(v2_test_config(first_addr, &[]), no_bans());
    let (mut second_rx, second_task) = spawn_v2_node(v2_test_config(second_addr, &[]), no_bans());

    let dialer_config = Config {
        peerset_initial_target_size: 1,
        ..v2_test_config(any_udp_addr(), &[first_addr, second_addr])
    };
    assert_eq!(dialer_config.v2_outbound_connection_limit(), 1);
    let (_dialer_rx, dialer_task) = spawn_v2_node(dialer_config, no_bans());

    // Configured peers are dialled in order, so the first one takes the only slot.
    let _first_client = tokio::time::timeout(V2_TEST_CONNECT_TIMEOUT, first_rx.next())
        .await
        .expect("the first configured peer is dialled")
        .expect("the listener is running");
    tokio::time::sleep(V2_TEST_QUIET_PERIOD).await;
    assert_eq!(
        drain_discovered_peers(&mut second_rx),
        0,
        "a configured peer must wait while the only slot holds a live connection"
    );

    first_task.abort();
    second_task.abort();
    dialer_task.abort();
}

/// With a single outbound version 2 slot, a configured peer that cannot be reached frees its
/// slot after its failed attempt, so the next configured peer is dialled instead of the dead one
/// keeping the slot forever.
#[tokio::test]
async fn v2_dialer_moves_on_from_unreachable_initial_peer() {
    let _init_guard = zebra_test::init();
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    // Nothing listens on the first address once its probe is dropped.
    let [unreachable_addr, live_addr] = free_udp_addrs();
    let (mut live_rx, live_task) = spawn_v2_node(v2_test_config(live_addr, &[]), no_bans());

    let dialer_config = Config {
        peerset_initial_target_size: 1,
        ..v2_test_config(any_udp_addr(), &[unreachable_addr, live_addr])
    };
    assert_eq!(dialer_config.v2_outbound_connection_limit(), 1);
    let (_dialer_rx, dialer_task) = spawn_v2_node(dialer_config, no_bans());

    // The unreachable peer is dialled first, and its attempt times out before the slot is freed.
    tokio::time::timeout(
        constants::HANDSHAKE_TIMEOUT + V2_TEST_CONNECT_TIMEOUT,
        live_rx.next(),
    )
    .await
    .expect("the live configured peer gets the slot after the unreachable one fails")
    .expect("the listener is running");

    live_task.abort();
    dialer_task.abort();
}

/// An early configured host that resolves to more addresses than there are outbound version 2
/// slots does not take every slot: each host gets a slot before any host gets a second one.
#[test]
fn v2_initial_peer_slots_go_to_each_host_before_any_second_address() {
    let _init_guard = zebra_test::init();
    let now = tokio::time::Instant::now();

    let early_host = [test_v2_addr(1), test_v2_addr(2), test_v2_addr(3)];
    let late_host = test_v2_addr(4);
    let mut slots = v2::InitialPeerSlots::new(2);
    slots.add_host(early_host);
    slots.add_host([late_host]);

    let dialled = [slots.take_next(now), slots.take_next(now)];
    assert_eq!(dialled, [Some(early_host[0]), Some(late_host)]);
    assert_eq!(slots.take_next(now), None, "both slots are taken");

    // With more slots than hosts, the early host's other addresses fill them.
    let mut slots = v2::InitialPeerSlots::new(4);
    slots.add_host(early_host);
    slots.add_host([late_host]);
    let dialled: HashSet<_> = std::iter::from_fn(|| slots.take_next(now)).collect();
    assert_eq!(dialled.len(), 4);
    assert!(dialled.contains(&late_host));
}

/// A freed slot goes to the next host in turn, then to the next address of a host, and an
/// address that freed its slot can take one again, so a single configured peer is redialled.
#[test]
fn v2_initial_peer_slots_rotate_freed_slots() {
    let _init_guard = zebra_test::init();
    let now = tokio::time::Instant::now();

    let (first, second) = (test_v2_addr(1), test_v2_addr(2));

    // Two hosts with one slot: the slot alternates between them.
    let mut slots = v2::InitialPeerSlots::new(1);
    slots.add_host([first]);
    slots.add_host([second]);
    assert_eq!(slots.take_next(now), Some(first));
    assert_eq!(slots.take_next(now), None);
    slots.free(&first);
    assert_eq!(slots.take_next(now), Some(second));
    slots.free(&second);
    assert_eq!(slots.take_next(now), Some(first));

    // One host with two addresses and one slot: the slot alternates between its addresses.
    let mut slots = v2::InitialPeerSlots::new(1);
    slots.add_host([first, second]);
    assert_eq!(slots.take_next(now), Some(first));
    slots.free(&first);
    assert_eq!(slots.take_next(now), Some(second));
    slots.free(&second);
    assert_eq!(slots.take_next(now), Some(first));

    // One address: it takes the slot again once it freed it.
    let mut slots = v2::InitialPeerSlots::new(2);
    slots.add_host([first]);
    assert_eq!(slots.take_next(now), Some(first));
    assert_eq!(
        slots.take_next(now),
        None,
        "an address holds at most one slot"
    );
    slots.free(&first);
    assert_eq!(slots.take_next(now), Some(first));
}

/// An address that two configured hosts resolve to holds at most one slot.
#[test]
fn v2_initial_peer_slots_dial_a_shared_address_once() {
    let _init_guard = zebra_test::init();
    let now = tokio::time::Instant::now();

    let shared = test_v2_addr(1);
    let mut slots = v2::InitialPeerSlots::new(2);
    slots.add_host([shared]);
    slots.add_host([shared]);
    assert_eq!(slots.take_next(now), Some(shared));
    assert_eq!(slots.take_next(now), None);
}

/// A host that resolves after the slots are full waits for a slot to free, and then gets it
/// before the hosts that already hold one.
#[test]
fn v2_initial_peer_slots_late_host_waits_for_a_free_slot() {
    let _init_guard = zebra_test::init();
    let now = tokio::time::Instant::now();

    let early_host = [test_v2_addr(1), test_v2_addr(2)];
    let late_host = test_v2_addr(3);
    let mut slots = v2::InitialPeerSlots::new(1);
    slots.add_host(early_host);
    assert_eq!(slots.take_next(now), Some(early_host[0]));

    slots.add_host([late_host]);
    assert_eq!(slots.take_next(now), None, "the only slot is taken");
    assert!(slots.waits_for_slot(&late_host));

    slots.free(&early_host[0]);
    assert_eq!(slots.take_next(now), Some(late_host));
    assert!(!slots.waits_for_slot(&late_host));
}

/// A slot is kept free for each configured host that has not resolved yet, so an early host
/// with many addresses cannot fill every slot before a later host resolves.
#[test]
fn v2_initial_peer_slots_keep_a_slot_for_each_unresolved_host() {
    let _init_guard = zebra_test::init();
    let now = tokio::time::Instant::now();

    let early_host = [test_v2_addr(1), test_v2_addr(2), test_v2_addr(3)];
    let late_host = test_v2_addr(4);
    let mut slots = v2::InitialPeerSlots::new(2);
    slots.set_unresolved_hosts(1);
    slots.add_host(early_host);
    assert_eq!(slots.take_next(now), Some(early_host[0]));
    assert_eq!(
        slots.take_next(now),
        None,
        "the last free slot is kept for the unresolved host"
    );

    slots.set_unresolved_hosts(0);
    slots.add_host([late_host]);
    assert_eq!(slots.take_next(now), Some(late_host));
    assert_eq!(slots.take_next(now), None, "both slots are taken");

    // With more unresolved hosts than free slots, each resolved host still gets one slot.
    let mut slots = v2::InitialPeerSlots::new(1);
    slots.set_unresolved_hosts(2);
    slots.add_host(early_host);
    assert_eq!(slots.take_next(now), Some(early_host[0]));
}

/// An address that was not dialled frees its slot at once, so the next host in turn gets it, and
/// is not offered a slot again until its deferral passed, even while a slot is free.
#[test]
fn v2_initial_peer_slots_defer_addresses_that_were_not_dialled() {
    let _init_guard = zebra_test::init();
    let now = tokio::time::Instant::now();
    let deferred_until = now + Duration::from_secs(60);

    let (banned, live, waiting) = (test_v2_addr(1), test_v2_addr(2), test_v2_addr(3));
    let mut slots = v2::InitialPeerSlots::new(2);
    slots.add_host([banned]);
    slots.add_host([live]);
    slots.add_host([waiting]);
    assert_eq!(slots.take_next(now), Some(banned));
    assert_eq!(slots.take_next(now), Some(live));
    assert_eq!(slots.take_next(now), None, "both slots are taken");
    assert_eq!(slots.next_deferral_end(), None);

    // The banned address is not dialled: its slot goes to the waiting host at once, and it does
    // not wait for a slot itself.
    slots.defer(banned, deferred_until);
    assert_eq!(slots.next_deferral_end(), Some(deferred_until));
    assert!(!slots.waits_for_slot(&banned));
    assert_eq!(slots.take_next(now), Some(waiting));

    // The waiting host's connection ends: the banned address would be next in turn, but it is
    // not offered the free slot before its deferral passed, so the slot goes to the waiting host.
    slots.free(&waiting);
    assert_eq!(slots.take_next(now), Some(waiting));

    // Once its deferral passed, the banned address is dialled again in turn.
    slots.free(&waiting);
    assert_eq!(slots.take_next(deferred_until), Some(banned));
    assert_eq!(slots.next_deferral_end(), None);

    // A deferred address is not offered a free slot even when it is the only address without one.
    let mut slots = v2::InitialPeerSlots::new(2);
    slots.add_host([banned]);
    slots.add_host([live]);
    assert_eq!(slots.take_next(now), Some(banned));
    assert_eq!(slots.take_next(now), Some(live));
    slots.defer(banned, deferred_until);
    assert_eq!(
        slots.take_next(now),
        None,
        "the only free address is deferred"
    );
    assert_eq!(slots.take_next(deferred_until), Some(banned));
}

/// The initial version 2 peers resolve by host, so each host's addresses stay together and share
/// its turns for the outbound slots, and an address is never counted for the wrong host.
#[tokio::test]
async fn v2_initial_peers_resolve_by_host() {
    let _init_guard = zebra_test::init();

    let hosts: IndexSet<String> = ["127.0.0.1:8233", "127.0.0.2:8234"]
        .into_iter()
        .map(String::from)
        .collect();
    let (resolved, unresolved) = Config::resolve_initial_v2_peers(&hosts).await;

    assert!(unresolved.is_empty());
    let resolved: Vec<_> = resolved
        .into_iter()
        .map(|(host, addrs)| (host, addrs.into_iter().collect::<Vec<_>>()))
        .collect();
    let expected: Vec<_> = hosts
        .iter()
        .map(|host| {
            let addr: SocketAddr = host.parse().expect("the test hosts are socket addresses");
            (host.clone(), vec![PeerSocketAddr::from(addr)])
        })
        .collect();
    assert_eq!(resolved, expected);
}

/// A distinct version 2 peer address for the slot tests, which never connect to it.
fn test_v2_addr(port: u16) -> PeerSocketAddr {
    SocketAddr::new(Ipv4Addr::new(203, 0, 113, 1).into(), port).into()
}

/// The version 2 dialer keeps one connection to each configured peer: it does not redial while
/// the connection is open, and redials once it closes.
#[tokio::test]
async fn v2_dialer_redials_after_disconnect() {
    let _init_guard = zebra_test::init();
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let listener_addr = free_udp_addr();
    let (mut listener_rx, listener_task) =
        spawn_v2_node(v2_test_config(listener_addr, &[]), no_bans());
    let (_dialer_rx, dialer_task) =
        spawn_v2_node(v2_test_config(any_udp_addr(), &[listener_addr]), no_bans());

    let first_client = tokio::time::timeout(V2_TEST_CONNECT_TIMEOUT, listener_rx.next())
        .await
        .expect("the configured peer is dialled")
        .expect("the listener is running");
    tokio::time::sleep(V2_TEST_QUIET_PERIOD).await;
    assert_eq!(
        drain_discovered_peers(&mut listener_rx),
        0,
        "an open connection must not be redialled"
    );

    // Dropping the client ends the connection.
    std::mem::drop(first_client);
    tokio::time::timeout(V2_TEST_CONNECT_TIMEOUT, listener_rx.next())
        .await
        .expect("the configured peer is redialled after the connection closes")
        .expect("the listener is running");

    listener_task.abort();
    dialer_task.abort();
}

/// With `v2_sync_peers` set, every completed version 2 handshake is registered as a sync peer
/// for the connection's address, and removed once the connection ends.
#[tokio::test]
async fn v2_handshakes_register_sync_peers() {
    let _init_guard = zebra_test::init();
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let (sync_peers_tx, mut sync_peers) = crate::SyncPeersSender::spawn();
    let listener_addr = free_udp_addr();
    let (mut listener_rx, listener_task) = spawn_v2_node(
        Config {
            v2_sync_peers: Some(sync_peers_tx),
            ..v2_test_config(listener_addr, &[])
        },
        no_bans(),
    );
    let (_dialer_rx, dialer_task) =
        spawn_v2_node(v2_test_config(any_udp_addr(), &[listener_addr]), no_bans());

    let (addr, client) = tokio::time::timeout(V2_TEST_CONNECT_TIMEOUT, listener_rx.next())
        .await
        .expect("the configured peer is dialled")
        .expect("the listener is running");
    let registered = tokio::time::timeout(
        V2_TEST_CONNECT_TIMEOUT,
        sync_peers.wait_for(|peers| !peers.is_empty()),
    )
    .await
    .expect("the handshake registers a sync peer")
    .expect("the registry runs")
    .clone();
    assert_eq!(registered.len(), 1);
    assert_eq!(registered[0].addr(), addr);
    assert!(!registered[0].is_closed());
    drop(registered);

    // Dropping the client ends the connection.
    std::mem::drop(client);
    tokio::time::timeout(
        V2_TEST_CONNECT_TIMEOUT,
        sync_peers.wait_for(|peers| peers.is_empty()),
    )
    .await
    .expect("the registry removes the ended connection")
    .expect("the registry runs");

    listener_task.abort();
    dialer_task.abort();
}

/// Two nodes that list each other as initial version 2 peers settle on one connection between
/// them, rather than repeatedly dialling connections that the other node's peer set drops as
/// duplicates.
#[tokio::test]
async fn v2_mutual_initial_peers_settle_on_one_connection() {
    let _init_guard = zebra_test::init();
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    // The listener's per-IP rate limit would hide repeated redials by refusing them, so lift it
    // well above the number of redials this test can see.
    let mutual_config = |listen_addr, peer_addr| Config {
        max_connections_per_ip: 1000,
        ..v2_test_config(listen_addr, &[peer_addr])
    };
    let [first_addr, second_addr] = free_udp_addrs();
    let (mut first_rx, first_task) =
        spawn_v2_node(mutual_config(first_addr, second_addr), no_bans());
    let (mut second_rx, second_task) =
        spawn_v2_node(mutual_config(second_addr, first_addr), no_bans());

    // Both nodes usually dial at once at startup, so the first connections can collide; the
    // redial jitter then lets one node's connection arrive before the other node redials. The
    // short inbound interval matters: the listener holds a new connection until its next accept,
    // so a long interval would line both nodes' connections up whatever the jitter.
    let mut first_peers = HashMap::new();
    let mut second_peers = HashMap::new();
    let start = Instant::now();
    let mut quiet_since = Instant::now();
    loop {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "the nodes never settled on one connection: {} and {} live peers",
            first_peers.len(),
            second_peers.len(),
        );

        let received = keep_peers_like_peer_set(&mut first_rx, &mut first_peers)
            + keep_peers_like_peer_set(&mut second_rx, &mut second_peers);
        if received > 0 || first_peers.len() != 1 || second_peers.len() != 1 {
            quiet_since = Instant::now();
        } else if quiet_since.elapsed() >= V2_TEST_QUIET_PERIOD {
            break;
        }

        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    first_task.abort();
    second_task.abort();
}

/// Starts two dialers from 127.0.0.1 to a version 2 listener with `listener_config`, and checks
/// that it accepts exactly one of them.
async fn assert_v2_listener_accepts_one_of_two_dialers(listener_config: Config) {
    let listener_addr = listener_config
        .v2_listen_addr
        .expect("version 2 test configs listen");
    let (mut listener_rx, listener_task) = spawn_v2_node(listener_config, no_bans());
    let (_first_rx, first_task) =
        spawn_v2_node(v2_test_config(any_udp_addr(), &[listener_addr]), no_bans());
    let (_second_rx, second_task) =
        spawn_v2_node(v2_test_config(any_udp_addr(), &[listener_addr]), no_bans());

    // Keep the accepted connection open, so it holds its slot.
    let _accepted = tokio::time::timeout(V2_TEST_CONNECT_TIMEOUT, listener_rx.next())
        .await
        .expect("one dialer is accepted")
        .expect("the listener is running");
    tokio::time::sleep(V2_TEST_QUIET_PERIOD).await;
    assert_eq!(
        drain_discovered_peers(&mut listener_rx),
        0,
        "the second dialer must be refused"
    );

    listener_task.abort();
    first_task.abort();
    second_task.abort();
}

/// Moves the clients waiting in `peerset_rx` to `peers`, like the peer set: a client for an
/// address that already has a live client is dropped. Closed clients are removed first.
///
/// Returns the number of clients received.
fn keep_peers_like_peer_set(
    peerset_rx: &mut mpsc::Receiver<DiscoveredPeer>,
    peers: &mut HashMap<PeerSocketAddr, peer::Client>,
) -> usize {
    peers.retain(|_, client| {
        !matches!(
            ServiceExt::<Request>::ready(client).now_or_never(),
            Some(Err(_))
        )
    });

    let mut received = 0;
    while let Ok((addr, client)) = peerset_rx.try_recv() {
        received += 1;
        peers.entry(addr).or_insert(client);
    }
    received
}

/// A version 2 test node's config: listening on `v2_listen_addr`, dialling `initial_v2_peers`,
/// and with a per-IP limit that lets several test nodes on 127.0.0.1 connect.
fn v2_test_config(v2_listen_addr: SocketAddr, initial_v2_peers: &[SocketAddr]) -> Config {
    Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        network: Mainnet,
        initial_mainnet_peers: IndexSet::new(),
        initial_testnet_peers: IndexSet::new(),
        cache_dir: CacheDir::disabled(),
        v2_listen_addr: Some(v2_listen_addr),
        initial_v2_peers: initial_v2_peers.iter().map(ToString::to_string).collect(),
        max_connections_per_ip: 10,
        ..Config::default()
    }
}

/// Starts a version 2 listener and dialer for `config`, with the test reconnection delay and
/// inbound connection interval.
///
/// Returns the channel of the clients it would send to its peer set, and its task.
fn spawn_v2_node(
    config: Config,
    bans_receiver: watch::Receiver<crate::BanList>,
) -> (
    mpsc::Receiver<DiscoveredPeer>,
    JoinHandle<Result<(), BoxError>>,
) {
    spawn_v2_node_with_delay(config, bans_receiver, V2_TEST_RECONNECTION_DELAY)
}

/// [`spawn_v2_node`], redialling configured peers after `reconnection_delay` plus jitter.
fn spawn_v2_node_with_delay(
    config: Config,
    bans_receiver: watch::Receiver<crate::BanList>,
    reconnection_delay: Duration,
) -> (
    mpsc::Receiver<DiscoveredPeer>,
    JoinHandle<Result<(), BoxError>>,
) {
    spawn_v2_node_with_connected_peers(
        config,
        bans_receiver,
        ConnectedPeers::unconnected(),
        reconnection_delay,
    )
}

/// [`spawn_v2_node_with_delay`], with the dialer skipping addresses `connected_peers` blocks.
fn spawn_v2_node_with_connected_peers(
    config: Config,
    bans_receiver: watch::Receiver<crate::BanList>,
    connected_peers: ConnectedPeers,
    reconnection_delay: Duration,
) -> (
    mpsc::Receiver<DiscoveredPeer>,
    JoinHandle<Result<(), BoxError>>,
) {
    let (peerset_tx, peerset_rx) = mpsc::channel(config.peerset_total_connection_limit());
    let (inv_collector, _) = broadcast::channel(16);
    // These tests do not check misbehavior reports; a closed channel drops them.
    let (misbehavior_tx, _) = tokio::sync::mpsc::channel(1);
    let task = v2::start_with_intervals(
        config,
        service_fn(|_| async { Ok::<Response, BoxError>(Response::Nil) }),
        inv_collector,
        NoChainTip,
        "v2 test node".to_string(),
        bans_receiver,
        misbehavior_tx,
        peerset_tx,
        connected_peers,
        reconnection_delay,
        MIN_INBOUND_PEER_CONNECTION_INTERVAL_FOR_TESTS,
    )
    .expect("version 2 test configs listen");

    (peerset_rx, task)
}

/// An empty ban list.
fn no_bans() -> watch::Receiver<crate::BanList> {
    let (_bans_tx, bans_rx) = watch::channel(Default::default());
    bans_rx
}

/// A UDP address on 127.0.0.1 that was free when probed, for a node that must be dialled at a
/// known address.
fn free_udp_addr() -> SocketAddr {
    let [addr] = free_udp_addrs();
    addr
}

/// `N` distinct UDP addresses on 127.0.0.1 that were free when probed: the probes are held
/// together, so the operating system cannot hand the same port out twice.
fn free_udp_addrs<const N: usize>() -> [SocketAddr; N] {
    let probes: [std::net::UdpSocket; N] = std::array::from_fn(|_| {
        std::net::UdpSocket::bind("127.0.0.1:0").expect("a UDP port is free")
    });
    probes
        .each_ref()
        .map(|probe| probe.local_addr().expect("a bound socket has an address"))
}

/// A UDP address for a node that only dials, so nothing needs to know its port in advance.
fn any_udp_addr() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

/// Closing the version 2 endpoint at shutdown tells connected peers at once, with `NO_ERROR`,
/// rather than leaving them a dead connection until their idle timeout.
#[tokio::test]
async fn closing_the_v2_endpoint_tells_peers() {
    let _init_guard = zebra_test::init();
    let network = Mainnet;

    let server = crate::quic::bind(&network, "127.0.0.1:0".parse().unwrap()).expect("binds");
    let client = crate::quic::bind(&network, "127.0.0.1:0".parse().unwrap()).expect("binds");
    let server_addr = server.local_addr().expect("bound");
    v2::OPEN_ENDPOINT
        .set(server.clone())
        .expect("nextest runs each test in its own process, so no other endpoint is registered");

    let (outbound, inbound) = tokio::join!(crate::quic::connect(&client, server_addr), async {
        server
            .accept()
            .await
            .expect("open")
            .accept()
            .expect("acceptable")
            .await
    });
    let outbound = outbound.expect("connects");
    let _inbound = inbound.expect("accepts");

    crate::close_v2_endpoint(Duration::from_secs(1)).await;

    let closed = tokio::time::timeout(Duration::from_secs(5), outbound.closed())
        .await
        .expect("the peer learns of the close long before its 120 second idle timeout");
    match closed {
        quinn::ConnectionError::ApplicationClosed(close) => assert_eq!(
            close.error_code,
            crate::quic::app_error_code(crate::protocol::v2::types::ErrorCode::NoError),
        ),
        other => panic!("expected an application close with NO_ERROR, got {other:?}"),
    }
}

/// Closing does nothing, and returns at once, when no version 2 endpoint was opened.
#[tokio::test]
async fn closing_without_a_v2_endpoint_does_nothing() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(
        Duration::from_secs(1),
        crate::close_v2_endpoint(Duration::from_secs(60)),
    )
    .await
    .expect("returns at once without an endpoint");
}

/// A discovered peer dial made by [`fake_discovered_dial`]: the address, and the sender that ends
/// the connection when it is dropped.
type FakeDial = (PeerSocketAddr, tokio::sync::oneshot::Sender<()>);

/// A dial function for the discovered peer dialer that connects at once, and reports each dial
/// on `dials`.
fn fake_discovered_dial(
    dials: tokio::sync::mpsc::UnboundedSender<FakeDial>,
) -> impl FnMut(
    PeerSocketAddr,
) -> futures::future::BoxFuture<
    'static,
    Result<
        (
            crate::meta_addr::MetaAddrChange,
            tokio::sync::oneshot::Receiver<()>,
        ),
        BoxError,
    >,
> {
    move |addr| {
        let dials = dials.clone();
        async move {
            let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
            let _ = dials.send((addr, exit_tx));
            let connected = MetaAddr::new_connected(
                addr,
                &PeerServices::NODE_NETWORK,
                false,
                "/fake/".to_string(),
                constants::CURRENT_NETWORK_PROTOCOL_VERSION,
                zebra_chain::block::Height(0),
            );
            Ok((connected, exit_rx))
        }
        .boxed()
    }
}

/// A discovered peer dialer test: a version 2 address segment, and what the dialer is given.
struct DiscoveryHarness {
    book: crate::v2_address_book::V2BookHandle,
    discovery: v2::Discovery,
    configured_tx: watch::Sender<HashSet<PeerSocketAddr>>,
    bans_tx: watch::Sender<crate::BanList>,
}

/// Returns a harness for a dialer with `limit` slots and a DNS seed `target`.
fn discovery_harness(limit: usize, target: usize) -> DiscoveryHarness {
    let config = Config::default();
    let (bans_tx, bans_rx) = watch::channel(crate::BanList::default());
    let (book, stats, _task) = crate::v2_address_book::spawn(
        &config,
        &config.network,
        "0.0.0.0:8233".parse().unwrap(),
        false,
        bans_rx,
    );
    let (configured_tx, configured) = watch::channel(HashSet::new());
    let (_open_inbound_tx, open_inbound) = watch::channel(v2::OpenInbound::new());
    DiscoveryHarness {
        book: book.clone(),
        discovery: v2::Discovery {
            book,
            stats,
            limit,
            target,
            configured,
            await_configured: false,
            open_inbound,
            dial_interval: constants::MIN_OUTBOUND_PEER_CONNECTION_INTERVAL,
            retry_interval: Duration::from_secs(60),
        },
        configured_tx,
        bans_tx,
    }
}

/// `count` relayable addresses, each in its own address group.
fn discovered_addrs(count: u8) -> Vec<PeerSocketAddr> {
    (1..=count)
        .map(|group| SocketAddr::new(Ipv4Addr::new(203, group, 0, 1).into(), 8233).into())
        .collect()
}

/// Relays `addrs` to the segment, as a peer's `get-addr` answer.
fn relay_to(book: &crate::v2_address_book::V2BookHandle, addrs: &[PeerSocketAddr]) {
    let now = DateTime32::now();
    book.intake(
        "192.0.2.1:8233".parse().unwrap(),
        addrs
            .iter()
            .map(|addr| MetaAddr::new_gossiped_meta_addr(*addr, PeerServices::NODE_NETWORK, now))
            .collect(),
        crate::v2_address_book::Via::GetAddr,
    );
}

/// Returns the dials reported on `dials` until none comes for a while.
async fn drain_fake_dials(
    dials: &mut tokio::sync::mpsc::UnboundedReceiver<FakeDial>,
) -> Vec<FakeDial> {
    let mut received = Vec::new();
    while let Ok(Some(dial)) = tokio::time::timeout(Duration::from_secs(5), dials.recv()).await {
        received.push(dial);
    }
    received
}

/// The discovered peer dialer fills its slots from the segment, and dials no more while the
/// connections stay up. When one ends, its slot goes to another address at once, without the
/// configured peers' reconnection delay.
#[tokio::test(start_paused = true)]
async fn v2_discovered_dials_fill_to_the_limit_and_free_slots_at_once() {
    let _init_guard = zebra_test::init();

    let harness = discovery_harness(3, 3);
    relay_to(&harness.book, &discovered_addrs(6));
    let (dials_tx, mut dials) = tokio::sync::mpsc::unbounded_channel();
    let dialer = tokio::spawn(v2::keep_discovered_peers_connected(
        harness.discovery,
        |_| None,
        fake_discovered_dial(dials_tx),
        || async { Vec::new() },
    ));

    let mut live = drain_fake_dials(&mut dials).await;
    assert_eq!(live.len(), 3, "the dialer fills its slots");
    let groups: HashSet<_> = live
        .iter()
        .map(|(addr, _)| crate::v2_address_book::Group::of(addr.ip()))
        .collect();
    assert_eq!(groups.len(), 3, "one connection per address group");

    tokio::time::sleep(Duration::from_secs(600)).await;
    assert!(
        dials.try_recv().is_err(),
        "no dials while every slot is held"
    );

    // A connection ends: its slot is taken again well before any reconnection delay.
    let ended_at = tokio::time::Instant::now();
    drop(live.pop());
    let (next, _exit) = tokio::time::timeout(Duration::from_secs(5), dials.recv())
        .await
        .expect("the freed slot is used")
        .expect("the dialer runs");
    assert!(
        ended_at.elapsed() < constants::MIN_PEER_RECONNECTION_DELAY / 10,
        "the slot is freed without a reconnection delay",
    );
    assert!(!live.iter().any(|(addr, _)| *addr == next));

    dialer.abort();
}

/// Discovered candidates that are banned, configured, or not dialled for any other reason are
/// never dialled.
#[tokio::test(start_paused = true)]
async fn v2_discovered_dials_skip_banned_and_configured_addresses() {
    let _init_guard = zebra_test::init();

    let harness = discovery_harness(5, 1);
    let addrs = discovered_addrs(4);
    let (banned, configured, skipped, dialled) = (addrs[0], addrs[1], addrs[2], addrs[3]);
    harness
        .configured_tx
        .send_modify(|addrs| _ = addrs.insert(configured));
    relay_to(&harness.book, &addrs);
    // Let the segment apply the records before the ban sweep.
    let _ = harness.book.cacheable().await;
    let mut bans = crate::BanList::default();
    bans.ban(banned.ip());
    harness.bans_tx.send_replace(bans);

    let (dials_tx, mut dials) = tokio::sync::mpsc::unbounded_channel();
    let dialer = tokio::spawn(v2::keep_discovered_peers_connected(
        harness.discovery,
        move |addr| (addr == skipped).then_some("already connected to us"),
        fake_discovered_dial(dials_tx),
        || async { Vec::new() },
    ));

    let dialled_addrs: Vec<_> = drain_fake_dials(&mut dials)
        .await
        .into_iter()
        .map(|(addr, exit)| {
            std::mem::forget(exit);
            addr
        })
        .collect();
    assert_eq!(dialled_addrs, [dialled]);

    dialer.abort();
}

/// A trusted candidate that is not dialled, because the host is connected over the other
/// protocol, is not recorded as a failure: it is dialled once the other connection is gone. A
/// failed trusted entry has no last-seen time, so it would never be a candidate again.
#[tokio::test(start_paused = true)]
async fn v2_discovered_dials_offer_a_skipped_candidate_again() {
    let _init_guard = zebra_test::init();

    let harness = discovery_harness(5, 1);
    let cached: PeerSocketAddr = "198.51.100.9:8233".parse().unwrap();
    harness
        .book
        .seed(vec![cached], crate::v2_address_book::Provenance::Cache)
        .await;
    let connected_elsewhere = Arc::new(AtomicBool::new(true));

    let (dials_tx, mut dials) = tokio::sync::mpsc::unbounded_channel();
    let dialer = tokio::spawn(v2::keep_discovered_peers_connected(
        harness.discovery,
        {
            let connected_elsewhere = connected_elsewhere.clone();
            move |_| {
                connected_elsewhere
                    .load(Ordering::SeqCst)
                    .then_some("already connected")
            }
        },
        fake_discovered_dial(dials_tx),
        || async { Vec::new() },
    ));
    assert!(
        drain_fake_dials(&mut dials).await.is_empty(),
        "the candidate is skipped while the host is connected over the other protocol",
    );

    // The other connection closes: the next round of dials dials the candidate.
    connected_elsewhere.store(false, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(61)).await;
    let dialled = drain_fake_dials(&mut dials).await;
    assert_eq!(
        dialled.iter().map(|(addr, _)| *addr).collect::<Vec<_>>(),
        [cached]
    );
    dialer.abort();
}

/// While a discovered connection is up, the dialer tells the segment the peer responded about
/// every heartbeat interval, so the entry stays in the `get-addr` answers and the peer cache, and
/// stays dialable once it disconnects.
#[tokio::test(start_paused = true)]
async fn v2_discovered_connections_refresh_their_segment_entry() {
    let _init_guard = zebra_test::init();

    let peer: PeerSocketAddr = "198.51.100.9:8233".parse().unwrap();
    let (book_tx, mut book_rx) = tokio::sync::mpsc::channel(16);
    let (changes_tx, mut changes) = tokio::sync::mpsc::unbounded_channel();
    // A segment with one candidate, that reports the changes it is sent.
    tokio::spawn(async move {
        let mut offered = false;
        while let Some(message) = book_rx.recv().await {
            match message {
                crate::v2_address_book::Message::NextCandidate { reply, .. } => {
                    let candidate = (!offered).then(|| {
                        MetaAddr::new_gossiped_meta_addr(
                            peer,
                            PeerServices::NODE_NETWORK,
                            DateTime32::now(),
                        )
                    });
                    offered = true;
                    let _ = reply.send(candidate);
                }
                crate::v2_address_book::Message::Change(change) => {
                    let _ = changes_tx.send(change);
                }
                _ => {}
            }
        }
    });
    let mut harness = discovery_harness(1, 0);
    harness.discovery.book = crate::v2_address_book::V2BookHandle::new(book_tx);

    let (dials_tx, mut dials) = tokio::sync::mpsc::unbounded_channel();
    let dialer = tokio::spawn(v2::keep_discovered_peers_connected(
        harness.discovery,
        |_| None,
        fake_discovered_dial(dials_tx),
        || async { Vec::new() },
    ));
    let (_, exit) = dials.recv().await.expect("the candidate is dialled");

    // Longer than the time a peer that responded stays in the answers and the cache.
    let connected_for = Duration::from_secs(4 * 60 * 60);
    tokio::time::sleep(connected_for).await;
    let mut responded = 0;
    while let Ok(change) = changes.try_recv() {
        if matches!(change, crate::meta_addr::MetaAddrChange::UpdateResponded { addr, .. } if addr == peer)
        {
            responded += 1;
        }
    }
    let heartbeats = connected_for.as_secs() / constants::HEARTBEAT_INTERVAL.as_secs();
    assert!(
        responded + 2 >= heartbeats,
        "{responded} refreshes in {heartbeats} heartbeat intervals",
    );

    // Refreshes stop with the connection, which is then recorded as ended.
    drop(exit);
    let shutdown = tokio::time::timeout(Duration::from_secs(5), changes.recv())
        .await
        .expect("the end is recorded")
        .expect("the segment runs");
    assert_eq!(shutdown, MetaAddr::new_shutdown(peer));
    tokio::time::sleep(constants::HEARTBEAT_INTERVAL * 3).await;
    assert!(
        changes.try_recv().is_err(),
        "no refresh after the connection ended"
    );
    dialer.abort();
}

/// With configured hosts, the discovered peer dialer waits for the first round of their
/// resolution, so a configured peer from the version 2 peer cache is never dialled as
/// discovered, even when its host resolves after the cache was loaded.
#[tokio::test(start_paused = true)]
async fn v2_discovered_dials_wait_for_configured_peers_to_resolve() {
    let _init_guard = zebra_test::init();

    let mut harness = discovery_harness(5, 1);
    harness.discovery.await_configured = true;
    let addrs = discovered_addrs(2);
    let (cached_configured, relayed) = (addrs[0], addrs[1]);
    harness
        .book
        .seed(
            vec![cached_configured],
            crate::v2_address_book::Provenance::Cache,
        )
        .await;
    relay_to(&harness.book, &[relayed]);
    let _ = harness.book.cacheable().await;

    let (dials_tx, mut dials) = tokio::sync::mpsc::unbounded_channel();
    let dialer = tokio::spawn(v2::keep_discovered_peers_connected(
        harness.discovery,
        |_| None,
        fake_discovered_dial(dials_tx),
        || async { Vec::new() },
    ));

    // The configured host's lookup is slow: nothing is dialled meanwhile.
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert!(
        dials.try_recv().is_err(),
        "no dials before the configured hosts resolve"
    );

    harness
        .configured_tx
        .send_modify(|configured| _ = configured.insert(cached_configured));
    let dialled_addrs: Vec<_> = drain_fake_dials(&mut dials)
        .await
        .into_iter()
        .map(|(addr, exit)| {
            std::mem::forget(exit);
            addr
        })
        .collect();
    assert_eq!(dialled_addrs, [relayed]);
    dialer.abort();

    // A round in which no host resolved also ends the wait.
    let mut harness = discovery_harness(5, 1);
    harness.discovery.await_configured = true;
    relay_to(&harness.book, &[relayed]);
    let _ = harness.book.cacheable().await;
    let (dials_tx, mut dials) = tokio::sync::mpsc::unbounded_channel();
    let dialer = tokio::spawn(v2::keep_discovered_peers_connected(
        harness.discovery,
        |_| None,
        fake_discovered_dial(dials_tx),
        || async { Vec::new() },
    ));
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert!(dials.try_recv().is_err());
    harness.configured_tx.send_modify(|_| {});
    let dialled = drain_fake_dials(&mut dials).await;
    assert_eq!(
        dialled.iter().map(|(addr, _)| *addr).collect::<Vec<_>>(),
        [relayed]
    );
    dialer.abort();
}

/// The DNS seeders are only asked when the segment has too few addresses to dial, at most once
/// per reseed interval.
#[tokio::test(start_paused = true)]
async fn v2_dns_reseeds_only_when_short_and_at_most_every_interval() {
    let _init_guard = zebra_test::init();

    let seed: PeerSocketAddr = "198.51.100.7:8233".parse().unwrap();
    let lookups = Arc::new(AtomicUsize::new(0));
    let resolver = |lookups: Arc<AtomicUsize>| {
        move || {
            let lookups = lookups.clone();
            async move {
                lookups.fetch_add(1, Ordering::SeqCst);
                vec![seed]
            }
        }
    };

    // An empty segment is short: the seeders are asked at once, and their address is dialled.
    let harness = discovery_harness(3, 3);
    let (dials_tx, mut dials) = tokio::sync::mpsc::unbounded_channel();
    let dialer = tokio::spawn(v2::keep_discovered_peers_connected(
        harness.discovery,
        |_| None,
        fake_discovered_dial(dials_tx),
        resolver(lookups.clone()),
    ));
    let first = drain_fake_dials(&mut dials).await;
    assert_eq!(
        first.iter().map(|(addr, _)| *addr).collect::<Vec<_>>(),
        [seed]
    );
    assert_eq!(lookups.load(Ordering::SeqCst), 1);

    // Still short, but the interval has not passed.
    tokio::time::sleep(v2::V2_DNS_RESEED_INTERVAL / 3).await;
    assert_eq!(lookups.load(Ordering::SeqCst), 1);

    // Once it has, the seeders are asked again.
    tokio::time::sleep(v2::V2_DNS_RESEED_INTERVAL).await;
    assert_eq!(lookups.load(Ordering::SeqCst), 2);
    dialer.abort();

    // A segment with enough addresses to dial is not short: the seeders are never asked.
    let lookups = Arc::new(AtomicUsize::new(0));
    let harness = discovery_harness(5, 3);
    relay_to(&harness.book, &discovered_addrs(3));
    let _ = harness.book.cacheable().await;
    let (dials_tx, mut dials) = tokio::sync::mpsc::unbounded_channel();
    let dialer = tokio::spawn(v2::keep_discovered_peers_connected(
        harness.discovery,
        |_| None,
        fake_discovered_dial(dials_tx),
        resolver(lookups.clone()),
    ));
    let held = drain_fake_dials(&mut dials).await;
    assert_eq!(held.len(), 3);
    tokio::time::sleep(v2::V2_DNS_RESEED_INTERVAL * 2).await;
    assert_eq!(lookups.load(Ordering::SeqCst), 0);
    dialer.abort();
}

/// Addresses that share the address group of a held slot can't be dialled, so they don't count
/// against a shortage: the DNS seeders are asked, and their address is dialled.
#[tokio::test(start_paused = true)]
async fn v2_dns_reseeds_when_candidates_share_a_busy_group() {
    let _init_guard = zebra_test::init();

    let seed: PeerSocketAddr = "198.51.100.7:8233".parse().unwrap();
    let lookups = Arc::new(AtomicUsize::new(0));
    let resolver = {
        let lookups = lookups.clone();
        move || {
            let lookups = lookups.clone();
            async move {
                lookups.fetch_add(1, Ordering::SeqCst);
                vec![seed]
            }
        }
    };

    let harness = discovery_harness(3, 3);
    let same_group: Vec<PeerSocketAddr> = (1..=4)
        .map(|host| SocketAddr::new(Ipv4Addr::new(203, 0, host, 1).into(), 8233).into())
        .collect();
    relay_to(&harness.book, &same_group);
    let _ = harness.book.cacheable().await;
    let (dials_tx, mut dials) = tokio::sync::mpsc::unbounded_channel();
    let dialer = tokio::spawn(v2::keep_discovered_peers_connected(
        harness.discovery,
        |_| None,
        fake_discovered_dial(dials_tx),
        resolver,
    ));

    let held = drain_fake_dials(&mut dials).await;
    let dialled: Vec<PeerSocketAddr> = held.iter().map(|(addr, _)| *addr).collect();
    assert_eq!(
        dialled.len(),
        2,
        "one address in the group, and the seed: {dialled:?}"
    );
    assert!(same_group.contains(&dialled[0]));
    assert_eq!(dialled[1], seed);
    assert_eq!(lookups.load(Ordering::SeqCst), 1);
    dialer.abort();
}

/// On Regtest, node C, configured with only node B, learns node A from B's `get-addr` answer,
/// because B dialled A, and connects to A over version 2.
#[tokio::test]
async fn v2_node_discovers_a_peer_through_get_addr() {
    let _init_guard = zebra_test::init();
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let regtest_config = |listen_addr, peers: &[SocketAddr]| Config {
        network: Network::new_regtest(Default::default()),
        ..v2_test_config(listen_addr, peers)
    };
    let [a_addr, b_addr, c_addr] = free_udp_addrs();

    let (mut a_rx, a_task) = spawn_v2_node(regtest_config(a_addr, &[]), no_bans());
    let (mut b_rx, b_task) = spawn_v2_node(regtest_config(b_addr, &[a_addr]), no_bans());

    // B dials A, and records A as a peer it connected to.
    let _b_to_a = next_client_from(&mut a_rx, b_addr)
        .await
        .expect("B dials A");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let (_c_rx, c_task) = spawn_v2_node(regtest_config(c_addr, &[b_addr]), no_bans());
    let _c_to_b = next_client_from(&mut b_rx, c_addr)
        .await
        .expect("C dials B");

    // C was never told about A.
    let _c_to_a = next_client_from(&mut a_rx, c_addr)
        .await
        .expect("C discovers A through B, and dials it");

    a_task.abort();
    b_task.abort();
    c_task.abort();
}

/// With discovery off, node C, configured with only node B, does not ask B for addresses, so it
/// never dials node A, which B dialled.
#[tokio::test]
async fn v2_node_without_discovery_only_dials_configured_peers() {
    let _init_guard = zebra_test::init();
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let regtest_config = |listen_addr, peers: &[SocketAddr], v2_peer_discovery| Config {
        network: Network::new_regtest(Default::default()),
        v2_peer_discovery,
        ..v2_test_config(listen_addr, peers)
    };
    let [a_addr, b_addr, c_addr] = free_udp_addrs();

    let (mut a_rx, a_task) = spawn_v2_node(regtest_config(a_addr, &[], true), no_bans());
    let (mut b_rx, b_task) = spawn_v2_node(regtest_config(b_addr, &[a_addr], true), no_bans());
    let _b_to_a = next_client_from(&mut a_rx, b_addr)
        .await
        .expect("B dials A");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let (_c_rx, c_task) = spawn_v2_node(regtest_config(c_addr, &[b_addr], false), no_bans());
    let _c_to_b = next_client_from(&mut b_rx, c_addr)
        .await
        .expect("C dials B");

    tokio::time::sleep(V2_TEST_QUIET_PERIOD).await;
    while let Ok((from, _client)) = a_rx.try_recv() {
        assert_ne!(*from, c_addr, "C must not discover A");
    }

    a_task.abort();
    b_task.abort();
    c_task.abort();
}

/// Waits for the next client in `peerset_rx` for a connection with `addr`, keeping the others
/// open, or returns `None` if none comes in time.
async fn next_client_from(
    peerset_rx: &mut mpsc::Receiver<DiscoveredPeer>,
    addr: SocketAddr,
) -> Option<peer::Client> {
    let mut others = Vec::new();
    tokio::time::timeout(V2_TEST_CONNECT_TIMEOUT, async {
        while let Some((from, client)) = peerset_rx.next().await {
            if *from == addr {
                return Some(client);
            }
            others.push(client);
        }
        None
    })
    .await
    .ok()
    .flatten()
}

/// The legacy crawler does not dial a candidate the peer set is already connected to, for
/// example over version 2, and dials the next candidate. The skipped candidate is left attempt
/// pending, not failed, so it waits the reconnection delay, then is a candidate again.
#[tokio::test]
async fn crawler_skips_candidates_the_peer_set_is_connected_to() {
    let connected_addr: PeerSocketAddr = "127.1.2.1:8233".parse().unwrap();
    crawler_skips_a_connected_candidate(
        MetaAddr::new_gossiped_meta_addr(
            connected_addr,
            PeerServices::NODE_NETWORK,
            DateTime32::now(),
        )
        .new_gossiped_change()
        .expect("a gossiped address can be added to the address book"),
    )
    .await;
}

/// A skipped initial peer, which has no last-seen time, is not marked failed, which would make
/// it ineligible for good: it is a candidate again after the reconnection delay.
#[tokio::test]
async fn crawler_skip_does_not_fail_an_initial_peer() {
    crawler_skips_a_connected_candidate(MetaAddr::new_initial_peer(
        "127.1.2.1:8233".parse().unwrap(),
    ))
    .await;
}

/// Checks that the crawler skips the candidate `connected_change` adds, which the peer set is
/// connected to.
async fn crawler_skips_a_connected_candidate(connected_change: crate::meta_addr::MetaAddrChange) {
    let connected_addr = connected_change.addr();
    let (_keys_tx, keys) = watch::channel(Arc::new(HashSet::from([connected_addr])));
    crawler_skips_a_blocked_candidate(connected_change, ConnectedPeers::new(keys, 1)).await;
}

/// The legacy crawler of a node with a version 2 endpoint leaves a configured version 2 peer's
/// IP to the version 2 dialer, whatever the candidate's port, and does not mark it failed.
#[tokio::test]
async fn crawler_skips_candidates_on_a_configured_v2_peer_ip() {
    let connected_peers = ConnectedPeers::unconnected();
    connected_peers.prefer_v2(["127.1.2.1:18233".parse().unwrap()]);
    crawler_skips_a_blocked_candidate(
        MetaAddr::new_initial_peer("127.1.2.1:8233".parse().unwrap()),
        connected_peers,
    )
    .await;
}

/// Checks that the crawler skips the candidate `blocked_change` adds, which `connected_peers`
/// blocks, dials another one, and leaves the skipped one eligible after the reconnection delay.
async fn crawler_skips_a_blocked_candidate(
    connected_change: crate::meta_addr::MetaAddrChange,
    connected_peers: ConnectedPeers,
) {
    let _init_guard = zebra_test::init();

    let connected_addr = connected_change.addr();
    let open_addr: PeerSocketAddr = "127.1.2.2:8233".parse().unwrap();
    let config = Config {
        // The crawler only skips when the node has a version 2 endpoint.
        v2_listen_addr: Some("127.0.0.1:0".parse().unwrap()),
        crawl_new_peer_interval: CRAWLER_TEST_CRAWL_NEW_PEER_INTERVAL,
        initial_mainnet_peers: IndexSet::new(),
        initial_testnet_peers: IndexSet::new(),
        cache_dir: CacheDir::disabled(),
        ..Config::default()
    };

    let (
        address_book,
        _bans_receiver,
        address_book_updater,
        address_book_service,
        _address_metrics,
        _address_book_updater_guard,
    ) = AddressBookUpdater::spawn(&config, config.listen_addr);
    address_book.lock().unwrap().update(connected_change);
    let change =
        MetaAddr::new_gossiped_meta_addr(open_addr, PeerServices::NODE_NETWORK, DateTime32::now())
            .new_gossiped_change()
            .expect("a gossiped address can be added to the address book");
    address_book.lock().unwrap().update(change);

    // The connector records every address it is asked to dial, and holds the connections open.
    let (dialled_tx, mut dialled_rx) = mpsc::unbounded();
    let outbound_connector = service_fn(move |req: OutboundConnectorRequest| {
        let dialled_tx = dialled_tx.clone();
        async move {
            let (fake_client, _harness) = ClientTestHarness::build().finish();
            dialled_tx
                .unbounded_send((req.addr, req.connection_tracker))
                .expect("the test holds the receiver");
            Ok::<_, BoxError>((req.addr, fake_client))
        }
    });

    let empty_peer_set = service_fn(|req| async move {
        match req {
            Request::Peers => Ok::<_, BoxError>(Response::Peers(Vec::new())),
            _ => unreachable!("unexpected request: {req:?}"),
        }
    });
    let (next_peer_service, crawl_service) =
        crawler_services(address_book_service.clone(), empty_peer_set);
    let channel_size = config.peerset_outbound_connection_limit() * 2;
    let (peerset_tx, mut peerset_rx) = mpsc::channel::<DiscoveredPeer>(channel_size);
    let (mut demand_tx, demand_rx) = mpsc::channel::<MorePeers>(channel_size);
    for _ in 0..2 {
        demand_tx.try_send(MorePeers).expect("the channel has room");
    }

    let crawl_task_handle = tokio::spawn(crawl_and_dial(
        config,
        demand_tx,
        demand_rx,
        next_peer_service,
        crawl_service,
        address_book_service,
        outbound_connector,
        peerset_tx,
        ActiveConnectionCounter::new_counter(),
        address_book_updater,
        connected_peers,
    ));

    let (dialled, _tracker) = tokio::time::timeout(CRAWLER_TEST_DURATION, dialled_rx.next())
        .await
        .expect("the crawler dials the candidate that is not connected")
        .expect("the crawler holds the connector");
    assert_eq!(dialled, open_addr);
    let (discovered, _client) = peerset_rx.next().await.expect("the crawler is running");
    assert_eq!(discovered, open_addr);

    tokio::time::timeout(CRAWLER_TEST_DURATION, async {
        loop {
            let peer = address_book
                .lock()
                .unwrap()
                .get(connected_addr)
                .expect("the address book has the connected candidate");
            if !peer.last_connection_state.is_never_attempted() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the crawler selects the connected candidate, and skips it");

    // Several crawl timer ticks, so the crawler had the chance to dial the skipped candidate, and
    // to finish recording the skip.
    tokio::time::sleep(CRAWLER_TEST_CRAWL_NEW_PEER_INTERVAL * 10).await;
    assert!(
        dialled_rx.try_recv().is_err(),
        "the connected candidate must never reach the connector"
    );

    let skipped = address_book
        .lock()
        .unwrap()
        .get(connected_addr)
        .expect("the address book has the connected candidate");
    assert_ne!(
        skipped.last_connection_state,
        PeerAddrState::Failed,
        "a skipped candidate is not a failed one"
    );
    let after_delay = constants::MIN_PEER_RECONNECTION_DELAY + Duration::from_secs(1);
    assert!(
        skipped.is_ready_for_connection_attempt(
            Instant::now() + after_delay,
            Utc::now() + after_delay,
            &Network::Mainnet,
        ),
        "the skipped candidate is a candidate again after the reconnection delay"
    );

    stop_crawler(crawl_task_handle).await;
}

/// A node without a version 2 endpoint dials a candidate whose IP is at the per-IP limit, as it
/// did before the version 2 protocol: the handshake verifies the candidate's listener, and the
/// peer set drops the connection afterwards.
#[tokio::test]
async fn legacy_only_crawler_dials_a_candidate_whose_ip_is_connected() {
    let _init_guard = zebra_test::init();

    // The peer set has a connection from another port of the candidate's IP.
    let candidate_addr: PeerSocketAddr = "127.1.2.1:8233".parse().unwrap();
    let connected_addr: PeerSocketAddr = "127.1.2.1:40000".parse().unwrap();
    let config = Config {
        v2_listen_addr: None,
        crawl_new_peer_interval: CRAWLER_TEST_CRAWL_NEW_PEER_INTERVAL,
        initial_mainnet_peers: IndexSet::new(),
        initial_testnet_peers: IndexSet::new(),
        cache_dir: CacheDir::disabled(),
        ..Config::default()
    };

    let (
        address_book,
        _bans_receiver,
        address_book_updater,
        address_book_service,
        _address_metrics,
        _address_book_updater_guard,
    ) = AddressBookUpdater::spawn(&config, config.listen_addr);
    let change = MetaAddr::new_gossiped_meta_addr(
        candidate_addr,
        PeerServices::NODE_NETWORK,
        DateTime32::now(),
    )
    .new_gossiped_change()
    .expect("a gossiped address can be added to the address book");
    address_book.lock().unwrap().update(change);

    let (dialled_tx, mut dialled_rx) = mpsc::unbounded();
    let outbound_connector = service_fn(move |req: OutboundConnectorRequest| {
        let dialled_tx = dialled_tx.clone();
        async move {
            let (fake_client, _harness) = ClientTestHarness::build().finish();
            dialled_tx
                .unbounded_send((req.addr, req.connection_tracker))
                .expect("the test holds the receiver");
            Ok::<_, BoxError>((req.addr, fake_client))
        }
    });

    let empty_peer_set = service_fn(|req| async move {
        match req {
            Request::Peers => Ok::<_, BoxError>(Response::Peers(Vec::new())),
            _ => unreachable!("unexpected request: {req:?}"),
        }
    });
    let (next_peer_service, crawl_service) =
        crawler_services(address_book_service.clone(), empty_peer_set);
    let channel_size = config.peerset_outbound_connection_limit() * 2;
    let (peerset_tx, _peerset_rx) = mpsc::channel::<DiscoveredPeer>(channel_size);
    let (mut demand_tx, demand_rx) = mpsc::channel::<MorePeers>(channel_size);
    demand_tx.try_send(MorePeers).expect("the channel has room");

    let (_keys_tx, keys) = watch::channel(Arc::new(HashSet::from([connected_addr])));
    let crawl_task_handle = tokio::spawn(crawl_and_dial(
        config,
        demand_tx,
        demand_rx,
        next_peer_service,
        crawl_service,
        address_book_service,
        outbound_connector,
        peerset_tx,
        ActiveConnectionCounter::new_counter(),
        address_book_updater,
        ConnectedPeers::new(keys, 1),
    ));

    let (dialled, _tracker) = tokio::time::timeout(CRAWLER_TEST_DURATION, dialled_rx.next())
        .await
        .expect("a legacy-only crawler dials a candidate whose IP is at the per-IP limit")
        .expect("the crawler holds the connector");
    assert_eq!(dialled, candidate_addr);

    stop_crawler(crawl_task_handle).await;
}

/// The version 2 dialer does not dial a configured peer whose address the peer set is already
/// connected to over the legacy protocol, which the peer set would drop, and dials it once that
/// connection is gone.
#[tokio::test]
async fn v2_dialer_defers_a_configured_peer_with_a_legacy_connection() {
    let _init_guard = zebra_test::init();
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    let listener_addr = free_udp_addr();
    let (mut listener_rx, listener_task) =
        spawn_v2_node(v2_test_config(listener_addr, &[]), no_bans());

    let legacy_connection = PeerSocketAddr::from(listener_addr);
    let (keys_tx, keys) = watch::channel(Arc::new(HashSet::from([legacy_connection])));
    let dialer_config = v2_test_config(any_udp_addr(), &[listener_addr]);
    let connected_peers = ConnectedPeers::new(keys, dialer_config.max_connections_per_ip);
    let (_dialer_rx, dialer_task) = spawn_v2_node_with_connected_peers(
        dialer_config,
        no_bans(),
        connected_peers,
        V2_TEST_RECONNECTION_DELAY,
    );

    // Several reconnection delays, so the dialer has retried while the legacy connection is up.
    tokio::time::sleep(V2_TEST_QUIET_PERIOD).await;
    assert_eq!(
        drain_discovered_peers(&mut listener_rx),
        0,
        "a configured peer with a legacy connection must not be dialled"
    );

    keys_tx.send_replace(Arc::new(HashSet::new()));
    tokio::time::timeout(V2_TEST_CONNECT_TIMEOUT, listener_rx.next())
        .await
        .expect("the configured peer is dialled once the legacy connection is gone")
        .expect("the listener is running");

    listener_task.abort();
    dialer_task.abort();
}

/// With one connection per IP, a node connected to a version 2 peer does not also dial that
/// peer's IP over the legacy protocol: the crawler skips the candidate, so the peer set never
/// gets a second connection to the IP to drop.
#[tokio::test]
async fn mixed_node_with_one_connection_per_ip_skips_legacy_dial_to_v2_host() {
    let _init_guard = zebra_test::init();
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }
    let network = Mainnet;

    let responder_v2_addr = free_udp_addr();
    let responder_config = Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        network: network.clone(),
        initial_mainnet_peers: IndexSet::new(),
        initial_testnet_peers: IndexSet::new(),
        cache_dir: CacheDir::disabled(),
        v2_listen_addr: Some(responder_v2_addr),
        ..Config::default()
    };
    let responder_inbound = service_fn(|request: Request| async move {
        match request {
            Request::FindHeaders { .. } => Ok(v2_headers_answer()),
            _ => Ok::<Response, BoxError>(Response::Nil),
        }
    });
    let (_responder_service, _responder_book, _, _advertiser) = init(
        responder_config,
        responder_inbound,
        NoChainTip,
        "v2 responder".to_string(),
    )
    .await;

    let config = Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        network: network.clone(),
        initial_mainnet_peers: IndexSet::new(),
        initial_testnet_peers: IndexSet::new(),
        cache_dir: CacheDir::disabled(),
        crawl_new_peer_interval: CRAWLER_TEST_CRAWL_NEW_PEER_INTERVAL,
        v2_listen_addr: Some(any_udp_addr()),
        initial_v2_peers: [responder_v2_addr.to_string()].into_iter().collect(),
        max_connections_per_ip: 1,
        ..Config::default()
    };
    let inbound = service_fn(|_| async { Ok::<Response, BoxError>(Response::Nil) });
    let (mut peer_set, address_book, _, _advertiser) =
        init(config, inbound, NoChainTip, "mixed node".to_string()).await;

    // The version 2 peer is the only peer, so its answer proves the connection is up.
    assert_eq!(
        first_response(&mut peer_set, v2_headers_request()).await,
        v2_headers_answer(),
    );

    // A legacy listener on the version 2 peer's IP, that counts the connections it accepts.
    let legacy_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a TCP port is free");
    let legacy_addr: PeerSocketAddr = legacy_listener
        .local_addr()
        .expect("a bound listener has an address")
        .into();
    let accepted = Arc::new(AtomicUsize::new(0));
    let accept_task = tokio::spawn({
        let accepted = accepted.clone();
        async move {
            while legacy_listener.accept().await.is_ok() {
                accepted.fetch_add(1, Ordering::SeqCst);
            }
        }
    });

    let change = MetaAddr::new_gossiped_meta_addr(
        legacy_addr,
        PeerServices::NODE_NETWORK,
        DateTime32::now(),
    )
    .new_gossiped_change()
    .expect("a gossiped address can be added to the address book");
    address_book.lock().unwrap().update(change);

    tokio::time::timeout(CRAWLER_TEST_DURATION, async {
        loop {
            let state = address_book
                .lock()
                .unwrap()
                .get(legacy_addr)
                .map(|peer| peer.last_connection_state);
            if state.is_some_and(|state| !state.is_never_attempted()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the crawler takes the legacy candidate");

    // Give a dial that was started anyway the time to reach the listener.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        0,
        "the crawler must not dial a second connection to the version 2 peer's IP"
    );
    assert_eq!(
        address_book
            .lock()
            .unwrap()
            .get(legacy_addr)
            .map(|peer| peer.last_connection_state),
        Some(PeerAddrState::AttemptPending),
        "the skipped candidate is not marked failed"
    );
    assert_eq!(
        first_response(&mut peer_set, v2_headers_request()).await,
        v2_headers_answer(),
        "the version 2 connection stays up"
    );

    accept_task.abort();
}
