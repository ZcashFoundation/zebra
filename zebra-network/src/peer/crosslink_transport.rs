//! Crosslink BFT peer transport layer.
//!
//! This module will provide QUIC/Noise (IK_25519_ChaChaPoly_BLAKE2s) transport
//! for BFT validator peer connections, replacing the tenderlink transport.
//!
//! ## Status
//!
//! This is a stub module. The transport implementation will be added when
//! the BFT consensus networking is fully integrated into zebra-network.
//!
//! ## Design
//!
//! BFT peers use a separate transport from PoW peers:
//! - PoW peers use TCP with the Bitcoin wire protocol
//! - BFT peers use QUIC with Noise IK for authenticated encryption
//!
//! The Noise IK pattern provides mutual authentication via static DH keys,
//! ensuring that only known validators can establish connections.
//!
//! ## Key Derivation
//!
//! Validator keys are derived from the peer address string using a seeded RNG,
//! which is a temporary mechanism for prototyping. In production, validators
//! will have proper key management.

use std::net::SocketAddr;

/// Configuration for a BFT peer connection.
#[derive(Clone, Debug)]
pub struct CrosslinkPeerConfig {
    /// The socket address of the BFT peer
    pub addr: SocketAddr,
    /// The peer's static Noise public key (32 bytes, Curve25519)
    pub noise_public_key: [u8; 32],
}

/// Placeholder for the crosslink transport handle.
///
/// This will eventually manage QUIC/Noise connections to BFT peers
/// and provide a bidirectional message channel.
#[derive(Debug)]
pub struct CrosslinkTransport {
    /// Our Noise static public key
    pub local_public_key: [u8; 32],
    /// Connected BFT peers
    pub peers: Vec<CrosslinkPeerConfig>,
}

impl CrosslinkTransport {
    /// Create a new (unconnected) transport.
    pub fn new(local_public_key: [u8; 32]) -> Self {
        CrosslinkTransport {
            local_public_key,
            peers: Vec::new(),
        }
    }
}
