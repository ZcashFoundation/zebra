use super::{config::*, scheduler::*, work_queue::WorkQueue, *};
use crate::zakura::{
    chain_frontier_from_parts, Frontier, FrontierUpdate, ServicePeerDirection, ServicePeerSnapshot,
    ZakuraBlockSyncCandidateState,
};

/// Hard ceiling on outbound block-range requests kept in flight to one peer.
///
/// A safety bound only; the binding per-peer concurrency is the minimum of this
/// ceiling, the peer's advertised `max_inflight_requests`, and the peer's
/// adaptive outbound request window.
pub(super) const EFFECTIVE_BS_OUTBOUND_INFLIGHT_PER_PEER: usize = 2048;

/// Cached chain frontiers used by the block-sync reactor.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BlockSyncFrontiers {
    /// Shared finalized height supplied by state.
    pub finalized_height: block::Height,
    /// Highest verified block-body height supplied by state.
    pub verified_block_tip: block::Height,
    /// Hash of [`verified_block_tip`](Self::verified_block_tip).
    pub verified_block_hash: block::Hash,
}

/// Startup inputs for the dependency-neutral block-sync reactor.
#[derive(Clone, Debug)]
pub struct BlockSyncStartup {
    /// Cached state frontiers at startup.
    pub frontiers: BlockSyncFrontiers,
    /// Durable best header tip at startup.
    pub best_header_tip: (block::Height, block::Hash),
    /// Header-sync best-tip watch used as the moving body-download target.
    pub header_tip: Option<watch::Receiver<(block::Height, block::Hash)>>,
    /// Shared sync exchange frontier stream used as the moving body-download target.
    pub frontier_updates: Option<watch::Receiver<FrontierUpdate>>,
    /// Local stream-6 configuration.
    pub config: ZakuraBlockSyncConfig,
    /// Shared shutdown signal owned by the embedding endpoint or test harness.
    pub shutdown: CancellationToken,
    /// Enables query actions for state-backed metadata.
    pub state_queries_enabled: bool,
    /// JSONL trace emitter for block-sync scheduling, download, and commit rows.
    pub trace: ZakuraTrace,
}

impl BlockSyncStartup {
    /// Build block-sync startup config from durable/frontier facts.
    pub fn new(
        frontiers: BlockSyncFrontiers,
        best_header_tip: (block::Height, block::Hash),
        header_tip: watch::Receiver<(block::Height, block::Hash)>,
        config: ZakuraBlockSyncConfig,
    ) -> Self {
        Self {
            frontiers,
            best_header_tip,
            header_tip: Some(header_tip),
            frontier_updates: None,
            config,
            shutdown: CancellationToken::new(),
            state_queries_enabled: true,
            trace: ZakuraTrace::noop(),
        }
    }

    /// Build block-sync startup config from shared sync exchange frontiers.
    pub fn new_with_exchange(
        frontiers: BlockSyncFrontiers,
        best_header_tip: (block::Height, block::Hash),
        frontier_updates: watch::Receiver<FrontierUpdate>,
        config: ZakuraBlockSyncConfig,
    ) -> Self {
        Self {
            frontiers,
            best_header_tip,
            header_tip: None,
            frontier_updates: Some(frontier_updates),
            config,
            shutdown: CancellationToken::new(),
            state_queries_enabled: true,
            trace: ZakuraTrace::noop(),
        }
    }

    /// Build a latest-value frontier update stream from legacy startup pieces.
    pub fn frontier_update_from_parts(
        frontiers: BlockSyncFrontiers,
        best_header_tip: (block::Height, block::Hash),
    ) -> FrontierUpdate {
        FrontierUpdate {
            frontier: chain_frontier_from_parts(
                frontiers.finalized_height,
                Frontier::new(frontiers.verified_block_tip, frontiers.verified_block_hash),
                Frontier::new(best_header_tip.0, best_header_tip.1),
            ),
            change: crate::zakura::FrontierChange::Snapshot,
        }
    }

    pub(super) fn inert(config: ZakuraBlockSyncConfig) -> Self {
        Self {
            frontiers: BlockSyncFrontiers {
                finalized_height: block::Height::MIN,
                verified_block_tip: block::Height::MIN,
                verified_block_hash: block::Hash([0; 32]),
            },
            best_header_tip: (block::Height::MIN, block::Hash([0; 32])),
            header_tip: None,
            frontier_updates: None,
            config,
            shutdown: CancellationToken::new(),
            state_queries_enabled: false,
            trace: ZakuraTrace::noop(),
        }
    }
}

/// Cheap cloneable handle used by services and drivers to inform block sync.
#[derive(Clone, Debug)]
pub struct BlockSyncHandle {
    pub(super) events: mpsc::Sender<BlockSyncEvent>,
    pub(super) lifecycle: mpsc::UnboundedSender<BlockSyncEvent>,
    pub(super) peers: watch::Receiver<ServicePeerSnapshot>,
    pub(super) status: watch::Receiver<BlockSyncStatus>,
    pub(super) candidates: watch::Receiver<ZakuraBlockSyncCandidateState>,
}

impl BlockSyncHandle {
    /// Send a fact/event to the block-sync reactor.
    pub async fn send(
        &self,
        event: BlockSyncEvent,
    ) -> Result<(), mpsc::error::SendError<BlockSyncEvent>> {
        self.events.send(event).await
    }

    /// Try to send a fact/event without awaiting.
    pub fn try_send(
        &self,
        event: BlockSyncEvent,
    ) -> Result<(), mpsc::error::TrySendError<BlockSyncEvent>> {
        self.events.try_send(event)
    }

    /// Send a control-plane event without sharing the bounded wire-event queue.
    pub fn send_control(
        &self,
        event: BlockSyncEvent,
    ) -> Result<(), mpsc::error::SendError<BlockSyncEvent>> {
        self.lifecycle
            .send(event)
            .map_err(|error| mpsc::error::SendError(error.0))
    }

    /// Send a peer lifecycle event without sharing the bounded wire-event queue.
    pub fn send_lifecycle(
        &self,
        event: BlockSyncEvent,
    ) -> Result<(), mpsc::error::SendError<BlockSyncEvent>> {
        self.send_control(event)
    }

    /// Return the currently cached peer slot snapshot.
    pub fn peer_snapshot(&self) -> ServicePeerSnapshot {
        *self.peers.borrow()
    }

    /// Subscribe to local block-sync status advertisements.
    pub fn subscribe_status(&self) -> watch::Receiver<BlockSyncStatus> {
        self.status.clone()
    }

    /// Return the currently cached local status advertisement.
    pub fn local_status(&self) -> BlockSyncStatus {
        *self.status.borrow()
    }

    /// Subscribe to block-sync candidate-selection hints.
    pub fn subscribe_candidate_state(&self) -> watch::Receiver<ZakuraBlockSyncCandidateState> {
        self.candidates.clone()
    }

    /// Return the currently cached block-sync candidate-selection hints.
    pub fn candidate_state(&self) -> ZakuraBlockSyncCandidateState {
        self.candidates.borrow().clone()
    }
}

#[derive(Clone, Debug)]
pub(super) struct BlockSyncState {
    pub(super) finalized_height: block::Height,
    pub(super) verified_block_hash: block::Hash,
    pub(super) servable_high: block::Height,
    pub(super) servable_hash: block::Hash,
    pub(super) best_header_tip: block::Height,
    pub(super) best_header_hash: block::Hash,
    pub(super) peers: HashMap<ZakuraPeerId, PeerBlockState>,
    pub(super) parked_peers: HashSet<ZakuraPeerId>,
    pub(super) disconnected_peers: HashSet<ZakuraPeerId>,
    /// Sorted set of needed download heights. Replaces the central
    /// `BlockRangeScheduler`: the per-peer issuance path pulls work in its own
    /// servable range, dedup/covered are `in_flight`, and the floor is GC only.
    /// `Arc` so the state stays cheaply `Clone` and the queue can be shared with
    /// the Sequencer task / per-peer routines in later stages.
    pub(super) work: Arc<WorkQueue>,
    pub(super) budget: ByteBudget,
    pub(super) needed_heights: Vec<block::Height>,
    pub(super) status_refresh: RateMeter,
    pub(super) pending_status_refresh: bool,
    pub(super) last_advertised_status: BlockSyncStatus,
    /// Round-robin cursor that rotates which peer the full scheduling pass fills
    /// first. Advanced once per full pass so a budget-constrained pass is not
    /// always consumed by the lowest-node-id peer. Deterministic for tests.
    pub(super) fill_rotation_cursor: usize,
    /// Throughput of bodies received off the wire (the download rate). Sampled
    /// each trace tick; compared against the Sequencer task's committed
    /// throughput it separates a download-limited sync from a commit-limited one.
    pub(super) received_throughput: ThroughputMeter,
}

impl BlockSyncState {
    pub(super) fn new(startup: &BlockSyncStartup) -> Self {
        let last_advertised_status = BlockSyncStatus {
            servable_low: block::Height::MIN,
            servable_high: startup.frontiers.verified_block_tip,
            tip_hash: startup.frontiers.verified_block_hash,
            max_blocks_per_response: startup.config.advertised_max_blocks_per_response(),
            max_inflight_requests: startup.config.advertised_max_inflight_requests(),
            max_response_bytes: startup.config.advertised_max_response_bytes(),
        };

        Self {
            finalized_height: startup.frontiers.finalized_height,
            verified_block_hash: startup.frontiers.verified_block_hash,
            servable_high: startup.frontiers.verified_block_tip,
            servable_hash: startup.frontiers.verified_block_hash,
            best_header_tip: startup.best_header_tip.0,
            best_header_hash: startup.best_header_tip.1,
            peers: HashMap::new(),
            parked_peers: HashSet::new(),
            disconnected_peers: HashSet::new(),
            work: Arc::new(WorkQueue::new(startup.frontiers.verified_block_tip)),
            budget: ByteBudget::new(startup.config.max_inflight_block_bytes),
            needed_heights: Vec::new(),
            status_refresh: RateMeter::new(startup.config.status_refresh_interval),
            pending_status_refresh: false,
            last_advertised_status,
            fill_rotation_cursor: 0,
            received_throughput: ThroughputMeter::new(Instant::now()),
        }
    }

    pub(super) fn peer_snapshot(&self, limits: ServicePeerLimits) -> ServicePeerSnapshot {
        let inbound = self
            .peers
            .values()
            .filter(|peer| peer.direction == ServicePeerDirection::Inbound)
            .count();
        let outbound = self
            .peers
            .values()
            .filter(|peer| peer.direction == ServicePeerDirection::Outbound)
            .count();
        ServicePeerSnapshot::new(inbound, outbound, limits)
    }

    /// Drain every outstanding request whose deadline has passed, releasing its
    /// byte reservation and re-queuing the range for retry. Returns whether any
    /// request expired so callers can reschedule. Invoked from scheduling hot
    /// paths (not just the periodic tick) so a stuck floor block is reclaimed
    /// within the request timeout rather than after the next tick.
    pub(super) fn expire_due_timeouts(&mut self, now: Instant) -> bool {
        let mut timed_out = Vec::new();
        for (peer_id, peer) in self.peers.iter_mut() {
            let mut index = 0;
            while index < peer.outstanding.len() {
                if peer.outstanding[index].deadline <= now {
                    peer.reduce_outbound_window_after_timeout();
                    timed_out.push((peer_id.clone(), peer.outstanding.remove(index)));
                } else {
                    index += 1;
                }
            }
        }

        let expired_any = !timed_out.is_empty();
        for (_peer_id, outstanding) in timed_out {
            self.budget.release(outstanding.reserved_bytes());
            // Return only the unreceived heights to the queue. Received heights
            // are already buffered (in `in_flight` until committed); re-queuing
            // them would re-fetch a body we already hold, which the WorkQueue's
            // single-owner `in_flight` invariant forbids.
            self.work.return_items(
                outstanding
                    .request
                    .expected_hashes
                    .iter()
                    .filter(|(height, _)| !outstanding.has_received(*height))
                    .map(|(height, _)| *height),
            );
        }
        expired_any
    }
}

#[derive(Clone, Debug)]
pub(super) struct PeerBlockState {
    pub(super) session: BlockSyncPeerSession,
    pub(super) direction: ServicePeerDirection,
    pub(super) servable_low: block::Height,
    pub(super) servable_high: block::Height,
    pub(super) max_blocks_per_response: u32,
    pub(super) max_inflight_requests: u16,
    pub(super) max_response_bytes: u32,
    pub(super) outbound_request_window: usize,
    pub(super) timeout_recovery_slots: usize,
    pub(super) received_status: bool,
    pub(super) outstanding: Vec<OutstandingBlockRange>,
    pub(super) inbound_status: RateMeter,
    pub(super) unsolicited: RateMeter,
    pub(super) served_blocks_inflight: u16,
    pub(super) served_block_requests: VecDeque<(block::Height, Instant)>,
    pub(super) misbehavior: u32,
}

impl PeerBlockState {
    pub(super) fn new(session: BlockSyncPeerSession, config: &ZakuraBlockSyncConfig) -> Self {
        Self {
            direction: session.direction(),
            session,
            servable_low: block::Height::MIN,
            servable_high: block::Height::MIN,
            max_blocks_per_response: config.advertised_max_blocks_per_response(),
            max_inflight_requests: config.advertised_max_inflight_requests(),
            max_response_bytes: config.advertised_max_response_bytes(),
            outbound_request_window: usize::from(config.advertised_max_inflight_requests()),
            timeout_recovery_slots: 0,
            received_status: false,
            outstanding: Vec::new(),
            inbound_status: RateMeter::new(
                config.status_refresh_interval.min(Duration::from_secs(1)),
            ),
            unsolicited: RateMeter::new(config.status_refresh_interval),
            served_blocks_inflight: 0,
            served_block_requests: VecDeque::new(),
            misbehavior: 0,
        }
    }

    pub(super) fn available_slots(&self) -> usize {
        let hard_capacity = self.hard_outbound_capacity();
        let adaptive_limit = hard_capacity.min(self.outbound_request_window);
        let adaptive_slots = adaptive_limit.saturating_sub(self.outstanding.len());
        if adaptive_slots > 0 {
            return adaptive_slots;
        }

        self.timeout_recovery_slots
            .min(hard_capacity.saturating_sub(self.outstanding.len()))
    }

    pub(super) fn reduce_outbound_window_after_timeout(&mut self) {
        self.outbound_request_window = self.outbound_request_window.saturating_div(2).max(1);
        self.timeout_recovery_slots = self
            .timeout_recovery_slots
            .saturating_add(1)
            .min(self.hard_outbound_capacity());
    }

    pub(super) fn increase_outbound_window_after_success(&mut self) {
        let max_window = self.hard_outbound_capacity();
        if self.outbound_request_window < max_window {
            self.outbound_request_window = self.outbound_request_window.saturating_add(1);
        }
    }

    pub(super) fn record_outbound_request_scheduled(&mut self) {
        let adaptive_limit = self
            .hard_outbound_capacity()
            .min(self.outbound_request_window);
        if self.outstanding.len() >= adaptive_limit && self.timeout_recovery_slots > 0 {
            self.timeout_recovery_slots = self.timeout_recovery_slots.saturating_sub(1);
        }
    }

    pub(super) fn hard_outbound_capacity(&self) -> usize {
        usize::from(self.max_inflight_requests).min(EFFECTIVE_BS_OUTBOUND_INFLIGHT_PER_PEER)
    }

    pub(super) fn can_serve_any(&self, heights: &[block::Height]) -> bool {
        self.received_status
            && heights
                .iter()
                .any(|height| self.servable_low <= *height && *height <= self.servable_high)
    }

    pub(super) fn outstanding_index_for_height(&self, height: block::Height) -> Option<usize> {
        self.outstanding
            .iter()
            .position(|outstanding| outstanding.request.contains(height))
    }

    pub(super) fn outstanding_index_for_start(&self, start_height: block::Height) -> Option<usize> {
        self.outstanding
            .iter()
            .position(|outstanding| outstanding.request.start_height == start_height)
    }

    pub(super) fn try_start_serving_blocks(
        &mut self,
        local_inflight_cap: u16,
        start_height: block::Height,
    ) -> bool {
        if self.served_blocks_inflight >= local_inflight_cap {
            return false;
        }
        self.served_blocks_inflight = self.served_blocks_inflight.saturating_add(1);
        self.served_block_requests
            .push_back((start_height, Instant::now()));
        true
    }

    pub(super) fn serving_blocks_elapsed(&self, start_height: block::Height) -> Option<Duration> {
        self.served_block_requests
            .iter()
            .find_map(|(start, started)| (*start == start_height).then(|| started.elapsed()))
    }

    pub(super) fn finish_serving_blocks(
        &mut self,
        start_height: block::Height,
    ) -> Option<Duration> {
        let elapsed = self
            .served_block_requests
            .iter()
            .position(|(start, _)| *start == start_height)
            .and_then(|index| self.served_block_requests.remove(index))
            .map(|(_, started)| started.elapsed());
        self.served_blocks_inflight = self.served_blocks_inflight.saturating_sub(1);
        elapsed
    }
}

#[derive(Clone, Debug)]
pub(super) struct OutstandingBlockRange {
    pub(super) request: BlockRangeRequest,
    pub(super) deadline: Instant,
    pub(super) received: HashSet<block::Height>,
}

impl OutstandingBlockRange {
    /// Worst-case bytes still reserved for this request: the per-block worst case
    /// for every requested height not yet received. The reservation for a request
    /// only ever shrinks, so releasing this (on timeout/disconnect/short response)
    /// never over-releases bytes that were already handed to the reorder buffer.
    pub(super) fn reserved_bytes(&self) -> u64 {
        let outstanding = self
            .request
            .expected_hashes
            .len()
            .saturating_sub(self.received.len());
        // `outstanding` is a count bounded by `MAX_BS_BLOCKS_PER_REQUEST`, so the
        // product cannot overflow `u64`; `saturating_mul` is belt-and-suspenders.
        BS_PER_BLOCK_WORST_CASE_BYTES.saturating_mul(outstanding as u64)
    }

    pub(super) fn estimated_bytes_for_height(&self, height: block::Height) -> Option<u64> {
        self.request.estimated_bytes_for_height(height)
    }

    pub(super) fn has_received(&self, height: block::Height) -> bool {
        self.received.contains(&height)
    }

    pub(super) fn mark_received(&mut self, height: block::Height) {
        self.received.insert(height);
    }

    /// Mark every requested height at or below `tip` as received and return the
    /// worst-case bytes that those newly-received heights had reserved, so the
    /// caller releases exactly the reservation those heights still held.
    pub(super) fn mark_received_through(&mut self, tip: block::Height) -> u64 {
        let newly_received = self
            .request
            .expected_hashes
            .iter()
            .filter(|(height, _)| *height <= tip && self.received.insert(*height))
            .count();
        // Bounded by `MAX_BS_BLOCKS_PER_REQUEST`; cannot overflow `u64`.
        BS_PER_BLOCK_WORST_CASE_BYTES.saturating_mul(newly_received as u64)
    }

    pub(super) fn is_complete(&self) -> bool {
        self.received.len() == self.request.expected_hashes.len()
    }
}

#[derive(Clone, Debug)]
pub(super) struct RateMeter {
    pub(super) next_allowed: Instant,
    pub(super) interval: Duration,
}

impl RateMeter {
    pub(super) fn new(interval: Duration) -> Self {
        Self {
            next_allowed: Instant::now(),
            interval,
        }
    }

    pub(super) fn try_take(&mut self, now: Instant) -> bool {
        if now < self.next_allowed {
            return false;
        }
        self.next_allowed = now + self.interval;
        true
    }

    pub(super) fn mark_taken(&mut self, now: Instant) {
        self.next_allowed = now + self.interval;
    }
}

/// Tracks block-body throughput (bytes and block counts) over the interval
/// between samples, so the trace snapshot can report download/commit rates while
/// driving toward the 1–2 Gbps target. `record` accumulates; `sample` snapshots
/// the per-second rate since the last sample and resets the window. The last
/// computed rate is cached so it can be read from the immutable trace path. Cost
/// is two saturating adds per body and one division per sample tick.
#[derive(Clone, Debug)]
pub(super) struct ThroughputMeter {
    bytes: u64,
    blocks: u64,
    window_start: Instant,
    last_bytes_per_sec: u64,
    last_blocks_per_sec: u64,
}

impl ThroughputMeter {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            bytes: 0,
            blocks: 0,
            window_start: now,
            last_bytes_per_sec: 0,
            last_blocks_per_sec: 0,
        }
    }

    pub(super) fn record(&mut self, bytes: u64) {
        self.bytes = self.bytes.saturating_add(bytes);
        self.blocks = self.blocks.saturating_add(1);
    }

    /// Recompute the cached per-second rates from the bytes/blocks accumulated
    /// since the last sample, then reset the window. A non-positive interval
    /// (clock not advanced between samples) leaves the cached rates untouched.
    pub(super) fn sample(&mut self, now: Instant) {
        let elapsed = now
            .saturating_duration_since(self.window_start)
            .as_secs_f64();
        if elapsed <= 0.0 {
            return;
        }
        // `as u64` truncates a finite, non-negative rate; both numerator and
        // denominator are non-negative so the cast cannot wrap or go negative.
        self.last_bytes_per_sec = (self.bytes as f64 / elapsed) as u64;
        self.last_blocks_per_sec = (self.blocks as f64 / elapsed) as u64;
        self.bytes = 0;
        self.blocks = 0;
        self.window_start = now;
    }

    pub(super) fn bytes_per_sec(&self) -> u64 {
        self.last_bytes_per_sec
    }

    pub(super) fn blocks_per_sec(&self) -> u64 {
        self.last_blocks_per_sec
    }
}

// `ByteBudget` was promoted to `transport/guard.rs` so byte-rate protection is
// reusable across services. Re-exported here so existing block_sync call sites
// (`reorder.rs`, `scheduler.rs`, `tests.rs`, and the field on this module's
// state) keep resolving unchanged.
pub(crate) use crate::zakura::transport::ByteBudget;

pub(super) fn next_height(height: block::Height) -> Option<block::Height> {
    height.0.checked_add(1).map(block::Height)
}

pub(super) fn previous_height(height: block::Height) -> Option<block::Height> {
    height.0.checked_sub(1).map(block::Height)
}

pub(super) fn height_after_count(start: block::Height, count: u32) -> Option<block::Height> {
    start.0.checked_add(count).map(block::Height)
}
