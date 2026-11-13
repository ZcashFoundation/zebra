//! The `get-block-range` client stream.
//!
//! The response is read by a task of its own, which checks each block on
//! arrival and hands it to the [`BlockRangeStream`] through a one-item
//! channel. So [`BlockRangeStream::next`] is cancellation-safe: the caller
//! can race it against a stall timer and call it again, and
//! [`BlockRangeStream::bytes_received`] counts a block while it is still
//! arriving.

use std::{
    io,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    task::{Context, Poll},
};

use bytes::Bytes;
use quinn::{Connection, ReadError, WriteError};
use tokio::{
    io::{AsyncRead, BufReader, ReadBuf},
    sync::{mpsc, OwnedSemaphorePermit},
    task::JoinHandle,
};
use tracing_futures::Instrument;

use zebra_chain::{
    block::{self, MAX_BLOCK_BYTES},
    serialization::ZcashDeserialize,
};

use crate::{
    peer::v2::connection::{ConnectionCloser, RequestSend, ResponseStream},
    protocol::v2::{
        request::GetBlockRange,
        response::block_range::{
            read_block_range_answer, read_block_range_element, BlockRangeAnswer, BlockRangeBounds,
        },
        types::{ErrorCode, StreamType},
        wire::{Violation, WireError},
    },
    quic::app_error_code,
};

use super::SyncPeerError;

/// How many checked items the reader task may queue for the caller. With the
/// item the task is reading and the one it is waiting to queue, a stream
/// buffers at most three blocks.
const QUEUED_ITEMS: usize = 1;

/// One item of a [`BlockRangeStream`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RangeItem {
    /// The next block, in descending height order.
    ///
    /// Checked on arrival: `hash` is the expected hash (the request's
    /// `final_hash` for the first block, the previous block's
    /// `previous_block_hash` after that), `raw.len()` is at most
    /// `MAX_BLOCK_BYTES`, and the count and byte bounds hold. Only the header
    /// was parsed: full deserialization and the merkle check are the caller's
    /// (the engine's convert stage), which reports failures with
    /// [`SyncPeer::report_violation`](super::SyncPeer::report_violation).
    Block {
        /// The block's complete serialization.
        raw: Bytes,
        /// The block's parsed header.
        header: Arc<block::Header>,
        /// The block's hash, equal to the expected hash.
        hash: block::Hash,
    },

    /// The responder finished the stream. Yielded once, after the last
    /// block; the stream then yields `None`.
    End {
        /// The number of blocks delivered.
        delivered: u32,
        /// The hash the next block would have had: the last delivered
        /// block's `previous_block_hash`, or the request's `final_hash` if
        /// nothing was delivered. A truncated unit resumes here.
        next_expected: block::Hash,
    },
}

/// An open `get-block-range` response.
///
/// Holds one of its peer's bulk permits. Dropping it before
/// [`RangeItem::End`] cancels the response with `STOP_SENDING(CANCELLED)`
/// and releases the permit.
#[derive(Debug)]
pub struct BlockRangeStream {
    /// The checked items, from the reader task.
    items: mpsc::Receiver<Result<RangeItem, SyncPeerError>>,
    /// The response bytes the reader task has received so far.
    bytes_received: Arc<AtomicU64>,
    /// The reader task, which owns the response stream: aborting it drops
    /// the stream, which cancels it if it has not been read to its end.
    reader: JoinHandle<()>,
    /// Whether the stream has yielded its last item.
    finished: bool,
    /// One of the peer's bulk permits, held for the stream's life.
    _permit: OwnedSemaphorePermit,
}

impl BlockRangeStream {
    /// Starts reading the response to `request` from `recv`, holding
    /// `permit`. Violations close the connection through `closer`.
    pub(super) fn spawn(
        recv: ResponseStream,
        request: GetBlockRange,
        permit: OwnedSemaphorePermit,
        closer: ConnectionCloser,
    ) -> Self {
        let (items_tx, items) = mpsc::channel(QUEUED_ITEMS);
        let bytes_received = Arc::new(AtomicU64::new(0));
        let reader = tokio::spawn(
            read_response(recv, request, bytes_received.clone(), items_tx, closer)
                .in_current_span(),
        );
        BlockRangeStream {
            items,
            bytes_received,
            reader,
            finished: false,
            _permit: permit,
        }
    }

    /// Reads the next item, or `None` after [`RangeItem::End`] or an error.
    ///
    /// Cancellation-safe: if the returned future is dropped before it
    /// completes, no item is lost, and the stream keeps reading.
    ///
    /// # Errors
    ///
    /// - [`SyncPeerError::NotFound`]: `result = 0x02` (first call only);
    /// - [`SyncPeerError::Refused`]: reset with `REFUSED` before any data;
    /// - [`SyncPeerError::Busy`]: reset with `BUSY` or `INTERNAL_ERROR`
    ///   before any data;
    /// - [`SyncPeerError::Cancelled`]: reset with `CANCELLED`;
    /// - [`SyncPeerError::Violation`]: a hash-chain, size, count, byte-bound
    ///   (`FLOOD`) or framing (`PROTOCOL_ERROR`) violation; a close of the
    ///   connection with that code has been queued (see the variant);
    /// - [`SyncPeerError::Transport`]: anything else.
    ///
    /// Blocks already yielded stay valid after any error.
    pub async fn next(&mut self) -> Option<Result<RangeItem, SyncPeerError>> {
        if self.finished {
            return None;
        }
        let item = self.items.recv().await.unwrap_or_else(|| {
            // The reader task sends a last item before it returns, so it panicked.
            Err(SyncPeerError::Transport(
                "the get-block-range reader task failed".to_string(),
            ))
        });
        if matches!(item, Ok(RangeItem::End { .. }) | Err(_)) {
            self.finished = true;
        }
        Some(item)
    }

    /// The serialized block bytes received so far, including a partially
    /// read block: the engine's progress and rate measure.
    ///
    /// The count is of the response's bytes as the reader takes them from
    /// the transport, so it also includes the `result` byte and the length
    /// prefixes: at most a few bytes per block.
    pub fn bytes_received(&self) -> u64 {
        self.bytes_received.load(Ordering::Relaxed)
    }
}

impl Drop for BlockRangeStream {
    fn drop(&mut self) {
        // The reader task drops the response stream, which sends `STOP_SENDING(CANCELLED)`
        // unless the stream was read to its end.
        self.reader.abort();
    }
}

/// Opens a request stream on `connection`, writes `stream_type` and `body`,
/// and finishes this node's direction.
///
/// Both halves are guarded from the moment the stream opens, so a request
/// abandoned part way cancels the stream in both directions, and dropping the
/// returned stream before its end sends `STOP_SENDING(CANCELLED)`. The same
/// as the peer set's `open_request`, except for how write errors map.
pub(super) async fn open(
    connection: &Connection,
    stream_type: StreamType,
    body: &[u8],
) -> Result<ResponseStream, SyncPeerError> {
    let (send, recv) = connection.open_bi().await.map_err(transport)?;
    let response = ResponseStream(recv);
    let mut guard = RequestSend(Some(send));
    let send = guard
        .0
        .as_mut()
        .expect("the guard holds the stream until it is finished");
    send.write_all(&[stream_type.code()])
        .await
        .map_err(write_error)?;
    send.write_all(body).await.map_err(write_error)?;
    send.finish().map_err(transport)?;
    // Finished: nothing is left for the guard to cancel.
    guard.0 = None;
    Ok(response)
}

/// Reads and checks the response to `request` from `recv`, and queues each
/// item for the [`BlockRangeStream`], until the response ends or fails, or
/// the stream is dropped.
async fn read_response(
    mut recv: ResponseStream,
    request: GetBlockRange,
    bytes_received: Arc<AtomicU64>,
    items: mpsc::Sender<Result<RangeItem, SyncPeerError>>,
    closer: ConnectionCloser,
) {
    // The readers take result and CompactSize bytes one at a time, and unbuffered each byte
    // would be its own `poll_read` on the stream. The buffer sits above the counter, so bytes
    // are counted as they arrive, not as the readers parse them.
    let mut reader = BufReader::new(CountingReader {
        inner: &mut recv.0,
        count: &bytes_received,
    });

    let answer = read_block_range_answer(&mut reader).await;
    let last = match answer {
        Ok(BlockRangeAnswer::Found) => read_blocks(&mut reader, request, &items, &closer).await,
        Ok(BlockRangeAnswer::NotFound) => Err(SyncPeerError::NotFound),
        Err(error) => Err(read_error(error, true, &closer)),
    };
    // If the caller dropped the stream, this task is being aborted, and there is no one to tell.
    let _ = items.send(last).await;
}

/// Reads and checks the blocks after a `Found` answer, queuing each one.
/// Returns the last item: [`RangeItem::End`] or the error that ended the
/// response.
async fn read_blocks<R: AsyncRead + Unpin>(
    reader: &mut R,
    request: GetBlockRange,
    items: &mpsc::Sender<Result<RangeItem, SyncPeerError>>,
    closer: &ConnectionCloser,
) -> Result<RangeItem, SyncPeerError> {
    let mut bounds = BlockRangeBounds::new(&request);
    let mut expected = request.final_hash;

    while let Some(raw) = read_block_range_element(reader, &mut bounds)
        .await
        .map_err(|error| read_error(error, false, closer))?
    {
        let header = check_block(&raw, expected).map_err(|reason| {
            closer.close(reason.clone(), ErrorCode::ProtocolError);
            SyncPeerError::Violation {
                code: ErrorCode::ProtocolError,
                reason,
            }
        })?;
        let hash = expected;
        expected = header.previous_block_hash;
        let block = RangeItem::Block {
            raw,
            header: Arc::new(header),
            hash,
        };
        if items.send(Ok(block)).await.is_err() {
            // The caller dropped the stream, so this task is being aborted.
            return Err(SyncPeerError::Cancelled);
        }
    }

    Ok(RangeItem::End {
        delivered: bounds.delivered,
        next_expected: expected,
    })
}

/// Checks a delivered block against the hash the chain expects next, and
/// returns its header, or why the block is a `PROTOCOL_ERROR`.
///
/// Only the header is parsed: the transactions and the merkle root are the
/// caller's to check.
pub(super) fn check_block(raw: &[u8], expected: block::Hash) -> Result<block::Header, String> {
    // `usize` fits in a `u64` on every supported target, so the cast is lossless.
    if raw.len() as u64 > MAX_BLOCK_BYTES {
        return Err(format!(
            "get-block-range block of {} bytes is above the {MAX_BLOCK_BYTES}-byte block limit",
            raw.len(),
        ));
    }
    let header = block::Header::zcash_deserialize(raw)
        .map_err(|error| format!("get-block-range block has a malformed header: {error}"))?;
    let hash = block::Hash::from(&header);
    if hash != expected {
        return Err(format!(
            "get-block-range delivered block {hash} where the chain needs {expected}"
        ));
    }
    Ok(header)
}

/// The error for a failure to read the response, closing the connection
/// through `closer` if it is a violation. `before_data` is whether the
/// `result` byte was still unread.
fn read_error(error: WireError, before_data: bool, closer: &ConnectionCloser) -> SyncPeerError {
    let code = match error.violation() {
        Some(Violation::Close(code)) => code,
        // No `get-block-range` read reports a misbehavior; any violation ends the response.
        Some(Violation::Misbehavior) => ErrorCode::ProtocolError,
        None => {
            return match error {
                WireError::Io(error) => io_error(error, before_data),
                error => SyncPeerError::Transport(error.to_string()),
            }
        }
    };
    let reason = error.to_string();
    closer.close(reason.clone(), code);
    SyncPeerError::Violation { code, reason }
}

/// The error for a failed read of the response stream: the peer's reset
/// codes are the draft's, anything else is the transport's.
fn io_error(error: io::Error, before_data: bool) -> SyncPeerError {
    let reset = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<ReadError>())
        .and_then(|inner| match inner {
            ReadError::Reset(code) => Some(*code),
            _ => None,
        });
    match reset {
        Some(code) if before_data && code == app_error_code(ErrorCode::Refused) => {
            SyncPeerError::Refused
        }
        Some(code) if before_data && is_busy(code) => SyncPeerError::Busy,
        Some(code) if code == app_error_code(ErrorCode::Cancelled) => SyncPeerError::Cancelled,
        _ => SyncPeerError::Transport(error.to_string()),
    }
}

/// Whether a stream error code says the peer serves the request, but not
/// now: `BUSY`, or `INTERNAL_ERROR`, which a requester treats the same way.
fn is_busy(code: quinn::VarInt) -> bool {
    code == app_error_code(ErrorCode::Busy) || code == app_error_code(ErrorCode::InternalError)
}

/// The error for a failed write of the request: a peer that stops the
/// request stream with `REFUSED`, `BUSY` or `CANCELLED` is refusing,
/// deferring or cancelling it.
fn write_error(error: WriteError) -> SyncPeerError {
    match error {
        WriteError::Stopped(code) if code == app_error_code(ErrorCode::Refused) => {
            SyncPeerError::Refused
        }
        WriteError::Stopped(code) if is_busy(code) => SyncPeerError::Busy,
        WriteError::Stopped(code) if code == app_error_code(ErrorCode::Cancelled) => {
            SyncPeerError::Cancelled
        }
        error => transport(error),
    }
}

/// Maps a stream or connection error from quinn.
fn transport(error: impl std::fmt::Display) -> SyncPeerError {
    SyncPeerError::Transport(error.to_string())
}

/// A reader that counts the bytes read through it into `count`.
struct CountingReader<'a, R> {
    inner: R,
    count: &'a AtomicU64,
}

impl<R: AsyncRead + Unpin> AsyncRead for CountingReader<'_, R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let poll = Pin::new(&mut self.inner).poll_read(cx, buf);
        // `filled` only grows during a read, and a `usize` fits in a `u64` on every supported
        // target, so the difference is exact.
        let read = (buf.filled().len() - before) as u64;
        self.count.fetch_add(read, Ordering::Relaxed);
        poll
    }
}
