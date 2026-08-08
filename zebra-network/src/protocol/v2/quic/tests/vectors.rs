//! QUIC transport tests: local endpoint pairs over loopback UDP.

use zebra_chain::parameters::Network;

use crate::protocol::v2::{
    constants::{ALPN_MAINNET, ALPN_REGTEST, ALPN_TESTNET},
    quic::{alpn_protocol, connect, generate_ephemeral_identity, new_endpoint, DUMMY_SERVER_NAME},
    types::ErrorCode,
};

/// The localhost address with an OS-allocated port.
const LOCALHOST_UNSPECIFIED_PORT: &str = "127.0.0.1:0";

fn local_endpoint(network: &Network) -> quinn::Endpoint {
    new_endpoint(
        LOCALHOST_UNSPECIFIED_PORT.parse().expect("valid address"),
        network,
    )
    .expect("endpoint creation succeeds")
}

#[test]
fn alpn_identifiers_match_networks() {
    let _init_guard = zebra_test::init();

    assert_eq!(alpn_protocol(&Network::Mainnet), ALPN_MAINNET);
    assert_eq!(alpn_protocol(&Network::new_default_testnet()), ALPN_TESTNET,);
    assert_eq!(
        alpn_protocol(&Network::new_regtest(Default::default())),
        ALPN_REGTEST
    );
}

#[test]
fn ephemeral_identities_are_unique() {
    let _init_guard = zebra_test::init();

    let (cert_a, key_a) = generate_ephemeral_identity().expect("identity generation succeeds");
    let (cert_b, key_b) = generate_ephemeral_identity().expect("identity generation succeeds");

    assert_ne!(cert_a, cert_b, "ephemeral certificates must be unique");
    assert_ne!(
        key_a.secret_pkcs8_der(),
        key_b.secret_pkcs8_der(),
        "ephemeral keys must be unique",
    );
}

/// Two endpoints on the same network connect, negotiate the network's ALPN
/// identifier, accept each other's self-signed certificates, and can
/// exchange stream data in both directions.
#[tokio::test]
async fn connect_same_network() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let server = local_endpoint(&network);
    let client = local_endpoint(&network);
    let server_addr = server.local_addr().expect("endpoint has a local address");

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.expect("endpoint accepts");
        let connection = crate::protocol::v2::quic::accept(incoming, &Network::Mainnet)
            .await
            .expect("inbound connection completes");

        let (mut send, mut recv) = connection.accept_bi().await.expect("stream accepted");
        let mut buf = [0u8; 5];
        tokio::io::AsyncReadExt::read_exact(&mut recv, &mut buf)
            .await
            .expect("stream data arrives");
        assert_eq!(&buf, b"hello");

        send.write_all(b"world")
            .await
            .expect("stream write succeeds");
        send.finish().expect("stream finishes");
        // Keep the connection alive until the client has read the response.
        connection.closed().await;
    });

    let connection = connect(&client, server_addr, &network)
        .await
        .expect("outbound connection completes");

    let (mut send, mut recv) = connection.open_bi().await.expect("stream opens");
    send.write_all(b"hello")
        .await
        .expect("stream write succeeds");
    send.finish().expect("stream finishes");

    let response = recv.read_to_end(1024).await.expect("response arrives");
    assert_eq!(response, b"world");

    connection.close(ErrorCode::NoError.into(), b"");
    server_task.await.expect("server task succeeds");
}

/// A peer presenting an ECDSA P-256 certificate is accepted: a node must be
/// prepared to accept both Ed25519 and ECDSA P-256 keys from its peers.
///
/// (Ed25519 acceptance is covered by `connect_same_network`: this node's own
/// certificates use Ed25519 keys.)
#[tokio::test]
async fn p256_certificates_are_accepted() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;

    // A server whose TLS identity is an ECDSA P-256 key.
    let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .expect("P-256 key generation succeeds");
    let params =
        rcgen::CertificateParams::new(Vec::<String>::new()).expect("empty params are valid");
    let cert = params
        .self_signed(&key_pair)
        .expect("self-signing succeeds");

    let server_config = crate::protocol::v2::quic::server_config(
        &network,
        cert.der().clone(),
        rustls::pki_types::PrivatePkcs8KeyDer::from(key_pair.serialize_der()),
    )
    .expect("server config builds with a P-256 identity");
    let server = quinn::Endpoint::server(
        server_config,
        LOCALHOST_UNSPECIFIED_PORT.parse().expect("valid address"),
    )
    .expect("endpoint creation succeeds");
    let server_addr = server.local_addr().expect("endpoint has a local address");

    let client = local_endpoint(&network);

    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.expect("endpoint accepts");
        let connection = crate::protocol::v2::quic::accept(incoming, &Network::Mainnet)
            .await
            .expect("inbound connection completes");

        let (mut send, _recv) = connection.accept_bi().await.expect("stream accepted");
        send.write_all(b"ack").await.expect("stream write succeeds");
        send.finish().expect("stream finishes");
        connection.closed().await;
    });

    let connection = connect(&client, server_addr, &network)
        .await
        .expect("a P-256 server certificate is accepted");

    let (mut send, mut recv) = connection.open_bi().await.expect("stream opens");
    send.finish().expect("stream finishes");
    let response = recv.read_to_end(16).await.expect("response arrives");
    assert_eq!(response, b"ack");

    connection.close(ErrorCode::NoError.into(), b"");
    server_task.await.expect("server task succeeds");
}

/// 0-RTT is never available on v2 endpoints: neither side enables TLS early
/// data, so even a client resuming a cached session cannot enter 0-RTT, and
/// requests are never carried in replayable early data.
#[tokio::test]
async fn zero_rtt_is_never_available() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let server = local_endpoint(&network);
    let client = local_endpoint(&network);
    let server_addr = server.local_addr().expect("endpoint has a local address");

    let server_task = tokio::spawn(async move {
        for _ in 0..2 {
            let incoming = server.accept().await.expect("endpoint accepts");
            let _connection = crate::protocol::v2::quic::accept(incoming, &Network::Mainnet)
                .await
                .expect("inbound connection completes");
        }
    });

    // Complete a first handshake, and give the client time to cache any
    // session tickets the server sends after it.
    let first = connect(&client, server_addr, &network)
        .await
        .expect("outbound connection completes");
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // A second connection must not offer 0-RTT, with or without a cached
    // session: early data is disabled on both sides.
    let connecting = client
        .connect(server_addr, DUMMY_SERVER_NAME)
        .expect("connect starts");
    let connecting = match connecting.into_0rtt() {
        Err(connecting) => connecting,
        Ok(_) => panic!("0-RTT must never be available on v2 endpoints"),
    };
    let second = connecting.await.expect("handshake completes");

    first.close(ErrorCode::NoError.into(), b"");
    second.close(ErrorCode::NoError.into(), b"");
    server_task.await.expect("server task succeeds");
}

/// Endpoints on different networks fail to connect: ALPN negotiation fails
/// in the TLS handshake, before any application data is exchanged.
#[tokio::test]
async fn connect_different_networks_fails() {
    let _init_guard = zebra_test::init();

    let server = local_endpoint(&Network::Mainnet);
    let client = local_endpoint(&Network::new_default_testnet());
    let server_addr = server.local_addr().expect("endpoint has a local address");

    // The server driving its accept loop lets the TLS handshake proceed far
    // enough to fail; it never yields a completed connection.
    let server_task = tokio::spawn(async move {
        while let Some(incoming) = server.accept().await {
            let _ = crate::protocol::v2::quic::accept(incoming, &Network::Mainnet).await;
        }
    });

    let result = connect(&client, server_addr, &Network::new_default_testnet()).await;
    assert!(
        result.is_err(),
        "connections between different networks must fail: {result:?}",
    );

    server_task.abort();
}
