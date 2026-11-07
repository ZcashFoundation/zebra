//! Direct handles to version 2 peers for bulk synchronization.
//!
//! Bulk streams bypass the peer set: the peer set routes by P2C, runs each
//! request under one `REQUEST_TIMEOUT`, and answers once, but the checkpoint
//! sync engine needs byte-level progress, stall cancellation, prefix resume
//! and exact blame on streams it controls. A [`SyncPeer`] is a cheap clone of
//! one version 2 connection; the [`registry`] actor publishes the connected
//! ones through a `watch` channel.

use std::{
    fmt,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use quinn::VarInt;
use tokio::sync::Semaphore;

use zebra_chain::block;

use crate::{
    peer::v2::{connection::ConnectionCloser, misbehavior::MisbehaviorScores},
    protocol::v2::request::Request as WireRequest,
    quic::app_error_code,
    types::PeerServices,
    PeerSocketAddr,
};

pub mod block_range;
pub mod registry;

#[cfg(test)]
mod tests;

pub use crate::protocol::v2::{request::GetBlockRange, types::ErrorCode};
pub use block_range::{BlockRangeStream, RangeItem};
pub use registry::{SyncPeers, SyncPeersSender};

/// The bulk streams one [`SyncPeer`] may have open at once, across clones.
pub const BULK_STREAMS_PER_PEER: usize = 2;

/// Identifies one version 2 connection for the life of the process: assigned
/// from a monotonic counter when the connection's handshake completes, never
/// reused.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ConnectionId(pub u64);

impl ConnectionId {
    /// The next unused ID.
    pub(crate) fn next() -> Self {
        /// The next ID to assign. 2^64 handshakes never complete in one process.
        static NEXT: AtomicU64 = AtomicU64::new(0);
        ConnectionId(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// A handle to one connected version 2 peer, for bulk requests that bypass
/// the peer set.
///
/// Clones share the connection and its bulk permits. A handle stays valid
/// after the connection closes: every request then fails with
/// [`SyncPeerError::Transport`].
#[derive(Clone)]
pub struct SyncPeer {
    id: ConnectionId,
    addr: PeerSocketAddr,
    services: PeerServices,
    remote_tip_height: block::Height,
    connection: quinn::Connection,
    bulk_permits: Arc<Semaphore>,
    misbehavior: MisbehaviorScores,
    /// Closes the connection through its connection task, so the close is in
    /// the task's error slot and log, as for a violation a stream task finds.
    closer: ConnectionCloser,
}

impl SyncPeer {
    /// Creates a handle for a connection whose handshake just completed.
    ///
    /// Called only from `peer_set::initialize::v2::finish_connection`, next to
    /// `spawn_connection`, which then sends it to the registry. `closer` is
    /// the connection task's.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        id: ConnectionId,
        addr: PeerSocketAddr,
        services: PeerServices,
        remote_tip_height: block::Height,
        connection: quinn::Connection,
        misbehavior: MisbehaviorScores,
        closer: ConnectionCloser,
    ) -> Self {
        SyncPeer {
            id,
            addr,
            services,
            remote_tip_height,
            connection,
            bulk_permits: Arc::new(Semaphore::new(BULK_STREAMS_PER_PEER)),
            misbehavior,
            closer,
        }
    }

    /// The connection's process-unique ID.
    pub fn id(&self) -> ConnectionId {
        self.id
    }

    /// The peer's address.
    pub fn addr(&self) -> PeerSocketAddr {
        self.addr
    }

    /// The services in the peer's `init` record.
    pub fn services(&self) -> PeerServices {
        self.services
    }

    /// The `start_height` of the peer's `init` record. Untrusted: the engine
    /// only uses it to *lower* its target.
    pub fn remote_tip_height(&self) -> block::Height {
        self.remote_tip_height
    }

    /// Whether the connection has closed or is closing: closed, a close has
    /// been queued for its connection task (by this handle, a clone, or one of
    /// the connection's stream tasks), or the connection task has ended.
    ///
    /// True as soon as [`SyncPeerError::Violation`] is returned, so a peer
    /// that was just blamed is not picked again, though the registry may list
    /// it until the connection task has closed it.
    pub fn is_closed(&self) -> bool {
        self.connection.close_reason().is_some() || self.closer.is_closed()
    }

    /// Waits until the connection has closed, or its connection task has
    /// ended, however it ended.
    pub(crate) async fn closed(&self) {
        tokio::select! {
            _ = self.connection.closed() => {}
            () = self.closer.closed() => {}
        }
    }

    /// Opens a `get-block-range` stream for `request`.
    ///
    /// Waits for one of the peer's [`BULK_STREAMS_PER_PEER`] permits, which the
    /// returned stream holds until it is dropped. Opens the bidirectional
    /// stream, writes `0x07 ‖ body`, and finishes the send side; blocks are
    /// read lazily by [`BlockRangeStream::next`]. There is no request
    /// timeout: stalls are the caller's to detect.
    ///
    /// # Errors
    ///
    /// [`SyncPeerError::LocalRequest`] if `request` is outside the protocol's
    /// bounds (nothing is sent); [`SyncPeerError::Transport`] if the stream
    /// cannot be opened.
    pub async fn get_block_range(
        &self,
        request: GetBlockRange,
    ) -> Result<BlockRangeStream, SyncPeerError> {
        request
            .check()
            .map_err(|error| SyncPeerError::LocalRequest(error.to_string()))?;
        let wire_request = WireRequest::BlockRange(request);
        let body = wire_request
            .encode()
            .map_err(|error| SyncPeerError::LocalRequest(error.to_string()))?;

        let permit = self
            .bulk_permits
            .clone()
            .acquire_owned()
            .await
            .expect("the bulk permits are never closed");

        let recv = block_range::open(&self.connection, wire_request.stream_type(), &body).await?;
        Ok(BlockRangeStream::spawn(
            recv,
            request,
            permit,
            self.closer.clone(),
        ))
    }

    /// Queues a close of the connection with `PROTOCOL_ERROR` because the
    /// caller found a violation the stream reader cannot see (a merkle-root or
    /// duplicate-txid failure of a delivered block). Returns at once;
    /// [`SyncPeer::is_closed`] is true from then on.
    pub fn report_violation(&self, reason: String) {
        tracing::debug!(addr = ?self.addr, %reason, "closing a sync peer's connection");
        self.closer.close(reason, ErrorCode::ProtocolError);
    }

    /// Adds `points` to the peer's misbehavior score (100 for an auth-data
    /// commitment failure, zips#1344 P:1404-1407). Closes the connection with
    /// `MISBEHAVIOR` if the score reaches the ban threshold.
    pub async fn penalize(&self, points: u32, reason: String) {
        if self
            .misbehavior
            .penalize(self.addr, points, reason.clone())
            .await
        {
            self.closer.close(reason, ErrorCode::Misbehavior);
        }
    }

    /// Sets the connection's receive window (`quinn::Connection::set_receive_window`):
    /// 64 MiB while the engine uses the peer, 8 MiB after.
    pub fn set_bulk_receive_window(&self, bytes: u32) {
        self.connection.set_receive_window(VarInt::from_u32(bytes));
    }

    /// Closes the connection with `NO_ERROR` if nothing has closed it yet.
    pub(crate) fn close_if_open(&self) {
        if self.connection.close_reason().is_none() {
            self.connection.close(
                app_error_code(ErrorCode::NoError),
                ErrorCode::NoError.name().as_bytes(),
            );
        }
    }
}

impl fmt::Debug for SyncPeer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyncPeer")
            .field("id", &self.id)
            .field("addr", &self.addr)
            .field("services", &self.services)
            .field("remote_tip_height", &self.remote_tip_height)
            .field("bulk_permits", &self.bulk_permits.available_permits())
            .finish_non_exhaustive()
    }
}

/// Why a [`SyncPeer`] request failed.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum SyncPeerError {
    /// The peer answered `not found` (`result = 0x02`).
    #[error("the peer does not have the requested data")]
    NotFound,

    /// The peer reset the stream with `REFUSED` before sending any data. The
    /// scheduler treats the peer as not serving this type for 10 minutes.
    #[error("the peer refused the request")]
    Refused,

    /// The stream was cancelled (`CANCELLED`), by either side.
    #[error("the request was cancelled")]
    Cancelled,

    /// The stream or connection failed for a transport reason.
    #[error("transport error: {0}")]
    Transport(String),

    /// The peer violated the protocol; this node has queued a close of the
    /// connection with `code`, and [`SyncPeer::is_closed`] is already true.
    /// The connection task closes the connection shortly after, with `code`,
    /// unless another close was already queued, whose code then wins.
    #[error("peer violated the protocol ({code}): {reason}")]
    Violation {
        /// The close code.
        code: ErrorCode,
        /// What the peer did.
        reason: String,
    },

    /// The request is outside the protocol's bounds, so it was not sent.
    #[error("request not sent: {0}")]
    LocalRequest(String),
}
