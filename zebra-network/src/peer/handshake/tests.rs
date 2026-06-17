//! Implements methods for testing [`Handshake`]

#![allow(clippy::unwrap_in_result)]

use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use super::*;
use crate::{
    peer_set::ActiveConnectionCounter,
    zakura::{Frame, InboundSink, InboundSinkReject, ZakuraPeerId, ZakuraUpgradeOutcome},
};
use tokio::io::duplex;
use tower::ServiceExt;
use zebra_chain::{block, chain_tip::NoChainTip};
use zebra_test::mock_service::{MockService, PanicAssertion};

impl<S, C> Handshake<S, C>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone + Send + 'static,
    S::Future: Send,
    C: ChainTip + Clone + Send + 'static,
{
    /// Returns a count of how many connection nonces are stored in this [`Handshake`]
    pub async fn nonce_count(&self) -> usize {
        self.nonces.lock().await.len()
    }
}

fn peer_addr(port: u16) -> PeerSocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port)).into()
}

fn test_config(v2_p2p: bool) -> Config {
    Config {
        v2_p2p,
        ..Config::default()
    }
}

fn upgraded_outcome() -> ZakuraUpgradeOutcome {
    ZakuraUpgradeOutcome::Upgraded {
        peer_id: ZakuraPeerId::new(vec![7; 32]).expect("test peer id is within bounds"),
    }
}

async fn negotiate_test_pair(
    local_config: Config,
    remote_config: Config,
) -> (Arc<ConnectionInfo>, Arc<ConnectionInfo>) {
    let (local_stream, remote_stream) = duplex(16 * 1024);
    let mut local_conn = Framed::new(
        local_stream,
        Codec::builder().for_network(&local_config.network).finish(),
    );
    let mut remote_conn = Framed::new(
        remote_stream,
        Codec::builder()
            .for_network(&remote_config.network)
            .finish(),
    );

    let local_addr = ConnectedAddr::new_outbound_direct(peer_addr(18233));
    let remote_addr = ConnectedAddr::new_inbound_direct(peer_addr(28233));

    let local_nonces = Arc::new(futures::lock::Mutex::new(IndexSet::new()));
    let remote_nonces = Arc::new(futures::lock::Mutex::new(IndexSet::new()));

    let local_min_version = MinimumPeerVersion::new(NoChainTip, &local_config.network);
    let remote_min_version = MinimumPeerVersion::new(NoChainTip, &remote_config.network);

    let local_services = configured_advertised_services(&local_config, PeerServices::NODE_NETWORK);
    let remote_services =
        configured_advertised_services(&remote_config, PeerServices::NODE_NETWORK);
    let local_user_agent = configured_user_agent(&local_config, "/Zebra:local-test/".to_string());
    let remote_user_agent =
        configured_user_agent(&remote_config, "/Zebra:remote-test/".to_string());

    let local_task = tokio::spawn(async move {
        negotiate_version(
            &mut local_conn,
            &local_addr,
            local_config,
            local_nonces,
            local_user_agent,
            local_services,
            true,
            local_min_version,
        )
        .await
    });

    let remote_task = tokio::spawn(async move {
        negotiate_version(
            &mut remote_conn,
            &remote_addr,
            remote_config,
            remote_nonces,
            remote_user_agent,
            remote_services,
            true,
            remote_min_version,
        )
        .await
    });

    let local_info = local_task.await.unwrap().unwrap();
    let remote_info = remote_task.await.unwrap().unwrap();

    (local_info, remote_info)
}

fn test_handshake(
    config: Config,
    address_book_updater: tokio::sync::mpsc::Sender<MetaAddrChange>,
    calls: Arc<AtomicUsize>,
    outcome: ZakuraUpgradeOutcome,
) -> Handshake<MockService<Request, Response, PanicAssertion>> {
    let inbound_service: MockService<Request, Response, PanicAssertion> =
        MockService::build().for_unit_tests();

    Handshake::builder()
        .with_config(config)
        .with_inbound_service(inbound_service)
        .with_address_book_updater(address_book_updater)
        .with_advertised_services(PeerServices::NODE_NETWORK)
        .with_user_agent("/Zebra:handshake-test/".to_string())
        .with_zakura_handshake_connector(crate::zakura::ZakuraHandshakeConnector::for_test(
            calls, outcome,
        ))
        .want_transactions(true)
        .finish()
        .unwrap()
}

fn test_handshake_without_zakura_connector(
    config: Config,
    address_book_updater: tokio::sync::mpsc::Sender<MetaAddrChange>,
) -> Handshake<MockService<Request, Response, PanicAssertion>> {
    let inbound_service: MockService<Request, Response, PanicAssertion> =
        MockService::build().for_unit_tests();

    Handshake::builder()
        .with_config(config)
        .with_inbound_service(inbound_service)
        .with_address_book_updater(address_book_updater)
        .with_advertised_services(PeerServices::NODE_NETWORK)
        .with_user_agent("/Zebra:handshake-test/".to_string())
        .want_transactions(true)
        .finish()
        .unwrap()
}

fn test_handshake_with_connector(
    config: Config,
    address_book_updater: tokio::sync::mpsc::Sender<MetaAddrChange>,
    connector: crate::zakura::ZakuraHandshakeConnector,
) -> Handshake<MockService<Request, Response, PanicAssertion>> {
    let inbound_service: MockService<Request, Response, PanicAssertion> =
        MockService::build().for_unit_tests();

    Handshake::builder()
        .with_config(config)
        .with_inbound_service(inbound_service)
        .with_address_book_updater(address_book_updater)
        .with_advertised_services(PeerServices::NODE_NETWORK)
        .with_user_agent("/Zebra:handshake-test/".to_string())
        .with_zakura_handshake_connector(connector)
        .want_transactions(true)
        .finish()
        .unwrap()
}

/// An inbound sink that drops every delivered frame, used to start a real Zakura
/// endpoint in tests without wiring an application service.
#[derive(Debug)]
struct DropSink;

impl InboundSink for DropSink {
    fn deliver(
        &self,
        _peer_id: ZakuraPeerId,
        _stream_kind: u16,
        _frame: Frame,
    ) -> Result<(), InboundSinkReject> {
        Ok(())
    }
}

/// Starts a real Zakura endpoint over loopback QUIC for an upgrade test.
async fn start_test_zakura_endpoint() -> crate::zakura::ZakuraEndpoint {
    crate::zakura::spawn_zakura_endpoint(&test_config(true), |_supervisor| {
        Arc::new(DropSink) as Arc<dyn InboundSink>
    })
    .await
    .expect("Zakura endpoint starts")
    .expect("v2_p2p is enabled in the test config")
}

/// Two mutually P2P-v2-capable nodes with live Zakura endpoints should exchange
/// the legacy upgrade prelude over the TCP stream, drop the legacy connection,
/// and establish a real Zakura QUIC connection that registers on both ends.
#[tokio::test]
async fn mutual_p2p_v2_legacy_upgrade_forms_zakura_connection() {
    let _init_guard = zebra_test::init();

    let local_endpoint = start_test_zakura_endpoint().await;
    let remote_endpoint = start_test_zakura_endpoint().await;

    let (local_stream, remote_stream) = duplex(16 * 1024);
    let (address_book_tx, _address_book_rx) = tokio::sync::mpsc::channel(8);

    let mut local_counter = ActiveConnectionCounter::new_counter();
    let mut remote_counter = ActiveConnectionCounter::new_counter();

    let local_handshake = test_handshake_with_connector(
        test_config(true),
        address_book_tx.clone(),
        local_endpoint.connector(),
    );
    let remote_handshake = test_handshake_with_connector(
        test_config(true),
        address_book_tx,
        remote_endpoint.connector(),
    );

    let local_task = tokio::spawn(local_handshake.oneshot(HandshakeRequest {
        data_stream: local_stream,
        connected_addr: ConnectedAddr::new_outbound_direct(peer_addr(18233)),
        connection_tracker: local_counter.track_connection(),
    }));
    let remote_task = tokio::spawn(remote_handshake.oneshot(HandshakeRequest {
        data_stream: remote_stream,
        connected_addr: ConnectedAddr::new_inbound_direct(peer_addr(28233)),
        connection_tracker: remote_counter.track_connection(),
    }));

    let local_error = local_task.await.unwrap().unwrap_err();
    let remote_error = remote_task.await.unwrap().unwrap_err();

    // A completed upgrade drops the legacy connection on both sides.
    assert!(local_error
        .downcast_ref::<HandshakeError>()
        .is_some_and(|error| matches!(error, HandshakeError::ZakuraUpgradeSelected)));
    assert!(remote_error
        .downcast_ref::<HandshakeError>()
        .is_some_and(|error| matches!(error, HandshakeError::ZakuraUpgradeSelected)));

    // The initiator dialed the responder's advertised Zakura address over QUIC;
    // both supervisors should register the other peer once it completes.
    let local_supervisor = local_endpoint.supervisor();
    let remote_supervisor = remote_endpoint.supervisor();
    let registered = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if !local_supervisor.registered_ids().await.is_empty()
                && !remote_supervisor.registered_ids().await.is_empty()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(
        registered.is_ok(),
        "the legacy upgrade should establish a Zakura connection registered on both endpoints",
    );

    local_endpoint.shutdown().await;
    remote_endpoint.shutdown().await;
}

#[tokio::test]
async fn p2p_v2_service_bit_advertisement_follows_config() {
    let _init_guard = zebra_test::init();

    assert!(!configured_advertised_services(
        &test_config(false),
        PeerServices::NODE_NETWORK | PeerServices::NODE_P2P_V2,
    )
    .contains(PeerServices::NODE_P2P_V2));

    let (disabled_peer_seen_by_enabled, enabled_peer_seen_by_disabled) =
        negotiate_test_pair(test_config(true), test_config(false)).await;

    assert!(!disabled_peer_seen_by_enabled
        .remote
        .services
        .contains(PeerServices::NODE_P2P_V2));
    assert!(!disabled_peer_seen_by_enabled
        .remote
        .address_from
        .untrusted_services()
        .contains(PeerServices::NODE_P2P_V2));

    assert!(enabled_peer_seen_by_disabled
        .remote
        .services
        .contains(PeerServices::NODE_P2P_V2));
    assert!(enabled_peer_seen_by_disabled
        .remote
        .address_from
        .untrusted_services()
        .contains(PeerServices::NODE_P2P_V2));
    assert!(enabled_peer_seen_by_disabled
        .remote
        .user_agent
        .starts_with("/Zakura:"));
    assert!(enabled_peer_seen_by_disabled
        .remote
        .user_agent
        .contains("/Zebra:local-test/"));
}

#[test]
fn zakura_upgrade_errors_are_neutral_disconnects() {
    let _init_guard = zebra_test::init();

    assert!(HandshakeError::ZakuraUpgradeSelected.is_neutral_disconnect());
    assert!(
        HandshakeError::ZakuraUpgrade(crate::zakura::ZakuraUpgradeError::Unavailable)
            .is_neutral_disconnect()
    );
    assert!(!HandshakeError::Timeout.is_neutral_disconnect());
}

#[test]
fn p2p_v2_upgrade_uses_main_version_services_only() {
    let _init_guard = zebra_test::init();

    let mut config = test_config(true);
    config.v2_p2p = true;

    let addr = peer_addr(18233);
    let inconsistent_remote = VersionMessage {
        version: constants::CURRENT_NETWORK_PROTOCOL_VERSION,
        services: PeerServices::NODE_NETWORK,
        timestamp: Utc::now(),
        address_recv: AddrInVersion::new(addr, PeerServices::NODE_NETWORK),
        address_from: AddrInVersion::new(
            addr,
            PeerServices::NODE_NETWORK | PeerServices::NODE_P2P_V2,
        ),
        nonce: Nonce(1),
        user_agent: "/Zebra:test/".to_string(),
        start_height: block::Height(0),
        relay: true,
    };

    let connection_info = ConnectionInfo {
        connected_addr: ConnectedAddr::new_outbound_direct(addr),
        local: inconsistent_remote.clone(),
        remote: inconsistent_remote,
        negotiated_version: constants::CURRENT_NETWORK_PROTOCOL_VERSION,
    };

    assert!(!should_attempt_zakura_upgrade(&config, &connection_info));
}

#[test]
fn p2p_v2_upgrade_requires_local_enable_and_remote_service_bit() {
    let _init_guard = zebra_test::init();

    let addr = peer_addr(18233);
    let version = |services| VersionMessage {
        version: constants::CURRENT_NETWORK_PROTOCOL_VERSION,
        services,
        timestamp: Utc::now(),
        address_recv: AddrInVersion::new(addr, PeerServices::NODE_NETWORK),
        address_from: AddrInVersion::new(addr, services),
        nonce: Nonce(1),
        user_agent: "/Zebra:test/".to_string(),
        start_height: block::Height(0),
        relay: true,
    };
    let connection_info = |remote_services| ConnectionInfo {
        connected_addr: ConnectedAddr::new_outbound_direct(addr),
        local: version(PeerServices::NODE_NETWORK | PeerServices::NODE_P2P_V2),
        remote: version(remote_services),
        negotiated_version: constants::CURRENT_NETWORK_PROTOCOL_VERSION,
    };

    assert!(!should_attempt_zakura_upgrade(
        &test_config(false),
        &connection_info(PeerServices::NODE_NETWORK | PeerServices::NODE_P2P_V2),
    ));
    assert!(!should_attempt_zakura_upgrade(
        &test_config(true),
        &connection_info(PeerServices::NODE_NETWORK),
    ));
    assert!(should_attempt_zakura_upgrade(
        &test_config(true),
        &connection_info(PeerServices::NODE_NETWORK | PeerServices::NODE_P2P_V2),
    ));
}

#[tokio::test]
async fn remote_p2p_v2_with_local_disabled_continues_legacy() {
    let _init_guard = zebra_test::init();

    let (local_stream, remote_stream) = duplex(16 * 1024);
    let (address_book_tx, _address_book_rx) = tokio::sync::mpsc::channel(8);
    let local_calls = Arc::new(AtomicUsize::new(0));
    let remote_calls = Arc::new(AtomicUsize::new(0));

    let mut local_counter = ActiveConnectionCounter::new_counter();
    let mut remote_counter = ActiveConnectionCounter::new_counter();

    let local_handshake = test_handshake(
        test_config(false),
        address_book_tx.clone(),
        local_calls.clone(),
        upgraded_outcome(),
    );
    let remote_handshake = test_handshake(
        test_config(true),
        address_book_tx,
        remote_calls.clone(),
        upgraded_outcome(),
    );

    let local_task = tokio::spawn(local_handshake.oneshot(HandshakeRequest {
        data_stream: local_stream,
        connected_addr: ConnectedAddr::new_outbound_direct(peer_addr(18233)),
        connection_tracker: local_counter.track_connection(),
    }));
    let remote_task = tokio::spawn(remote_handshake.oneshot(HandshakeRequest {
        data_stream: remote_stream,
        connected_addr: ConnectedAddr::new_inbound_direct(peer_addr(28233)),
        connection_tracker: remote_counter.track_connection(),
    }));

    let local_client = local_task.await.unwrap().unwrap();
    let remote_client = remote_task.await.unwrap().unwrap();

    assert_eq!(local_calls.load(Ordering::SeqCst), 0);
    assert_eq!(remote_calls.load(Ordering::SeqCst), 0);

    drop(local_client);
    drop(remote_client);
}

#[tokio::test]
async fn mutual_p2p_v2_without_connector_fails_closed() {
    let _init_guard = zebra_test::init();

    let (local_stream, remote_stream) = duplex(16 * 1024);
    let (address_book_tx, mut address_book_rx) = tokio::sync::mpsc::channel(8);

    let mut local_counter = ActiveConnectionCounter::new_counter();
    let mut remote_counter = ActiveConnectionCounter::new_counter();

    let local_handshake =
        test_handshake_without_zakura_connector(test_config(true), address_book_tx.clone());
    let remote_handshake =
        test_handshake_without_zakura_connector(test_config(true), address_book_tx);

    let local_task = tokio::spawn(local_handshake.oneshot(HandshakeRequest {
        data_stream: local_stream,
        connected_addr: ConnectedAddr::new_outbound_direct(peer_addr(18233)),
        connection_tracker: local_counter.track_connection(),
    }));
    let remote_task = tokio::spawn(remote_handshake.oneshot(HandshakeRequest {
        data_stream: remote_stream,
        connected_addr: ConnectedAddr::new_inbound_direct(peer_addr(28233)),
        connection_tracker: remote_counter.track_connection(),
    }));

    let local_error = local_task.await.unwrap().unwrap_err();
    let remote_error = remote_task.await.unwrap().unwrap_err();

    let mut saw_unavailable = false;
    for error in [&local_error, &remote_error] {
        let error = error
            .downcast_ref::<HandshakeError>()
            .expect("handshake task errors should be HandshakeError");
        saw_unavailable |= matches!(
            error,
            HandshakeError::ZakuraUpgrade(crate::zakura::ZakuraUpgradeError::Unavailable)
        );
        assert!(matches!(
            error,
            HandshakeError::ZakuraUpgrade(crate::zakura::ZakuraUpgradeError::Unavailable)
                | HandshakeError::ConnectionClosed
        ));
    }
    assert!(saw_unavailable);

    assert!(address_book_rx.try_recv().is_err());
    assert_eq!(local_counter.update_count(), 0);
    assert_eq!(remote_counter.update_count(), 0);
}

#[tokio::test]
async fn mutual_p2p_v2_selected_upgrade_skips_legacy_connection() {
    let _init_guard = zebra_test::init();

    let (local_stream, remote_stream) = duplex(16 * 1024);
    let (address_book_tx, mut address_book_rx) = tokio::sync::mpsc::channel(8);
    let local_calls = Arc::new(AtomicUsize::new(0));
    let remote_calls = Arc::new(AtomicUsize::new(0));

    let mut local_counter = ActiveConnectionCounter::new_counter();
    let mut remote_counter = ActiveConnectionCounter::new_counter();

    let local_handshake = test_handshake(
        test_config(true),
        address_book_tx.clone(),
        local_calls.clone(),
        upgraded_outcome(),
    );
    let remote_handshake = test_handshake(
        test_config(true),
        address_book_tx,
        remote_calls.clone(),
        upgraded_outcome(),
    );

    let local_task = tokio::spawn(local_handshake.oneshot(HandshakeRequest {
        data_stream: local_stream,
        connected_addr: ConnectedAddr::new_outbound_direct(peer_addr(18233)),
        connection_tracker: local_counter.track_connection(),
    }));
    let remote_task = tokio::spawn(remote_handshake.oneshot(HandshakeRequest {
        data_stream: remote_stream,
        connected_addr: ConnectedAddr::new_inbound_direct(peer_addr(28233)),
        connection_tracker: remote_counter.track_connection(),
    }));

    let local_error = local_task.await.unwrap().unwrap_err();
    let remote_error = remote_task.await.unwrap().unwrap_err();

    assert!(local_error
        .downcast_ref::<HandshakeError>()
        .is_some_and(|error| matches!(error, HandshakeError::ZakuraUpgradeSelected)));
    assert!(remote_error
        .downcast_ref::<HandshakeError>()
        .is_some_and(|error| matches!(error, HandshakeError::ZakuraUpgradeSelected)));

    assert_eq!(local_calls.load(Ordering::SeqCst), 1);
    assert_eq!(remote_calls.load(Ordering::SeqCst), 1);
    assert!(address_book_rx.try_recv().is_err());
    assert_eq!(local_counter.update_count(), 0);
    assert_eq!(remote_counter.update_count(), 0);
}
