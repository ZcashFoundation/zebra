//! TLS configuration for the QUIC transport (draft, "Certificates").
//!
//! Connections are encrypted but endpoints are not authenticated. This node
//! presents a self-signed certificate for a fresh Ed25519 key, and accepts
//! any certificate its peer presents, for an Ed25519 or ECDSA P-256 key,
//! without checking names, expiry, extensions, or a chain to an authority.
//! The handshake signature is still verified against the presented key, so
//! the channel is bound to whoever holds it.
//!
//! Session resumption is off on both sides, so every connection runs a full
//! handshake with a certificate and a verified signature.

use std::sync::Arc;

use rustls::{
    client::{
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        Resumption,
    },
    crypto::{ring, verify_tls13_signature, CryptoProvider},
    pki_types::{CertificateDer, ServerName, UnixTime},
    server::NoServerSessionStorage,
    version::TLS13,
    ClientConfig, DigitallySignedStruct, Error as RustlsError, PeerIncompatible, PeerMisbehaved,
    ServerConfig, SignatureScheme,
};

use super::EndpointError;

/// The crypto provider every configuration uses: ring, which the rest of the
/// workspace already links.
fn provider() -> Arc<CryptoProvider> {
    Arc::new(ring::default_provider())
}

/// The signature schemes a peer's certificate may use: the two key types
/// the draft names (draft, "Certificates").
const ACCEPTED_SIGNATURE_SCHEMES: [SignatureScheme; 2] = [
    SignatureScheme::ED25519,
    SignatureScheme::ECDSA_NISTP256_SHA256,
];

/// A server configuration that presents a fresh self-signed certificate for
/// an `algorithm` key, offers only `alpn`, and requests no client certificate.
pub(super) fn server_config(
    alpn: &[u8],
    algorithm: &'static rcgen::SignatureAlgorithm,
) -> Result<ServerConfig, EndpointError> {
    let key = rcgen::KeyPair::generate_for(algorithm)?;
    // No subject names: peers do not check them (draft, "Certificates").
    let certificate = rcgen::CertificateParams::new(Vec::<String>::new())?.self_signed(&key)?;

    let mut config = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![certificate.into()], key.into())?;
    config.alpn_protocols = vec![alpn.to_vec()];
    // No tickets, so no resumption and no 0-RTT: every connection presents a certificate.
    config.session_storage = Arc::new(NoServerSessionStorage {});
    config.send_tls13_tickets = 0;
    Ok(config)
}

/// A client configuration that offers only `alpn`, accepts any server
/// certificate, and sends no client certificate.
pub(super) fn client_config(alpn: &[u8]) -> Result<ClientConfig, EndpointError> {
    let provider = provider();
    let mut config = ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCertificate { provider }))
        .with_no_client_auth();
    config.alpn_protocols = vec![alpn.to_vec()];
    // No resumption, so no 0-RTT: a resumed session would skip the certificate and its signature.
    config.resumption = Resumption::disabled();
    Ok(config)
}

/// The draft's trust model: any Ed25519 or P-256 end-entity certificate is accepted with no
/// authority, name, or validity checks, and only the handshake signature is verified against it.
/// The explicit scheme check is load-bearing: rustls's helper only rejects schemes the provider lacks.
#[derive(Debug)]
struct AcceptAnyCertificate {
    /// Supplies the signature verification algorithms.
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for AcceptAnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        // QUIC requires TLS 1.3, and the configurations only enable TLS 1.3.
        Err(RustlsError::PeerIncompatible(
            PeerIncompatible::Tls12NotOfferedOrEnabled,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        // The provider knows more schemes than this node advertises; hold the peer to the
        // advertised ones.
        if !ACCEPTED_SIGNATURE_SCHEMES.contains(&dss.scheme) {
            return Err(RustlsError::PeerMisbehaved(
                PeerMisbehaved::SignedHandshakeWithUnadvertisedSigScheme,
            ));
        }
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        ACCEPTED_SIGNATURE_SCHEMES.to_vec()
    }
}
