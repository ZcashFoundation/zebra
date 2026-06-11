//! The known-hash IBD engine task.
//!
//! One task owns everything: a fixed-capacity ring window over the
//! uncommitted height range and one [`FuturesUnordered`] of per-block staged
//! futures. There are no internal data channels and no second stateful task,
//! so there is a single byte-accounting point for the memory bounds in
//! `docs/design/known-hash-ibd.md` §3.3.
//!
//! This is the D1 skeleton: the window, slot states, and constants are in
//! place, but the engine loop (refill, tiered fetch-ahead, gap hedging, and
//! the stall ladder, design doc §4.1) lands in later Phase D commits.

use std::{collections::VecDeque, pin::Pin, sync::Arc, time::Duration};

use futures::{future::AbortHandle, stream::FuturesUnordered, Future};
use tokio::time::Instant;
use tower::Service;

use zebra_chain::block::{self, Block, MAX_BLOCK_BYTES};
use zebra_network as zn;
use zebra_state as zs;

use super::{DeclineReason, IbdOutcome};
use crate::BoxError;

/// The maximum number of blocks requested in one batched `BlocksByHash`
/// request.
///
/// This is the wire serving limit (`GETDATA_MAX_BLOCK_COUNT`): overflow is
/// **silently dropped** by serving nodes, so batches must never exceed it.
pub const IBD_BATCH_MAX_BLOCKS: usize = 16;

/// The batch layer's `max_items_weight_in_batch`: the size-hint weight at
/// which a batch is flushed.
///
/// This is a flush-after-crossing threshold: all but the last item in a batch
/// sum to under this weight, so a flushed batch's weight is under the
/// threshold plus at most one block. The 800 KB value leaves a 200 KB margin
/// under zebrad's 1 MB serving limit for size-hint quantization, so every
/// prefix of an honest batch except the final block stays servable in full.
///
/// `usize` to match `tower_batch_control::RequestWeight`.
pub const IBD_BATCH_MAX_WEIGHT: usize = 800_000;

/// The batch layer's `max_latency`: how long a partially-full batch waits
/// before being flushed.
///
/// Fills batches without adding meaningful per-block latency.
pub const IBD_BATCH_FLUSH_LATENCY: Duration = Duration::from_millis(10);

/// The size-hint quantum, in bytes.
///
/// A size hint `w` (1..=255) means the block's serialized size is at most
/// `w × SIZE_HINT_UNIT`, so hints are a priori upper bounds: byte budgets
/// reserved from hints can only over-reserve, never under-count.
pub const SIZE_HINT_UNIT: u64 = MAX_BLOCK_BYTES.div_ceil(255);

// The spec pins `SIZE_HINT_UNIT` at 7,844 bytes (design doc §3.2), and the
// maximum hint must cover the largest possible block; a change to
// `MAX_BLOCK_BYTES` would silently change every size-hint bound.
const _: () = assert!(SIZE_HINT_UNIT == 7_844);
const _: () = assert!(255 * SIZE_HINT_UNIT >= MAX_BLOCK_BYTES);

/// The hard cap on the window's height span, and the ring's capacity.
///
/// Allocated once at startup, the ring never grows. This bounds the slot
/// metadata (~40 B per slot, ~2.6 MB total) and the auto-scaled lookahead
/// regardless of how small blocks are.
pub const IBD_SPAN_MAX: usize = 65_536;

/// The maximum number of unresolved verify-and-commit futures.
///
/// Commit futures resolve only after the database write, so this cap is the
/// only real state backpressure signal during IBD.
pub const IBD_COMMIT_PIPELINE_BLOCKS: usize = 1_024;

/// The byte bound on unresolved verify-and-commit futures.
pub const IBD_COMMIT_PIPELINE_BYTES: usize = 64 * 1024 * 1024;

/// The number of heights above the commit frontier treated as gap-critical.
///
/// In-flight blocks in this span block the frontier directly, so they are
/// eligible for hedged refetches.
pub const IBD_FRONTIER_CRITICAL_SPAN: u32 = 64;

/// The default age of a frontier-critical in-flight slot before the engine
/// hedges it with a single-hash refetch from a different peer.
///
/// Overridable via the `sync.known_hash_gap_hedge_secs` config field.
pub const IBD_GAP_HEDGE_AFTER: Duration = Duration::from_secs(5);

/// The base of the per-height retry backoff:
/// `IBD_HEIGHT_RETRY_BACKOFF_BASE × 2^attempts`, capped at
/// [`IBD_HEIGHT_RETRY_BACKOFF_CAP`].
///
/// Paces retries of heights that keep failing without stalling recovery of
/// transient losses.
pub const IBD_HEIGHT_RETRY_BACKOFF_BASE: Duration = Duration::from_millis(500);

/// The cap on the per-height retry backoff.
pub const IBD_HEIGHT_RETRY_BACKOFF_CAP: Duration = Duration::from_secs(30);

/// The number of times a block may fail its state commit before the failure
/// is treated as deterministic and fatal (design doc §4.6).
pub const COMMIT_FAILURE_ATTEMPT_LIMIT: u8 = 3;

/// Returns the batch layer's `max_concurrent_batches` limit for the current
/// number of ready peers.
///
/// About 3× oversubscription keeps every freed connection instantly busy;
/// the cap bounds worst-case in-flight response bytes (design doc §3.3).
pub fn ibd_max_concurrent_batches(ready_peers: usize) -> usize {
    ready_peers.saturating_mul(3).min(96)
}

/// The state of one height in the engine's ring window.
///
/// Every uncommitted height in `[base, base + window.len())` has exactly one
/// slot, so duplicate fetch results and hedge losers always collapse onto a
/// single accounting point.
#[derive(Debug)]
pub enum Slot {
    /// The block is not currently requested: not yet issued, or returned here
    /// by a failed fetch awaiting its retry backoff.
    Unrequested {
        /// The number of fetch attempts so far.
        attempts: u8,
        /// The number of explicit peer `notfound` responses so far.
        not_founds: u8,
        /// The earliest time the next fetch may be issued, if backing off.
        backoff_until: Option<Instant>,
    },

    /// A fetch future for this height is in the engine's future collection.
    InFlight {
        /// The number of fetch attempts so far, including this one.
        attempts: u8,
        /// When the current fetch was issued; drives gap hedging.
        since: Instant,
        /// Aborts the hedged single-hash refetch, if one was issued.
        hedged: Option<AbortHandle>,
        /// Aborts the primary in-flight fetch.
        abort: AbortHandle,
    },

    /// The fetch stage is done; the verify-and-commit continuation is pending
    /// or in flight.
    ///
    /// The block is retained until its commit resolves, for commit-reset
    /// recovery (design doc §4.6) — it is the same [`Arc`] the state queue
    /// holds, so retention costs no extra memory.
    Fetched {
        /// The fetched block.
        block: Arc<Block>,
        /// The block's exact serialized size.
        bytes: u32,
    },

    /// The block is in the disk overflow tier (design doc §4.5); it is never
    /// re-requested from the network.
    Cached {
        /// The block's exact serialized size.
        bytes: u32,
    },
}

/// A per-block staged future in the engine's single future collection.
///
/// TODO(known-hash-ibd D2): replace the placeholder `()` payload with the
/// fetch/commit stage outcomes from design doc §4.1.
pub type BlockFut = Pin<Box<dyn Future<Output = (block::Height, ())> + Send>>;

/// The known-hash IBD engine: a ring window over the uncommitted height
/// range, and one collection of per-block staged futures.
pub struct Engine<ZN, ZS>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZN::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZS::Future: Send,
{
    /// The ring window: slot `i` tracks height `base + i`.
    ///
    /// `std`'s ring buffer, allocated once with at least [`IBD_SPAN_MAX`]
    /// capacity; the span-cap invariant means it never grows (every push is
    /// `debug_assert!`ed against the preallocated capacity).
    window: VecDeque<Slot>,

    /// The lowest uncommitted height; slots are popped from the front as the
    /// commit frontier advances.
    base: block::Height,

    /// The per-block staged futures: at most one fetch (plus one hedge) or
    /// one commit future per window slot.
    blocks: FuturesUnordered<BlockFut>,

    /// A network service which is used to download blocks by hash.
    // TODO(known-hash-ibd D2): drive the batched fetch service (design doc
    // §4.2) from this handle.
    #[allow(dead_code)]
    peer_set: ZN,

    /// A buffered state service which is used to commit verified blocks.
    // TODO(known-hash-ibd D3): drive the verify-and-commit service (design
    // doc §4.3) from this handle.
    #[allow(dead_code)]
    state: ZS,
}

impl<ZN, ZS> Engine<ZN, ZS>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZN::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZS::Future: Send,
{
    /// Returns a new engine whose window starts at `base`, the lowest
    /// uncommitted height.
    ///
    /// Allocates the full ring capacity up front: the window never grows
    /// after construction, so the engine's slot metadata is bounded for its
    /// whole lifetime.
    pub fn new(peer_set: ZN, state: ZS, base: block::Height) -> Self {
        Self {
            window: VecDeque::with_capacity(IBD_SPAN_MAX),
            base,
            blocks: FuturesUnordered::new(),
            peer_set,
            state,
        }
    }

    /// The lowest uncommitted height.
    pub fn base(&self) -> block::Height {
        self.base
    }

    /// The number of heights currently tracked by the window.
    pub fn window_len(&self) -> usize {
        self.window.len()
    }

    /// The window's allocated slot capacity (for tests and metrics).
    pub fn window_capacity(&self) -> usize {
        self.window.capacity()
    }

    /// Runs the engine until it completes the known-hash range, or returns
    /// an outcome that hands off to the legacy syncer.
    ///
    /// The D1 skeleton always declines: the engine loop lands in later
    /// Phase D commits.
    pub async fn run(self) -> Result<IbdOutcome, BoxError> {
        info!(
            base = ?self.base,
            window_len = self.window_len(),
            in_flight_blocks = self.blocks.len(),
            "known-hash IBD engine core is not yet implemented; \
             handing off to the legacy syncer",
        );

        Ok(IbdOutcome::Declined(DeclineReason::NotYetImplemented))
    }
}
