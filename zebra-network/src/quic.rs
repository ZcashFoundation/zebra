//! The QUIC transport for the version 2 protocol (draft, "QUIC Transport").
//!
//! QUIC version 1 over UDP, secured with TLS 1.3. The network is selected by
//! ALPN, endpoints present self-signed certificates that peers accept
//! without any authority, 0-RTT and session resumption are off, datagrams
//! are not advertised, and the transport parameters meet the draft's stream
//! and idle-timeout minimums.
//!
//! # Known gaps against the draft
//!
//! These follow from what rustls and quinn expose, and are noted for spec
//! feedback:
//!
//! - A peer that attempts 0-RTT is not closed with `PROTOCOL_ERROR`: the
//!   server never accepts early data, so the attempt is simply ignored and a
//!   full handshake follows. Neither library reports the attempt.
//! - Certificate parsing is stricter than "MUST NOT reject on contents": a
//!   certificate that is not X.509 v3, or that carries an unknown critical
//!   extension, is rejected before the signature is checked.
//! - rustls can present or require raw public keys (RFC 7250) but cannot
//!   accept both encodings from a peer on one connection, so this node
//!   presents and accepts X.509 only; a peer presenting a raw public key
//!   fails the TLS handshake.

use std::{io, net::SocketAddr, sync::Arc, time::Duration};

use quinn::{
    crypto::rustls::{QuicClientConfig, QuicServerConfig},
    ClientConfig, Connection, Endpoint, EndpointConfig, IdleTimeout, ServerConfig, TokioRuntime,
    TransportConfig, VarInt,
};
use thiserror::Error;

use zebra_chain::parameters::Network;

use crate::{
    constants::HANDSHAKE_TIMEOUT,
    protocol::v2::{
        constants::{
            alpn_protocol_id, MAX_RECORD_PAYLOAD_LEN, MIN_BIDIRECTIONAL_STREAMS, MIN_IDLE_TIMEOUT,
            MIN_UNIDIRECTIONAL_STREAMS,
        },
        types::ErrorCode,
    },
};

mod tls;

#[cfg(test)]
mod tests;

/// Bidirectional streams a peer may have open at once: request streams plus
/// the handshake stream; twice the draft's minimum, which it treats as the
/// per-peer bound on outstanding requests (draft, "Transport Requirements").
const MAX_CONCURRENT_BIDI_STREAMS: u32 = 2 * MIN_BIDIRECTIONAL_STREAMS;

/// Unidirectional streams a peer may have open at once: one per announcement
/// type, with headroom for future types; twice the draft's minimum (draft,
/// "Transport Requirements").
const MAX_CONCURRENT_UNI_STREAMS: u32 = 2 * MIN_UNIDIRECTIONAL_STREAMS;

/// How often an idle connection sends a QUIC PING (draft, "Connection
/// Management"): well under `MIN_IDLE_TIMEOUT`, so a few lost pings do not
/// drop a healthy connection.
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// The per-stream receive window, in bytes: two maximum-size records, so a
/// block announcement or a `get-blocks` entry never stalls on flow control
/// alone; the draft's 16 MiB bulk recommendation waits for the bulk stream types.
// `MAX_RECORD_PAYLOAD_LEN` is 2 MiB, so the product fits in a `u32`; checked below.
const STREAM_RECEIVE_WINDOW: u32 = (2 * MAX_RECORD_PAYLOAD_LEN) as u32;

/// The per-connection receive window, in bytes: two streams, so at most 8 MiB
/// buffered per peer, or about 790 MiB if every one of the default 99 version 2
/// connections filled its window; the draft's 64 MiB bulk recommendation waits
/// for the bulk stream types.
const CONNECTION_RECEIVE_WINDOW: u32 = 2 * STREAM_RECEIVE_WINDOW;

// The parameters above meet the draft's minimums; checked at compile time.
const _: () = {
    assert!(KEEP_ALIVE_INTERVAL.as_millis() * 2 <= MIN_IDLE_TIMEOUT.as_millis());
    // Guards the truncating `as u32` cast in `STREAM_RECEIVE_WINDOW`'s definition.
    // `STREAM_RECEIVE_WINDOW` is a `u32`, which always fits in a `usize` on supported targets.
    assert!(STREAM_RECEIVE_WINDOW as usize >= MAX_RECORD_PAYLOAD_LEN);
};

/// An error building or binding a QUIC endpoint.
#[derive(Error, Debug)]
pub enum EndpointError {
    /// The draft assigns no ALPN identifier to this network.
    #[error("the version 2 protocol has no ALPN identifier for {network}; QUIC is unavailable")]
    NoAlpnIdentifier {
        /// The network.
        network: Network,
    },

    /// Generating the self-signed certificate failed.
    #[error("could not generate a self-signed certificate: {0}")]
    Certificate(#[from] rcgen::Error),

    /// rustls rejected the configuration.
    #[error("TLS configuration error: {0}")]
    Tls(#[from] rustls::Error),

    /// rustls produced a configuration QUIC cannot use.
    #[error("TLS configuration is not usable for QUIC: {0}")]
    NoQuicCipherSuite(#[from] quinn::crypto::rustls::NoInitialCipherSuite),

    /// Binding the UDP socket, or registering it with the Tokio runtime,
    /// failed.
    #[error("could not bind the QUIC endpoint: {0}")]
    Bind(#[source] io::Error),
}

/// An error opening a QUIC connection.
#[derive(Error, Debug)]
pub enum ConnectError {
    /// The connection could not be started.
    #[error("could not start QUIC connection: {0}")]
    Start(#[from] quinn::ConnectError),

    /// The QUIC handshake failed, including on an ALPN mismatch.
    #[error("QUIC handshake failed: {0}")]
    Handshake(#[from] quinn::ConnectionError),

    /// The QUIC handshake did not complete within [`HANDSHAKE_TIMEOUT`].
    #[error("QUIC handshake timed out")]
    Timeout,
}

/// The transport parameters this node uses for every connection.
fn transport_config() -> TransportConfig {
    let mut config = TransportConfig::default();
    config
        .max_concurrent_bidi_streams(VarInt::from_u32(MAX_CONCURRENT_BIDI_STREAMS))
        .max_concurrent_uni_streams(VarInt::from_u32(MAX_CONCURRENT_UNI_STREAMS))
        .max_idle_timeout(Some(
            IdleTimeout::try_from(MIN_IDLE_TIMEOUT)
                .expect("the idle timeout is far below the VarInt limit"),
        ))
        .keep_alive_interval(Some(KEEP_ALIVE_INTERVAL))
        .stream_receive_window(VarInt::from_u32(STREAM_RECEIVE_WINDOW))
        .receive_window(VarInt::from_u32(CONNECTION_RECEIVE_WINDOW))
        // The draft does not use datagrams: do not advertise them, and buffer none.
        .datagram_receive_buffer_size(None)
        .datagram_send_buffer_size(0)
        // The spin bit lets on-path observers measure round-trip times; nothing here needs it.
        .allow_spin(false);
    config
}

/// A server configuration offering `alpn`, presenting a fresh self-signed
/// certificate for an `algorithm` key.
fn server_config(
    alpn: &'static [u8],
    algorithm: &'static rcgen::SignatureAlgorithm,
) -> Result<ServerConfig, EndpointError> {
    let crypto = tls::server_config(alpn, algorithm)?;
    let crypto = QuicServerConfig::try_from(crypto)?;

    let mut config = ServerConfig::with_crypto(Arc::new(crypto));
    config.transport_config(Arc::new(transport_config()));
    Ok(config)
}

/// A client configuration offering `alpn`, accepting any peer certificate.
fn client_config(alpn: &'static [u8]) -> Result<ClientConfig, EndpointError> {
    let crypto = tls::client_config(alpn)?;
    let crypto = QuicClientConfig::try_from(crypto)?;

    let mut config = ClientConfig::new(Arc::new(crypto));
    config.transport_config(Arc::new(transport_config()));
    Ok(config)
}

/// Binds an endpoint for `network` on `addr`; must be called inside a Tokio
/// runtime, which drives the socket. The server identity is generated here
/// and lives as long as the endpoint.
///
/// The network is fixed when the endpoint is bound, so every connection it
/// opens or accepts negotiates the same ALPN identifier, and rustls refuses
/// any other (draft, "Network Identification").
///
/// The endpoint's address family is `addr`'s: an endpoint bound to an IPv4
/// address cannot dial IPv6 peers, and [`connect`] fails at once for them.
///
/// # Panics
///
/// If called outside a Tokio runtime: tokio panics when the socket is
/// registered with the runtime.
pub fn bind(network: &Network, addr: SocketAddr) -> Result<Endpoint, EndpointError> {
    let alpn = alpn_protocol_id(network).ok_or_else(|| EndpointError::NoAlpnIdentifier {
        network: network.clone(),
    })?;
    let server = server_config(alpn, &rcgen::PKCS_ED25519)?;
    let client = client_config(alpn)?;

    let socket = std::net::UdpSocket::bind(addr).map_err(EndpointError::Bind)?;
    let mut endpoint_config = EndpointConfig::default();
    // QUIC version 1 only (RFC 9000); quinn's default also accepts the pre-standard draft versions.
    endpoint_config.supported_versions(vec![0x0000_0001]);
    let mut endpoint = Endpoint::new(
        endpoint_config,
        Some(server),
        socket,
        Arc::new(TokioRuntime),
    )
    .map_err(EndpointError::Bind)?;
    endpoint.set_default_client_config(client);
    Ok(endpoint)
}

/// Opens a connection to `addr`, bounded by [`HANDSHAKE_TIMEOUT`] like the
/// legacy connect (an unreachable address must not hold the caller until the
/// idle timeout).
///
/// No server name is sent: peers are addressed by IP and certificates are
/// not checked against names. Always a full handshake, never 0-RTT or
/// resumption.
pub async fn connect(endpoint: &Endpoint, addr: SocketAddr) -> Result<Connection, ConnectError> {
    // `connect` requires a textual server name; an IP address literal is sent as no SNI.
    let server_name = addr.ip().to_string();
    let connecting = endpoint.connect(addr, &server_name)?;
    let connection = tokio::time::timeout(HANDSHAKE_TIMEOUT, connecting)
        .await
        .map_err(|_elapsed| ConnectError::Timeout)??;
    Ok(connection)
}

/// The QUIC application error code for `code`.
pub fn app_error_code(code: ErrorCode) -> VarInt {
    VarInt::from_u64(code.code()).expect(
        "the draft's error codes are all below 0x40, \
         which fits in any QUIC variable-length integer",
    )
}
