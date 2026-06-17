use super::{config::*, reorder::*, scheduler::*, *};
use crate::zakura::{ServicePeerDirection, ServicePeerSnapshot, ZakuraBlockSyncCandidateState};

pub(super) const EFFECTIVE_BS_OUTBOUND_INFLIGHT_PER_PEER: usize = 8;

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
    pub header_tip: watch::Receiver<(block::Height, block::Hash)>,
    /// Local stream-6 configuration.
    pub config: ZakuraBlockSyncConfig,
    /// Shared shutdown signal owned by the embedding endpoint or test harness.
    pub shutdown: CancellationToken,
    /// Enables query actions for state-backed metadata.
    pub state_queries_enabled: bool,
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
            header_tip,
            config,
            shutdown: CancellationToken::new(),
            state_queries_enabled: true,
        }
    }

    pub(super) fn inert(config: ZakuraBlockSyncConfig) -> Self {
        let (tip_tx, header_tip) = watch::channel((block::Height::MIN, block::Hash([0; 32])));
        drop(tip_tx);
        Self {
            frontiers: BlockSyncFrontiers {
                finalized_height: block::Height::MIN,
                verified_block_tip: block::Height::MIN,
                verified_block_hash: block::Hash([0; 32]),
            },
            best_header_tip: (block::Height::MIN, block::Hash([0; 32])),
            header_tip,
            config,
            shutdown: CancellationToken::new(),
            state_queries_enabled: false,
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

    /// Send a peer lifecycle event without sharing the bounded wire-event queue.
    pub fn send_lifecycle(
        &self,
        event: BlockSyncEvent,
    ) -> Result<(), mpsc::error::SendError<BlockSyncEvent>> {
        self.lifecycle
            .send(event)
            .map_err(|error| mpsc::error::SendError(error.0))
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
    pub(super) servable_high: block::Height,
    pub(super) servable_hash: block::Hash,
    pub(super) best_header_tip: block::Height,
    pub(super) best_header_hash: block::Hash,
    pub(super) peers: HashMap<ZakuraPeerId, PeerBlockState>,
    pub(super) parked_peers: HashSet<ZakuraPeerId>,
    pub(super) schedule: BlockRangeScheduler,
    pub(super) reorder: ReorderBuffer,
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
            servable_high: startup.frontiers.verified_block_tip,
            servable_hash: startup.frontiers.verified_block_hash,
            best_header_tip: startup.best_header_tip.0,
            best_header_hash: startup.best_header_tip.1,
            peers: HashMap::new(),
            parked_peers: HashSet::new(),
            schedule: BlockRangeScheduler::new(startup.config.fanout),
            reorder: ReorderBuffer::new(),
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

    pub(super) fn outstanding_index_for_height(&self, height: block::Height) -> Option<usize> {
        self.outstanding
            .iter()
            .position(|outstanding| outstanding.request.contains(height))
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
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct ByteBudget {
    max_bytes: u64,
    reserved_bytes: u64,
}

impl ByteBudget {
    pub(super) fn new(max_bytes: u64) -> Self {
        Self {
            max_bytes,
            reserved_bytes: 0,
        }
    }

    pub(super) fn available(self) -> u64 {
        self.max_bytes.saturating_sub(self.reserved_bytes)
    }

    pub(super) fn reserved(self) -> u64 {
        self.reserved_bytes
    }

    pub(super) fn try_reserve(&mut self, bytes: u64) -> bool {
        if bytes == 0 || bytes > self.available() {
            return false;
        }
        self.reserved_bytes = self.reserved_bytes.saturating_add(bytes);
        true
    }

    pub(super) fn release(&mut self, bytes: u64) {
        self.reserved_bytes = self.reserved_bytes.saturating_sub(bytes);
    }
}

pub(super) fn next_height(height: block::Height) -> Option<block::Height> {
    height.0.checked_add(1).map(block::Height)
}

pub(super) fn previous_height(height: block::Height) -> Option<block::Height> {
    height.0.checked_sub(1).map(block::Height)
}

pub(super) fn height_after_count(start: block::Height, count: u32) -> Option<block::Height> {
    start.0.checked_add(count).map(block::Height)
}
