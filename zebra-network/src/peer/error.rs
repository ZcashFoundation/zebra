//! Peer-related errors.

use std::{borrow::Cow, sync::Arc};

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

impl SharedPeerError {
    /// Returns a debug-formatted string describing the inner [`PeerError`].
    ///
    /// Unfortunately, [`TracedError`] makes it impossible to get a reference to the original error.
    pub fn inner_debug(&self) -> String {
        format!("{:?}", self.0.as_ref())
    }
}

/// An error related to peer connection handling.
#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum PeerError {
    /// The remote peer closed the connection.
    #[error("Peer closed connection")]
    ConnectionClosed,

    /// Zebra dropped the [`Connection`].
    #[error("Internal connection dropped")]
    ConnectionDropped,

    /// Zebra dropped the [`Client`].
    #[error("Internal client dropped")]
    ClientDropped,

    /// A [`Client`]'s internal connection task exited.
    #[error("Internal peer connection task exited")]
    ConnectionTaskExited,

    /// Zebra's [`Client`] cancelled its heartbeat task.
    #[error("Internal client cancelled its heartbeat task")]
    ClientCancelledHeartbeatTask,

    /// Zebra's internal heartbeat task exited.
    #[error("Internal heartbeat task exited")]
    HeartbeatTaskExited,

    /// Sending a message to a remote peer took too long.
    #[error("Sending Client request timed out")]
    ConnectionSendTimeout,

    /// Receiving a response to a [`peer::Client`] request took too long.
    #[error("Receiving client response timed out")]
    ConnectionReceiveTimeout,

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

    /// We requested data that the peer couldn't find.
    #[error("Remote peer could not find items: {0:?}")]
    NotFound(Vec<InventoryHash>),
}

impl PeerError {
    /// Returns the Zebra internal handler type as a string.
    pub fn kind(&self) -> Cow<'static, str> {
        match self {
            PeerError::ConnectionClosed => "ConnectionClosed".into(),
            PeerError::ConnectionDropped => "ConnectionDropped".into(),
            PeerError::ClientDropped => "ClientDropped".into(),
            PeerError::ClientCancelledHeartbeatTask => "ClientCancelledHeartbeatTask".into(),
            PeerError::HeartbeatTaskExited => "HeartbeatTaskExited".into(),
            PeerError::ConnectionTaskExited => "ConnectionTaskExited".into(),
            PeerError::ConnectionSendTimeout => "ConnectionSendTimeout".into(),
            PeerError::ConnectionReceiveTimeout => "ConnectionReceiveTimeout".into(),
            // TODO: add error kinds or summaries to `SerializationError`
            PeerError::Serialization(inner) => format!("Serialization({})", inner).into(),
            PeerError::DuplicateHandshake => "DuplicateHandshake".into(),
            PeerError::Overloaded => "Overloaded".into(),
            PeerError::NotFound(_) => "NotFound".into(),
        }
    }
}

/// A shared error slot for peer errors.
///
/// # Correctness
///
/// Error slots are shared between sync and async code. In async code, the error
/// mutex should be held for as short a time as possible. This avoids blocking
/// the async task thread on acquiring the mutex.
///
/// > If the value behind the mutex is just data, it’s usually appropriate to use a blocking mutex
/// > ...
/// > wrap the `Arc<Mutex<...>>` in a struct
/// > that provides non-async methods for performing operations on the data within,
/// > and only lock the mutex inside these methods
///
/// https://docs.rs/tokio/1.15.0/tokio/sync/struct.Mutex.html#which-kind-of-mutex-should-you-use
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
            Err(AlreadyErrored { original_error })
        } else {
            *guard = Some(e);
            Ok(())
        }
    }
}

/// Error returned when the `ErrorSlot` already contains an error.
#[derive(Clone, Debug)]
pub struct AlreadyErrored {
    /// The original error in the error slot.
    pub original_error: SharedPeerError,
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
    /// Sending or receiving a message timed out.
    #[error("Timeout when sending or receiving a message to peer")]
    Timeout,
}

impl From<tokio::time::error::Elapsed> for HandshakeError {
    fn from(_source: tokio::time::error::Elapsed) -> Self {
        HandshakeError::Timeout
    }
}
