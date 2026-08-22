//! The QUIC transport for the version 2 Zcash P2P network protocol.
//!
//! The QUIC transport uses QUIC version 1 over UDP, secured with TLS 1.3.
//! Connections are encrypted but endpoints are not authenticated: the goal
//! is protection against passive network observers, not endpoint identity.
//!
//! - The network of a connection is identified by an ALPN protocol
//!   identifier, negotiated in the QUIC-TLS handshake; a connection between
//!   peers on different networks fails before any application data is
//!   exchanged.
//! - Each node presents a self-signed X.509 certificate for an ephemeral
//!   Ed25519 key, and accepts any certificate from its peer — for both
//!   Ed25519 and ECDSA P-256 keys: certificates are not required to chain to
//!   a certification authority, and connections are never rejected on the
//!   basis of certificate contents.
//! - RFC 7250 raw public keys are not accepted yet: rustls can only offer a
//!   single certificate type per endpoint (a client that requires raw public
//!   keys stops interoperating with X.509 peers), so accepting both
//!   encodings, as the draft requires, is blocked on the TLS ecosystem. See
//!   the spec feedback section of `SPEC-CONFORMANCE.md`.
//! - 0-RTT early data is never used: requests carried in 0-RTT data would be
//!   replayable by an attacker. Neither side enables TLS early data, so a
//!   peer cannot negotiate 0-RTT at all — an attempt is rejected in the TLS
//!   handshake, below the application layer.
//! - QUIC datagrams are not used: support is not advertised, and DATAGRAM
//!   frames are never sent.

use std::sync::Arc;

use quinn::{
    crypto::rustls::{HandshakeData, QuicClientConfig, QuicServerConfig},
    IdleTimeout, TransportConfig, VarInt,
};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider},
    pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
    DigitallySignedStruct, SignatureScheme,
};

use zebra_chain::parameters::Network;

use crate::BoxError;

use super::{
    constants::{
        ALPN_MAINNET, ALPN_REGTEST, ALPN_TESTNET, IDLE_TIMEOUT, KEEP_ALIVE_INTERVAL,
        MAX_RECORD_PAYLOAD_LEN, MIN_CONCURRENT_BIDI_STREAMS, MIN_CONCURRENT_UNI_STREAMS,
    },
    types::ErrorCode,
};

/// The maximum number of concurrent bidirectional streams the remote peer may
/// open: request streams, bounded well above the specified minimum.
pub const MAX_CONCURRENT_REMOTE_BIDI_STREAMS: u32 = 4 * MIN_CONCURRENT_BIDI_STREAMS;

/// The maximum number of concurrent unidirectional streams the remote peer
/// may open: the three announcement stream types, plus headroom for future
/// types, bounded above the specified minimum.
pub const MAX_CONCURRENT_REMOTE_UNI_STREAMS: u32 = 2 * MIN_CONCURRENT_UNI_STREAMS;

/// The TLS server name offered when dialing peers.
///
/// Peer certificates are not validated against any name (peers use ephemeral
/// self-signed certificates), so this value carries no meaning; rustls
/// requires one to be present.
pub const DUMMY_SERVER_NAME: &str = "zcash";

impl From<ErrorCode> for VarInt {
    fn from(code: ErrorCode) -> VarInt {
        VarInt::from_u64(code.wire_code())
            .expect("v2 protocol error codes are single bytes, which fit in a QUIC varint")
    }
}

/// Returns the ALPN protocol identifier for `network`.
//
// TODO: the draft ZIP only defines ALPN identifiers for Mainnet, Testnet,
//       and Regtest. Custom testnets currently share the Testnet identifier,
//       and rely on the genesis hash mismatch being detected after the
//       handshake, like the legacy protocol's shared Testnet network magic.
pub fn alpn_protocol(network: &Network) -> &'static [u8] {
    if network.is_regtest() {
        ALPN_REGTEST
    } else if matches!(network, Network::Testnet(_)) {
        ALPN_TESTNET
    } else {
        ALPN_MAINNET
    }
}

/// Generates an ephemeral Ed25519 key and a self-signed certificate for it.
///
/// A fresh key is generated for each endpoint, so nodes cannot be linked
/// across restarts by their TLS keys.
pub fn generate_ephemeral_identity(
) -> Result<(CertificateDer<'static>, PrivatePkcs8KeyDer<'static>), BoxError> {
    let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519)?;

    // The certificate contents are irrelevant: peers must not reject a
    // connection on the basis of certificate contents.
    let params = rcgen::CertificateParams::new(Vec::<String>::new())?;
    let cert = params.self_signed(&key_pair)?;

    Ok((
        cert.der().clone(),
        PrivatePkcs8KeyDer::from(key_pair.serialize_der()),
    ))
}

/// Returns the QUIC transport parameters for v2 protocol connections.
fn transport_config() -> Result<TransportConfig, BoxError> {
    let mut transport = TransportConfig::default();

    transport
        .max_idle_timeout(Some(IdleTimeout::try_from(IDLE_TIMEOUT)?))
        .keep_alive_interval(Some(KEEP_ALIVE_INTERVAL))
        .max_concurrent_bidi_streams(MAX_CONCURRENT_REMOTE_BIDI_STREAMS.into())
        .max_concurrent_uni_streams(MAX_CONCURRENT_REMOTE_UNI_STREAMS.into())
        // Never advertise QUIC datagram support.
        .datagram_receive_buffer_size(None)
        // Allow a maximum-size element (a 2 MiB block) plus framing to be in
        // flight on a single stream without flow control stalls.
        .stream_receive_window(VarInt::from_u32(2 * MAX_RECORD_PAYLOAD_LEN as u32))
        .receive_window(VarInt::from_u32(8 * MAX_RECORD_PAYLOAD_LEN as u32));

    Ok(transport)
}

/// Builds the QUIC server configuration for `network`.
///
/// The server presents `cert` and `key`, offers the network's ALPN
/// identifier, and does not request TLS client authentication.
pub fn server_config(
    network: &Network,
    cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
) -> Result<quinn::ServerConfig, BoxError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());

    let mut crypto = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key.into())?;
    crypto.alpn_protocols = vec![alpn_protocol(network).to_vec()];

    let mut config =
        quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(crypto)?));
    config.transport_config(Arc::new(transport_config()?));

    Ok(config)
}

/// Builds the QUIC client configuration for `network`.
///
/// The client accepts any server certificate (see
/// [`AcceptAnyServerCert`]), and offers the network's ALPN identifier.
pub fn client_config(network: &Network) -> Result<quinn::ClientConfig, BoxError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());

    let mut crypto = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert::new(&provider)))
        .with_no_client_auth();
    crypto.alpn_protocols = vec![alpn_protocol(network).to_vec()];

    let mut config = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(crypto)?));
    config.transport_config(Arc::new(transport_config()?));

    Ok(config)
}

/// Creates a QUIC endpoint for `network`, bound to `listen_addr`, that can
/// both accept inbound connections and dial outbound connections.
///
/// A fresh ephemeral TLS identity is generated for the endpoint.
pub fn new_endpoint(
    listen_addr: std::net::SocketAddr,
    network: &Network,
) -> Result<quinn::Endpoint, BoxError> {
    let (cert, key) = generate_ephemeral_identity()?;

    let mut endpoint = quinn::Endpoint::server(server_config(network, cert, key)?, listen_addr)?;
    endpoint.set_default_client_config(client_config(network)?);

    Ok(endpoint)
}

/// Dials a v2 protocol connection to `addr` from `endpoint`, and checks that
/// ALPN negotiation selected the identifier of `network`.
pub async fn connect(
    endpoint: &quinn::Endpoint,
    addr: std::net::SocketAddr,
    network: &Network,
) -> Result<quinn::Connection, BoxError> {
    let connection = endpoint.connect(addr, DUMMY_SERVER_NAME)?.await?;

    check_negotiated_alpn(&connection, network)?;

    Ok(connection)
}

/// Accepts a v2 protocol connection from `incoming`, and checks that ALPN
/// negotiation selected the identifier of `network`.
pub async fn accept(
    incoming: quinn::Incoming,
    network: &Network,
) -> Result<quinn::Connection, BoxError> {
    let connection = incoming.await?;

    check_negotiated_alpn(&connection, network)?;

    Ok(connection)
}

/// Checks that ALPN negotiation on `connection` selected the identifier of
/// `network`, closing the connection if it did not.
///
/// The TLS handshake fails on its own when the ALPN identifiers offered by
/// the two endpoints do not overlap; this check enforces the requirement
/// that a node must not complete a connection on which ALPN negotiation did
/// not select its network's identifier, even against a peer that skipped
/// ALPN altogether.
fn check_negotiated_alpn(
    connection: &quinn::Connection,
    network: &Network,
) -> Result<(), BoxError> {
    let negotiated = connection
        .handshake_data()
        .and_then(|data| data.downcast::<HandshakeData>().ok())
        .and_then(|data| data.protocol);

    if negotiated.as_deref() != Some(alpn_protocol(network)) {
        connection.close(ErrorCode::ProtocolError.into(), b"ALPN mismatch");
        return Err(format!(
            "connection to {} did not negotiate the {} ALPN identifier",
            connection.remote_address(),
            String::from_utf8_lossy(alpn_protocol(network)),
        )
        .into());
    }

    Ok(())
}

/// A TLS certificate verifier that accepts any certificate, verifying only
/// the TLS 1.3 handshake signature.
///
/// The v2 protocol encrypts connections but deliberately does not
/// authenticate endpoints: a node must not require the peer's certificate to
/// chain to any certification authority, and must not reject a connection on
/// the basis of certificate contents. The handshake signature is still
/// verified against the presented certificate's public key, which binds the
/// TLS channel keys to the certificate.
#[derive(Debug)]
pub struct AcceptAnyServerCert {
    /// The signature verification algorithms of the TLS crypto provider.
    algorithms: rustls::crypto::WebPkiSupportedAlgorithms,
}

impl AcceptAnyServerCert {
    /// Creates a verifier using `provider`'s signature verification
    /// algorithms.
    pub fn new(provider: &CryptoProvider) -> Self {
        AcceptAnyServerCert {
            algorithms: provider.signature_verification_algorithms,
        }
    }
}

impl ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // QUIC only uses TLS 1.3, so this method is never called.
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests;
