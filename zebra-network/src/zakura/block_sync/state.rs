use super::{config::*, events::BlockApplyToken, reorder::*, scheduler::*, *};
use crate::zakura::{
    chain_frontier_from_parts, Frontier, FrontierUpdate, ServicePeerDirection, ServicePeerSnapshot,
    ZakuraBlockSyncCandidateState,
};

/// Hard ceiling on outbound block-range requests kept in flight to one peer.
///
/// A safety bound only; the binding per-peer concurrency is the peer's advertised
/// `max_inflight_requests` (config `max_inflight_requests`, clamped to
/// [`DEFAULT_BS_MAX_INFLIGHT`]).
pub(super) const EFFECTIVE_BS_OUTBOUND_INFLIGHT_PER_PEER: usize = 16;

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
    pub(super) verified_block_tip: block::Height,
    pub(super) verified_block_hash: block::Hash,
    pub(super) body_download_floor: block::Height,
    pub(super) servable_high: block::Height,
    pub(super) servable_hash: block::Hash,
    pub(super) best_header_tip: block::Height,
    pub(super) best_header_hash: block::Hash,
    pub(super) peers: HashMap<ZakuraPeerId, PeerBlockState>,
    pub(super) parked_peers: HashSet<ZakuraPeerId>,
    pub(super) disconnected_peers: HashSet<ZakuraPeerId>,
    pub(super) schedule: BlockRangeScheduler,
    pub(super) reorder: ReorderBuffer,
    pub(super) applying: BTreeMap<block::Height, ApplyingBlock>,
    pub(super) submitted_applies: BTreeMap<block::Height, Vec<(block::Hash, usize)>>,
    pub(super) next_apply_token: BlockApplyToken,
    pub(super) budget: ByteBudget,
    pub(super) needed_heights: Vec<block::Height>,
    pub(super) status_refresh: RateMeter,
    pub(super) pending_status_refresh: bool,
    pub(super) last_advertised_status: BlockSyncStatus,
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
            verified_block_tip: startup.frontiers.verified_block_tip,
            verified_block_hash: startup.frontiers.verified_block_hash,
            body_download_floor: startup.frontiers.verified_block_tip,
            servable_high: startup.frontiers.verified_block_tip,
            servable_hash: startup.frontiers.verified_block_hash,
            best_header_tip: startup.best_header_tip.0,
            best_header_hash: startup.best_header_tip.1,
            peers: HashMap::new(),
            parked_peers: HashSet::new(),
            disconnected_peers: HashSet::new(),
            schedule: BlockRangeScheduler::new(startup.config.fanout),
            reorder: ReorderBuffer::new(),
            applying: BTreeMap::new(),
            submitted_applies: BTreeMap::new(),
            next_apply_token: 1,
            budget: ByteBudget::new(startup.config.max_inflight_block_bytes),
            needed_heights: Vec::new(),
            status_refresh: RateMeter::new(startup.config.status_refresh_interval),
            pending_status_refresh: false,
            last_advertised_status,
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
}

#[derive(Clone, Debug)]
pub(super) struct ApplyingBlock {
    pub(super) token: BlockApplyToken,
    pub(super) hash: block::Hash,
    pub(super) block: Arc<block::Block>,
    pub(super) bytes: u64,
    pub(super) submitted: bool,
    /// The peer that delivered this body, used to attribute an apply rejection
    /// for misbehavior scoring.
    pub(super) source_peer: ZakuraPeerId,
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
    pub(super) received_status: bool,
    pub(super) outstanding: Vec<OutstandingBlockRange>,
    pub(super) inbound_status: RateMeter,
    pub(super) unsolicited: RateMeter,
    pub(super) served_blocks_inflight: u16,
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
            received_status: false,
            outstanding: Vec::new(),
            inbound_status: RateMeter::new(
                config.status_refresh_interval.min(Duration::from_secs(1)),
            ),
            unsolicited: RateMeter::new(config.status_refresh_interval),
            served_blocks_inflight: 0,
            misbehavior: 0,
        }
    }

    pub(super) fn available_slots(&self) -> usize {
        usize::from(self.max_inflight_requests)
            .min(EFFECTIVE_BS_OUTBOUND_INFLIGHT_PER_PEER)
            .saturating_sub(self.outstanding.len())
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

    pub(super) fn try_start_serving_blocks(&mut self, local_inflight_cap: u16) -> bool {
        if self.served_blocks_inflight >= local_inflight_cap {
            return false;
        }
        self.served_blocks_inflight = self.served_blocks_inflight.saturating_add(1);
        true
    }

    pub(super) fn finish_serving_blocks(&mut self) {
        self.served_blocks_inflight = self.served_blocks_inflight.saturating_sub(1);
    }
}

#[derive(Clone, Debug)]
pub(super) struct OutstandingBlockRange {
    pub(super) request: BlockRangeRequest,
    pub(super) deadline: Instant,
    pub(super) received: HashSet<block::Height>,
}

impl OutstandingBlockRange {
    pub(super) fn reserved_bytes(&self) -> u64 {
        self.request
            .expected_bytes
            .iter()
            .filter(|(height, _)| !self.received.contains(height))
            .map(|(_, bytes)| *bytes)
            .sum()
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

    pub(super) fn mark_received_through(&mut self, tip: block::Height) -> u64 {
        self.request
            .expected_bytes
            .iter()
            .filter_map(|(height, bytes)| {
                (*height <= tip && self.received.insert(*height)).then_some(*bytes)
            })
            .sum()
    }

    pub(super) fn is_complete(&self) -> bool {
        self.received.len() == self.request.expected_hashes.len()
    }

    pub(super) fn missing_retry_requests<F>(&self, mut should_retry: F) -> Vec<BlockRangeRequest>
    where
        F: FnMut(block::Height, block::Hash) -> bool,
    {
        let mut requests = Vec::new();
        let mut segment_hashes = Vec::new();
        let mut segment_bytes = Vec::new();

        for (height, hash) in &self.request.expected_hashes {
            let retry = !self.received.contains(height) && should_retry(*height, *hash);
            let contiguous = segment_hashes
                .last()
                .and_then(|(last_height, _)| next_height(*last_height))
                == Some(*height);

            if !retry || (!segment_hashes.is_empty() && !contiguous) {
                if let Some(request) = Self::retry_request_from_segment(
                    std::mem::take(&mut segment_hashes),
                    std::mem::take(&mut segment_bytes),
                ) {
                    requests.push(request);
                }
            }

            if retry {
                if let Some(bytes) = self.request.estimated_bytes_for_height(*height) {
                    segment_hashes.push((*height, *hash));
                    segment_bytes.push((*height, bytes));
                }
            }
        }

        if let Some(request) = Self::retry_request_from_segment(segment_hashes, segment_bytes) {
            requests.push(request);
        }

        requests
    }

    fn retry_request_from_segment(
        expected_hashes: Vec<(block::Height, block::Hash)>,
        expected_bytes: Vec<(block::Height, u64)>,
    ) -> Option<BlockRangeRequest> {
        let (start_height, anchor_hash) = *expected_hashes.first()?;
        let count = u32::try_from(expected_hashes.len())
            .expect("block-sync retry segment length is bounded by original request count");
        let estimated_bytes = expected_bytes
            .iter()
            .fold(0u64, |total, (_, bytes)| total.saturating_add(*bytes));

        Some(BlockRangeRequest {
            start_height,
            count,
            anchor_hash,
            estimated_bytes,
            expected_hashes,
            expected_bytes,
        })
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
