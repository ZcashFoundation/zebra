//! Fixed test cases for peer handshakes.

use tower::{service_fn, ServiceExt};

use crate::peer_set::ActiveConnectionCounter;

use super::super::*;

/// The timeout for each handshake test.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

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
            start_height: block::Height(0),
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
/// on a connection with the given `connected_addr` direction.
async fn negotiate_with_remote_services(
    connected_addr: ConnectedAddr,
    remote_services: PeerServices,
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
            MinimumPeerVersion::new(NoChainTip, &network),
        ),
    )
    .await
    .expect("handshake must not time out");

    remote_peer.abort();

    result
}

/// Outbound direct handshakes must reject peers that don't advertise `NODE_NETWORK`.
#[tokio::test]
async fn outbound_direct_rejects_non_serving_peer() {
    let _init_guard = zebra_test::init();

    let connected_addr = ConnectedAddr::new_outbound_direct("127.0.0.1:8233".parse().unwrap());
    let result = negotiate_with_remote_services(connected_addr, PeerServices::empty()).await;

    assert!(
        matches!(
            result,
            Err(HandshakeError::MissingRequiredServices { services }) if services.is_empty()
        ),
        "expected MissingRequiredServices, got: {result:?}",
    );
}

/// Outbound proxy handshakes must reject peers that don't advertise `NODE_NETWORK`.
#[tokio::test]
async fn outbound_proxy_rejects_non_serving_peer() {
    let _init_guard = zebra_test::init();

    let connected_addr = ConnectedAddr::new_outbound_proxy(
        "127.0.0.1:9050".parse().unwrap(),
        "127.0.0.1:32000".parse().unwrap(),
    );
    let result = negotiate_with_remote_services(connected_addr, PeerServices::empty()).await;

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
    let result = negotiate_with_remote_services(connected_addr, PeerServices::empty()).await;

    let connection_info = result.expect("inbound handshake with a non-serving peer succeeds");
    assert_eq!(connection_info.remote.services, PeerServices::empty());
}

/// Isolated handshakes must accept peers that don't advertise `NODE_NETWORK`.
#[tokio::test]
async fn isolated_accepts_non_serving_peer() {
    let _init_guard = zebra_test::init();

    let connected_addr = ConnectedAddr::new_isolated();
    let result = negotiate_with_remote_services(connected_addr, PeerServices::empty()).await;

    result.expect("isolated handshake with a non-serving peer succeeds");
}

/// Outbound handshakes must accept peers that advertise `NODE_NETWORK`.
#[tokio::test]
async fn outbound_direct_accepts_serving_peer() {
    let _init_guard = zebra_test::init();

    let connected_addr = ConnectedAddr::new_outbound_direct("127.0.0.1:8233".parse().unwrap());
    let result = negotiate_with_remote_services(connected_addr, PeerServices::NODE_NETWORK).await;

    let connection_info = result.expect("outbound handshake with a serving peer succeeds");
    assert_eq!(connection_info.remote.services, PeerServices::NODE_NETWORK);
}

/// A rejected non-serving outbound peer must have its advertised services recorded in the
/// address book, so the crawler stops re-dialing it.
#[tokio::test]
async fn rejected_non_serving_peer_is_recorded_in_address_book() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let config = Config {
        network: network.clone(),
        ..Config::default()
    };

    let (address_book_tx, mut address_book_rx) = tokio::sync::mpsc::channel(10);

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

    let change = address_book_rx
        .recv()
        .await
        .expect("the handshake sends an update for the rejected peer");
    assert_eq!(change.addr(), book_addr);
    assert_eq!(change.untrusted_services(), Some(PeerServices::empty()));
    assert!(
        matches!(change, MetaAddrChange::UpdateFailed { .. }),
        "expected UpdateFailed, got: {change:?}",
    );
}
