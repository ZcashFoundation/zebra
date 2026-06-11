//! The known-hash IBD engine task.
//!
//! One task owns everything: a fixed-capacity ring window over the
//! uncommitted height range and one [`FuturesUnordered`] of per-block staged
//! futures. There are no internal data channels and no second stateful task,
//! so there is a single byte-accounting point for the memory bounds in
//! `docs/design/known-hash-ibd.md` §3.3. (One control signal exists: a
//! [`watch`] publishing the memory-eligible boundary, read by in-flight
//! fetches for the tier re-check.)
//!
//! D2 state: the engine loop downloads its whole range through the weighted
//! batched fetch service (design doc §4.1–4.2) — refill with auto-scaled
//! lookahead, per-height retry policy with NotFound/transport classification,
//! gap hedging, and the stall warning ladder. The verify-and-commit stage-2
//! futures, base advancement, and the disk overflow tier land in D3–D5, so
//! [`Engine::run`] still declines; [`Engine::run_fetch_only`] drives the
//! fetch pipeline for tests.

use std::{collections::VecDeque, pin::Pin, sync::Arc, time::Duration};

use futures::{
    future::{AbortHandle, Abortable, Aborted},
    stream::FuturesUnordered,
    Future, FutureExt, StreamExt,
};
use tokio::{sync::watch, time::Instant};
use tower::{Service, ServiceExt};

use zebra_chain::{
    block::{self, Block, MAX_BLOCK_BYTES},
    parameters::known_hashes::KnownHashList,
    serialization::ZcashSerialize,
};
use zebra_network::{self as zn, PeerSetStatus, PeerSocketAddr};
use zebra_state as zs;

use super::{
    fetch::{
        self, BatchFetchResponse, BatchedFetch, FetchError, FetchFailureKind, FetchRequest,
        FetchedBlock, DEFAULT_SIZE_HINT,
    },
    peer_stats::PeerStats,
    DeclineReason, IbdOutcome, IBD_STALL_WARN_INTERVAL,
};
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
/// the cap bounds worst-case in-flight response bytes (design doc §3.3). At
/// least one batch is always allowed, so the engine can start (and issue its
/// first requests into the peer set's own readiness queue) before any peer
/// is ready.
pub fn ibd_max_concurrent_batches(ready_peers: usize) -> usize {
    ready_peers.saturating_mul(3).clamp(1, 96)
}

/// The per-height retry backoff: 500 ms × 2^attempts, capped at 30 s.
fn retry_backoff(attempts: u8) -> Duration {
    // The exponent is capped at 7 because 500 ms × 2^7 already exceeds the
    // 30 s cap, so larger shifts cannot change the result (and cannot
    // overflow the `u32` multiplier).
    let exponent = u32::from(attempts.min(7));

    IBD_HEIGHT_RETRY_BACKOFF_BASE
        .saturating_mul(1u32 << exponent)
        .min(IBD_HEIGHT_RETRY_BACKOFF_CAP)
}

/// The source of the engine's pinned hashes and size hints.
///
/// [`KnownHashList`] is the production implementation; the trait exists so
/// engine tests can run against in-memory lists built from real test-vector
/// blocks instead of the ~103 MB on-disk asset set.
pub trait HashList: Send + 'static {
    /// The highest height covered by the list.
    fn max_height(&self) -> block::Height;

    /// Returns the pinned hash for `height`, or `None` past the end of the
    /// list.
    ///
    /// Takes `&mut self` because the on-disk list loads chunks on demand.
    fn hash(&mut self, height: block::Height) -> Result<Option<block::Hash>, BoxError>;

    /// Returns the size hint for `height` (design doc §6.2), in `1..=255`.
    ///
    /// Defaults to [`DEFAULT_SIZE_HINT`] until the size-hint asset ships
    /// (TODO(known-hash-ibd E2)).
    fn size_hint(&mut self, _height: block::Height) -> u8 {
        DEFAULT_SIZE_HINT
    }

    /// Drops list resources that only cover heights below `height`.
    fn release_below(&mut self, _height: block::Height) {}
}

impl HashList for KnownHashList {
    fn max_height(&self) -> block::Height {
        KnownHashList::max_height(self)
    }

    fn hash(&mut self, height: block::Height) -> Result<Option<block::Hash>, BoxError> {
        Ok(KnownHashList::hash(self, height)?)
    }

    fn release_below(&mut self, height: block::Height) {
        KnownHashList::release_below(self, height);
    }
}

/// The window tier a block is fetched into (design doc §4.5).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Within the memory byte budget: the block is held in its window slot
    /// and flows straight to verify-and-commit.
    Memory,

    /// Beyond the memory budget: the block is saved raw to the disk overflow
    /// tier and promoted later.
    ///
    /// TODO(known-hash-ibd D5): issued by overflow fetch-ahead once
    /// `cache.rs` lands; D2 never issues past the memory budget.
    Disk,
}

/// Returns the tier a completed fetch resolves into.
///
/// This is the §4.1 tier re-check **at completion**: the frontier may have
/// advanced mid-fetch, so a height issued for the disk tier that has become
/// frontier-critical commits directly instead of round-tripping through
/// disk.
fn completion_tier(
    height: block::Height,
    memory_eligible: &watch::Receiver<block::Height>,
) -> Tier {
    if height <= *memory_eligible.borrow() {
        Tier::Memory
    } else {
        Tier::Disk
    }
}

/// Which of a slot's in-flight futures produced an outcome.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FetchOrigin {
    /// The batched stage-1 fetch.
    Primary,

    /// The single-hash gap hedge.
    Hedge,
}

/// The result of a stage-1 fetch future.
#[derive(Debug)]
pub enum FetchOutcome {
    /// The block arrived and is memory-eligible.
    Fetched {
        /// The fetched block.
        block: Arc<Block>,
        /// The delivering peer, when known.
        source: Option<PeerSocketAddr>,
    },

    // TODO(known-hash-ibd D5): `Saved { bytes }` for disk-tier fetches that
    // wrote the raw block to the overflow cache.
    /// The fetch failed with a classified error; the engine's retry policy
    /// decides what happens next.
    Failed(FetchFailureKind),
}

/// The completion payload of one per-block staged future.
#[derive(Debug)]
pub enum StageOutcome {
    /// A stage-1 fetch completed.
    Fetch {
        /// Which of the slot's futures completed.
        origin: FetchOrigin,
        /// What it resolved to.
        outcome: FetchOutcome,
    },

    // TODO(known-hash-ibd D3/D4): `Commit(CommitOutcome)` for the stage-2
    // verify-and-commit continuations.
    /// The future was cancelled: it lost a hedge race or the engine is
    /// shutting down. The slot was already updated by the winner.
    Aborted,
}

/// A per-block staged future in the engine's single future collection.
pub type BlockFut = Pin<Box<dyn Future<Output = (block::Height, StageOutcome)> + Send>>;

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
        /// The number of explicit peer `notfound` responses so far.
        not_founds: u8,
        /// When the current fetch was issued; drives gap hedging.
        since: Instant,
        /// The byte-budget reservation for this height: the size-hint upper
        /// bound, reconciled to the exact size on arrival.
        reserved_bytes: u32,
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

impl Slot {
    /// A fresh, never-attempted slot.
    fn unrequested() -> Self {
        Slot::Unrequested {
            attempts: 0,
            not_founds: 0,
            backoff_until: None,
        }
    }

    /// Is this height's block in memory or on disk?
    fn is_fetched(&self) -> bool {
        matches!(self, Slot::Fetched { .. } | Slot::Cached { .. })
    }
}

/// Merges `deadline` into the engine loop's next wake-up time.
fn merge_wake(next_wake: &mut Option<Instant>, deadline: Instant) {
    *next_wake = Some(next_wake.map_or(deadline, |wake| wake.min(deadline)));
}

/// The known-hash IBD engine: a ring window over the uncommitted height
/// range, and one collection of per-block staged futures.
pub struct Engine<ZN, ZS, L>
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
    L: HashList,
{
    /// The ring window: slot `i` tracks height `base + i`.
    ///
    /// `std`'s ring buffer, allocated once with at least [`IBD_SPAN_MAX`]
    /// capacity; the span-cap invariant means it never grows (every push is
    /// `debug_assert!`ed against the preallocated capacity).
    window: VecDeque<Slot>,

    /// The capacity allocated at startup, for the never-grows invariant.
    ring_capacity: usize,

    /// The lowest uncommitted height; slots are popped from the front as the
    /// commit frontier advances.
    base: block::Height,

    /// The per-block staged futures: at most one fetch (plus one hedge) or
    /// one commit future per window slot.
    blocks: FuturesUnordered<BlockFut>,

    /// A network service which is used to download blocks by hash.
    ///
    /// Batched fetches go through [`Self::batched_fetch`]; this direct handle
    /// is the gap-hedge path (single-hash `BlocksByHash`, design doc §4.1).
    peer_set: ZN,

    /// A buffered state service which is used to commit verified blocks.
    // TODO(known-hash-ibd D3): drive the verify-and-commit service (design
    // doc §4.3) from this handle.
    #[allow(dead_code)]
    state: ZS,

    /// The pinned hash list; owned by the engine, queried synchronously from
    /// the refill step.
    list: L,

    /// The weighted batched fetch service (design doc §4.2).
    batched_fetch: BatchedFetch<ZN>,

    /// Live peer set status, used for batch-concurrency sizing and stall
    /// diagnostics.
    peer_status: watch::Receiver<PeerSetStatus>,

    /// The memory-eligible boundary: the highest height whose block may be
    /// held in the memory tier. In-flight fetches read this for the
    /// completion tier re-check (design doc §4.1).
    memory_eligible: watch::Sender<block::Height>,

    /// The lookahead byte budget (`sync.known_hash_lookahead_bytes`).
    lookahead_bytes: u64,

    /// Bytes counted against the lookahead budget: size-hint upper bounds
    /// for in-flight heights, exact serialized sizes for fetched ones.
    budget_used: u64,

    /// How long a frontier-critical fetch may be in flight before it is
    /// hedged (`sync.known_hash_gap_hedge_secs`).
    gap_hedge_after: Duration,

    /// Per-height peer exclusions and `notfound` accounting.
    peer_stats: PeerStats,

    /// The total number of blocks fetched into the window.
    fetched_blocks: u64,

    /// The total number of gap hedges issued.
    hedge_count: u64,

    /// The window offset of the lowest non-fetched height when the fetch
    /// frontier last advanced, for stall detection.
    stall_watermark: usize,

    /// When the fetch frontier last advanced.
    frontier_progress_at: Instant,

    /// When the engine last warned about a stalled frontier.
    last_stall_warn: Option<Instant>,
}

impl<ZN, ZS, L> Engine<ZN, ZS, L>
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
    L: HashList,
{
    /// Returns a new engine whose window starts at `base`, the lowest
    /// uncommitted height, using:
    /// - `peer_set`: the Buffer'd zebra-network peer set,
    /// - `state`: the Buffer'd zebra-state service,
    /// - `list`: the verified known-hash list,
    /// - `peer_status`: the peer set's status watch (sizes the batch
    ///   concurrency; until D6 plumbs the real watch through `start.rs`, the
    ///   supervisor passes a default),
    /// - `lookahead_bytes` and `gap_hedge_after`: the
    ///   `sync.known_hash_lookahead_bytes` / `known_hash_gap_hedge_secs`
    ///   config fields.
    ///
    /// Allocates the full ring capacity up front: the window never grows
    /// after construction, so the engine's slot metadata is bounded for its
    /// whole lifetime. Must be called within a Tokio runtime (the batched
    /// fetch worker is spawned onto it).
    pub fn new(
        peer_set: ZN,
        state: ZS,
        base: block::Height,
        list: L,
        peer_status: watch::Receiver<PeerSetStatus>,
        lookahead_bytes: usize,
        gap_hedge_after: Duration,
    ) -> Self {
        let ready_peers = peer_status.borrow().ready_peers;
        let batched_fetch =
            fetch::batched_fetch(peer_set.clone(), ibd_max_concurrent_batches(ready_peers));

        let window = VecDeque::with_capacity(IBD_SPAN_MAX);
        let ring_capacity = window.capacity();

        let (memory_eligible, _) = watch::channel(base);

        let now = Instant::now();

        Self {
            window,
            ring_capacity,
            base,
            blocks: FuturesUnordered::new(),
            peer_set,
            state,
            list,
            batched_fetch,
            peer_status,
            memory_eligible,
            // usize values always fit in u64 on supported platforms
            lookahead_bytes: lookahead_bytes as u64,
            budget_used: 0,
            gap_hedge_after,
            peer_stats: PeerStats::new(now),
            fetched_blocks: 0,
            hedge_count: 0,
            stall_watermark: 0,
            frontier_progress_at: now,
            last_stall_warn: None,
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

    /// Bytes currently counted against the lookahead budget (for tests and
    /// metrics).
    pub fn budget_used(&self) -> u64 {
        self.budget_used
    }

    /// The total number of blocks fetched into the window.
    pub fn fetched_blocks(&self) -> u64 {
        self.fetched_blocks
    }

    /// The total number of gap hedges issued.
    pub fn hedge_count(&self) -> u64 {
        self.hedge_count
    }

    /// The engine's per-height peer exclusions (for tests and diagnostics).
    pub fn peer_stats(&self) -> &PeerStats {
        &self.peer_stats
    }

    /// The slot tracking `height`, if it is in the window.
    pub fn slot(&self, height: block::Height) -> Option<&Slot> {
        let offset = height.0.checked_sub(self.base.0)?;
        self.window.get(offset as usize)
    }

    /// Runs the engine until it completes the known-hash range, or returns
    /// an outcome that hands off to the legacy syncer.
    ///
    /// Still declines: the fetch pipeline (D2) is driven by
    /// [`Self::run_fetch_only`] until the verify-and-commit stage (D3) and
    /// commit pipeline (D4) replace this stub with the production loop.
    pub async fn run(self) -> Result<IbdOutcome, BoxError> {
        info!(
            base = ?self.base,
            window_len = self.window_len(),
            in_flight_blocks = self.blocks.len(),
            "known-hash IBD engine commit pipeline is not yet implemented; \
             handing off to the legacy syncer",
        );

        Ok(IbdOutcome::Declined(DeclineReason::NotYetImplemented))
    }

    /// Runs the fetch pipeline until every height in `[base, end]` is
    /// fetched into the window, without committing anything.
    ///
    /// This is the D2 slice of the engine loop: refill, retry policy, gap
    /// hedging, and stall warnings, with the fetch→commit stage boundary in
    /// place but no stage-2 futures. D3/D4 replace the all-fetched exit
    /// condition with stage-2 commit pushes in [`Self::on_fetched`] and base
    /// advancement on commit completions, without restructuring this loop.
    ///
    /// Test-only by design: production semantics never advance past a height
    /// without committing it. (Public so the fetch pipeline is reachable from
    /// the crate's test targets, but hidden: it is not part of zebrad's API.)
    #[doc(hidden)]
    pub async fn run_fetch_only(&mut self, end: block::Height) -> Result<(), BoxError> {
        assert!(
            end >= self.base && end <= HashList::max_height(&self.list),
            "fetch range must be within the window base and the list",
        );

        // `end >= base` is asserted above; the widening +1 cannot overflow.
        let target = u64::from(end.0 - self.base.0) + 1;

        loop {
            let now = Instant::now();
            let mut next_wake: Option<Instant> = None;

            self.peer_stats.maybe_rotate(now);
            self.refill(end, now, &mut next_wake)?;
            self.hedge_frontier(now, &mut next_wake)?;
            self.update_gauges();

            if self.fetched_blocks >= target {
                debug_assert!(self.window.iter().all(Slot::is_fetched));
                return Ok(());
            }

            self.track_stall(now, &mut next_wake);

            tokio::select! {
                completion = self.blocks.next(), if !self.blocks.is_empty() => {
                    if let Some((height, outcome)) = completion {
                        self.on_stage_complete(height, outcome, Instant::now());
                    }
                }

                _ = async {
                    match next_wake {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                }, if next_wake.is_some() => {}

                else => {
                    // No futures in flight and no timer to wait for, but the
                    // range is not fetched: an engine invariant is broken.
                    return Err("known-hash engine stalled with no pending work".into());
                }
            }
        }
    }

    /// Refills the window, lowest-missing-first, with auto-scaled lookahead
    /// (design doc §4.1 step 1).
    ///
    /// Issues a stage-1 fetch future for every `Unrequested` height with an
    /// expired backoff while `budget_used + hint_upper(next)` stays within
    /// the lookahead byte budget and the ring span allows. The block-count
    /// lookahead is not configured anywhere: it emerges from the byte budget
    /// and the per-block size hints.
    ///
    /// Collects the earliest pending backoff expiry into `next_wake`.
    // TODO(known-hash-ibd D4): track the lowest non-fetched offset
    // incrementally instead of rescanning, once commits move the base.
    fn refill(
        &mut self,
        end: block::Height,
        now: Instant,
        next_wake: &mut Option<Instant>,
    ) -> Result<(), BoxError> {
        // `end >= base` is checked by the caller, the +1 cannot overflow in
        // u64, and the span is clamped to the ring capacity, so it fits usize.
        let span = usize::try_from(u64::from(end.0 - self.base.0) + 1)
            .unwrap_or(usize::MAX)
            .min(self.ring_capacity);

        // Once the budget rejects the lowest missing height, stop issuing
        // (lowest-missing-first), but keep scanning for backoff deadlines.
        let mut budget_open = true;

        for offset in 0..span {
            // `offset` is bounded by the u32 height span.
            let height = block::Height(self.base.0 + offset as u32);

            if offset == self.window.len() {
                // Extending the ring: only when the new height can be issued
                // immediately, so the window never holds trailing idle slots.
                if !budget_open || !self.budget_allows(height) {
                    break;
                }

                debug_assert!(
                    self.window.len() < self.ring_capacity,
                    "span cap holds, so the preallocated ring never grows",
                );
                self.window.push_back(Slot::unrequested());
                debug_assert_eq!(
                    self.window.capacity(),
                    self.ring_capacity,
                    "the ring must never reallocate",
                );

                self.issue_fetch(offset, height, now)?;
                continue;
            }

            let Slot::Unrequested { backoff_until, .. } = &self.window[offset] else {
                continue;
            };

            if let Some(until) = backoff_until {
                if *until > now {
                    merge_wake(next_wake, *until);
                    continue;
                }
            }

            if !budget_open || !self.budget_allows(height) {
                budget_open = false;
                continue;
            }

            self.issue_fetch(offset, height, now)?;
        }

        Ok(())
    }

    /// Does the lookahead byte budget have room for `height`'s size-hint
    /// upper bound?
    fn budget_allows(&mut self, height: block::Height) -> bool {
        let hint_upper = u64::from(self.list.size_hint(height).max(1)) * SIZE_HINT_UNIT;
        self.budget_used + hint_upper <= self.lookahead_bytes
    }

    /// Issues the stage-1 fetch future for `height` and moves its slot to
    /// `InFlight`, reserving the size-hint upper bound against the budget.
    //
    // The `expect`s check internal invariants (heights pre-bounded by the
    // list, hint bounds far below `u32::MAX`); the `Result` is only for real
    // list I/O errors.
    #[allow(clippy::unwrap_in_result)]
    fn issue_fetch(
        &mut self,
        offset: usize,
        height: block::Height,
        now: Instant,
    ) -> Result<(), BoxError> {
        // Synchronous list lookups stay in the refill step by design: a
        // chunk fault reads ~5 MB from disk (~20 ms, once per 150,000
        // heights), which is cheaper than threading file I/O through the
        // per-block futures.
        let hash = self
            .list
            .hash(height)?
            .expect("height was bounded by the list's max height in refill");
        let size_hint = self.list.size_hint(height).max(1);

        let request = FetchRequest {
            height,
            hash,
            size_hint,
        };
        let reserved = request.hint_upper_bytes();

        let (attempts, not_founds) = match &self.window[offset] {
            Slot::Unrequested {
                attempts,
                not_founds,
                ..
            } => (*attempts, *not_founds),
            other => unreachable!("issue_fetch is only called on Unrequested slots: {other:?}"),
        };

        let service = self.batched_fetch.clone();
        let memory_eligible = self.memory_eligible.subscribe();
        let (abort, registration) = AbortHandle::new_pair();

        let fetch = Abortable::new(fetch_stage(service, request, memory_eligible), registration);
        self.blocks.push(
            async move {
                match fetch.await {
                    Ok(outcome) => (
                        height,
                        StageOutcome::Fetch {
                            origin: FetchOrigin::Primary,
                            outcome,
                        },
                    ),
                    Err(Aborted) => (height, StageOutcome::Aborted),
                }
            }
            .boxed(),
        );

        self.budget_used += reserved;

        // Raise the memory-eligible boundary: D2 only issues memory-tier
        // fetches, so every issued height is eligible. D5's overflow
        // fetch-ahead issues `Tier::Disk` heights *above* this boundary.
        self.memory_eligible.send_if_modified(|boundary| {
            if height > *boundary {
                *boundary = height;
                true
            } else {
                false
            }
        });

        self.window[offset] = Slot::InFlight {
            attempts: attempts.saturating_add(1),
            not_founds,
            since: now,
            // the hint upper bound is at most 255 × SIZE_HINT_UNIT ≈ 2 MB,
            // which fits in u32
            reserved_bytes: u32::try_from(reserved)
                .expect("size-hint upper bounds are far below u32::MAX"),
            hedged: None,
            abort,
        };

        Ok(())
    }

    /// Issues single-hash hedge fetches for frontier-critical heights that
    /// have been in flight too long (design doc §4.1 step 3).
    ///
    /// Collects the earliest pending hedge deadline into `next_wake`.
    //
    // The `expect` checks an internal invariant (in-window heights are
    // pre-bounded by the list); the `Result` is only for real list I/O
    // errors.
    #[allow(clippy::unwrap_in_result)]
    fn hedge_frontier(
        &mut self,
        now: Instant,
        next_wake: &mut Option<Instant>,
    ) -> Result<(), BoxError> {
        let critical_span = (IBD_FRONTIER_CRITICAL_SPAN as usize).min(self.window.len());

        let mut due = Vec::new();

        for offset in 0..critical_span {
            if let Slot::InFlight {
                since,
                hedged: None,
                ..
            } = &self.window[offset]
            {
                let deadline = *since + self.gap_hedge_after;
                if deadline <= now {
                    due.push(offset);
                } else {
                    merge_wake(next_wake, deadline);
                }
            }
        }

        for offset in due {
            // `offset` is within the frontier-critical span, so it fits u32.
            let height = block::Height(self.base.0 + offset as u32);
            let hash = self
                .list
                .hash(height)?
                .expect("in-window heights are within the list");

            // A single-hash `BlocksByHash` through the plain peer set gets
            // inventory-aware routing (`route_inv`) for free, and the
            // one-request-per-connection rule lands it on a different peer
            // than the still-pending primary by construction.
            let (abort, registration) = AbortHandle::new_pair();
            let hedge = Abortable::new(
                fetch::fetch_single(self.peer_set.clone(), height, hash),
                registration,
            );

            self.blocks.push(
                async move {
                    match hedge.await {
                        Ok(result) => (
                            height,
                            StageOutcome::Fetch {
                                origin: FetchOrigin::Hedge,
                                outcome: match result {
                                    Ok(FetchedBlock { block, source }) => {
                                        FetchOutcome::Fetched { block, source }
                                    }
                                    Err(error) => FetchOutcome::Failed(error.kind()),
                                },
                            },
                        ),
                        Err(Aborted) => (height, StageOutcome::Aborted),
                    }
                }
                .boxed(),
            );

            match &mut self.window[offset] {
                Slot::InFlight { hedged, .. } => *hedged = Some(abort),
                other => unreachable!("hedge candidates stay InFlight within this step: {other:?}"),
            }

            self.hedge_count += 1;
            metrics::counter!("ibd.gap.hedge.count").increment(1);
        }

        Ok(())
    }

    /// Applies one completed staged future to its slot.
    fn on_stage_complete(&mut self, height: block::Height, outcome: StageOutcome, now: Instant) {
        // Completions for heights the frontier already passed are stale.
        let Some(offset) = height.0.checked_sub(self.base.0) else {
            return;
        };
        // u32 always fits in usize on supported platforms
        let offset = offset as usize;
        if offset >= self.window.len() {
            return;
        }

        let StageOutcome::Fetch { origin, outcome } = outcome else {
            // Aborted: the slot was already updated by the winning future.
            return;
        };

        match outcome {
            FetchOutcome::Fetched { block, source } => {
                self.on_fetch_success(offset, height, block, source);
            }
            FetchOutcome::Failed(kind) => {
                self.on_fetch_failure(offset, height, origin, kind, now);
            }
        }
    }

    /// Stores an arrived block in its slot, reconciling the byte budget from
    /// the hint upper bound to the exact serialized size.
    fn on_fetch_success(
        &mut self,
        offset: usize,
        height: block::Height,
        block: Arc<Block>,
        source: Option<PeerSocketAddr>,
    ) {
        // One serialized-size pass per arrived block; the exact size replaces
        // the hint upper bound in the budget, which can only free budget.
        let bytes = block.zcash_serialized_size();
        // serialized blocks are bounded by the 2 MB protocol limit, enforced
        // at deserialization in zebra-network, so they fit u32 and u64
        let bytes_u32 = u32::try_from(bytes).expect("blocks are far below u32::MAX bytes");
        let bytes_u64 = u64::from(bytes_u32);

        match &mut self.window[offset] {
            Slot::InFlight {
                abort,
                hedged,
                reserved_bytes,
                ..
            } => {
                // First completion wins; cancel the loser (aborting an
                // already-completed future is a no-op).
                abort.abort();
                if let Some(hedged) = hedged {
                    hedged.abort();
                }

                self.budget_used = self
                    .budget_used
                    .checked_sub(u64::from(*reserved_bytes))
                    .expect("the reservation was added when this fetch was issued")
                    + bytes_u64;
            }
            Slot::Fetched { .. } | Slot::Cached { .. } => {
                // A lost hedge race delivered a duplicate: the single slot
                // per height is the dedup point; drop it.
                return;
            }
            Slot::Unrequested { .. } => {
                // The other future failed and reset the slot (releasing the
                // reservation) before this one delivered: accept the block.
                self.budget_used += bytes_u64;
            }
        }

        self.window[offset] = Slot::Fetched {
            block,
            bytes: bytes_u32,
        };
        self.fetched_blocks += 1;

        metrics::counter!("ibd.fetched.block.count").increment(1);
        // Compatibility increment so existing sync dashboards keep working
        // (design doc §8).
        metrics::counter!("sync.downloaded.block.count").increment(1);

        self.on_fetched(height, source);
    }

    /// Hook called when a height's fetch stage completes into its slot.
    ///
    /// TODO(known-hash-ibd D3/D4): push the stage-2 verify-and-commit
    /// continuation here while the commit pipeline caps
    /// ([`IBD_COMMIT_PIPELINE_BLOCKS`] / [`IBD_COMMIT_PIPELINE_BYTES`]) have
    /// room (carrying `source` for misbehavior attribution), retain the slot
    /// until the commit resolves, and advance `base` (with
    /// `list.release_below` and `peer_stats.release_below`) as commits
    /// complete. D2 stops at the stage boundary.
    fn on_fetched(&mut self, _height: block::Height, _source: Option<PeerSocketAddr>) {}

    /// Applies the retry policy to a failed fetch (design doc §4.1):
    /// `NotFound` excludes the reporting peer for this height; transport
    /// losses exclude no one; either way the height backs off and reassigns.
    fn on_fetch_failure(
        &mut self,
        offset: usize,
        height: block::Height,
        origin: FetchOrigin,
        kind: FetchFailureKind,
        now: Instant,
    ) {
        if let FetchFailureKind::NotFound { peer } = kind {
            self.peer_stats.record_not_found(height, peer);
        }

        match &mut self.window[offset] {
            Slot::InFlight {
                attempts,
                not_founds,
                hedged,
                abort,
                reserved_bytes,
                ..
            } => {
                if let FetchFailureKind::NotFound { .. } = kind {
                    *not_founds = not_founds.saturating_add(1);
                }

                match (origin, hedged.take()) {
                    // The primary failed but its hedge is still in flight:
                    // promote the hedge and keep the slot (and reservation).
                    (FetchOrigin::Primary, Some(hedge)) => {
                        *abort = hedge;
                    }

                    // A hedge failed while another future is still in
                    // flight: just forget the hedge handle. (When a promoted
                    // hedge and a newer hedge are both in flight, we cannot
                    // tell which one failed; either way exactly one live
                    // future remains, and its own completion settles the
                    // slot.)
                    (FetchOrigin::Hedge, Some(_)) => {}

                    // The last in-flight future for this height failed:
                    // release the reservation and back off before
                    // reassigning. (A hedge failure with `hedged: None`
                    // means the hedge had been promoted to primary.)
                    (FetchOrigin::Primary | FetchOrigin::Hedge, None) => {
                        self.budget_used = self
                            .budget_used
                            .checked_sub(u64::from(*reserved_bytes))
                            .expect("the reservation was added when this fetch was issued");

                        let attempts = *attempts;
                        let not_founds = *not_founds;

                        // TODO(known-hash-ibd D6): after 8 attempts, also
                        // send `MorePeers` demand to the address book
                        // (stall ladder step 4); the backoff cap already
                        // paces retries at that point.
                        self.window[offset] = Slot::Unrequested {
                            attempts,
                            not_founds,
                            backoff_until: Some(now + retry_backoff(attempts)),
                        };
                    }
                }
            }

            // The other future already delivered the block (or a failure
            // already reset the slot): nothing to do.
            Slot::Fetched { .. } | Slot::Cached { .. } | Slot::Unrequested { .. } => {}
        }
    }

    /// Tracks fetch-frontier progress and warns on the
    /// [`IBD_STALL_WARN_INTERVAL`] cadence while it is stalled (stall ladder
    /// step 5, design doc §4.1).
    fn track_stall(&mut self, now: Instant, next_wake: &mut Option<Instant>) {
        let lowest_missing = self.window.iter().position(|slot| !slot.is_fetched());

        let Some(lowest_missing) = lowest_missing else {
            // Nothing missing (or the window is empty): not stalled.
            self.frontier_progress_at = now;
            return;
        };

        if lowest_missing > self.stall_watermark {
            self.stall_watermark = lowest_missing;
            self.frontier_progress_at = now;
            self.last_stall_warn = None;
            return;
        }

        let stalled_for = now.duration_since(self.frontier_progress_at);
        if stalled_for < IBD_STALL_WARN_INTERVAL {
            merge_wake(
                next_wake,
                self.frontier_progress_at + IBD_STALL_WARN_INTERVAL,
            );
            return;
        }

        let warn_due = self
            .last_stall_warn
            .is_none_or(|last| now.duration_since(last) >= IBD_STALL_WARN_INTERVAL);

        if warn_due {
            // `lowest_missing` is within the u32 height span.
            let height = block::Height(self.base.0 + lowest_missing as u32);
            let (attempts, not_founds) = match &self.window[lowest_missing] {
                Slot::Unrequested {
                    attempts,
                    not_founds,
                    ..
                }
                | Slot::InFlight {
                    attempts,
                    not_founds,
                    ..
                } => (*attempts, *not_founds),
                Slot::Fetched { .. } | Slot::Cached { .. } => (0, 0),
            };
            let ready_peers = self.peer_status.borrow().ready_peers;

            warn!(
                ?height,
                attempts,
                not_founds,
                ready_peers,
                stalled_secs = stalled_for.as_secs(),
                "known-hash IBD fetch frontier is stalled",
            );
            metrics::gauge!("ibd.stall.seconds").set(stalled_for.as_secs_f64());

            self.last_stall_warn = Some(now);
        }

        let last = self.last_stall_warn.unwrap_or(now);
        merge_wake(next_wake, last + IBD_STALL_WARN_INTERVAL);
    }

    /// Publishes the window gauges (design doc §8).
    fn update_gauges(&self) {
        metrics::gauge!("ibd.window.bytes").set(self.budget_used as f64);
        metrics::gauge!("ibd.lookahead.blocks").set(self.window.len() as f64);
    }
}

/// The stage-1 fetch future body: one weighted batched fetch, then the tier
/// re-check at completion (design doc §4.1).
async fn fetch_stage<S>(
    service: S,
    request: FetchRequest,
    memory_eligible: watch::Receiver<block::Height>,
) -> FetchOutcome
where
    S: Service<FetchRequest, Response = BatchFetchResponse, Error = BoxError> + Send + 'static,
    S::Future: Send,
{
    let height = request.height;

    match service.oneshot(request).await {
        Ok(BatchFetchResponse::Fetched(FetchedBlock { block, source })) => {
            match completion_tier(height, &memory_eligible) {
                Tier::Memory => FetchOutcome::Fetched { block, source },
                // TODO(known-hash-ibd D5): save the raw block to the disk
                // overflow cache and resolve `Saved { bytes }`. D2 never
                // issues past the memory-eligible boundary, so this arm is
                // unreachable until overflow fetch-ahead lands; holding the
                // block in its slot keeps it correct even if it were hit.
                Tier::Disk => FetchOutcome::Fetched { block, source },
            }
        }
        Ok(BatchFetchResponse::Flushed) => {
            debug_assert!(false, "per-item calls never resolve `Flushed`");
            FetchOutcome::Failed(FetchFailureKind::Transport)
        }
        Err(error) => FetchOutcome::Failed(FetchError::classify(&error)),
    }
}
