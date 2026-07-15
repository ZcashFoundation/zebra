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

    /// Zebra dropped the [`Connection`](crate::peer::Connection).
    #[error("Internal connection dropped")]
    ConnectionDropped,

    /// Zebra dropped the [`Client`](crate::peer::Client).
    #[error("Internal client dropped")]
    ClientDropped,

    /// A [`Client`](crate::peer::Client)'s internal connection task exited.
    #[error("Internal peer connection task exited")]
    ConnectionTaskExited,

    /// Zebra's [`Client`](crate::peer::Client) cancelled its heartbeat task.
    #[error("Internal client cancelled its heartbeat task")]
    ClientCancelledHeartbeatTask,

    /// Zebra's internal heartbeat task exited.
    #[error("Internal heartbeat task exited with message: {0:?}")]
    HeartbeatTaskExited(String),

    /// Sending a message to a remote peer took too long.
    #[error("Sending Client request timed out")]
    ConnectionSendTimeout,

    /// Receiving a response to a [`Client`](crate::peer::Client) request took too long.
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

    /// There are no ready remote peers.
    #[error("No ready peers available")]
    NoReadyPeers,

    /// This peer request's caused an internal service timeout, so the connection was dropped
    /// to shed load or prevent attacks.
    #[error("Internal services timed out")]
    InboundTimeout,

    /// This node's internal services are no longer able to service requests.
    #[error("Internal services have failed or shutdown")]
    ServiceShutdown,

    /// We requested data, but the peer replied with a `notfound` message.
    /// (Or it didn't respond before the request finished.)
    ///
    /// This error happens when the peer doesn't have any of the requested data,
    /// so that the original request can be retried.
    ///
    /// This is a temporary error.
    ///
    /// Zebra can try different peers if the request is retried,
    /// or peers can download and verify the missing data.
    ///
    /// If the peer has some of the data, the request returns an [`Ok`] response,
    /// with any `notfound` data is marked as [`Missing`][1].
    ///
    /// [1]: crate::protocol::internal::InventoryResponse::Missing
    #[error("Remote peer could not find any of the items: {0:?}")]
    NotFoundResponse(Vec<InventoryHash>),

    /// We requested data, but all our ready peers are marked as recently
    /// [`Missing`][1] that data in our local inventory registry.
    ///
    /// This is a temporary error.
    ///
    /// Peers with the inventory can finish their requests and become ready, or
    /// other peers can download and verify the missing data.
    ///
    /// # Correctness
    ///
    /// This error is produced using Zebra's local inventory registry, without
    /// contacting any peers.
    ///
    /// Client responses containing this error must not be used to update the
    /// inventory registry. This makes sure that we eventually expire our local
    /// cache of missing inventory, and send requests to peers again.
    ///
    /// [1]: crate::protocol::internal::InventoryResponse::Missing
    #[error("All ready peers are registered as recently missing these items: {0:?}")]
    NotFoundRegistry(Vec<InventoryHash>),
}

impl PeerError {
    /// Returns the Zebra internal handler type as a string.
    pub fn kind(&self) -> Cow<'static, str> {
        match self {
            PeerError::ConnectionClosed => "ConnectionClosed".into(),
            PeerError::ConnectionDropped => "ConnectionDropped".into(),
            PeerError::ClientDropped => "ClientDropped".into(),
            PeerError::ClientCancelledHeartbeatTask => "ClientCancelledHeartbeatTask".into(),
            PeerError::HeartbeatTaskExited(_) => "HeartbeatTaskExited".into(),
            PeerError::ConnectionTaskExited => "ConnectionTaskExited".into(),
            PeerError::ConnectionSendTimeout => "ConnectionSendTimeout".into(),
            PeerError::ConnectionReceiveTimeout => "ConnectionReceiveTimeout".into(),
            // TODO: add error kinds or summaries to `SerializationError`
            PeerError::Serialization(inner) => format!("Serialization({inner})").into(),
            PeerError::DuplicateHandshake => "DuplicateHandshake".into(),
            PeerError::Overloaded => "Overloaded".into(),
            PeerError::NoReadyPeers => "NoReadyPeers".into(),
            PeerError::InboundTimeout => "InboundTimeout".into(),
            PeerError::ServiceShutdown => "ServiceShutdown".into(),
            PeerError::NotFoundResponse(_) => "NotFoundResponse".into(),
            PeerError::NotFoundRegistry(_) => "NotFoundRegistry".into(),
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
/// <https://docs.rs/tokio/1.15.0/tokio/sync/struct.Mutex.html#which-kind-of-mutex-should-you-use>
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
    #[allow(clippy::unwrap_in_result)]
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
    #[allow(clippy::unwrap_in_result)]
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

/// Error returned when the [`ErrorSlot`] already contains an error.
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
    RemoteNonceReuse,
    /// The peer connector created a duplicate random nonce. This is very unlikely,
    /// because the range of the data type is 2^64.
    #[error("Unexpectedly created a duplicate random local nonce")]
    LocalDuplicateNonce,
    /// The remote peer closed the connection.
    #[error("Peer closed connection")]
    ConnectionClosed,
    /// An error occurred while performing an IO operation.
    #[error("Underlying IO error: {0}")]
    Io(#[from] std::io::Error),
    /// A serialization error occurred while reading or writing a message.
    #[error("Serialization error: {0}")]
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
