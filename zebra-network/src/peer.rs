//! Peer handling.

/// Handles outbound requests from our node to the network.
mod client;
/// Asynchronously connects to peers.
mod connector;
/// Peer-related errors.
mod error;
/// Handles inbound requests from the network to our node.
mod server;

pub use client::PeerClient;
pub use connector::PeerConnector;
pub use error::{PeerError, SharedPeerError};
pub use server::PeerServer;
