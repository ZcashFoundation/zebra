//! Loopback tests for the QUIC transport.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use quinn::{crypto::rustls::QuicServerConfig, Connection, Endpoint, VarInt};
use rustls::{
    crypto::ring::sign::any_ecdsa_type,
    sign::{CertifiedKey, Signer, SigningKey, SingleCertAndKey},
    version::TLS13,
    SignatureAlgorithm, SignatureScheme,
};

use zebra_chain::parameters::{testnet, Network};

use crate::protocol::external::types::PeerServices;

use super::{
    bind, client_endpoint, connect, tls, transport_config, ConnectError, EndpointError, Windows,
};

/// Every loopback exchange finishes in well under this.
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The loopback address with an ephemeral port.
fn loopback() -> SocketAddr {
    "127.0.0.1:0".parse().expect("valid socket address")
}

/// Binds an endpoint for `network` on loopback.
fn bind_loopback(network: &Network) -> Endpoint {
    bind(network, loopback()).expect("loopback endpoints bind")
}

/// Accepts one connection on `endpoint`.
///
/// A handshake that already failed (for example on ALPN) is reported by
/// `accept` itself, before the connection future.
async fn accept_one(endpoint: &Endpoint) -> Result<Connection, quinn::ConnectionError> {
    endpoint
        .accept()
        .await
        .expect("the endpoint is open")
        .accept()?
        .await
}

/// Neither configuration permits 0-RTT. These are the rustls defaults; the
/// draft makes them a MUST, so this test pins them.
/// Each connection's send buffer is capped at the [`Windows`] send window,
/// not quinn's larger default.
#[test]
fn transport_config_caps_the_send_window() {
    let config = format!("{:?}", transport_config(Windows::DEFAULT));
    let send_window = Windows::DEFAULT.send;
    assert!(
        config.contains(&format!("send_window: {send_window},")),
        "got {config}"
    );
}

/// A node that advertises `NODE_SYNC_ARTIFACTS` gets the serving windows: an
/// 8 MiB send window and the default receive windows. Any other node gets
/// the defaults.
#[test]
fn serving_nodes_get_a_larger_send_window() {
    let serving = Windows::for_services(
        PeerServices::NODE_NETWORK_LIMITED | PeerServices::NODE_SYNC_ARTIFACTS,
    );
    assert_eq!(serving, Windows::SERVING);
    assert_eq!(serving.send, 8 << 20);
    assert_eq!(serving.stream_receive, Windows::DEFAULT.stream_receive);
    assert_eq!(
        serving.connection_receive,
        Windows::DEFAULT.connection_receive
    );
    assert_ne!(serving.send, Windows::DEFAULT.send);

    assert_eq!(
        Windows::for_services(PeerServices::NODE_NETWORK),
        Windows::DEFAULT
    );
    assert_eq!(
        Windows::for_services(PeerServices::empty()),
        Windows::DEFAULT
    );

    // The window reaches the transport configuration.
    let config = format!("{:?}", transport_config(Windows::SERVING));
    assert!(
        config.contains(&format!("send_window: {},", Windows::SERVING.send)),
        "got {config}"
    );
}

/// The bootstrap's windows widen only the receive side, and reach the
/// transport configuration.
#[test]
fn bootstrap_windows_widen_the_receive_side() {
    assert_eq!(Windows::BOOTSTRAP.stream_receive, 8 << 20);
    assert_eq!(Windows::BOOTSTRAP.connection_receive, 16 << 20);
    assert_eq!(Windows::BOOTSTRAP.send, Windows::DEFAULT.send);

    let config = format!("{:?}", transport_config(Windows::BOOTSTRAP));
    assert!(
        config.contains(&format!(
            "stream_receive_window: {},",
            Windows::BOOTSTRAP.stream_receive
        )) && config.contains(&format!(
            "receive_window: {},",
            Windows::BOOTSTRAP.connection_receive
        )),
        "got {config}"
    );
}

/// A client-only endpoint dials peers like a bound one, but accepts no
/// connections: a peer that dials it gets no handshake and times out.
#[tokio::test]
async fn client_endpoints_dial_but_do_not_accept() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let server = bind_loopback(&network);
        let server_addr = server.local_addr().expect("bound");
        let client = client_endpoint(&network, loopback(), Windows::BOOTSTRAP)
            .expect("client-only loopback endpoints bind");
        let client_addr = client.local_addr().expect("bound");

        let (outbound, inbound) = tokio::join!(connect(&client, server_addr), accept_one(&server));
        outbound.expect("a client-only endpoint connects");
        inbound.expect("the server accepts");

        let error = connect(&server, client_addr)
            .await
            .expect_err("a client-only endpoint accepts no connections");
        assert!(
            matches!(error, ConnectError::Timeout | ConnectError::Handshake(_)),
            "got {error:?}"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

#[test]
fn configurations_disable_early_data() {
    let server = tls::server_config(b"zcash/main", &rcgen::PKCS_ED25519).expect("valid");
    assert_eq!(server.max_early_data_size, 0);
    assert_eq!(server.alpn_protocols, vec![b"zcash/main".to_vec()]);

    let client = tls::client_config(b"zcash/main").expect("valid");
    assert!(!client.enable_early_data);
    assert_eq!(client.alpn_protocols, vec![b"zcash/main".to_vec()]);
}

/// A custom testnet has no ALPN identifier, so no endpoint.
#[test]
fn custom_testnet_has_no_endpoint() {
    let network = testnet::Parameters::build()
        .with_network_name("v2quic")
        .expect("valid network name")
        .to_network()
        .expect("valid custom testnet parameters");
    let error = bind(&network, loopback()).expect_err("no ALPN identifier");
    assert!(
        matches!(error, EndpointError::NoAlpnIdentifier { .. }),
        "got {error:?}"
    );
}

/// Endpoints on different networks fail to connect: ALPN negotiation
/// selects nothing and the handshake is rejected on both sides.
#[tokio::test]
async fn alpn_mismatch_rejects_the_connection() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(TEST_TIMEOUT, async {
        let testnet = Network::new_default_testnet();
        let server = bind_loopback(&Network::Mainnet);
        let client = bind_loopback(&testnet);
        let server_addr = server.local_addr().expect("bound");

        let (outbound, inbound) = tokio::join!(connect(&client, server_addr), accept_one(&server));

        let error = outbound.expect_err("the client must not connect across networks");
        assert!(matches!(error, ConnectError::Handshake(_)), "got {error:?}");
        assert!(
            inbound.is_err(),
            "the server must not accept across networks"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A peer presenting an ECDSA P-256 certificate is accepted, as the draft
/// requires, not only an Ed25519 one.
#[tokio::test]
async fn ecdsa_p256_peer_certificates_are_accepted() {
    let _init_guard = zebra_test::init();
    let network = Network::new_regtest(Default::default());

    tokio::time::timeout(TEST_TIMEOUT, async {
        let server = Endpoint::server(
            super::server_config(
                b"zcash/regtest",
                &rcgen::PKCS_ECDSA_P256_SHA256,
                Windows::DEFAULT,
            )
            .expect("valid"),
            loopback(),
        )
        .expect("binds");
        let server_addr = server.local_addr().expect("bound");

        let client = bind_loopback(&network);
        let (outbound, inbound) = tokio::join!(connect(&client, server_addr), accept_one(&server));
        outbound.expect("an ECDSA peer is accepted");
        inbound.expect("the server accepts");
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A peer whose key can only sign with a scheme the client did not offer
/// (outside the draft's Ed25519 and ECDSA P-256) cannot complete the
/// handshake: a conforming TLS server finds no scheme in common and aborts
/// before signing.
#[tokio::test]
async fn other_signature_schemes_are_rejected() {
    let _init_guard = zebra_test::init();
    let network = Network::new_regtest(Default::default());

    tokio::time::timeout(TEST_TIMEOUT, async {
        let server = Endpoint::server(
            super::server_config(
                b"zcash/regtest",
                &rcgen::PKCS_ECDSA_P384_SHA384,
                Windows::DEFAULT,
            )
            .expect("valid"),
            loopback(),
        )
        .expect("binds");
        let server_addr = server.local_addr().expect("bound");

        let client = bind_loopback(&network);
        let (outbound, inbound) = tokio::join!(connect(&client, server_addr), accept_one(&server));
        let error = outbound.expect_err("a P-384 peer is rejected");
        assert!(matches!(error, ConnectError::Handshake(_)), "got {error:?}");
        assert!(
            inbound.is_err(),
            "the server does not complete the handshake"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// A signing key that ignores the schemes the client offered and always
/// signs with ECDSA P-384, like a non-conforming TLS server.
#[derive(Debug)]
struct IgnoresOfferedSchemes(Arc<dyn SigningKey>);

impl SigningKey for IgnoresOfferedSchemes {
    fn choose_scheme(&self, _offered: &[SignatureScheme]) -> Option<Box<dyn Signer>> {
        self.0
            .choose_scheme(&[SignatureScheme::ECDSA_NISTP384_SHA384])
    }

    fn algorithm(&self) -> SignatureAlgorithm {
        self.0.algorithm()
    }
}

/// A server that signs the handshake with a scheme the client did not
/// advertise is rejected by the client's certificate verifier, even though
/// the signature itself is valid and rustls could verify it.
#[tokio::test]
async fn unadvertised_signature_schemes_are_rejected() {
    let _init_guard = zebra_test::init();
    let network = Network::new_regtest(Default::default());

    tokio::time::timeout(TEST_TIMEOUT, async {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384).expect("valid");
        let certificate = rcgen::CertificateParams::new(Vec::<String>::new())
            .expect("valid")
            .self_signed(&key)
            .expect("valid");
        let signing_key = any_ecdsa_type(&key.into()).expect("rustls supports P-384 keys");
        let certified_key = CertifiedKey::new(
            vec![certificate.into()],
            Arc::new(IgnoresOfferedSchemes(signing_key)),
        );

        let mut crypto = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&TLS13])
        .expect("TLS 1.3 is supported")
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(SingleCertAndKey::from(certified_key)));
        crypto.alpn_protocols = vec![b"zcash/regtest".to_vec()];
        let crypto = QuicServerConfig::try_from(crypto).expect("TLS 1.3 has a QUIC cipher suite");
        let server = Endpoint::server(
            quinn::ServerConfig::with_crypto(Arc::new(crypto)),
            loopback(),
        )
        .expect("binds");
        let server_addr = server.local_addr().expect("bound");

        let client = bind_loopback(&network);
        let (outbound, _inbound) = tokio::join!(connect(&client, server_addr), accept_one(&server));
        let error = outbound.expect_err("an unadvertised signature scheme is rejected");
        assert!(
            matches!(
                &error,
                ConnectError::Handshake(quinn::ConnectionError::TransportError(transport))
                    if transport.reason.contains("SignedHandshakeWithUnadvertisedSigScheme")
            ),
            "got {error:?}"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// An endpoint bound to an IPv4 address cannot dial an IPv6 peer, and says so
/// at once rather than after a timeout.
#[tokio::test]
async fn ipv4_endpoints_cannot_dial_ipv6_peers() {
    let _init_guard = zebra_test::init();

    let client = bind_loopback(&Network::Mainnet);
    let ipv6_peer: SocketAddr = "[::1]:8233".parse().expect("valid socket address");
    let error = tokio::time::timeout(TEST_TIMEOUT, connect(&client, ipv6_peer))
        .await
        .expect("the attempt fails at once")
        .expect_err("an IPv4 endpoint cannot send to an IPv6 address");
    assert!(
        matches!(
            error,
            ConnectError::Start(quinn::ConnectError::InvalidRemoteAddress(addr)) if addr == ipv6_peer
        ),
        "got {error:?}"
    );
}

/// Binding outside a Tokio runtime panics rather than returning an error.
#[test]
#[should_panic(expected = "Tokio 1.x runtime")]
fn bind_outside_a_runtime_panics() {
    let _ = bind(&Network::Mainnet, loopback());
}

/// Neither a fresh client nor one that has already completed a handshake
/// with the server can attempt 0-RTT: resumption is disabled, so no session
/// ticket is ever cached.
#[tokio::test]
async fn zero_rtt_is_never_attempted() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    tokio::time::timeout(TEST_TIMEOUT, async {
        let server = bind_loopback(&network);
        let client = bind_loopback(&network);
        let server_addr = server.local_addr().expect("bound");

        // First connection: a full handshake, which would normally seed the session cache.
        let (first, accepted) = tokio::join!(connect(&client, server_addr), accept_one(&server));
        let first = first.expect("the first connection completes");
        accepted.expect("the server accepts");
        first.close(VarInt::from_u32(0), b"done");

        // Second connection from the same client endpoint: still no 0-RTT available.
        let connecting = client
            .connect(server_addr, &server_addr.ip().to_string())
            .expect("connect starts");
        let connecting = connecting
            .into_0rtt()
            .err()
            .expect("0-RTT must not be available even after a previous handshake");

        let (second, accepted) = tokio::join!(connecting, accept_one(&server));
        second.expect("the full handshake completes");
        accepted.expect("the server accepts");
    })
    .await
    .expect("the test completes well within the timeout");
}
