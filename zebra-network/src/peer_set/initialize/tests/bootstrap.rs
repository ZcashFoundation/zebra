//! Tests for the state-snapshot bootstrap dialer.

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use futures::{channel::mpsc, FutureExt, StreamExt};
use indexmap::IndexSet;
use tokio::{
    sync::{broadcast, watch},
    task::JoinHandle,
};
use tower::{service_fn, ServiceExt};

use zebra_chain::{block, chain_tip::NoChainTip, parameters::Network};

use crate::{
    config::CacheDir,
    constants::{HANDSHAKE_TIMEOUT, MAX_PEER_MISBEHAVIOR_SCORE},
    peer::v2::misbehavior::MisbehaviorScores,
    peer_set::{
        initialize::{
            v2::{
                self,
                bootstrap::{
                    bootstrap_minimum_peer_version, select_peers, spawn_report_log,
                    v2_bootstrap_clients_with_timeout, MAX_BOOTSTRAP_PEERS,
                },
                v2_bootstrap_clients,
            },
            DiscoveredPeer,
        },
        ConnectedPeers,
    },
    protocol::types::PeerServices,
    BoxError, Config, PeerSocketAddr, Request, Response,
};

/// How long the tests wait for a connection that should be made.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// How long the tests watch for connections that should not be made: several
/// redials of the dialer under test.
const QUIET_PERIOD: Duration = Duration::from_secs(3);

/// The reconnection delay of the dialers under test: short, so the quiet
/// period sees several redials.
const RECONNECTION_DELAY: Duration = Duration::from_millis(300);

/// The interval between the connections a test listener accepts.
const INBOUND_INTERVAL: Duration = Duration::from_millis(25);

/// A distinct peer address that the tests never connect to.
fn test_addr(ip: impl Into<IpAddr>, port: u16) -> PeerSocketAddr {
    SocketAddr::new(ip.into(), port).into()
}

/// The bootstrap dials each IP address once, in configuration order, at most
/// [`MAX_BOOTSTRAP_PEERS`] of them, and canonicalizes the addresses first, so
/// an IPv4-mapped IPv6 spelling of an IPv4 address is the same peer.
#[test]
fn bootstrap_peers_are_deduplicated_by_ip_and_capped() {
    let _init_guard = zebra_test::init();

    let host = |n: u8| Ipv4Addr::new(203, 0, 113, n);

    // The same IP on several ports is one peer, the first spelling.
    let repeated = [
        test_addr(host(1), 8233),
        test_addr(host(1), 8234),
        test_addr(host(2), 8233),
        test_addr(host(1), 8235),
        test_addr(host(2), 8235),
        test_addr(Ipv6Addr::LOCALHOST, 8233),
    ];
    assert_eq!(
        select_peers(&repeated),
        vec![
            test_addr(host(1), 8233),
            test_addr(host(2), 8233),
            test_addr(Ipv6Addr::LOCALHOST, 8233),
        ]
    );

    // An IPv4-mapped IPv6 address is its IPv4 address.
    let mapped = [
        test_addr(host(3).to_ipv6_mapped(), 8233),
        test_addr(host(3), 8233),
    ];
    assert_eq!(select_peers(&mapped), vec![test_addr(host(3), 8233)]);

    // Duplicates are removed before the cap applies, and the cap keeps the first peers.
    let many: Vec<_> = (1..=12)
        .flat_map(|n| [test_addr(host(n), 8233), test_addr(host(n), 8234)])
        .collect();
    let selected = select_peers(&many);
    assert_eq!(selected.len(), MAX_BOOTSTRAP_PEERS);
    assert_eq!(
        selected,
        (1..=8)
            .map(|n| test_addr(host(n), 8233))
            .collect::<Vec<_>>()
    );

    assert!(select_peers(&[]).is_empty());
}

/// The bootstrap negotiates using the highest checkpoint's protocol epoch,
/// not the genesis-era minimum that an empty chain tip would select.
#[test]
fn bootstrap_minimum_version_uses_the_checkpoint_epoch() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let mut minimum = bootstrap_minimum_peer_version(&network);
    let expected = crate::protocol::external::types::Version::min_remote_for_height(
        &network,
        Some(network.checkpoint_list().max_height()),
    );
    assert_ne!(
        expected,
        crate::protocol::external::types::Version::min_remote_for_height(&network, None),
        "this network distinguishes the checkpoint and genesis protocol epochs"
    );
    assert_eq!(minimum.current(), expected);
}

/// The bootstrap drains the misbehavior reports of its peers in place of the
/// address book, without a dropped-receiver warning. The reported score is
/// retained, so a later penalty still closes the connection at the threshold.
#[tokio::test]
#[tracing_test::traced_test]
async fn bootstrap_misbehavior_reports_are_received() {
    let scores = MisbehaviorScores::spawn(spawn_report_log(1), 1);
    let addr = test_addr(Ipv4Addr::new(203, 0, 113, 9), 8233);
    assert!(
        scores
            .penalize(addr, MAX_PEER_MISBEHAVIOR_SCORE, "a test penalty")
            .await,
        "the threshold is reached"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !logs_contain("state snapshot bootstrap peer misbehaved") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the bootstrap's report drain logs the report");
    assert!(!logs_contain("could not report a misbehaving v2 peer"));
    assert!(
        scores.penalize(addr, 1, "a later penalty").await,
        "the reported score is retained at the threshold"
    );
    assert!(!logs_contain("could not report a misbehaving v2 peer"));
}

/// The bootstrap connects to a pruned node that serves snapshot objects,
/// which declines the normal syncing dialer, and declines a node that serves
/// blocks but no objects. The servers see a node with no services, no chain,
/// and no interest in relay. Closing the bootstrap ends its connections.
#[tokio::test]
async fn bootstrap_dialer_connects_to_artifact_servers_the_normal_dialer_declines() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    // A pruned node serving snapshot objects, and an archive node serving blocks only.
    let pruned_services = PeerServices::NODE_NETWORK_LIMITED | PeerServices::NODE_SYNC_ARTIFACTS;
    let [pruned_addr, archive_addr] = free_udp_addrs();
    let (mut pruned_rx, pruned_task) =
        spawn_v2_node(v2_test_config(pruned_addr, &[]), pruned_services);
    let (mut archive_rx, archive_task) = spawn_v2_node(
        v2_test_config(archive_addr, &[]),
        PeerServices::NODE_NETWORK,
    );

    // The bootstrap dials both, and connects to the pruned node only.
    let bootstrap_config = Config {
        network: network.clone(),
        ..Config::default()
    };
    let peers = [pruned_addr.into(), archive_addr.into()];
    let bootstrap = tokio::time::timeout(
        CONNECT_TIMEOUT,
        v2_bootstrap_clients(&bootstrap_config, &peers, "/bootstrap test/".to_string()),
    )
    .await
    .expect("the attempts finish within their handshake timeouts")
    .expect("the bootstrap dials");
    assert_eq!(
        bootstrap
            .clients
            .iter()
            .map(|(addr, _)| *addr)
            .collect::<Vec<_>>(),
        vec![PeerSocketAddr::from(pruned_addr)],
        "only the artifact server is accepted"
    );
    let (_, client) = &bootstrap.clients[0];
    assert_eq!(client.connection_info.remote.services(), pruned_services);
    assert!(
        !client.connection_info.connected_addr.is_inbound(),
        "the bootstrap dialled the server"
    );

    // The pruned server sees the bootstrap as a node with no services, no chain, and no relay.
    let (_, mut seen_by_pruned) = tokio::time::timeout(CONNECT_TIMEOUT, pruned_rx.next())
        .await
        .expect("the pruned server is dialled")
        .expect("the pruned server is running");
    let remote = &seen_by_pruned.connection_info.remote;
    assert_eq!(remote.services(), PeerServices::empty());
    assert!(!remote.relay(), "the bootstrap wants no transaction relay");
    assert_eq!(remote.start_height(), block::Height(0));
    assert_eq!(
        remote
            .as_init_record()
            .expect("a version 2 peer")
            .user_agent,
        "/bootstrap test/"
    );

    // The archive node declined nothing itself: the bootstrap declined it.
    tokio::time::sleep(QUIET_PERIOD).await;
    while let Some(Some((_, declined))) = archive_rx.next().now_or_never() {
        // The responder may complete its handshake before the bootstrap's close arrives.
        drop(declined);
    }
    assert!(
        archive_rx.next().now_or_never().is_none(),
        "the bootstrap does not retry the archive node"
    );

    // Closing the bootstrap ends the pruned server's connection.
    bootstrap.close().await;
    tokio::time::timeout(CONNECT_TIMEOUT, async {
        loop {
            if ServiceExt::<Request>::ready(&mut seen_by_pruned)
                .await
                .is_err()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the server's connection ends when the bootstrap closes");

    // The normal syncing dialer requires `NODE_NETWORK`, so it never connects to the pruned node.
    let (mut dialer_rx, dialer_task) = spawn_v2_node(
        v2_test_config(any_udp_addr(), &[pruned_addr]),
        PeerServices::NODE_NETWORK,
    );
    tokio::time::sleep(QUIET_PERIOD).await;
    assert!(
        dialer_rx.next().now_or_never().is_none(),
        "the normal dialer declines the pruned server"
    );
    // The same dialer connects to the archive node, so it was the services that stopped it.
    let (_, _dialer_to_archive) = spawn_v2_node(
        v2_test_config(any_udp_addr(), &[archive_addr]),
        PeerServices::NODE_NETWORK,
    );
    tokio::time::timeout(CONNECT_TIMEOUT, archive_rx.next())
        .await
        .expect("the normal dialer connects to the archive node")
        .expect("the archive node is running");

    pruned_task.abort();
    archive_task.abort();
    dialer_task.abort();
}

/// A whole-dial timeout closes an already-connected peer before returning the error to a caller
/// that might immediately start ordinary sync.
#[tokio::test]
async fn bootstrap_dial_timeout_closes_connected_peers_before_returning() {
    let _init_guard = zebra_test::init();
    let [artifact_addr] = free_udp_addrs();
    let (mut artifact_rx, artifact_task) = spawn_v2_node(
        v2_test_config(artifact_addr, &[]),
        PeerServices::NODE_SYNC_ARTIFACTS,
    );
    let config = Config {
        network: Network::Mainnet,
        ..Config::default()
    };
    // A distinct TEST-NET IP keeps this attempt open until the whole-dial timeout, while the
    // loopback artifact server completes its handshake.
    let silent = test_addr(Ipv4Addr::new(192, 0, 2, 1), 9);
    let peers = [PeerSocketAddr::from(artifact_addr), silent];

    let error = v2_bootstrap_clients_with_timeout(
        &config,
        &peers,
        "/bootstrap test/".to_string(),
        Duration::from_secs(1),
    )
    .await
    .expect_err("the silent peer holds the dial open until its overall deadline");
    assert!(error.to_string().contains("did not finish"), "{error}");

    let (_, mut seen_by_artifact) = artifact_rx
        .next()
        .now_or_never()
        .flatten()
        .expect("the artifact server accepted the bootstrap before the timeout");
    assert!(
        matches!(
            ServiceExt::<Request>::ready(&mut seen_by_artifact).now_or_never(),
            Some(Err(_))
        ),
        "the connected peer is already closed when the timeout error returns"
    );
    artifact_task.abort();
}

/// A bootstrap whose peers are unreachable returns no clients, once each
/// attempt has timed out, rather than failing or waiting.
#[tokio::test]
async fn bootstrap_dialer_returns_no_clients_for_unreachable_peers() {
    let _init_guard = zebra_test::init();

    // Nothing listens on these ports: they were free when probed, and the probes are dropped.
    // The IPv6 peer, where the host has IPv6, makes the bootstrap bind both address families;
    // where it has not, the bootstrap skips that family and dials the IPv4 peer alone.
    let [silent_v4] = free_udp_addrs();
    let mut peers = vec![PeerSocketAddr::from(silent_v4)];
    if let Ok(probe) = std::net::UdpSocket::bind("[::1]:0") {
        peers.push(
            probe
                .local_addr()
                .expect("a bound socket has an address")
                .into(),
        );
    }
    let config = Config {
        network: Network::Mainnet,
        ..Config::default()
    };
    let bootstrap = tokio::time::timeout(
        HANDSHAKE_TIMEOUT * 3,
        v2_bootstrap_clients(&config, &peers, "/bootstrap test/".to_string()),
    )
    .await
    .expect("the attempts time out within the handshake timeout")
    .expect("the bootstrap dials");
    assert!(bootstrap.clients.is_empty());
    bootstrap.close().await;

    // No peers at all is an error, as is an oversized user agent, and a network without a version
    // 2 transport.
    let error = v2_bootstrap_clients(&config, &[], "/bootstrap test/".to_string())
        .await
        .expect_err("no peers to dial");
    assert!(error
        .to_string()
        .contains("no state snapshot bootstrap peers"));
    let error = v2_bootstrap_clients(&config, &peers, "/".repeat(257))
        .await
        .expect_err("the user agent is over the limit");
    assert!(error.to_string().contains("user agent"), "got {error}");
    let custom_testnet = Config {
        network: zebra_chain::parameters::testnet::Parameters::build()
            .with_network_name("bootstrap")
            .expect("valid network name")
            .to_network()
            .expect("valid custom testnet parameters"),
        ..Config::default()
    };
    let error = v2_bootstrap_clients(&custom_testnet, &peers, "/bootstrap test/".to_string())
        .await
        .expect_err("a custom testnet has no version 2 transport");
    assert!(error.to_string().contains("ALPN"), "got {error}");
}

/// A version 2 test node's config: listening on `v2_listen_addr`, dialling
/// `initial_v2_peers`, and with a per-IP limit that lets several test nodes
/// on 127.0.0.1 connect.
fn v2_test_config(v2_listen_addr: SocketAddr, initial_v2_peers: &[SocketAddr]) -> Config {
    Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        network: Network::Mainnet,
        initial_mainnet_peers: IndexSet::new(),
        initial_testnet_peers: IndexSet::new(),
        cache_dir: CacheDir::disabled(),
        v2_listen_addr: Some(v2_listen_addr),
        initial_v2_peers: initial_v2_peers.iter().map(ToString::to_string).collect(),
        max_connections_per_ip: 10,
        ..Config::default()
    }
}

/// Starts a version 2 listener and dialer for `config`, advertising
/// `local_services`, with the test reconnection delay and inbound interval.
///
/// Returns the channel of the clients it would send to its peer set, and its
/// task.
fn spawn_v2_node(
    config: Config,
    local_services: PeerServices,
) -> (
    mpsc::Receiver<DiscoveredPeer>,
    JoinHandle<Result<(), BoxError>>,
) {
    let (peerset_tx, peerset_rx) = mpsc::channel(config.peerset_total_connection_limit());
    let (inv_collector, _) = broadcast::channel(16);
    // These tests do not check misbehavior reports; a closed channel drops them.
    let (misbehavior_tx, _) = tokio::sync::mpsc::channel(1);
    let (_bans_tx, bans_rx) = watch::channel(Default::default());
    let task = v2::start_with_intervals(
        config,
        local_services,
        service_fn(|_| async { Ok::<Response, BoxError>(Response::Nil) }),
        inv_collector,
        NoChainTip,
        "v2 test node".to_string(),
        bans_rx,
        misbehavior_tx,
        peerset_tx,
        ConnectedPeers::unconnected(),
        RECONNECTION_DELAY,
        INBOUND_INTERVAL,
    )
    .expect("version 2 test configs listen");

    (peerset_rx, task)
}

/// `N` distinct UDP addresses on 127.0.0.1 that were free when probed: the
/// probes are held together, so the operating system cannot hand the same
/// port out twice.
fn free_udp_addrs<const N: usize>() -> [SocketAddr; N] {
    let probes: [std::net::UdpSocket; N] = std::array::from_fn(|_| {
        std::net::UdpSocket::bind("127.0.0.1:0").expect("a UDP port is free")
    });
    probes
        .each_ref()
        .map(|probe| probe.local_addr().expect("a bound socket has an address"))
}

/// A UDP address for a node that only dials, so nothing needs to know its
/// port in advance.
fn any_udp_addr() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}
