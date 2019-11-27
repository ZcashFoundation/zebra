use std::sync::{Arc, Mutex};

use thiserror::Error;

use zebra_chain::serialization::SerializationError;

/// A wrapper around `Arc<PeerError>` that implements `Error`.
#[derive(Error, Debug, Clone)]
#[error("{0}")]
pub struct SharedPeerError(#[from] Arc<PeerError>);

/// An error related to peer connection handling.
#[derive(Error, Debug)]
pub enum PeerError {
    /// The remote peer closed the connection.
    #[error("Peer closed connection")]
    ConnectionClosed,
    /// The [`peer::Client`] half of the [`peer::Client`]/[`PeerServer`] pair died before
    /// the [`Server`] half did.
    #[error("peer::Client instance died")]
    DeadClient,
    /// The [`PeerServer`] half of the [`PeerServer`]/[`peer::Client`] pair died before
    /// the [`peer::Client`] half did.
    #[error("PeerServer instance died")]
    DeadPeerServer,
    /// The remote peer did not respond to a [`peer::Client`] request in time.
    #[error("Client request timed out")]
    ClientRequestTimeout,
    /// A serialization error occurred while reading or writing a message.
    #[error("Serialization error")]
    Serialization(#[from] SerializationError),
    /// A badly-behaved remote peer sent a handshake message after the handshake was
    /// already complete.
    #[error("Remote peer sent handshake messages after handshake")]
    DuplicateHandshake,
    /// A badly-behaved remote peer sent the wrong nonce in response to a heartbeat `Ping`.
    #[error("Remote peer sent the wrong heartbeat nonce")]
    HeartbeatNonceMismatch,
    /// This node's internal services were overloaded, so the connection was dropped
    /// to shed load.
    #[error("Internal services over capacity")]
    Overloaded,
    /// We got a `Reject` message. This does not necessarily mean that
    /// the peer connection is in a bad state, but for the time being
    /// we are considering it a PeerError.
    // TODO: Create a different error type (more at the application
    // level than network/connection level) that will include the
    // appropriate error when a `Reject` message is received.
    #[error("Received a Reject message")]
    Rejected,
}

#[derive(Default, Clone)]
pub(super) struct ErrorSlot(pub(super) Arc<Mutex<Option<SharedPeerError>>>);

impl ErrorSlot {
    pub fn try_get_error(&self) -> Option<SharedPeerError> {
        self.0
            .lock()
            .expect("error mutex should be unpoisoned")
            .as_ref()
            .map(|e| e.clone())
    }
}

/// An error during a handshake with a remote peer.
#[derive(Error, Debug)]
pub enum HandshakeError {
    /// The remote peer sent an unexpected message during the handshake.
    #[error("The remote peer sent an unexpected message: {0:?}")]
    UnexpectedMessage(crate::protocol::external::Message),
    /// The peer connector detected handshake nonce reuse, possibly indicating self-connection.
    #[error("Detected nonce reuse, possible self-connection")]
    NonceReuse,
    /// The remote peer closed the connection.
    #[error("Peer closed connection")]
    ConnectionClosed,
    /// An error occurred while performing an IO operation.
    #[error("Underlying IO error")]
    Io(#[from] std::io::Error),
    /// A serialization error occurred while reading or writing a message.
    #[error("Serialization error")]
    Serialization(#[from] SerializationError),
}
