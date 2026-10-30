//! Fixed test cases for peer handshakes.

use tower::{service_fn, ServiceExt};

use crate::{address_book_updater, peer_set::ActiveConnectionCounter};

use super::super::*;

/// The timeout for each handshake test.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

#[test]
fn serialization_error_metric_labels_ignore_details() {
    for detail in ["first error detail", "second error detail"] {
        let error = SerializationError::from(std::io::Error::other(detail));
        assert_eq!(serialization_error_metric_label(&error), "io");
        assert_eq!(
            serialization_error_metric_label(&SerializationError::Parse(detail)),
            "parse",
        );
    }
}

/// The height advertised by the scripted remote peer.
const REMOTE_START_HEIGHT: block::Height = block::Height(2_500_000);

/// Spawns a scripted remote peer on `remote_stream` that answers a handshake with a version
/// message advertising `remote_services`, then a verack.
fn spawn_remote_peer(
    remote_stream: tokio::io::DuplexStream,
    network: &Network,
    remote_services: PeerServices,
) -> tokio::task::JoinHandle<()> {
    let mut remote_conn = Framed::new(
        remote_stream,
        Codec::builder().for_network(network).finish(),
    );

    tokio::spawn(async move {
        // Wait for the local version message.
        loop {
            match remote_conn.next().await {
                Some(Ok(Message::Version(_))) => break,
                Some(Ok(_)) => continue,
                Some(Err(_)) | None => return,
            }
        }

        let listen_addr: SocketAddr = "127.0.0.1:8233".parse().unwrap();
        let remote_version = VersionMessage {
            version: constants::CURRENT_NETWORK_PROTOCOL_VERSION,
            services: remote_services,
            timestamp: Utc::now(),
            address_recv: AddrInVersion::new(listen_addr, PeerServices::NODE_NETWORK),
            address_from: AddrInVersion::new(listen_addr, remote_services),
            nonce: Nonce::default(),
            user_agent: "/test-remote/".to_string(),
            start_height: REMOTE_START_HEIGHT,
            relay: false,
        };

        if remote_conn.send(remote_version.into()).await.is_err() {
            return;
        }
        if remote_conn.send(Message::Verack).await.is_err() {
            return;
        }

        // Drain messages until the local side closes the connection.
        while let Some(msg) = remote_conn.next().await {
            if msg.is_err() {
                return;
            }
        }
    })
}

/// Negotiates a handshake with a scripted remote peer advertising `remote_services`,
/// on a connection with the given `connected_addr` direction, with the local node's sync
/// state taken from `chain_tip`.
///
/// [`NoChainTip`] means the local node is still syncing; use a
/// [`MockChainTip`](zebra_chain::chain_tip::mock::MockChainTip) with an estimated distance of
/// zero for a node at the network tip.
async fn negotiate_with_remote_services(
    connected_addr: ConnectedAddr,
    remote_services: PeerServices,
    chain_tip: impl ChainTip + Clone + Send + 'static,
) -> Result<Arc<ConnectionInfo>, HandshakeError> {
    let network = Network::Mainnet;
    let config = Config {
        network: network.clone(),
        ..Config::default()
    };

    let (local_stream, remote_stream) = tokio::io::duplex(65536);
    let remote_peer = spawn_remote_peer(remote_stream, &network, remote_services);

    let mut local_conn = Framed::new(
        local_stream,
        Codec::builder().for_network(&network).finish(),
    );

    let result = timeout(
        TEST_TIMEOUT,
        negotiate_version(
            &mut local_conn,
            &connected_addr,
            config,
            Arc::new(futures::lock::Mutex::new(IndexSet::new())),
            "/test-local/".to_string(),
            PeerServices::NODE_NETWORK,
            false,
            MinimumPeerVersion::new(chain_tip, &network),
        ),
    )
    .await
    .expect("handshake must not time out");

    remote_peer.abort();

    result
}

/// Returns a [`ChainTip`] that reports the local node as being at the network tip,
/// and the sender that keeps it alive.
fn chain_tip_at_network_tip() -> (
    zebra_chain::chain_tip::mock::MockChainTip,
    zebra_chain::chain_tip::mock::MockChainTipSender,
) {
    let (chain_tip, sender) = zebra_chain::chain_tip::mock::MockChainTip::new();
    sender.send_best_tip_height(block::Height(3_000_000));
    sender.send_best_tip_block_time(chrono::Utc::now());
    (chain_tip, sender)
}

/// While syncing, outbound direct handshakes must reject peers that don't advertise
/// `NODE_NETWORK`.
#[tokio::test]
async fn outbound_direct_rejects_non_serving_peer_while_syncing() {
    let _init_guard = zebra_test::init();

    let connected_addr = ConnectedAddr::new_outbound_direct("127.0.0.1:8233".parse().unwrap());
    let result =
        negotiate_with_remote_services(connected_addr, PeerServices::empty(), NoChainTip).await;

    assert!(
        matches!(
            result,
            Err(HandshakeError::MissingRequiredServices { services }) if services.is_empty()
        ),
        "expected MissingRequiredServices, got: {result:?}",
    );
}

/// While syncing, outbound proxy handshakes must reject peers that don't advertise
/// `NODE_NETWORK`.
#[tokio::test]
async fn outbound_proxy_rejects_non_serving_peer_while_syncing() {
    let _init_guard = zebra_test::init();

    let connected_addr = ConnectedAddr::new_outbound_proxy(
        "127.0.0.1:9050".parse().unwrap(),
        "127.0.0.1:32000".parse().unwrap(),
    );
    let result =
        negotiate_with_remote_services(connected_addr, PeerServices::empty(), NoChainTip).await;

    assert!(
        matches!(
            result,
            Err(HandshakeError::MissingRequiredServices { services }) if services.is_empty()
        ),
        "expected MissingRequiredServices, got: {result:?}",
    );
}

/// Inbound handshakes must accept peers that don't advertise `NODE_NETWORK`,
/// so light clients can connect to us.
#[tokio::test]
async fn inbound_direct_accepts_non_serving_peer() {
    let _init_guard = zebra_test::init();

    let connected_addr = ConnectedAddr::new_inbound_direct("127.0.0.1:32000".parse().unwrap());
    let result =
        negotiate_with_remote_services(connected_addr, PeerServices::empty(), NoChainTip).await;

    let connection_info = result.expect("inbound handshake with a non-serving peer succeeds");
    assert_eq!(connection_info.remote.services, PeerServices::empty());
}

/// Isolated handshakes must accept peers that don't advertise `NODE_NETWORK`.
#[tokio::test]
async fn isolated_accepts_non_serving_peer() {
    let _init_guard = zebra_test::init();

    let connected_addr = ConnectedAddr::new_isolated();
    let result =
        negotiate_with_remote_services(connected_addr, PeerServices::empty(), NoChainTip).await;

    result.expect("isolated handshake with a non-serving peer succeeds");
}

/// At or near the network tip, outbound handshakes must accept peers that don't advertise
/// `NODE_NETWORK`, so Zebra can peer with pruned nodes that serve recent blocks.
#[tokio::test]
async fn outbound_direct_accepts_non_serving_peer_at_tip() {
    let _init_guard = zebra_test::init();

    let (chain_tip, _sender) = chain_tip_at_network_tip();
    let connected_addr = ConnectedAddr::new_outbound_direct("127.0.0.1:8233".parse().unwrap());
    let result =
        negotiate_with_remote_services(connected_addr, PeerServices::empty(), chain_tip).await;

    let connection_info =
        result.expect("outbound handshake with a non-serving peer at the network tip succeeds");
    assert_eq!(connection_info.remote.services, PeerServices::empty());
}

/// Outbound handshakes must accept peers that advertise `NODE_NETWORK`.
#[tokio::test]
async fn outbound_direct_accepts_serving_peer() {
    let _init_guard = zebra_test::init();

    let connected_addr = ConnectedAddr::new_outbound_direct("127.0.0.1:8233".parse().unwrap());
    let result =
        negotiate_with_remote_services(connected_addr, PeerServices::NODE_NETWORK, NoChainTip)
            .await;

    let connection_info = result.expect("outbound handshake with a serving peer succeeds");
    assert_eq!(connection_info.remote.services, PeerServices::NODE_NETWORK);
}

/// Both actual version messages must opt in; configuration and user agents cannot substitute.
#[tokio::test]
async fn early_block_relay_requires_bilateral_handshake() {
    let _init_guard = zebra_test::init();
    timeout(TEST_TIMEOUT, async {
        for local_setting in [
            "",
            "experimental_early_block_relay = false",
            "experimental_early_block_relay = true",
        ] {
            for remote_enabled in [false, true] {
                for isolated in [false, true] {
                    let config: Config = toml::from_str(local_setting).unwrap();
                    let local_enabled = config.experimental_early_block_relay;
                    // Exercise the serialized operator configuration, not just a Rust field.
                    let config = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
                    let remote_config = Config {
                        experimental_early_block_relay: remote_enabled,
                        ..Config::default()
                    };
                    let (local, remote) = tokio::io::duplex(65536);
                    let network = Network::Mainnet;
                    let codec = || Codec::builder().for_network(&network).finish();
                    let mut local = Framed::new(local, codec());
                    let mut remote = Framed::new(remote, codec());
                    let address = "127.0.0.1:8233".parse().unwrap();
                    let local_addr = if isolated {
                        ConnectedAddr::Isolated
                    } else {
                        ConnectedAddr::new_outbound_direct(address)
                    };
                    let remote_addr = ConnectedAddr::new_inbound_direct(address);
                    let (local_info, remote_info) = tokio::try_join!(
                        negotiate_version(
                            &mut local,
                            &local_addr,
                            config,
                            Arc::default(),
                            "/same-user-agent/".to_string(),
                            PeerServices::NODE_NETWORK,
                            false,
                            MinimumPeerVersion::new(NoChainTip, &network),
                        ),
                        negotiate_version(
                            &mut remote,
                            &remote_addr,
                            remote_config,
                            Arc::default(),
                            "/same-user-agent/".to_string(),
                            PeerServices::NODE_NETWORK,
                            false,
                            MinimumPeerVersion::new(NoChainTip, &network),
                        ),
                    )
                    .expect("both peers support this network version");
                    let bit = PeerServices::NODE_EARLY_BLOCK_RELAY;
                    assert_eq!(local_info.remote.services.contains(bit), remote_enabled);
                    assert_eq!(
                        remote_info.remote.services.contains(bit),
                        local_enabled && !isolated
                    );
                    let negotiated = local_enabled && remote_enabled && !isolated;
                    assert_eq!(local_info.early_block_relay, negotiated);
                    assert_eq!(remote_info.early_block_relay, negotiated);
                }
            }
        }
    })
    .await
    .expect("bilateral handshakes must complete within the test deadline");
}

/// The version and sender address use the same capabilities for archive and pruned nodes.
#[tokio::test]
async fn local_version_advertises_configured_services() {
    let _init_guard = zebra_test::init();
    timeout(TEST_TIMEOUT, async {
        for archive in [false, true] {
            for experimental_early_block_relay in [false, true] {
                let mut config = Config {
                    listen_addr: "127.0.0.1:8233".parse().unwrap(),
                    experimental_early_block_relay,
                    ..Config::default()
                };
                if !archive {
                    config.local_services.remove(PeerServices::NODE_NETWORK);
                }
                let mut expected = config.local_services;
                if experimental_early_block_relay {
                    expected.insert(PeerServices::NODE_EARLY_BLOCK_RELAY);
                }
                let listen_addr = config.listen_addr;
                let codec = Codec::builder().for_network(&config.network).finish();
                let handshake = Handshake::builder()
                    .with_config(config)
                    .with_inbound_service(service_fn(|_req| async {
                        Ok::<Response, BoxError>(Response::Nil)
                    }))
                    .finish()
                    .expect("the config and inbound service are set");
                let (local, remote) = tokio::io::duplex(65536);
                let mut remote = Framed::new(remote, codec);
                let (client, ()) = tokio::join!(
                    handshake.oneshot(HandshakeRequest {
                        data_stream: local,
                        connected_addr: ConnectedAddr::new_outbound_direct(listen_addr.into()),
                        connection_tracker: ActiveConnectionCounter::new_counter()
                            .track_connection(),
                    }),
                    async {
                        let Message::Version(mut version) = remote.next().await.unwrap().unwrap()
                        else {
                            panic!("the handshake starts with a version message");
                        };
                        assert_eq!(version.services, expected);
                        assert_eq!(
                            version.address_from,
                            AddrInVersion::new(listen_addr, expected),
                        );
                        version.services.insert(PeerServices::NODE_NETWORK);
                        version.nonce = Nonce::default();
                        remote.send(Message::Version(version)).await.unwrap();
                        remote.send(Message::Verack).await.unwrap();
                        assert_eq!(remote.next().await.unwrap().unwrap(), Message::Verack);
                    },
                );
                let client = client.expect("the remote peer advertises archive service");
                assert_eq!(
                    client.connection_info.early_block_relay,
                    experimental_early_block_relay
                );
            }
        }
    })
    .await
    .expect("service capability handshakes must finish within the test deadline");
}

/// Cached alternate listeners retain the service mask received in the version address.
#[tokio::test]
async fn alternate_listener_cache_preserves_advertised_services() {
    let _init_guard = zebra_test::init();
    timeout(TEST_TIMEOUT, async {
        for services in [
            PeerServices::empty(),
            PeerServices::NODE_EARLY_BLOCK_RELAY,
            PeerServices::NODE_NETWORK | PeerServices::NODE_EARLY_BLOCK_RELAY,
        ] {
            let network = Network::Mainnet;
            let handshake = Handshake::builder()
                .with_config(Config {
                    network: network.clone(),
                    ..Config::default()
                })
                .with_inbound_service(service_fn(|_req| async {
                    Ok::<Response, BoxError>(Response::Nil)
                }))
                .finish()
                .expect("the config and inbound service are set");
            let (local, remote) = tokio::io::duplex(65536);
            let _remote_peer =
                AbortOnDropHandle::new(spawn_remote_peer(remote, &network, services));
            let mut client = handshake
                .oneshot(HandshakeRequest {
                    data_stream: local,
                    connected_addr: ConnectedAddr::new_inbound_direct(
                        "127.0.0.2:41234".parse().unwrap(),
                    ),
                    connection_tracker: ActiveConnectionCounter::new_counter().track_connection(),
                })
                .await
                .expect("inbound handshakes accept pruned peers");
            // Keep the client alive: dropping it shuts down the connection before the reply.
            let response = client.ready().await.unwrap().call(Request::Peers).await;
            let Response::Peers(peers) = response.unwrap() else {
                panic!("peer requests return cached listener addresses");
            };

            // Both the canonical listener and the TCP-IP-derived listener keep the services.
            assert_eq!(peers.len(), 2);
            for peer in peers {
                assert_eq!(peer.services(), Some(services));
            }
        }
    })
    .await
    .expect("cached listener requests must finish within the test deadline");
}

/// Successful handshakes publish the remote start height for inbound and outbound peers.
#[tokio::test]
async fn handshake_records_start_height() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let book_addr: PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();

    for connected_addr in [
        ConnectedAddr::new_inbound_direct(book_addr),
        ConnectedAddr::new_outbound_direct(book_addr),
    ] {
        let (address_book_tx, mut address_book_rx) = address_book_updater::change_channel(10);
        let handshake = Handshake::builder()
            .with_config(Config {
                network: network.clone(),
                ..Config::default()
            })
            .with_inbound_service(service_fn(|_req| async move {
                Ok::<Response, BoxError>(Response::Nil)
            }))
            .with_latest_chain_tip(NoChainTip)
            .with_address_book_updater(address_book_tx)
            .finish()
            .expect("provided mandatory builder parameters");
        let (local_stream, remote_stream) = tokio::io::duplex(65536);
        let remote_peer = spawn_remote_peer(remote_stream, &network, PeerServices::NODE_NETWORK);
        let client = timeout(
            TEST_TIMEOUT,
            handshake.oneshot(HandshakeRequest {
                data_stream: local_stream,
                connected_addr,
                connection_tracker: ActiveConnectionCounter::new_counter().track_connection(),
            }),
        )
        .await
        .expect("handshake must not time out")
        .expect("serving peer handshake succeeds");

        let update = timeout(TEST_TIMEOUT, address_book_rx.recv())
            .await
            .expect("address book update must not time out")
            .expect("successful handshake publishes an address book update")
            .into_change()
            .expect("handshake publishes a peer change");
        let now = Utc::now();
        let instant = std::time::Instant::now();
        let initial = MetaAddr::new_initial_peer(book_addr)
            .into_new_meta_addr(instant, now.try_into().unwrap());
        for previous in [None, Some(initial)] {
            let peer = update
                .apply_to_meta_addr(previous, instant, now)
                .expect("successful handshake updates new and existing peers");
            assert_eq!(peer.start_height(), Some(REMOTE_START_HEIGHT));
        }

        drop(client);
        remote_peer.abort();
    }
}

/// A rejected non-serving outbound peer must NOT have its advertised services recorded by the
/// handshake: the rejection only applies while syncing, and storing non-serving services would
/// permanently exclude the peer from outbound connections, so it could never be dialed once
/// the node reaches the network tip. The crawler records a plain retryable failure instead.
#[tokio::test]
async fn rejected_non_serving_peer_is_not_recorded_with_services() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let config = Config {
        network: network.clone(),
        ..Config::default()
    };

    let (address_book_tx, mut address_book_rx) = address_book_updater::change_channel(10);

    let handshake = Handshake::builder()
        .with_config(config)
        .with_inbound_service(service_fn(|_req| async move {
            Ok::<Response, BoxError>(Response::Nil)
        }))
        .with_user_agent("/test-local/".to_string())
        .with_latest_chain_tip(NoChainTip)
        .with_address_book_updater(address_book_tx)
        .finish()
        .expect("provided mandatory builder parameters");

    let (local_stream, remote_stream) = tokio::io::duplex(65536);
    let remote_peer = spawn_remote_peer(remote_stream, &network, PeerServices::empty());

    let book_addr: PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
    let connected_addr = ConnectedAddr::new_outbound_direct(book_addr);
    let connection_tracker = ActiveConnectionCounter::new_counter().track_connection();

    let result = timeout(
        TEST_TIMEOUT,
        handshake.oneshot(HandshakeRequest {
            data_stream: local_stream,
            connected_addr,
            connection_tracker,
        }),
    )
    .await
    .expect("handshake must not time out");

    remote_peer.abort();

    assert!(
        result.is_err(),
        "outbound handshake with a non-serving peer must fail",
    );

    // Any address book update would have been sent before the handshake future returned, so
    // an empty channel here means the handshake recorded nothing for the rejected peer.
    assert!(
        address_book_rx.try_recv().is_err(),
        "unexpected address book update from a rejected handshake for {book_addr}",
    );
}
