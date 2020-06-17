use std::sync::{Arc, Mutex};

use thiserror::Error;

use tracing_error::TracedError;
use zebra_chain::serialization::SerializationError;

/// A wrapper around `Arc<PeerError>` that implements `Error`.
#[derive(Error, Debug, Clone)]
#[error(transparent)]
pub struct SharedPeerError(Arc<TracedError<PeerError>>);

impl<E> From<E> for SharedPeerError
where
    PeerError: From<E>,
{
    fn from(source: E) -> Self {
        Self(Arc::new(TracedError::from(PeerError::from(source))))
    }
}

/// An error related to peer connection handling.
#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum PeerError {
    /// The remote peer closed the connection.
    #[error("Peer closed connection")]
    ConnectionClosed,
    /// The [`peer::Client`] half of the [`peer::Client`]/[`peer::Server`] pair died before
    /// the [`Server`] half did.
    #[error("peer::Client instance died")]
    DeadClient,
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
    /// This node's internal services were overloaded, so the connection was dropped
    /// to shed load.
    #[error("Internal services over capacity")]
    Overloaded,
    /// A peer sent us a message we don't support or instructed to
    /// disconnect from upon receipt.
    #[error("Remote peer sent an unsupported message type.")]
    UnsupportedMessage,
    /// We got a `Reject` message. This does not necessarily mean that
    /// the peer connection is in a bad state, but for the time being
    /// we are considering it a PeerError.
    // TODO: Create a different error type (more at the application
    // level than network/connection level) that will include the
    // appropriate error when a `Reject` message is received.
    #[error("Received a Reject message")]
    Rejected,
    /// The remote peer responded with a block we didn't ask for.
    #[error("Remote peer responded with a block we didn't ask for.")]
    WrongBlock,
}

#[derive(Default, Clone)]
pub(super) struct ErrorSlot(pub(super) Arc<Mutex<Option<SharedPeerError>>>);

impl ErrorSlot {
    pub fn try_get_error(&self) -> Option<SharedPeerError> {
        self.0
            .lock()
            .expect("error mutex should be unpoisoned")
            .as_ref()
            .cloned()
    }
}

/// An error during a handshake with a remote peer.
#[derive(Error, Debug)]
pub enum HandshakeError {
    /// The remote peer sent an unexpected message during the handshake.
    #[error("The remote peer sent an unexpected message: {0:?}")]
    UnexpectedMessage(Box<crate::protocol::external::Message>),
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
    /// The remote peer offered a version older than our minimum version.
    #[error("Peer offered obsolete version: {0:?}")]
    ObsoleteVersion(crate::protocol::external::types::Version),
}
