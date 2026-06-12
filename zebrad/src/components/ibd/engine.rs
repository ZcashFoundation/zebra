//! The known-hash IBD engine task.
//!
//! One task owns everything: a fixed-capacity ring window over the
//! uncommitted height range and one [`FuturesUnordered`] of per-block staged
//! futures. There are no internal data channels and no second stateful task,
//! so there is a single byte-accounting point for the memory bounds in
//! `docs/design/known-hash-ibd.md` §3.3. (The only external control input is
//! the peer set's status [`watch`], which sizes the fetch concurrency.)
//!
//! The engine loop downloads its whole range through the weighted batched
//! fetch service (design doc §4.1–4.2) — refill with auto-scaled lookahead,
//! per-height retry policy with NotFound/transport classification, gap
//! hedging, and the stall warning ladder — and commits it in height order
//! through stage-2 verify-and-commit continuations capped by the commit
//! pipeline bounds (§4.4). Heights beyond the memory byte budget overflow to
//! the disk tier (§4.5) and are promoted back through the same
//! verify-and-commit path as commits free budget. Commit failures follow the
//! §4.6 recovery: descendants dropped by a write-thread reset are resubmitted
//! from their retained [`Arc`]s (a re-verification, never a re-download),
//! only the failed height itself is refetched, and repeated byte-identical
//! failures are a fatal diagnostic.

use std::{
    collections::{HashMap, VecDeque},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use futures::{
    future::{AbortHandle, Abortable, Aborted},
    stream::FuturesUnordered,
    Future, FutureExt, StreamExt,
};
use thiserror::Error;
use tokio::{sync::watch, time::Instant};
use tower::{Service, ServiceExt};

use zebra_chain::{
    block::{self, Block},
    parameters::{known_hashes::KnownHashList, Network, GENESIS_PREVIOUS_BLOCK_HASH},
    serialization::ZcashSerialize,
};
use zebra_network::{self as zn, PeerSetStatus, PeerSocketAddr};
use zebra_state as zs;

use super::{
    cache::BlockCache,
    convert::{BlockPayload, ConvertError, IbdBlock, VerifyAndCommit, VerifyAndCommitError},
    fetch::{
        self, BatchFetchResponse, BatchedFetch, FetchError, FetchFailureKind, FetchRequest,
        FetchedBlock, DEFAULT_SIZE_HINT,
    },
    IbdOutcome, IBD_STALL_WARN_INTERVAL,
};
use crate::BoxError;

/// The maximum number of blocks requested in one batched `BlocksByHash`
/// request.
///
/// This is the wire serving limit: overflow is **silently dropped** by serving
/// nodes, so batches must never exceed it. Aliased to the inbound serving
/// constant so the request and serving sides can't drift apart.
pub const IBD_BATCH_MAX_BLOCKS: usize = crate::components::inbound::GETDATA_MAX_BLOCK_COUNT;

/// The batch layer's `max_items_weight_in_batch`: the size-hint weight at
/// which a batch is flushed.
///
/// This is a flush-after-crossing threshold: all but the last item in a batch
/// sum to under this weight, so a flushed batch's weight is under the
/// threshold plus at most one block. Derived from the serving-side byte limit
/// ([`GETDATA_SENT_BYTES_LIMIT`](crate::components::inbound::GETDATA_SENT_BYTES_LIMIT))
/// so the two can't drift apart, minus a 200 KB margin for size-hint
/// quantization, so every prefix of an honest batch except the final block
/// stays servable in full.
///
/// `usize` to match `tower_batch_control::RequestWeight`.
pub const IBD_BATCH_MAX_WEIGHT: usize =
    crate::components::inbound::GETDATA_SENT_BYTES_LIMIT - 200_000;

/// The batch layer's `max_latency`: how long a partially-full batch waits
/// before being flushed.
///
/// Fills batches without adding meaningful per-block latency.
pub const IBD_BATCH_FLUSH_LATENCY: Duration = Duration::from_millis(10);

/// The defensive ceiling on the window's height span (slot bookkeeping only —
/// **not** a memory bound, and not policy; maintainer-directed).
///
/// The window's real bounds are the byte budgets: the RAM block-buffer budget
/// (`sync.known_hash_lookahead_bytes`) plus the disk fetch-ahead allowance
/// ([`DISK_FETCH_AHEAD_FACTOR`]). The span that fits those budgets is
/// emergent from actual block sizes. This ceiling only stops runaway slot
/// bookkeeping if accounting breaks (~40 B per slot, so ~80 MB at the
/// ceiling); it should never bind in practice.
pub const IBD_SPAN_MAX: usize = 2_000_000;

/// The disk overflow tier's fetch-ahead allowance, as a multiple of the RAM
/// block-buffer budget.
///
/// The engine keeps fetching past the RAM budget — storing arrivals on disk —
/// until RAM-held plus disk-held block bytes reach
/// `budget × (1 + DISK_FETCH_AHEAD_FACTOR)`. Proportional to the configured
/// budget, so transient disk usage scales with what the node was sized for
/// (the chain state needs that space soon anyway).
pub const DISK_FETCH_AHEAD_FACTOR: u64 = 4;

/// The maximum number of unresolved verify-and-commit futures.
///
/// Commit futures resolve only after the database write, so this cap is the
/// only real state backpressure signal during IBD.
pub const IBD_COMMIT_PIPELINE_BLOCKS: usize = 1_024;

/// The byte bound on unresolved verify-and-commit futures.
pub const IBD_COMMIT_PIPELINE_BYTES: usize = 64 * 1024 * 1024;

/// The operational ceiling on the window's block count (fetch-ahead distance).
///
/// The refill walk is `O(window length)` per loop event, so an unbounded
/// window starves the loop: in small-block eras the byte budget alone allows
/// hundreds of thousands of blocks of fetch-ahead, and the per-event rescan
/// then dominates the engine thread and *lowers* throughput as peers (and the
/// window) grow. This caps the fetch-ahead to a depth that still keeps every
/// peer and the commit pipeline fed — far more than the in-flight fetch
/// capacity plus [`IBD_COMMIT_PIPELINE_BLOCKS`] — while keeping the rescan
/// cheap. In large-block eras the byte budgets bind well below this, so it
/// only takes effect when blocks are small.
pub const IBD_WINDOW_MAX_BLOCKS: usize = 16_384;

const _: () = assert!(IBD_WINDOW_MAX_BLOCKS <= IBD_SPAN_MAX);

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

/// How many heights the commit frontier advances between batched disk-tier
/// eviction rounds (design doc §4.5).
///
/// Eviction is lazy: each round deletes every cached entry the frontier has
/// passed, so the batch interval only bounds how long committed entries
/// linger on disk, not how many are deleted.
pub const IBD_CACHE_EVICT_INTERVAL: u32 = 1_024;

/// The hard ceiling on concurrent fetch batches, regardless of peer count.
///
/// The batch layer is built once at this ceiling (the per-batch
/// `max_concurrent_batches` is fixed for the worker's lifetime), and the live
/// per-peer limit is enforced dynamically at issuance by
/// [`Engine::fetch_slots_available`]. Building the layer at a startup snapshot
/// of the peer count would freeze concurrency at whatever was ready before the
/// first handshake — typically zero, i.e. one batch for the whole run.
pub const IBD_MAX_CONCURRENT_BATCHES: usize = 96;

/// Returns the live `max_concurrent_batches` limit for the current number of
/// ready peers, used to gate issuance (not to size the batch layer).
///
/// About 3× oversubscription keeps every freed connection instantly busy;
/// the cap bounds worst-case in-flight response bytes (design doc §3.3). At
/// least one batch is always allowed, so the engine can issue its first
/// requests into the peer set's own readiness queue before any peer is ready.
pub fn ibd_max_concurrent_batches(ready_peers: usize) -> usize {
    ready_peers
        .saturating_mul(3)
        .clamp(1, IBD_MAX_CONCURRENT_BATCHES)
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

/// A fatal engine exit (design doc §4.6–4.7).
///
/// The supervisor restarts the engine only for
/// [retryable](EngineError::is_retryable) errors; diagnostics and shutdowns
/// propagate so they are never cured by silent fallback.
#[derive(Debug, Error)]
pub enum EngineError {
    /// The known-hash list (or its loader) disagrees with a block whose
    /// header hash it pins; refetching can never cure this (design doc
    /// §4.3).
    #[error("fatal known-hash list diagnostic: {0}")]
    ListDiagnostic(#[source] ConvertError),

    /// A byte-identical block failed its state commit
    /// [`COMMIT_FAILURE_ATTEMPT_LIMIT`] times: a known-hash list-vs-chain
    /// inconsistency or local database corruption (design doc §4.6).
    #[error(
        "block {hash:?} at {height:?} failed its state commit {attempts} times \
         with byte-identical copies: known-hash list inconsistency or local \
         database corruption; not retrying"
    )]
    DeterministicCommitFailure {
        /// The height of the failing block.
        height: block::Height,
        /// The pinned hash of the failing block.
        hash: block::Hash,
        /// How many times byte-identical copies failed.
        attempts: u8,
        /// The state's last commit error.
        #[source]
        error: BoxError,
    },

    /// The state write task, the state service, or the rayon pool is gone:
    /// Zebra is shutting down, or the state is broken (design doc §4.6).
    #[error("cannot verify and commit blocks; Zebra may be shutting down: {0}")]
    Shutdown(#[source] BoxError),

    /// Reading the known-hash list failed.
    #[error("known-hash list read failed: {0}")]
    List(#[source] BoxError),

    /// An engine invariant was broken, or a transient subsystem failed.
    #[error("known-hash IBD engine error: {0}")]
    Internal(String),
}

impl EngineError {
    /// Returns true when a supervisor restart might cure this error (design
    /// doc §4.7).
    ///
    /// Fatal diagnostics and shutdowns return false: restarting cannot fix a
    /// broken list or a closed state, and retrying them would be the silent
    /// fallback §4.6 forbids.
    pub fn is_retryable(&self) -> bool {
        matches!(self, EngineError::List(_) | EngineError::Internal(_))
    }
}

/// Returns true when `error`'s source chain contains the state's
/// [`zs::CommitBlockError::WriteTaskExited`].
///
/// At the commit frontier this means the write task is really gone (the
/// frontier block is always at the write thread's next valid height, so it
/// is never reset-dropped): the engine shuts down instead of refetching
/// (design doc §4.6).
fn is_write_task_exited(error: &BoxError) -> bool {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(&**error);

    while let Some(error) = current {
        if matches!(
            error.downcast_ref::<zs::CommitBlockError>(),
            Some(zs::CommitBlockError::WriteTaskExited { .. })
        ) {
            return true;
        }

        current = error.source();
    }

    false
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
    /// Defaults to [`DEFAULT_SIZE_HINT`] for heights whose chunk does not
    /// embed size hints.
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

    fn size_hint(&mut self, height: block::Height) -> u8 {
        // The hint only sizes fetch batches, so a chunk read error here is
        // not fatal: fall back to the conservative default, and let the
        // `hash` lookup for the same height surface the error.
        match KnownHashList::size_hint(self, height) {
            Ok(Some(hint)) => hint.max(1),
            Ok(None) | Err(_) => DEFAULT_SIZE_HINT,
        }
    }

    fn release_below(&mut self, height: block::Height) {
        KnownHashList::release_below(self, height);
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

/// The result of a stage-1 fetch future: the arrived block, or a classified
/// failure for the engine's retry policy.
///
/// The engine loop decides an arrived block's placement: memory while the
/// RAM block buffer has room, the disk overflow tier otherwise (design doc
/// §4.5; maintainer-directed arrival-time placement).
pub type FetchOutcome = Result<FetchedBlock, FetchFailureKind>;

/// The result of a stage-2 verify-and-commit future, resolved after the
/// database write: the committed hash, or an error the engine maps per the
/// design doc §4.3/§4.6 semantics.
pub type CommitOutcome = Result<block::Hash, VerifyAndCommitError>;

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

    /// A stage-2 verify-and-commit continuation completed.
    Commit(CommitOutcome),

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
        /// Aborts the hedged single-hash refetch, if one was issued.
        hedged: Option<AbortHandle>,
        /// Aborts the primary in-flight fetch.
        abort: AbortHandle,
    },

    /// The fetch stage is done; the verify-and-commit continuation is pending
    /// (`committing: false`) or in flight (`committing: true`).
    ///
    /// The block is retained until its commit resolves, for commit-reset
    /// recovery (design doc §4.6) — it is the same [`Arc`] the state queue
    /// holds, so retention costs no extra memory.
    Fetched {
        /// The fetched block.
        block: Arc<Block>,
        /// The block's exact serialized size.
        bytes: u32,
        /// Whether a stage-2 verify-and-commit future is in flight.
        committing: bool,
        /// The peer that delivered the block, when known; carried for
        /// commit-time attribution (design doc §4.6).
        source: Option<PeerSocketAddr>,
    },

    /// The block is in the disk overflow tier (design doc §4.5); it is never
    /// re-requested from the network. A stage-2 promotion future is in
    /// flight when `promoted` is set.
    Cached {
        /// The block's exact serialized size.
        bytes: u32,
        /// Whether a stage-2 promotion future is in flight.
        promoted: bool,
    },

    /// The block's commit resolved after the database write; the slot is
    /// popped once the contiguous committed prefix reaches it.
    Committed,
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

    /// Is this height's block in memory, on disk, or already committed?
    fn is_fetched(&self) -> bool {
        matches!(
            self,
            Slot::Fetched { .. } | Slot::Cached { .. } | Slot::Committed
        )
    }
}

/// Merges `deadline` into the engine loop's next wake-up time.
fn merge_wake(next_wake: &mut Option<Instant>, deadline: Instant) {
    *next_wake = Some(next_wake.map_or(deadline, |wake| wake.min(deadline)));
}

/// The refill action derived from a slot, so the borrow of the window ends
/// before the action mutates the engine.
enum RefillAction {
    /// Extend the ring with a new trailing height and fetch it.
    Extend,
    /// Issue a fetch for an `Unrequested` slot with an expired backoff.
    Issue,
    /// Promote a `Cached` slot through the verify-and-commit path.
    Promote {
        /// The cached block's exact serialized size.
        bytes: u32,
    },
    /// Push the stage-2 continuation for a `Fetched` slot.
    Commit {
        /// The fetched block's exact serialized size.
        bytes: u32,
    },
    /// Nothing to do for this height.
    Skip,
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
    /// `std`'s ring buffer, grown organically with the active span — the
    /// span is emergent from the byte budgets and actual block sizes
    /// (maintainer-directed), with [`IBD_SPAN_MAX`] as a defensive
    /// bookkeeping ceiling only.
    window: VecDeque<Slot>,

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

    /// The verify-and-commit service over the buffered state (design doc
    /// §4.3); one stage-2 future per block goes through it.
    verify_and_commit: VerifyAndCommit<ZS>,

    /// The pinned hash list; owned by the engine, queried synchronously from
    /// the refill step.
    list: L,

    /// The weighted batched fetch service (design doc §4.2).
    batched_fetch: BatchedFetch<ZN>,

    /// The disk overflow tier (design doc §4.5), under
    /// `<state cache_dir>/ibd-block-cache/`.
    cache: BlockCache,

    /// Live peer set status, used for batch-concurrency sizing and stall
    /// diagnostics.
    peer_status: watch::Receiver<PeerSetStatus>,

    /// The RAM block-buffer budget (`sync.known_hash_lookahead_bytes`):
    /// the configured memory for checkpoint-verified blocks awaiting commit.
    lookahead_bytes: u64,

    /// The exact serialized bytes of blocks currently held in RAM:
    /// `Fetched` slots plus blocks behind unresolved stage-2 futures
    /// (including promotions read back from disk). Updated at arrival
    /// placement and commit resolution — the live counter the
    /// maintainer-directed placement rule compares against the budget.
    budget_used: u64,

    /// The number of in-flight stage-1 fetches (`InFlight` slots).
    ///
    /// Fetch issuance is bounded by the network's useful concurrency, not by
    /// the byte budgets: placement happens at arrival.
    inflight_fetches: usize,

    /// The number of unresolved stage-2 verify-and-commit futures.
    commit_blocks: usize,

    /// The exact bytes of the blocks behind unresolved stage-2 futures.
    commit_bytes: u64,

    /// The block-count cap on unresolved stage-2 futures
    /// ([`IBD_COMMIT_PIPELINE_BLOCKS`]; overridable in tests).
    commit_pipeline_max_blocks: usize,

    /// The byte cap on unresolved stage-2 futures
    /// ([`IBD_COMMIT_PIPELINE_BYTES`]; overridable in tests).
    commit_pipeline_max_bytes: u64,

    /// Commit-failure tracking for the §4.6 deterministic-failure detector:
    /// per failing height, the byte-identical failure count and the failed
    /// copy it is compared against.
    ///
    /// Entries are dropped when their height commits or the frontier passes
    /// it, so the map is bounded by the actively-failing pipeline span.
    commit_failures: HashMap<block::Height, (u8, Option<Arc<Block>>)>,

    /// The frontier below which committed disk-tier entries were last
    /// evicted (lazily batched, design doc §4.5).
    cache_evicted_through: block::Height,

    /// How long a frontier-critical fetch may be in flight before it is
    /// hedged (`sync.known_hash_gap_hedge_secs`).
    gap_hedge_after: Duration,

    /// The total number of blocks fetched into the window (memory or disk),
    /// reported in the completion log.
    fetched_blocks: u64,

    /// The lowest non-fetched height when the fetch frontier last advanced,
    /// for stall detection.
    fetch_watermark: block::Height,

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
    /// - `network`: the configured network, for the verify stage's merkle
    ///   and branch-ID checks,
    /// - `peer_set`: the Buffer'd zebra-network peer set,
    /// - `state`: the Buffer'd zebra-state service,
    /// - `list`: the verified known-hash list,
    /// - `peer_status`: the peer set's status watch from
    ///   [`zebra_network::init`] (sizes the fetch concurrency),
    /// - `cache`: the disk overflow tier (design doc §4.5),
    /// - `lookahead_bytes` and `gap_hedge_after`: the
    ///   `sync.known_hash_lookahead_bytes` / `known_hash_gap_hedge_secs`
    ///   config fields. A `gap_hedge_after` too large for the clock
    ///   (`Duration::MAX`) disables hedging.
    ///
    /// Must be called within a Tokio runtime (the batched fetch worker is
    /// spawned onto it).
    //
    // Everything here is a distinct service handle or config value used by a
    // different engine stage; bundling them into a struct would only move
    // the argument list into a literal at the single call site.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        network: Network,
        peer_set: ZN,
        state: ZS,
        base: block::Height,
        list: L,
        peer_status: watch::Receiver<PeerSetStatus>,
        cache: BlockCache,
        lookahead_bytes: usize,
        gap_hedge_after: Duration,
    ) -> Self {
        // Size the batch layer at the hard ceiling: its `max_concurrent_batches`
        // is fixed for the worker's lifetime, and the live per-peer limit is
        // applied at issuance by `fetch_slots_available`. Sizing it from the
        // current peer count would freeze concurrency at the startup snapshot
        // (typically zero ready peers, i.e. one batch for the whole run).
        let batched_fetch = fetch::batched_fetch(peer_set.clone(), IBD_MAX_CONCURRENT_BATCHES);
        let verify_and_commit = VerifyAndCommit::new(network, state);

        let now = Instant::now();

        Self {
            window: VecDeque::new(),
            base,
            blocks: FuturesUnordered::new(),
            peer_set,
            verify_and_commit,
            list,
            batched_fetch,
            cache,
            peer_status,
            // usize values always fit in u64 on supported platforms
            lookahead_bytes: lookahead_bytes as u64,
            budget_used: 0,
            inflight_fetches: 0,
            commit_blocks: 0,
            commit_bytes: 0,
            commit_pipeline_max_blocks: IBD_COMMIT_PIPELINE_BLOCKS,
            // the constant fits u64 on all supported platforms
            commit_pipeline_max_bytes: IBD_COMMIT_PIPELINE_BYTES as u64,
            commit_failures: HashMap::new(),
            cache_evicted_through: base,
            gap_hedge_after,
            fetched_blocks: 0,
            fetch_watermark: base,
            frontier_progress_at: now,
            last_stall_warn: None,
        }
    }

    /// Runs the engine until every height through the list's max height is
    /// committed, returning [`IbdOutcome::Completed`].
    ///
    /// Restores the disk overflow tier left by a previous run, then loops:
    /// refill (promotion, stage-2 pushes, tiered fetch issuance), gap
    /// hedging, stall tracking, and one staged-future completion per
    /// iteration. Returns an [`EngineError`] for fatal diagnostics,
    /// shutdowns, and broken invariants; the supervisor decides which of
    /// those to retry (design doc §4.7).
    pub async fn run(&mut self) -> Result<IbdOutcome, EngineError> {
        let end = HashList::max_height(&self.list);

        self.restore_cache(end)?;

        loop {
            if self.base > end {
                // Every height through the end of the list is committed.
                if let Err(error) = self.cache.remove_all() {
                    warn!(
                        %error,
                        "failed to remove the IBD block cache directory at handoff",
                    );
                }

                info!(
                    final_height = ?end,
                    fetched_blocks = self.fetched_blocks,
                    "known-hash IBD engine committed every block in the list",
                );

                return Ok(IbdOutcome::Completed { final_height: end });
            }

            let now = Instant::now();
            let mut next_wake: Option<Instant> = None;

            self.refill(end, now, &mut next_wake)?;
            self.hedge_frontier(now, &mut next_wake)?;
            self.update_gauges();
            self.track_stall(now, &mut next_wake);

            tokio::select! {
                completion = self.blocks.next(), if !self.blocks.is_empty() => {
                    if let Some((height, outcome)) = completion {
                        self.on_stage_complete(height, outcome, Instant::now())?;
                    }

                    // Batch arrivals and pipelined commits resolve many
                    // futures at nearly the same time. Drain every
                    // already-ready completion before the next loop pass, so
                    // the `O(window)` refill and stall scans above run once
                    // per ready batch instead of once per future.
                    while let Some(Some((height, outcome))) = self.blocks.next().now_or_never() {
                        self.on_stage_complete(height, outcome, Instant::now())?;
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
                    // range is not committed: an engine invariant is broken.
                    return Err(EngineError::Internal(
                        "engine stalled with no in-flight work and no timer".into(),
                    ));
                }
            }
        }
    }

    /// Restores the disk overflow tier on (re)start: scans the cache
    /// directory, prunes entries the engine no longer needs, and marks
    /// surviving heights `Cached` so they are promoted instead of refetched
    /// (design doc §4.5).
    fn restore_cache(&mut self, end: block::Height) -> Result<(), EngineError> {
        // The state tip is the height below the first uncommitted height.
        let state_tip = self.base.0.checked_sub(1).map(block::Height);

        let survivors = self.cache.scan(state_tip, end).map_err(|error| {
            EngineError::Internal(format!("IBD block cache scan failed: {error}"))
        })?;

        for (height, bytes) in survivors {
            // `scan` pruned everything at or below the state tip, so
            // survivors are at or above `base`.
            let offset = height
                .0
                .checked_sub(self.base.0)
                .expect("scan only returns heights above the state tip")
                as usize;

            if offset >= IBD_SPAN_MAX {
                // A previous run's span cannot reach past the current span,
                // but be defensive: leave the entry for a later scan rather
                // than break the ring bound.
                warn!(
                    height = height.0,
                    "cached block is beyond the ring span; leaving it on disk",
                );
                break;
            }

            while self.window.len() <= offset {
                self.window.push_back(Slot::unrequested());
            }

            self.window[offset] = Slot::Cached {
                bytes,
                promoted: false,
            };
        }

        Ok(())
    }

    /// Refills the window, lowest-missing-first, with auto-scaled lookahead
    /// (design doc §4.1):
    ///
    /// 1. promotes `Cached` slots and pushes stage-2 continuations for
    ///    `Fetched` slots while the commit pipeline caps (§4.4) hold,
    /// 2. issues fetches while the total storage allowance
    ///    ([`Self::storage_allows`]: the RAM block buffer plus the disk
    ///    fetch-ahead allowance) and the live fetch concurrency
    ///    ([`Self::fetch_slots_available`]) both have room — the block-count
    ///    lookahead is not configured anywhere: it emerges from the byte
    ///    budgets and actual block sizes, capped at
    ///    [`IBD_WINDOW_MAX_BLOCKS`]. Memory-vs-disk placement happens at
    ///    arrival, not at issuance (§4.5).
    ///
    /// The frontier height bypasses the byte budget: its commit unblocks
    /// everything else, and after a §4.6 recovery the budget may be full of
    /// retained descendants parked behind it.
    ///
    /// Collects the earliest pending backoff expiry into `next_wake`.
    fn refill(
        &mut self,
        end: block::Height,
        now: Instant,
        next_wake: &mut Option<Instant>,
    ) -> Result<(), EngineError> {
        // `base <= end` is checked by the caller, the +1 cannot overflow in
        // u64, and the span is clamped to the defensive ceiling, so it fits
        // usize.
        let span = usize::try_from(u64::from(end.0 - self.base.0) + 1)
            .unwrap_or(usize::MAX)
            .min(IBD_SPAN_MAX);

        // Fetch issuance is bounded by the network's useful concurrency and
        // the total storage allowance, not the RAM budget: placement happens
        // at arrival (maintainer-directed). One flag tracks each bound so the
        // walk keeps scanning for promotions, commit pushes, and backoff
        // deadlines after issuance stops.
        let mut can_issue = self.storage_allows() && self.fetch_slots_available();

        for offset in 0..span {
            // `offset` is bounded by the u32 height span.
            let height = block::Height(self.base.0 + offset as u32);

            let action = if offset == self.window.len() {
                RefillAction::Extend
            } else {
                match &self.window[offset] {
                    Slot::Unrequested { backoff_until, .. } => {
                        if let Some(until) = backoff_until {
                            if *until > now {
                                merge_wake(next_wake, *until);
                                continue;
                            }
                        }
                        RefillAction::Issue
                    }
                    Slot::Cached {
                        promoted: false,
                        bytes,
                    } => RefillAction::Promote { bytes: *bytes },
                    Slot::Fetched {
                        committing: false,
                        bytes,
                        ..
                    } => RefillAction::Commit { bytes: *bytes },
                    _ => RefillAction::Skip,
                }
            };

            match action {
                RefillAction::Extend => {
                    if !can_issue || self.window.len() >= IBD_WINDOW_MAX_BLOCKS {
                        // Extending only creates fetch work, so the walk is
                        // done once issuance is blocked, the window end is
                        // reached, or the fetch-ahead cap is hit (keeping the
                        // per-event rescan bounded; see IBD_WINDOW_MAX_BLOCKS).
                        break;
                    }

                    self.window.push_back(Slot::unrequested());
                    self.issue_fetch(offset, height, now)?;
                    can_issue = self.storage_allows() && self.fetch_slots_available();
                }

                RefillAction::Issue => {
                    // The frontier height bypasses the storage gate: its
                    // commit unblocks everything else.
                    if can_issue || height == self.base {
                        self.issue_fetch(offset, height, now)?;
                        can_issue = self.storage_allows() && self.fetch_slots_available();
                    }
                }

                RefillAction::Promote { bytes } => {
                    // Promotion brings bytes back into RAM, so it takes the
                    // RAM budget (exact, already known) and the commit
                    // pipeline caps. The frontier height bypasses the budget:
                    // after a §4.6 recovery the budget may be full of
                    // retained descendants parked behind it.
                    let is_frontier = height == self.base;
                    let budget_fits =
                        self.budget_used + u64::from(bytes) <= self.lookahead_bytes || is_frontier;

                    if budget_fits && self.commit_caps_allow(bytes, is_frontier) {
                        self.promote(offset, height, bytes)?;
                    }
                }

                RefillAction::Commit { bytes } => {
                    if self.commit_caps_allow(bytes, height == self.base) {
                        self.push_commit(offset, height)?;
                    }
                }

                RefillAction::Skip => {}
            }
        }

        Ok(())
    }

    /// Does the total storage allowance — the RAM block buffer plus the disk
    /// fetch-ahead allowance — have room for more fetch-ahead?
    ///
    /// Arrivals are placed at completion time, so this gate is approximate
    /// by design: in-flight fetches can overshoot it by at most the
    /// outstanding fetch count × the 2 MB block cap, which
    /// [`Self::fetch_slots_available`] bounds.
    fn storage_allows(&self) -> bool {
        let disk_bytes = self.cache.bytes();
        let allowance = self
            .lookahead_bytes
            .saturating_mul(1 + DISK_FETCH_AHEAD_FACTOR);

        self.budget_used.saturating_add(disk_bytes) < allowance
    }

    /// Is the in-flight fetch count under the network's useful concurrency?
    ///
    /// More outstanding single-block fetches than the batch pipeline can
    /// carry just queue inside the batcher, so issuance pauses there.
    fn fetch_slots_available(&self) -> bool {
        let ready_peers = self.peer_status.borrow().ready_peers;
        self.inflight_fetches
            < ibd_max_concurrent_batches(ready_peers).saturating_mul(IBD_BATCH_MAX_BLOCKS)
    }

    /// Does the commit pipeline (design doc §4.4) have room for one more
    /// unresolved stage-2 future of `bytes`?
    ///
    /// The frontier block always passes: its commit is what unblocks the
    /// state's in-order drain, which is what lets every parked above-frontier
    /// commit resolve and free the caps. Without this bypass the pipeline can
    /// deadlock — above-frontier commits fill the caps while their drain
    /// waits on the frontier, and the frontier's own commit is then refused.
    /// At most one frontier commit is over-cap at a time (the slot is marked
    /// `committing` once pushed, and the frontier only advances when its
    /// commit resolves), so the overshoot is bounded to one block.
    ///
    /// Otherwise, an empty pipeline always admits one block, so a non-frontier
    /// block larger than the byte cap could never wedge the caps (unreachable
    /// today: the 2 MB protocol cap is far below the 64 MiB pipeline cap).
    fn commit_caps_allow(&self, bytes: u32, is_frontier: bool) -> bool {
        is_frontier
            || (self.commit_blocks < self.commit_pipeline_max_blocks
                && (self.commit_blocks == 0
                    || self.commit_bytes + u64::from(bytes) <= self.commit_pipeline_max_bytes))
    }

    /// Returns the pinned hash `list[height]`.
    //
    // The `expect` checks an internal invariant (in-window heights, and the
    // frontier's resident parent, are pre-bounded by the list); the `Result`
    // is only for real list I/O errors.
    #[allow(clippy::unwrap_in_result)]
    fn pinned_hash(&mut self, height: block::Height) -> Result<block::Hash, EngineError> {
        Ok(self
            .list
            .hash(height)
            .map_err(EngineError::List)?
            .expect("in-window heights and the frontier's parent are within the list"))
    }

    /// Returns the pinned hash for `height` and its parent pin:
    /// `(list[height], list[height - 1])`, with the genesis parent pinned to
    /// [`GENESIS_PREVIOUS_BLOCK_HASH`].
    fn pinned_hashes(
        &mut self,
        height: block::Height,
    ) -> Result<(block::Hash, block::Hash), EngineError> {
        let expected = self.pinned_hash(height)?;

        let prev_expected = match height.0.checked_sub(1) {
            None => GENESIS_PREVIOUS_BLOCK_HASH,
            Some(prev) => self.pinned_hash(block::Height(prev))?,
        };

        Ok((expected, prev_expected))
    }

    /// Wraps a stage-1 fetch future in an abort handle, tags it with
    /// `origin`, and pushes it into the staged-future collection.
    ///
    /// Returns the abort handle for the slot.
    fn push_fetch(
        &mut self,
        height: block::Height,
        origin: FetchOrigin,
        fetch: impl Future<Output = FetchOutcome> + Send + 'static,
    ) -> AbortHandle {
        let (abort, registration) = AbortHandle::new_pair();
        let fetch = Abortable::new(fetch, registration);

        self.blocks.push(
            async move {
                match fetch.await {
                    Ok(outcome) => (height, StageOutcome::Fetch { origin, outcome }),
                    Err(Aborted) => (height, StageOutcome::Aborted),
                }
            }
            .boxed(),
        );

        abort
    }

    /// Issues the stage-1 fetch future for `height` and moves its slot to
    /// `InFlight`.
    ///
    /// Placement (memory vs disk tier) happens when the block arrives, not
    /// here (maintainer-directed arrival-time placement).
    fn issue_fetch(
        &mut self,
        offset: usize,
        height: block::Height,
        now: Instant,
    ) -> Result<(), EngineError> {
        // Synchronous list lookups stay in the refill step by design: a
        // chunk fault reads ~5 MB from disk (~20 ms, once per 150,000
        // heights), which is cheaper than threading file I/O through the
        // per-block futures.
        let hash = self.pinned_hash(height)?;
        let size_hint = self.list.size_hint(height).max(1);

        let request = FetchRequest {
            height,
            hash,
            size_hint,
        };

        let (attempts, not_founds) = match &self.window[offset] {
            Slot::Unrequested {
                attempts,
                not_founds,
                ..
            } => (*attempts, *not_founds),
            other => unreachable!("issue_fetch is only called on Unrequested slots: {other:?}"),
        };

        let abort = self.push_fetch(
            height,
            FetchOrigin::Primary,
            fetch_stage(self.batched_fetch.clone(), request),
        );

        self.inflight_fetches += 1;

        self.window[offset] = Slot::InFlight {
            attempts: attempts.saturating_add(1),
            not_founds,
            since: now,
            hedged: None,
            abort,
        };

        Ok(())
    }

    /// Pushes the stage-2 verify-and-commit continuation for the `Fetched`
    /// block at `offset` (design doc §4.4).
    ///
    /// The pinned `list[height]` / `list[height - 1]` hashes are threaded at
    /// issuance, so the verify stage needs no list access of its own.
    fn push_commit(&mut self, offset: usize, height: block::Height) -> Result<(), EngineError> {
        let (expected, prev_expected) = self.pinned_hashes(height)?;

        let (payload, bytes, source) = match &mut self.window[offset] {
            Slot::Fetched {
                block,
                bytes,
                committing,
                source,
            } => {
                debug_assert!(!*committing, "one stage-2 future per height at a time");
                *committing = true;
                (BlockPayload::from(block.clone()), *bytes, *source)
            }
            other => unreachable!("push_commit is only called on Fetched slots: {other:?}"),
        };

        self.spawn_commit(
            height,
            bytes,
            self.verify_and_commit.clone().oneshot(IbdBlock {
                height,
                expected,
                prev_expected,
                block: payload,
                source,
            }),
        );

        Ok(())
    }

    /// Pushes the stage-2 promotion future for the `Cached` block at
    /// `offset`: the disk read happens inside the future, the raw bytes are
    /// deserialized inside the verify stage's rayon closure, and the block
    /// flows through the same verify-and-commit path as a network block
    /// (design doc §4.5).
    fn promote(
        &mut self,
        offset: usize,
        height: block::Height,
        bytes: u32,
    ) -> Result<(), EngineError> {
        let (expected, prev_expected) = self.pinned_hashes(height)?;

        // The disk read is a synchronous page-cache-bound ≤ 2 MB read,
        // called directly from the loop like every cache operation
        // (single-owner, no-mutex design; see the cache module docs). An
        // unreadable entry was already dropped by the cache: reset the slot
        // for an immediate network refetch (§4.5 exception (a)).
        let cached = match self.cache.get(height) {
            Some(cached) => cached,
            None => {
                metrics::counter!("ibd.duplicate.download.count", "reason" => "corrupt-cache")
                    .increment(1);

                self.window[offset] = Slot::unrequested();
                return Ok(());
            }
        };

        match &mut self.window[offset] {
            Slot::Cached { promoted, .. } => {
                debug_assert!(!*promoted, "one promotion future per height at a time");
                *promoted = true;
            }
            other => unreachable!("promote is only called on Cached slots: {other:?}"),
        }

        // Promoted blocks enter memory (inside the commit pipeline), so they
        // count against the RAM block buffer until their commit resolves.
        self.budget_used += u64::from(bytes);

        self.spawn_commit(
            height,
            bytes,
            self.verify_and_commit.clone().oneshot(IbdBlock {
                height,
                expected,
                prev_expected,
                block: BlockPayload::Raw(cached.bytes),
                source: cached.source,
            }),
        );

        Ok(())
    }

    /// Pushes `commit` into the staged-future collection and accounts it
    /// against the commit pipeline caps (design doc §4.4).
    fn spawn_commit(
        &mut self,
        height: block::Height,
        bytes: u32,
        commit: impl Future<Output = CommitOutcome> + Send + 'static,
    ) {
        self.blocks
            .push(async move { (height, StageOutcome::Commit(commit.await)) }.boxed());

        self.commit_blocks += 1;
        self.commit_bytes += u64::from(bytes);
    }

    /// Issues single-hash hedge fetches for frontier-critical heights that
    /// have been in flight too long (design doc §4.1 step 3).
    ///
    /// Collects the earliest pending hedge deadline into `next_wake`.
    fn hedge_frontier(
        &mut self,
        now: Instant,
        next_wake: &mut Option<Instant>,
    ) -> Result<(), EngineError> {
        let critical_span = (IBD_FRONTIER_CRITICAL_SPAN as usize).min(self.window.len());

        let mut due = Vec::new();

        for offset in 0..critical_span {
            if let Slot::InFlight {
                since,
                hedged: None,
                ..
            } = &self.window[offset]
            {
                // A deadline past the clock's range means hedging is
                // disabled (tests use this to keep paused-clock
                // auto-advance away from the hedge timer).
                let Some(deadline) = since.checked_add(self.gap_hedge_after) else {
                    continue;
                };

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
            let hash = self.pinned_hash(height)?;

            // A single-hash `BlocksByHash` through the plain peer set gets
            // inventory-aware routing (`route_inv`) for free, and the
            // one-request-per-connection rule lands it on a different peer
            // than the still-pending primary by construction.
            let hedge = fetch::fetch_single(self.peer_set.clone(), height, hash)
                .map(|result| result.map_err(|error| error.kind()));
            let abort = self.push_fetch(height, FetchOrigin::Hedge, hedge);

            match &mut self.window[offset] {
                Slot::InFlight { hedged, .. } => *hedged = Some(abort),
                other => unreachable!("hedge candidates stay InFlight within this step: {other:?}"),
            }

            metrics::counter!("ibd.gap.hedge.count").increment(1);
        }

        Ok(())
    }

    /// Applies one completed staged future to its slot.
    fn on_stage_complete(
        &mut self,
        height: block::Height,
        outcome: StageOutcome,
        now: Instant,
    ) -> Result<(), EngineError> {
        // Completions for heights the frontier already passed are stale
        // (only fetch futures can outlive their heights, via hedge races).
        let Some(offset) = height.0.checked_sub(self.base.0) else {
            return Ok(());
        };
        // u32 always fits in usize on supported platforms
        let offset = offset as usize;
        if offset >= self.window.len() {
            return Ok(());
        }

        match outcome {
            StageOutcome::Fetch { origin, outcome } => {
                match outcome {
                    Ok(FetchedBlock { block, source }) => {
                        self.on_fetch_success(offset, height, block, source);
                    }
                    Err(kind) => {
                        self.on_fetch_failure(offset, origin, kind, now);
                    }
                }
                Ok(())
            }

            StageOutcome::Commit(outcome) => self.on_commit_complete(offset, height, outcome, now),

            // Aborted: the slot was already updated by the winning future.
            StageOutcome::Aborted => Ok(()),
        }
    }

    /// Places an arrived block at completion time (maintainer-directed):
    /// into its window slot while the RAM block buffer has room, or raw into
    /// the disk overflow tier when over budget (design doc §4.5).
    fn on_fetch_success(
        &mut self,
        offset: usize,
        height: block::Height,
        block: Arc<Block>,
        source: Option<PeerSocketAddr>,
    ) {
        // One serialized-size pass per arrived block: the placement decision
        // needs the exact size.
        let bytes = block.zcash_serialized_size();
        // serialized blocks are bounded by the 2 MB protocol limit, enforced
        // at deserialization in zebra-network, so they fit u32 and u64
        let bytes_u32 = u32::try_from(bytes).expect("blocks are far below u32::MAX bytes");
        let bytes_u64 = u64::from(bytes_u32);

        match &mut self.window[offset] {
            Slot::InFlight { abort, hedged, .. } => {
                // First completion wins; cancel the loser (aborting an
                // already-completed future is a no-op).
                abort.abort();
                if let Some(hedged) = hedged {
                    hedged.abort();
                }

                self.inflight_fetches = self.inflight_fetches.saturating_sub(1);
            }
            Slot::Fetched { .. } | Slot::Cached { .. } | Slot::Committed => {
                // A lost hedge race delivered a duplicate: the single slot
                // per height is the dedup point; drop it.
                return;
            }
            Slot::Unrequested { .. } => {
                // The other future failed and reset the slot before this one
                // delivered: accept the block.
            }
        }

        // Arrival-time placement: the frontier height always stays in memory
        // (its commit unblocks everything else); other blocks go to the disk
        // tier once the RAM block buffer is over budget.
        let over_budget = self.budget_used + bytes_u64 > self.lookahead_bytes;
        let placed_on_disk = if over_budget && offset > 0 {
            match self.cache.put(height, &block, source) {
                Ok(cached_bytes) => {
                    self.window[offset] = Slot::Cached {
                        bytes: cached_bytes,
                        promoted: false,
                    };
                    true
                }
                Err(error) => {
                    // A failed cache write must not waste the download:
                    // hold the block in memory instead — a bounded budget
                    // overshoot.
                    warn!(
                        %error,
                        height = height.0,
                        "failed to write a block to the IBD disk cache; keeping it in memory",
                    );
                    false
                }
            }
        } else {
            false
        };

        if !placed_on_disk {
            self.budget_used += bytes_u64;
            self.window[offset] = Slot::Fetched {
                block,
                bytes: bytes_u32,
                committing: false,
                source,
            };
        }

        self.fetched_blocks += 1;

        metrics::counter!("ibd.fetched.block.count").increment(1);
        metrics::counter!("sync.downloaded.block.count").increment(1);
    }

    /// Applies the retry policy to a failed fetch (design doc §4.1):
    /// explicit `NotFound` responses are counted separately from transport
    /// losses; either way the height backs off and reassigns.
    fn on_fetch_failure(
        &mut self,
        offset: usize,
        origin: FetchOrigin,
        kind: FetchFailureKind,
        now: Instant,
    ) {
        if matches!(kind, FetchFailureKind::NotFound { .. }) {
            metrics::counter!("ibd.peer.notfound.count").increment(1);
        }

        match &mut self.window[offset] {
            Slot::InFlight {
                attempts,
                not_founds,
                hedged,
                abort,
                since,
                ..
            } => {
                if let FetchFailureKind::NotFound { .. } = kind {
                    *not_founds = not_founds.saturating_add(1);
                }

                match (origin, hedged.take()) {
                    // The primary failed but its hedge is still in flight:
                    // promote the hedge and keep the slot. Restart the hedge
                    // clock from now so the promoted hedge gets a full
                    // interval before it is itself hedged — otherwise the
                    // stale `since` is already past the deadline and
                    // `hedge_frontier` re-hedges on the next loop pass.
                    (FetchOrigin::Primary, Some(hedge)) => {
                        *abort = hedge;
                        *since = now;
                    }

                    // A hedge failed while another future is still in
                    // flight: forget the hedge handle and restart the hedge
                    // clock (same reason as above — without this, the slot
                    // re-hedges every loop pass in a tight CPU spin). (When a
                    // promoted hedge and a newer hedge are both in flight, we
                    // cannot tell which one failed; either way exactly one
                    // live future remains, and its own completion settles the
                    // slot.)
                    (FetchOrigin::Hedge, Some(_)) => {
                        *since = now;
                    }

                    // The last in-flight future for this height failed: back
                    // off before reassigning. (A hedge failure with
                    // `hedged: None` means the hedge had been promoted to
                    // primary.)
                    (FetchOrigin::Primary | FetchOrigin::Hedge, None) => {
                        self.inflight_fetches = self.inflight_fetches.saturating_sub(1);

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
            Slot::Fetched { .. }
            | Slot::Cached { .. }
            | Slot::Committed
            | Slot::Unrequested { .. } => {}
        }
    }

    /// Applies a resolved stage-2 future to its slot: releases the commit
    /// pipeline accounting, then commits, refetches, or recovers per the
    /// outcome (design doc §4.4, §4.6).
    fn on_commit_complete(
        &mut self,
        offset: usize,
        height: block::Height,
        outcome: CommitOutcome,
        now: Instant,
    ) -> Result<(), EngineError> {
        let bytes = match &self.window[offset] {
            Slot::Fetched {
                bytes,
                committing: true,
                ..
            }
            | Slot::Cached {
                bytes,
                promoted: true,
            } => *bytes,
            other => {
                debug_assert!(
                    false,
                    "commit completions always find the slot that pushed them: {other:?}",
                );
                return Ok(());
            }
        };

        // The unresolved-future accounting for this stage-2 ends here,
        // whatever the outcome.
        self.commit_blocks = self
            .commit_blocks
            .checked_sub(1)
            .expect("every resolved stage-2 future was counted when it was pushed");
        self.commit_bytes = self
            .commit_bytes
            .checked_sub(u64::from(bytes))
            .expect("every resolved stage-2 future reserved its bytes when it was pushed");

        match outcome {
            Ok(committed_hash) => {
                // Both held (`Fetched`) and promoted (`Cached`) blocks
                // counted against the budget until this resolution.
                self.budget_used = self
                    .budget_used
                    .checked_sub(u64::from(bytes))
                    .expect("committed blocks were counted against the budget");

                debug!(?height, ?committed_hash, "committed a known-hash block");

                self.window[offset] = Slot::Committed;
                self.commit_failures.remove(&height);

                metrics::counter!("ibd.converted.block.count").increment(1);

                self.advance_base();
                Ok(())
            }

            Err(error) => self.on_commit_failure(offset, height, error, now),
        }
    }

    /// Maps a failed stage-2 future per the design doc failure semantics
    /// (§4.3 verify failures, §4.6 commit failures).
    fn on_commit_failure(
        &mut self,
        offset: usize,
        height: block::Height,
        error: VerifyAndCommitError,
        now: Instant,
    ) -> Result<(), EngineError> {
        match error {
            // The state Buffer failed its readiness check: the service is
            // gone, and so is every future commit.
            VerifyAndCommitError::StateUnready(error) => Err(EngineError::Shutdown(error)),

            VerifyAndCommitError::Verify(error) => {
                self.on_verify_failure(offset, height, error, now)
            }

            VerifyAndCommitError::Commit { error, hash, .. } => {
                self.on_commit_stage_failure(offset, height, hash, error, now)
            }
        }
    }

    /// Maps a verify-stage failure (design doc §4.3): list diagnostics are
    /// fatal, corrupt copies are discarded and refetched — from a different
    /// peer when the failure is attributable.
    fn on_verify_failure(
        &mut self,
        offset: usize,
        height: block::Height,
        error: ConvertError,
        now: Instant,
    ) -> Result<(), EngineError> {
        if error.is_fatal_list_diagnostic() {
            return Err(EngineError::ListDiagnostic(error));
        }

        if matches!(error, ConvertError::RayonShutdown) {
            return Err(EngineError::Shutdown(error.into()));
        }

        // A corrupt body (peer-attributable) or corrupt cached bytes: either
        // way the copy never met the §4.5 invariant condition, so the
        // refetch is inside the invariant — but counted, for observability
        // (gate 8).
        let reason = if error.is_peer_attributable() {
            "unconfirmed-copy"
        } else {
            "corrupt-cache"
        };
        metrics::counter!("ibd.duplicate.download.count", "reason" => reason).increment(1);

        // TODO(known-hash-ibd D6): report `error.source_peer()`'s misbehavior
        // through the address book updater once start.rs wires the channel
        // through.

        warn!(
            %error,
            height = height.0,
            "discarding a block copy that failed verification; refetching",
        );

        self.discard_slot_copy(offset, height, now);
        Ok(())
    }

    /// Maps a commit-stage failure (design doc §4.6).
    ///
    /// Above the frontier this is a descendant dropped by the write thread's
    /// reset: the retained copy is resubmitted through re-verification —
    /// never refetched. At the frontier it is the genuinely-failing block
    /// (the state commits in strict height order): the implicated copy is
    /// discarded and refetched, and byte-identical failures count toward the
    /// deterministic-failure limit.
    fn on_commit_stage_failure(
        &mut self,
        offset: usize,
        height: block::Height,
        hash: block::Hash,
        error: BoxError,
        now: Instant,
    ) -> Result<(), EngineError> {
        if height > self.base {
            // A descendant dropped by a reset (or a frontier error racing
            // ahead of lower commit completions — it fails again at the
            // frontier and is classified there). Resubmission re-runs
            // `convert` on the retained copy: a re-verification, never a
            // re-download.
            debug!(
                %error,
                height = height.0,
                base = self.base.0,
                "commit dropped above the frontier; resubmitting the retained copy",
            );

            match &mut self.window[offset] {
                Slot::Fetched { committing, .. } => *committing = false,
                Slot::Cached { bytes, promoted } => {
                    *promoted = false;
                    // Release the promotion's budget reservation;
                    // re-promotion re-reserves it.
                    let bytes = u64::from(*bytes);
                    self.budget_used = self
                        .budget_used
                        .checked_sub(bytes)
                        .expect("promoted blocks were counted against the budget");
                }
                other => unreachable!("commit failures always find their pushed slot: {other:?}"),
            }

            return Ok(());
        }

        // The frontier block is always at the write thread's next valid
        // height, so it is never reset-dropped: `WriteTaskExited` here means
        // the write task is really gone.
        if is_write_task_exited(&error) {
            return Err(EngineError::Shutdown(error));
        }

        // The genuinely-failing block (e.g. a v5 auth-data substitution only
        // detectable at commit): count byte-identical failures.
        let copy = match &self.window[offset] {
            Slot::Fetched { block, .. } => Some(block.clone()),
            // A promoted copy's bytes went through the rayon closure; the
            // next failure of its refetched (memory-held) copy starts the
            // identity tracking.
            _ => None,
        };

        let entry = self.commit_failures.entry(height).or_insert((0, None));
        let byte_identical = matches!((&entry.1, &copy), (Some(prev), Some(new)) if prev == new);
        if byte_identical {
            entry.0 = entry.0.saturating_add(1);
        } else {
            *entry = (1, copy);
        }
        let attempts = entry.0;

        if attempts >= COMMIT_FAILURE_ATTEMPT_LIMIT {
            return Err(EngineError::DeterministicCommitFailure {
                height,
                hash,
                attempts,
                error,
            });
        }

        warn!(
            %error,
            height = height.0,
            attempts,
            "state commit failed at the frontier; discarding the implicated copy \
             and refetching",
        );
        metrics::counter!("ibd.duplicate.download.count", "reason" => "unconfirmed-copy")
            .increment(1);

        // TODO(known-hash-ibd D6): report the implicated copy's source peer
        // (the `Slot::Fetched` `source` field) through the address book
        // updater.

        self.discard_slot_copy(offset, height, now);
        Ok(())
    }

    /// Discards the block copy held (or cached) for `height`, releasing its
    /// budget and resetting the slot for an immediate refetch.
    fn discard_slot_copy(&mut self, offset: usize, height: block::Height, now: Instant) {
        let bytes = match &self.window[offset] {
            Slot::Fetched { bytes, .. } => *bytes,
            Slot::Cached { bytes, .. } => {
                let bytes = *bytes;
                // The implicated cached copy is discarded from disk too
                // (synchronous single-file unlink; see the cache module
                // docs for the single-owner, no-mutex design).
                self.cache.remove(height);
                bytes
            }
            other => unreachable!("discarded copies are held or cached: {other:?}"),
        };

        self.budget_used = self
            .budget_used
            .checked_sub(u64::from(bytes))
            .expect("held and promoted blocks were counted against the budget");

        // Back off before refetching. A corrupt copy is not a missing block,
        // but without a delay a peer that keeps serving a bad body for the
        // same height drives a tight fetch -> verify-fail -> refetch CPU spin
        // (routing may return the refetch to the same peer). The base
        // backoff paces these retries without materially slowing recovery
        // from a one-off corrupt delivery.
        self.window[offset] = Slot::Unrequested {
            attempts: 0,
            not_founds: 0,
            backoff_until: Some(now + IBD_HEIGHT_RETRY_BACKOFF_BASE),
        };
    }

    /// Pops the contiguous committed prefix off the window front, advancing
    /// the commit frontier and releasing everything keyed below it.
    fn advance_base(&mut self) {
        let old_base = self.base;

        while matches!(self.window.front(), Some(Slot::Committed)) {
            self.window.pop_front();
            self.base = block::Height(self.base.0 + 1);
        }

        if self.base == old_base {
            return;
        }

        // `base > 0` here because it just advanced.
        metrics::gauge!("ibd.committed.height").set(f64::from(self.base.0 - 1));

        // Keep `base - 1` resident: the next stage-2 push pins its parent
        // with `list[base - 1]`.
        self.list
            .release_below(block::Height(self.base.0.saturating_sub(1)));
        self.commit_failures
            .retain(|height, _| *height >= self.base);

        // Lazily evict committed entries from the disk tier (§4.5), batched
        // so each round scans the cache index once. Synchronous unlinks of a
        // bounded batch, called directly from the loop like every cache
        // operation (single-owner, no-mutex design).
        if !self.cache.is_empty()
            && self.base.0.saturating_sub(self.cache_evicted_through.0) >= IBD_CACHE_EVICT_INTERVAL
        {
            self.cache_evicted_through = self.base;
            self.cache.evict_through(block::Height(self.base.0 - 1));
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

        // `lowest_missing` is within the u32 height span.
        let height = block::Height(self.base.0 + lowest_missing as u32);

        if height > self.fetch_watermark {
            self.fetch_watermark = height;
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
                Slot::Fetched { .. } | Slot::Cached { .. } | Slot::Committed => (0, 0),
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

/// The stage-1 fetch future body: one weighted batched fetch.
///
/// Placement (memory vs the disk overflow tier) is the engine loop's job at
/// arrival, against its live RAM counter (maintainer-directed) — the future
/// just delivers the block.
async fn fetch_stage<S>(service: S, request: FetchRequest) -> FetchOutcome
where
    S: Service<FetchRequest, Response = BatchFetchResponse, Error = BoxError> + Send + 'static,
    S::Future: Send,
{
    match service.oneshot(request).await {
        Ok(BatchFetchResponse::Fetched(fetched)) => Ok(fetched),
        Ok(BatchFetchResponse::Flushed) => {
            debug_assert!(false, "per-item calls never resolve `Flushed`");
            Err(FetchFailureKind::Transport)
        }
        Err(error) => Err(FetchError::classify(&error)),
    }
}
