//! The `get-hashes` client (design §5).
//!
//! One `get-hashes` exchange uses one bidirectional stream. The request is
//! `0x06 ‖ start_height ‖ count`. The response is a bare body
//! (`CompactSize n ‖ n entries`, with no `result` byte) that ends at the
//! responder's FIN. A conforming response is at most
//! [`MAX_HASHES_RESPONSE_LEN`] bytes, so it is read whole and decoded at once.

use quinn::{ReadError, ReadToEndError};

use zebra_chain::block::{self, SyncHashEntry};

use crate::{
    constants::REQUEST_TIMEOUT,
    protocol::v2::{
        request::{GetHashes, Request as WireRequest},
        response::hashes::{decode_hashes_response, MAX_HASHES_RESPONSE_LEN},
        types::ErrorCode,
        wire::WireError,
    },
    quic::app_error_code,
};

use super::{SyncPeer, SyncPeerError};

#[cfg(test)]
mod tests;

impl SyncPeer {
    /// Fetches `count` `get-hashes` entries starting at `start`.
    ///
    /// Takes one of the peer's
    /// [`HASHES_STREAMS_PER_PEER`](super::HASHES_STREAMS_PER_PEER) permits for
    /// the exchange, which `get-block-range` streams do not use. The response
    /// is decoded with
    /// `response::hashes::decode_hashes_response`; a non-conforming response
    /// closes the connection (`PROTOCOL_ERROR`) and returns
    /// [`SyncPeerError::Violation`]. The entries are *unverified*: the caller
    /// must check them against a pin before use. Fewer than `count` entries
    /// (a prefix) is success.
    ///
    /// The exchange is bounded by `REQUEST_TIMEOUT` from when the permit is
    /// taken, as a response is at most about 3.4 MB. The wait for the permit
    /// is not: it only waits for this peer's other `get-hashes` exchanges,
    /// which are bounded. A response longer than any conforming response to
    /// `count` is a violation, and is not read past that length.
    pub async fn get_hashes(
        &self,
        start: block::Height,
        count: u32,
    ) -> Result<Vec<SyncHashEntry>, SyncPeerError> {
        let request = GetHashes {
            start_height: start,
            count,
        };
        let wire_request = WireRequest::Hashes(request);
        let body = request
            .check()
            .and_then(|()| wire_request.encode())
            .map_err(|error| SyncPeerError::LocalRequest(error.to_string()))?;

        // A conforming response for `count` entries is at most a 5-byte count and `count`
        // entries of 32 + 4 × 9 bytes, so a peer cannot make this read buffer more than that.
        // `count` is a `u32`, which fits in a `usize` on every supported target.
        let max_response_len = (count as usize)
            .checked_mul(32 + 4 * 9)
            .and_then(|len| len.checked_add(5))
            .map_or(MAX_HASHES_RESPONSE_LEN, |len| {
                len.min(MAX_HASHES_RESPONSE_LEN)
            });

        let _permit = self
            .hashes_permits
            .clone()
            .acquire_owned()
            .await
            .expect("the get-hashes permit semaphore is never closed: SyncPeer owns it");

        let exchange = async {
            let (mut send, mut recv) = self.connection.open_bi().await.map_err(transport)?;
            let response = async {
                send.write_all(&[wire_request.stream_type().code()])
                    .await
                    .map_err(transport)?;
                send.write_all(&body).await.map_err(transport)?;
                send.finish().map_err(transport)?;
                recv.read_to_end(max_response_len)
                    .await
                    .map_err(|error| match error {
                        ReadToEndError::TooLong => Failure::Violation(
                            "get-hashes response longer than any conforming response".to_string(),
                        ),
                        ReadToEndError::Read(error) => Failure::Peer(read_error(error)),
                    })
            }
            .await;
            if response.is_err() {
                // Tell the responder to stop: the response will not be read.
                let _ = recv.stop(app_error_code(ErrorCode::Cancelled));
                let _ = send.reset(app_error_code(ErrorCode::Cancelled));
            }
            response
        };

        // A timed-out exchange is dropped, and quinn then stops and resets its streams.
        let response = tokio::time::timeout(REQUEST_TIMEOUT, exchange)
            .await
            .unwrap_or_else(|_elapsed| {
                Err(Failure::Peer(SyncPeerError::Transport(
                    "get-hashes response timed out".to_string(),
                )))
            })
            .and_then(|body| decode_hashes_response(&body, &request).map_err(Failure::from));

        match response {
            Ok(entries) => Ok(entries),
            Err(Failure::Peer(error)) => Err(error),
            Err(Failure::Violation(reason)) => Err(self.close_for_violation(reason)),
        }
    }

    /// Closes the connection with `PROTOCOL_ERROR` for a non-conforming
    /// response, through the connection task (see
    /// [`SyncPeer::report_violation`]), and returns the error for the caller.
    fn close_for_violation(&self, reason: String) -> SyncPeerError {
        self.report_violation(reason.clone());
        SyncPeerError::Violation {
            code: ErrorCode::ProtocolError,
            reason,
        }
    }
}

/// Why a `get-hashes` exchange failed.
enum Failure {
    /// A failure that is not a protocol violation: a refusal, a cancellation,
    /// a transport error or a timeout.
    Peer(SyncPeerError),

    /// The response does not conform to the request, so the connection is
    /// closed.
    Violation(String),
}

impl From<WireError> for Failure {
    fn from(error: WireError) -> Self {
        // Every decoding error of a complete in-memory body is a violation. `decode_hashes_response`
        // reports them all as `PROTOCOL_ERROR`, and it reads no stream, so it has no I/O error.
        match error {
            WireError::Io(error) => Failure::Peer(SyncPeerError::Transport(error.to_string())),
            error => Failure::Violation(error.to_string()),
        }
    }
}

/// Maps a stream or connection error from quinn.
fn transport(error: impl std::fmt::Display) -> Failure {
    Failure::Peer(SyncPeerError::Transport(error.to_string()))
}

/// Maps a read error, recognizing the responder's `REFUSED` and `CANCELLED`
/// resets.
fn read_error(error: ReadError) -> SyncPeerError {
    match error {
        ReadError::Reset(code) if code == app_error_code(ErrorCode::Refused) => {
            SyncPeerError::Refused
        }
        ReadError::Reset(code) if code == app_error_code(ErrorCode::Cancelled) => {
            SyncPeerError::Cancelled
        }
        error => SyncPeerError::Transport(error.to_string()),
    }
}
