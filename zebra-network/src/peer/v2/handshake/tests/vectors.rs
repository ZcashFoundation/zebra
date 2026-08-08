//! Handshake tests over loopback QUIC connections.

use zebra_chain::{block, parameters::Network};

use crate::{
    peer::{
        handshake::HandshakeNonces,
        v2::handshake::{initiate, respond, HandshakeParams, V2HandshakeError},
    },
    protocol::{
        external::types::{Nonce, PeerServices, Version},
        v2::{constants::MIN_V2_PROTOCOL_VERSION, init::InitRecord, quic, types::ErrorCode},
    },
};

/// A handshake timeout that is generous enough for loopback tests.
const TEST_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn test_init(version: Version, nonce: Nonce) -> InitRecord {
    InitRecord {
        version,
        services: PeerServices::NODE_NETWORK,
        nonce,
        user_agent: "/ZebraV2Test:0.0.1/".to_string(),
        start_height: block::Height(0),
        relay: true,
        announce: false,
        full_ids: false,
    }
}

fn test_params(version: Version, nonce: Nonce) -> HandshakeParams {
    HandshakeParams {
        local_init: test_init(version, nonce),
        min_remote_version: MIN_V2_PROTOCOL_VERSION,
        nonces: HandshakeNonces::default(),
        nonce_limit: 100,
    }
}

/// Establishes a pair of connected loopback QUIC connections.
async fn connected_pair() -> (quinn::Connection, quinn::Connection) {
    let network = Network::Mainnet;
    let server = quic::new_endpoint("127.0.0.1:0".parse().expect("valid address"), &network)
        .expect("endpoint creation succeeds");
    let client = quic::new_endpoint("127.0.0.1:0".parse().expect("valid address"), &network)
        .expect("endpoint creation succeeds");
    let server_addr = server.local_addr().expect("endpoint has a local address");

    let accept_task = tokio::spawn(async move {
        let incoming = server.accept().await.expect("endpoint accepts");
        let connection = quic::accept(incoming, &Network::Mainnet)
            .await
            .expect("inbound connection completes");
        // Keep the server endpoint alive for the life of the test connection.
        (connection, server)
    });

    let outbound = quic::connect(&client, server_addr, &network)
        .await
        .expect("outbound connection completes");
    let (inbound, server) = accept_task.await.expect("accept task succeeds");

    // Leak the endpoints so they outlive the test connections.
    std::mem::forget(server);
    std::mem::forget(client);

    (outbound, inbound)
}

#[tokio::test]
async fn handshake_succeeds() {
    let _init_guard = zebra_test::init();

    let (outbound, inbound) = connected_pair().await;

    let initiator_params = test_params(Version(MIN_V2_PROTOCOL_VERSION.0 + 10), Nonce(1111));
    let responder_params = test_params(MIN_V2_PROTOCOL_VERSION, Nonce(2222));

    let responder = tokio::spawn(async move {
        tokio::time::timeout(TEST_HANDSHAKE_TIMEOUT, respond(&inbound, &responder_params))
            .await
            .expect("handshake completes in time")
    });

    let initiator_handshake = tokio::time::timeout(
        TEST_HANDSHAKE_TIMEOUT,
        initiate(&outbound, &initiator_params),
    )
    .await
    .expect("handshake completes in time")
    .expect("initiator handshake succeeds");

    let responder_handshake = responder
        .await
        .expect("responder task succeeds")
        .expect("responder handshake succeeds");

    // Both peers negotiate the lower of the two advertised versions.
    assert_eq!(
        initiator_handshake.negotiated_version,
        MIN_V2_PROTOCOL_VERSION,
    );
    assert_eq!(
        responder_handshake.negotiated_version,
        MIN_V2_PROTOCOL_VERSION,
    );

    assert_eq!(initiator_handshake.remote_init.nonce, Nonce(2222));
    assert_eq!(responder_handshake.remote_init.nonce, Nonce(1111));
    assert_eq!(
        initiator_handshake.remote_init.user_agent,
        "/ZebraV2Test:0.0.1/",
    );
}

#[tokio::test]
async fn handshake_rejects_obsolete_version() {
    let _init_guard = zebra_test::init();

    let (outbound, inbound) = connected_pair().await;

    // The initiator advertises a version below the responder's minimum.
    let initiator_params = test_params(Version(MIN_V2_PROTOCOL_VERSION.0 - 1), Nonce(1111));
    let responder_params = test_params(MIN_V2_PROTOCOL_VERSION, Nonce(2222));

    let responder = tokio::spawn(async move {
        tokio::time::timeout(TEST_HANDSHAKE_TIMEOUT, respond(&inbound, &responder_params))
            .await
            .expect("handshake fails in time")
    });

    let initiator_result = tokio::time::timeout(
        TEST_HANDSHAKE_TIMEOUT,
        initiate(&outbound, &initiator_params),
    )
    .await
    .expect("handshake fails in time");

    let responder_result = responder.await.expect("responder task succeeds");
    assert!(
        matches!(responder_result, Err(V2HandshakeError::ObsoleteVersion(_))),
        "got: {responder_result:?}",
    );

    // The initiator observes the connection close with the OBSOLETE code.
    // Depending on timing, the initiator's own handshake may complete just
    // before the close arrives: the responder sends its init record before
    // validating the initiator's.
    let closed = tokio::time::timeout(TEST_HANDSHAKE_TIMEOUT, outbound.closed())
        .await
        .expect("close arrives in time");
    match closed {
        quinn::ConnectionError::ApplicationClosed(closed) => {
            assert_eq!(
                u64::from(closed.error_code),
                ErrorCode::Obsolete.wire_code(),
            );
        }
        // The responder's close can outrace its init record: the initiator
        // then fails its handshake read and closes its side first.
        quinn::ConnectionError::LocallyClosed => {
            assert!(
                initiator_result.is_err(),
                "a locally-closed connection implies an initiator handshake error",
            );
        }
        other => panic!("expected an OBSOLETE application close, got: {other:?}"),
    }
}

#[tokio::test]
async fn handshake_detects_self_connection() {
    let _init_guard = zebra_test::init();

    let (outbound, inbound) = connected_pair().await;

    // Both sides share the same nonce set and use the same nonce, as when a
    // node connects to its own listener.
    let nonces = HandshakeNonces::default();
    let shared_nonce = Nonce(0xAAAA_BBBB_CCCC_DDDD);

    let mut initiator_params = test_params(MIN_V2_PROTOCOL_VERSION, shared_nonce);
    initiator_params.nonces = nonces.clone();
    let mut responder_params = test_params(MIN_V2_PROTOCOL_VERSION, shared_nonce);
    responder_params.nonces = nonces;

    let responder = tokio::spawn(async move {
        tokio::time::timeout(TEST_HANDSHAKE_TIMEOUT, respond(&inbound, &responder_params))
            .await
            .expect("handshake fails in time")
    });

    let initiator_result = tokio::time::timeout(
        TEST_HANDSHAKE_TIMEOUT,
        initiate(&outbound, &initiator_params),
    )
    .await
    .expect("handshake fails in time");
    let responder_result = responder.await.expect("responder task succeeds");

    // At least one side must detect the self-connection; the other may see
    // either the same detection or the resulting connection close.
    let self_connection_detected = matches!(
        initiator_result,
        Err(V2HandshakeError::SelfConnection) | Err(V2HandshakeError::LocalDuplicateNonce)
    ) || matches!(
        responder_result,
        Err(V2HandshakeError::SelfConnection) | Err(V2HandshakeError::LocalDuplicateNonce)
    );
    assert!(
        self_connection_detected,
        "self connection must be detected: initiator: {initiator_result:?}, \
         responder: {responder_result:?}",
    );
}

#[tokio::test]
async fn responder_refuses_streams_before_handshake() {
    let _init_guard = zebra_test::init();

    let (outbound, inbound) = connected_pair().await;

    let initiator_params = test_params(MIN_V2_PROTOCOL_VERSION, Nonce(1111));
    let responder_params = test_params(MIN_V2_PROTOCOL_VERSION, Nonce(2222));

    let responder = tokio::spawn(async move {
        tokio::time::timeout(TEST_HANDSHAKE_TIMEOUT, respond(&inbound, &responder_params))
            .await
            .expect("handshake completes in time")
    });

    // Open a request stream before the handshake stream: the responder
    // refuses it and still completes the handshake.
    let (mut early_send, _early_recv) = outbound.open_bi().await.expect("stream opens");
    early_send
        .write_all(&[crate::protocol::v2::types::StreamType::GetHeaders.byte()])
        .await
        .expect("stream write succeeds");

    let initiator_handshake = tokio::time::timeout(
        TEST_HANDSHAKE_TIMEOUT,
        initiate(&outbound, &initiator_params),
    )
    .await
    .expect("handshake completes in time")
    .expect("initiator handshake succeeds");

    let responder_handshake = responder
        .await
        .expect("responder task succeeds")
        .expect("responder handshake succeeds");

    assert_eq!(initiator_handshake.remote_init.nonce, Nonce(2222));
    assert_eq!(responder_handshake.remote_init.nonce, Nonce(1111));

    // The early stream is eventually stopped with the REFUSED error code.
    let stopped = tokio::time::timeout(TEST_HANDSHAKE_TIMEOUT, early_send.stopped())
        .await
        .expect("refusal arrives in time")
        .expect("stopped() resolves");
    assert_eq!(stopped.map(u64::from), Some(ErrorCode::Refused.wire_code()),);
}
