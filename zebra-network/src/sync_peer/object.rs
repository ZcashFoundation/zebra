//! The `get-object` client: the connection's `get-object` requester, driven
//! on one [`SyncPeer`]'s connection so that an artifact is fetched whole from
//! one peer (zips#1344 P:1583-1596).

use bytes::Bytes;
use tokio::time::Instant;

use crate::{
    constants::REQUEST_TIMEOUT,
    peer::{
        v2::connection::{fetch_object, Failure},
        PeerError,
    },
    protocol::internal::Response,
};

use super::{ObjectHash, SyncPeer, SyncPeerError};

/// One `get-object` answer.
///
/// (The design calls this `ObjectAnswer`; it is renamed so it does not clash
/// with the wire-level `protocol::v2::response::ObjectAnswer`, which carries
/// only the size.)
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObjectChunk {
    /// The peer has the object. `size` is its total length as the peer
    /// claims it; `data` starts at the requested offset, and may be shorter
    /// than requested (the remainder can be re-requested from any offset).
    Found {
        /// The object's claimed total size.
        size: u64,
        /// The delivered bytes.
        data: Bytes,
    },

    /// The peer does not have the object.
    NotFound,
}

impl SyncPeer {
    /// Fetches up to `length` bytes of object `hash` from `offset`, whole
    /// from this one peer (zips#1344 P:1583-1596).
    ///
    /// Waits for one of the peer's bulk permits, and holds it for the
    /// exchange. The exchange is bounded by [`REQUEST_TIMEOUT`]: an answer
    /// that the deadline cuts off after its `result` and `size`, or that the
    /// peer resets part way, is a short [`ObjectChunk::Found`] with the bytes
    /// received, which the caller resumes from the next offset.
    ///
    /// The data is unauthenticated: the caller hashes the assembled object
    /// and, on a mismatch of a complete object from this peer, calls
    /// [`SyncPeer::penalize`] with 100 points. An answer whose `size` differs
    /// from an earlier answer for the same hash on this connection, whether
    /// to this handle or to the peer set, is a `PROTOCOL_ERROR` violation.
    ///
    /// # Errors
    ///
    /// - [`SyncPeerError::LocalRequest`]: the request is outside the draft's
    ///   bounds, so nothing was sent;
    /// - [`SyncPeerError::Refused`]: the peer reset the stream with `REFUSED`
    ///   before answering;
    /// - [`SyncPeerError::Violation`]: the answer broke the draft's format or
    ///   bounds, or changed the object's size (`PROTOCOL_ERROR` or `FLOOD`);
    ///   the connection has been closed with that code;
    /// - [`SyncPeerError::Transport`]: anything else, including an answer
    ///   whose `result` and `size` did not arrive in time.
    pub async fn get_object(
        &self,
        hash: ObjectHash,
        offset: u64,
        length: u32,
    ) -> Result<ObjectChunk, SyncPeerError> {
        let _permit = self
            .bulk_permits
            .acquire()
            .await
            .map_err(|_closed| SyncPeerError::Transport("bulk permits closed".to_string()))?;

        // The requester keeps the data it has read at the deadline, but the wait for the answer's
        // `result` and `size` is bounded here, as the connection's request driver bounds it.
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        let fetched = tokio::time::timeout_at(
            deadline,
            fetch_object(
                &self.connection,
                &self.object_sizes,
                deadline,
                hash,
                offset,
                length,
            ),
        )
        .await
        .map_err(|_elapsed| {
            SyncPeerError::Transport(format!("get-object answer for {hash} timed out"))
        })?;

        match fetched {
            Ok(Response::Object { size, data }) => Ok(ObjectChunk::Found {
                size,
                data: data.into(),
            }),
            Ok(Response::ObjectNotFound) => Ok(ObjectChunk::NotFound),
            Ok(response) => unreachable!(
                "fetch_object only answers with Object or ObjectNotFound, not {response}"
            ),
            Err(failure) => Err(self.object_failure(failure)),
        }
    }

    /// Maps a failed `get-object` exchange to its error, closing the
    /// connection first if the peer violated the protocol.
    fn object_failure(&self, failure: Failure) -> SyncPeerError {
        match failure {
            Failure::Violation { reason, code } => {
                tracing::debug!(
                    addr = ?self.addr,
                    %code,
                    %reason,
                    "closing a sync peer whose get-object answer violated the protocol",
                );
                self.closer.close(reason.clone(), code);
                SyncPeerError::Violation { code, reason }
            }
            Failure::PeerRefused => SyncPeerError::Refused,
            Failure::Refused(PeerError::RequestOutOfBounds(reason)) => {
                SyncPeerError::LocalRequest(reason)
            }
            Failure::Refused(error) => SyncPeerError::LocalRequest(error.to_string()),
            Failure::Transport(reason) => SyncPeerError::Transport(reason),
            // The `get-object` requester has no timeout of its own, and none of its answers is a
            // scored misbehavior, a legacy not-found, or an unanswered response.
            failure @ (Failure::Timeout
            | Failure::Misbehavior(_)
            | Failure::NotFound(_)
            | Failure::Unanswered) => SyncPeerError::Transport(failure.to_string()),
        }
    }
}
