use std::sync::Arc;

use thiserror::Error;

use tracing_error::TracedError;
use zebra_chain::serialization::SerializationError;

use crate::protocol::external::InventoryHash;

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

    /// The remote peer did not respond to a [`peer::Client`] request in time.
    #[error("Client request timed out")]
    ClientRequestTimeout,

    /// A serialization error occurred while reading or writing a message.
    #[error("Serialization error: {0}")]
    Serialization(#[from] SerializationError),

    /// A badly-behaved remote peer sent a handshake message after the handshake was
    /// already complete.
    #[error("Remote peer sent handshake messages after handshake")]
    DuplicateHandshake,

    /// This node's internal services were overloaded, so the connection was dropped
    /// to shed load.
    #[error("Internal services over capacity")]
    Overloaded,

    // TODO: stop closing connections on these errors (#2107)
    //       log info or debug logs instead
    //
    /// A peer sent us a message we don't support.
    #[error("Remote peer sent an unsupported message type: {0}")]
    UnsupportedMessage(&'static str),

    /// We requested data that the peer couldn't find.
    #[error("Remote peer could not find items: {0:?}")]
    NotFound(Vec<InventoryHash>),
}

/// A shared error slot for peer errors.
///
/// # Correctness
///
/// Error slots are shared between sync and async code. In async code, the error
/// mutex should be held for as short a time as possible. This avoids blocking
/// the async task thread on acquiring the mutex.
#[derive(Default, Clone)]
pub struct ErrorSlot(Arc<std::sync::Mutex<Option<SharedPeerError>>>);

impl std::fmt::Debug for ErrorSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // don't hang if the mutex is locked
        // show the panic if the mutex was poisoned
        f.debug_struct("ErrorSlot")
            .field("error", &self.0.try_lock())
            .finish()
    }
}

impl ErrorSlot {
    /// Read the current error in the slot.
    ///
    /// Returns `None` if there is no error in the slot.
    ///
    /// # Correctness
    ///
    /// Briefly locks the error slot's threaded `std::sync::Mutex`, to get a
    /// reference to the error in the slot.
    pub fn try_get_error(&self) -> Option<SharedPeerError> {
        self.0
            .lock()
            .expect("error mutex should be unpoisoned")
            .as_ref()
            .cloned()
    }

    /// Update the current error in the slot.
    ///
    /// Returns `Err(AlreadyErrored)` if there was already an error in the slot.
    ///
    /// # Correctness
    ///
    /// Briefly locks the error slot's threaded `std::sync::Mutex`, to check for
    /// a previous error, then update the error in the slot.
    pub fn try_update_error(&self, e: SharedPeerError) -> Result<(), AlreadyErrored> {
        let mut guard = self.0.lock().expect("error mutex should be unpoisoned");

        if let Some(original_error) = guard.clone() {
            error!(?original_error, new_error = ?e, "peer connection already errored");
            Err(AlreadyErrored)
        } else {
            *guard = Some(e);
            Ok(())
        }
    }
}

/// The `ErrorSlot` already contains an error.
pub struct AlreadyErrored;

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
    /// Sending or receiving a message timed out.
    #[error("Timeout when sending or receiving a message to peer")]
    Timeout,
}

impl From<tokio::time::error::Elapsed> for HandshakeError {
    fn from(_source: tokio::time::error::Elapsed) -> Self {
        HandshakeError::Timeout
    }
}
