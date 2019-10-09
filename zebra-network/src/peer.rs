//! Peer handling.

/// Handles outbound requests from our node to the network.
mod client;
/// Asynchronously connects to peers.
mod connector;
/// Handles inbound requests from the network to our node.
mod server;

pub use client::PeerClient;
pub use connector::PeerConnector;
pub use server::PeerServer;

/// An error related to a peer connection.
#[derive(Fail, Debug, Clone)]
pub enum PeerError {
    /// Wrapper around `failure::Error` that can be `Clone`.
    #[fail(display = "{}", _0)]
    Inner(std::sync::Arc<failure::Error>),
}

impl From<failure::Error> for PeerError {
    fn from(e: failure::Error) -> PeerError {
        PeerError::Inner(std::sync::Arc::new(e))
    }
}
