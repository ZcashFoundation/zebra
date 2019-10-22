//! Peer handling.

/// Handles outbound requests from our node to the network.
mod client;
/// Peer-related errors.
mod error;
/// Performs peer handshakes.
mod handshake;
/// Handles inbound requests from the network to our node.
mod server;

pub use client::PeerClient;
pub use error::{HandshakeError, PeerError, SharedPeerError};
pub use handshake::PeerHandshake;
pub use server::PeerServer;
