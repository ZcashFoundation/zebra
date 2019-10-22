//! Peer handling.

/// Handles outbound requests from our node to the network.
mod client;
/// Wrapper around handshake logic that also opens a TCP connection.
mod connector;
/// Peer-related errors.
mod error;
/// Performs peer handshakes.
mod handshake;
/// Handles inbound requests from the network to our node.
mod server;

pub use client::PeerClient;
pub use connector::PeerConnector;
pub use error::{HandshakeError, PeerError, SharedPeerError};
pub use handshake::PeerHandshake;
pub use server::PeerServer;
