//! Serving `get-block-range` requests (draft, "`get-block-range`").
//!
//! A `get-block-range` response is a `result` byte, then the ancestor chain of
//! the request's `final_hash`, highest block first, until `count` blocks or
//! `max_bytes` bytes of them (the first block exempt) have been sent. The
//! response is read from the inbound service in chunks of at most
//! [`RANGE_READ_CHUNK_BLOCKS`] blocks and [`RANGE_READ_CHUNK`] bytes, each
//! continuing from the last block's parent, and each block is written as it
//! is read.
//!
//! A range can be up to 64 MiB, far more than a peer can be expected to read
//! within one request deadline, so a range response is not bounded by the
//! whole-exchange deadline of the other request types. Instead:
//! - each inbound service call must answer within [`REQUEST_TIMEOUT`], which
//!   bounds this node's own reads;
//! - the peer must take the response at [`RANGE_MIN_SERVE_RATE`] or faster,
//!   measured from the moment its request is complete and over the time the
//!   response's writes wait for it, with [`RANGE_FIRST_WRITE_TIMEOUT`] to
//!   start and at most [`RANGE_PROGRESS_WINDOW`] of slack later (see
//!   [`Pace`]). A write is abandoned at the moment the peer falls behind, so
//!   a reader that grants no flow-control credit holds its permits for
//!   seconds;
//! - the whole stream must be done within [`RANGE_STREAM_TIMEOUT`] of being
//!   accepted, so no stream holds a permit indefinitely.
//!
//! A response that misses any of them is abandoned like any other: reset
//! with `REFUSED` while nothing is written, and `CANCELLED` once it is (see
//! the parent module).
//!
//! # Security
//!
//! A request costs the peer 41 bytes and can ask for 64 MiB, so range
//! streams are bounded on their own, separately from the
//! [`INBOUND_REQUEST_CONCURRENCY`](super::INBOUND_REQUEST_CONCURRENCY)
//! permits of the other request types, which range streams would otherwise
//! starve: at most [`RANGE_STREAMS_PER_PEER`] per peer and
//! [`RANGE_STREAMS_GLOBAL`] per node. Excess streams are refused at once with
//! `BUSY`, never queued, so the requester asks another peer and may ask this
//! node again later. `REFUSED` is kept for a node that does not serve ranges. A stream
//! takes its peer permit when its type byte is read, and its node permit
//! only once its request is decoded, so streams that never send a request
//! hold only their own peer's permits. Each stream
//! holds at most one chunk of blocks, so the memory and disk reads of range
//! serving are bounded whatever the number of peers.

use std::{sync::Arc, time::Duration};

use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::Instant,
};
use tower::Service;

use zebra_chain::{block, parameters::GENESIS_PREVIOUS_BLOCK_HASH, serialization::ZcashSerialize};

use crate::{
    constants::REQUEST_TIMEOUT,
    protocol::{
        internal::{Request, Response},
        v2::{
            request::GetBlockRange,
            response::block_range::{
                write_block_range_answer, write_block_range_element, BlockRangeAnswer,
            },
            types::{ErrorCode, StreamType},
            wire::WireError,
        },
    },
    BoxError,
};

use super::{call_service, Inbound, Outcome, ResponseSend};

/// How many `get-block-range` streams one peer may have served at once; the
/// rest are refused rather than queued.
///
/// The requester's own bound on its bulk streams to one peer is the same, so
/// a conforming requester is never refused for this limit.
pub(crate) const RANGE_STREAMS_PER_PEER: usize = 2;

/// How many `get-block-range` streams this node serves at once, over all its
/// peers; the rest are refused rather than queued.
///
/// # Security
///
/// Each served stream holds at most one chunk of blocks, at most
/// [`RANGE_READ_CHUNK`] of serialized blocks plus the chunk's first block,
/// and two buffers of up to one block's serialization each (the block being
/// written, and its response element). At the 2 MB block size limit that is
/// at most about 8 MB of serialized block data per stream, plus the
/// in-memory overhead of the chunk's deserialized blocks, which are dropped
/// as they are written. So this bounds the memory and the state reads of
/// range serving, whatever the number of peers.
pub(crate) const RANGE_STREAMS_GLOBAL: usize = 8;

/// The most serialized bytes of blocks one inbound service call reads for a
/// `get-block-range` response, the call's first block exempt.
pub(crate) const RANGE_READ_CHUNK: u32 = 2 * 1024 * 1024;

/// The most blocks one inbound service call reads for a `get-block-range`
/// response.
pub(crate) const RANGE_READ_CHUNK_BLOCKS: u32 = 64;

/// How long after its stream was accepted a `get-block-range` response must
/// be complete, after which it is abandoned.
///
/// At the draft's 64 MiB maximum, this serves a reader of about 220 KiB/s.
pub(crate) const RANGE_STREAM_TIMEOUT: Duration = Duration::from_secs(300);

/// The lowest rate, in response bytes per second, at which a peer must take
/// a `get-block-range` response, below which the response is abandoned (see
/// [`Pace`]).
///
/// # Security
///
/// Without it, a reader could take each write just within a deadline and
/// hold a node permit for the whole [`RANGE_STREAM_TIMEOUT`] at almost no
/// cost: a few such peers would deny range serving to every other peer.
/// 64 KiB/s is far below the rate a bulk sync requester needs, so this only
/// abandons responses that are of little use to the requester anyway.
pub(crate) const RANGE_MIN_SERVE_RATE: u64 = 64 * 1024;

/// How long the peer has, from the moment its `get-block-range` request is
/// complete, to start taking the response: the time the first write may wait
/// for the peer beyond what its bytes take at [`RANGE_MIN_SERVE_RATE`].
///
/// # Security
///
/// A peer that sends a request and grants no stream flow-control credit
/// blocks the first write. This is how long it holds a node permit for the
/// 41 bytes of its request, so it is a few seconds, not a request deadline.
pub(crate) const RANGE_FIRST_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// The most slack a `get-block-range` reader that is ahead of
/// [`RANGE_MIN_SERVE_RATE`] keeps: how long it may then stop reading before
/// the response is abandoned. It rides out a short network stall without
/// letting a reader that started fast hold a permit while it reads nothing.
pub(crate) const RANGE_PROGRESS_WINDOW: Duration = Duration::from_secs(10);

/// Returns the node-wide `get-block-range` stream permits,
/// [`RANGE_STREAMS_GLOBAL`] of them, to be shared by every version 2
/// connection.
pub(crate) fn global_block_range_permits() -> Arc<Semaphore> {
    Arc::new(Semaphore::new(RANGE_STREAMS_GLOBAL))
}

/// What serving one peer's `get-block-range` requests needs from its
/// connection.
#[derive(Clone, Debug)]
pub(crate) struct BlockRangeServing {
    /// Whether this node serves `get-block-range` requests at all
    /// (`network.v2_serve_block_ranges`).
    enabled: bool,

    /// The peer's permits, [`RANGE_STREAMS_PER_PEER`] of them.
    peer_permits: Arc<Semaphore>,

    /// The node's permits, shared by every connection (see
    /// [`global_block_range_permits`]).
    global_permits: Arc<Semaphore>,

    /// How long after its stream was accepted a response must be complete:
    /// [`RANGE_STREAM_TIMEOUT`], except in tests.
    stream_timeout: Duration,

    /// How fast the peer must take a response: [`RANGE_MIN_SERVE_RATE`],
    /// with [`RANGE_FIRST_WRITE_TIMEOUT`] to start and
    /// [`RANGE_PROGRESS_WINDOW`] of slack, except in tests.
    min_progress: MinProgress,
}

/// The least progress the reader of a `get-block-range` response must make.
#[derive(Copy, Clone, Debug)]
pub(super) struct MinProgress {
    /// The lowest rate, in bytes per second; zero for no rate floor, where
    /// each write may wait for `window`.
    rate: u64,
    /// How long the first write may wait beyond its bytes' time at `rate`.
    first: Duration,
    /// The most slack a reader ahead of `rate` keeps.
    window: Duration,
}

/// How far ahead of its [`MinProgress`] the reader of one response is.
///
/// The peer is only on the clock while a write waits for it, so the time
/// this node takes to read blocks is never counted against the peer. Each
/// write is allowed the time its bytes take at the floor rate, plus the
/// slack the reader has left, and is abandoned at the moment that runs out:
/// a write that blocks is cut off when the peer falls behind, not at the next
/// check between writes.
#[derive(Debug)]
struct Pace {
    min_progress: MinProgress,
    /// How much longer than the floor rate allows the writes so far could
    /// have waited: the slack the next write starts with.
    slack: Duration,
    /// Whether a write was abandoned because the peer fell behind.
    stalled: bool,
}

impl Pace {
    /// The pace of a response whose request has just been completed.
    fn new(min_progress: MinProgress) -> Self {
        Self {
            min_progress,
            slack: min_progress.first,
            stalled: false,
        }
    }

    /// How long a write of `len` bytes may wait for the peer.
    fn allowance(&self, len: usize) -> Duration {
        let MinProgress { rate, window, .. } = self.min_progress;
        if rate == 0 {
            return window;
        }
        let len = u64::try_from(len).unwrap_or(u64::MAX);
        let at_rate = Duration::from_micros(len.saturating_mul(1_000_000) / rate);
        self.slack.min(window).saturating_add(at_rate)
    }
}

impl BlockRangeServing {
    /// Returns the serving state of a new connection: `enabled` from the
    /// config, fresh permits for the peer, and the node's `global_permits`.
    pub(crate) fn new(enabled: bool, global_permits: Arc<Semaphore>) -> Self {
        Self {
            enabled,
            peer_permits: Arc::new(Semaphore::new(RANGE_STREAMS_PER_PEER)),
            global_permits,
            stream_timeout: RANGE_STREAM_TIMEOUT,
            min_progress: MinProgress {
                rate: RANGE_MIN_SERVE_RATE,
                first: RANGE_FIRST_WRITE_TIMEOUT,
                window: RANGE_PROGRESS_WINDOW,
            },
        }
    }

    /// Returns these serving settings with a whole-stream deadline of
    /// `stream_timeout` instead of [`RANGE_STREAM_TIMEOUT`].
    #[cfg(test)]
    pub(crate) fn with_stream_timeout(mut self, stream_timeout: Duration) -> Self {
        self.stream_timeout = stream_timeout;
        self
    }

    /// Returns these serving settings with a minimum rate of `rate` bytes
    /// per second, and `window` both for the first write and as the most
    /// slack, instead of [`RANGE_MIN_SERVE_RATE`],
    /// [`RANGE_FIRST_WRITE_TIMEOUT`] and [`RANGE_PROGRESS_WINDOW`]. A `rate`
    /// of zero lets each write wait for `window`.
    #[cfg(test)]
    pub(crate) fn with_min_progress(mut self, rate: u64, window: Duration) -> Self {
        self.min_progress = MinProgress {
            rate,
            first: window,
            window,
        };
        self
    }

    /// How long after its stream was accepted a response must be complete.
    pub(super) fn stream_timeout(&self) -> Duration {
        self.stream_timeout
    }

    /// How fast the peer must take a response.
    pub(super) fn min_progress(&self) -> MinProgress {
        self.min_progress
    }

    /// Claims a peer permit for one `get-block-range` stream, before its
    /// request is read, or refuses the stream: with `REFUSED` if serving is
    /// disabled, and with `BUSY` if the peer's permits are exhausted. Never
    /// waits.
    pub(super) fn claim_peer(&self) -> Result<PeerRangePermit, Outcome> {
        if !self.enabled {
            tracing::debug!("refusing get-block-range stream: serving is disabled");
            return Err(Outcome::Refuse(ErrorCode::Refused));
        }
        let peer = self
            .peer_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| busy("peer limit"))?;
        Ok(PeerRangePermit(peer))
    }

    /// Adds a node permit to the `peer` permit of a stream whose request was
    /// read and decoded, or refuses the stream with `BUSY` if the node's
    /// permits are exhausted. Never waits.
    ///
    /// # Security
    ///
    /// The node's permits are only claimed for a complete request, so a peer
    /// that opens range streams and never sends their requests holds only its
    /// own permits, and cannot deny range serving to every other peer.
    pub(super) fn claim_node(&self, peer: PeerRangePermit) -> Result<RangePermits, Outcome> {
        let node = self
            .global_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| busy("node limit"))?;
        Ok(RangePermits {
            _peer: peer.0,
            _node: node,
        })
    }
}

/// Refuses a `get-block-range` stream this node would serve if it were not at
/// a limit, for `reason`: `BUSY` asks the requester to retry after a delay,
/// where `REFUSED` would ask it not to repeat the request.
fn busy(reason: &'static str) -> Outcome {
    tracing::debug!(reason, "refusing get-block-range stream as busy");
    Outcome::Refuse(ErrorCode::Busy)
}

/// The peer permit a `get-block-range` stream holds while its request is
/// read.
#[derive(Debug)]
pub(super) struct PeerRangePermit(OwnedSemaphorePermit);

/// The permits one served `get-block-range` stream holds until it ends.
#[derive(Debug)]
pub(super) struct RangePermits {
    _peer: OwnedSemaphorePermit,
    _node: OwnedSemaphorePermit,
}

/// A decoded `get-block-range` request waiting to be served, with the
/// permits it was admitted with.
#[derive(Debug)]
pub(super) struct PendingRange {
    /// The request.
    pub(super) request: GetBlockRange,
    /// Held until the response ends.
    pub(super) permits: RangePermits,
}

/// Serves a `get-block-range` request, writing the whole response and
/// finishing the stream, by `stream_deadline` at the latest, each inbound
/// service call within [`REQUEST_TIMEOUT`], and with the peer taking it at
/// `min_progress` or faster.
///
/// The response is finished early, which the draft allows (the requester
/// resumes from the last block's parent), at genesis, when the inbound
/// service has no more ancestors (a missing or pruned body), and when an
/// inbound service call fails, or does not answer with a block range, after
/// some blocks were written. If that happens on the first call, the stream
/// is refused instead.
pub(super) async fn serve_block_range<S>(
    response: &mut ResponseSend,
    range: PendingRange,
    service: Inbound<'_, S>,
    stream_deadline: Instant,
    min_progress: MinProgress,
) -> Result<(), Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError> + Clone,
{
    let PendingRange {
        request,
        permits: _permits,
    } = range;
    let mut progress = Progress {
        next: request.final_hash,
        delivered: 0,
        delivered_bytes: 0,
        raw: Vec::new(),
        element: Vec::new(),
    };
    let mut writer = RangeWriter {
        response,
        stream_deadline,
        pace: Pace::new(min_progress),
    };

    // `count = 0` is a valid request, answered with no blocks.
    if request.count == 0 {
        let mut answer = Vec::with_capacity(1);
        write_block_range_answer(&mut answer, BlockRangeAnswer::Found)?;
        writer.write(&answer).await?;
    }

    while progress.delivered < request.count {
        match serve_chunk(&mut writer, &request, service.clone(), &mut progress).await? {
            Continue::More => {}
            Continue::Done => break,
        }
    }

    metrics::counter!(
        "zebra.net.out.responses",
        "command" => StreamType::GetBlockRange.name(),
    )
    .increment(1);

    // `finish` only fails if the stream was already closed, which the peer may do at will.
    let _ = writer.response.send.finish();
    Ok(())
}

/// The response stream of one `get-block-range` request, with the bounds on
/// how long its writes may wait for the peer.
struct RangeWriter<'a> {
    response: &'a mut ResponseSend,
    /// When the whole response must be complete.
    stream_deadline: Instant,
    /// How far ahead of its minimum progress the peer is.
    pace: Pace,
}

impl RangeWriter<'_> {
    /// Writes `bytes` to the response stream, or abandons the response if
    /// the peer does not take them within its [`Pace`], or by the stream
    /// deadline.
    async fn write(&mut self, bytes: &[u8]) -> Result<(), Outcome> {
        let allowance = self.pace.allowance(bytes.len());
        let started = Instant::now();
        let behind_at = started + allowance;
        let written = tokio::time::timeout_at(
            behind_at.min(self.stream_deadline),
            self.response.write(bytes),
        )
        .await;
        match written {
            Ok(result) => {
                self.pace.slack = allowance.saturating_sub(started.elapsed());
                result
            }
            Err(_elapsed) => {
                self.pace.stalled = behind_at <= self.stream_deadline;
                tracing::debug!(
                    stalled = self.pace.stalled,
                    ?allowance,
                    "get-block-range response was not taken in time, abandoning it"
                );
                Err(Outcome::Refuse(self.response.abandon_code()))
            }
        }
    }
}

/// How far a `get-block-range` response has got.
struct Progress {
    /// The hash of the next block to send.
    next: block::Hash,
    /// How many blocks were written.
    delivered: u32,
    /// How many serialized block bytes were written, the first block
    /// included.
    delivered_bytes: u64,
    /// A reused buffer for one block's serialization.
    raw: Vec<u8>,
    /// A reused buffer for one response element.
    element: Vec<u8>,
}

/// Whether a `get-block-range` response continues after a chunk.
enum Continue {
    /// Read and write the next chunk.
    More,
    /// The response is complete: finish the stream.
    Done,
}

/// Reads one chunk of the range from the inbound service and writes it.
async fn serve_chunk<S>(
    writer: &mut RangeWriter<'_>,
    request: &GetBlockRange,
    service: Inbound<'_, S>,
    progress: &mut Progress,
) -> Result<Continue, Outcome>
where
    S: Service<Request, Response = Response, Error = BoxError>,
{
    let first = progress.delivered == 0;
    let count = request
        .count
        .saturating_sub(progress.delivered)
        .min(RANGE_READ_CHUNK_BLOCKS);
    // # Security
    //
    // Every block after the first must fit in what is left of `max_bytes`, so no read asks for
    // more than that: a request with a small `max_bytes` makes a small read, not a whole chunk.
    // The inbound service always returns a read's first block, so the response's first block is
    // sent whatever `max_bytes` is.
    let left = u64::from(request.max_bytes).saturating_sub(progress.delivered_bytes);
    if !first && left == 0 {
        return Ok(Continue::Done);
    }
    let max_bytes = u32::try_from(left)
        .expect("left is at most request.max_bytes, which is a u32")
        .min(RANGE_READ_CHUNK);

    // # Security
    //
    // The read is bounded on its own, so a read that never answers does not hold the stream's
    // permits until the stream deadline. The peer is not on the clock while it runs.
    let read_deadline = (Instant::now() + REQUEST_TIMEOUT).min(writer.stream_deadline);
    let answer = tokio::time::timeout_at(
        read_deadline,
        call_service(
            service,
            Request::BlockRange {
                from_hash: progress.next,
                count,
                max_bytes,
            },
        ),
    )
    .await
    .map_err(|_elapsed| {
        tracing::debug!(
            delivered = progress.delivered,
            delivered_bytes = progress.delivered_bytes,
            "get-block-range read missed its deadline, abandoning the response"
        );
        Outcome::Refuse(writer.response.abandon_code())
    })?;
    let blocks = match answer {
        Ok(Response::BlockRange(blocks)) => blocks,
        // Any other answer, such as the `Nil` of an inbound service that is still starting, says
        // nothing about whether this node has the blocks, so it must not be answered as not
        // found: the stream is refused, and the requester asks another peer.
        Ok(other) if first => {
            tracing::debug!(
                response = %other,
                "inbound service did not answer a block range read, refusing the stream"
            );
            return Err(Outcome::Refuse(ErrorCode::Refused));
        }
        // Blocks are already written: finish early so the requester keeps them.
        Ok(other) => {
            tracing::debug!(
                response = %other,
                "inbound service did not answer a block range read part way, finishing early"
            );
            return Ok(Continue::Done);
        }
        // Blocks are already written: finish early so the requester keeps them.
        Err(Outcome::Refuse(code)) if !first => {
            tracing::debug!(%code, "block range read failed part way, finishing early");
            return Ok(Continue::Done);
        }
        Err(error) => return Err(error),
    };

    if blocks.is_empty() {
        if first {
            // This node does not have `final_hash`: a not-found answer, with nothing following.
            progress.element.clear();
            write_block_range_answer(&mut progress.element, BlockRangeAnswer::NotFound)?;
            writer.write(&progress.element).await?;
        }
        // Otherwise the inbound service has no more ancestors: finish early.
        return Ok(Continue::Done);
    }

    // Each block is dropped once it is written, so a chunk's memory is freed as it goes.
    // `count` is at most `RANGE_READ_CHUNK_BLOCKS`, which fits in any `usize`.
    for block in blocks.into_iter().take(count as usize) {
        // # Correctness
        //
        // The inbound service returns an ancestor chain, but a block that does not chain would
        // make the requester close the connection and blame this node, so the response is
        // finished before it instead.
        if block.hash() != progress.next {
            tracing::warn!(
                expected = %progress.next,
                got = %block.hash(),
                "inbound service returned a block outside the requested ancestor chain, \
                 finishing the get-block-range response early"
            );
            if progress.delivered == 0 {
                progress.element.clear();
                write_block_range_answer(&mut progress.element, BlockRangeAnswer::NotFound)?;
                writer.write(&progress.element).await?;
            }
            return Ok(Continue::Done);
        }

        progress.raw.clear();
        block
            .zcash_serialize(&mut progress.raw)
            .map_err(WireError::Io)?;
        // `usize` fits in `u64` on every supported platform.
        let block_len = progress.raw.len() as u64;

        // Both bounds are exact, but the first block is always sent (draft, "`get-block-range`").
        if progress.delivered > 0
            && progress.delivered_bytes.saturating_add(block_len) > u64::from(request.max_bytes)
        {
            return Ok(Continue::Done);
        }

        progress.element.clear();
        if progress.delivered == 0 {
            write_block_range_answer(&mut progress.element, BlockRangeAnswer::Found)?;
        }
        write_block_range_element(&mut progress.element, &progress.raw)?;
        writer.write(&progress.element).await?;

        progress.delivered += 1;
        progress.delivered_bytes = progress.delivered_bytes.saturating_add(block_len);
        progress.next = block.header.previous_block_hash;

        // The genesis block has no parent to continue from.
        if progress.next == GENESIS_PREVIOUS_BLOCK_HASH {
            return Ok(Continue::Done);
        }
    }

    Ok(if progress.delivered < request.count {
        Continue::More
    } else {
        Continue::Done
    })
}
