use super::{error::*, events::*, scheduler::*, validation::*, wire::*, *};

#[derive(Clone, Debug)]
pub(super) struct HeaderSyncState {
    pub(super) anchor: (block::Height, block::Hash),
    pub(super) finalized_height: block::Height,
    pub(super) verified_block_tip: block::Height,
    pub(super) best_header_tip: block::Height,
    pub(super) best_header_hash: block::Hash,
    pub(super) peers: HashMap<ZakuraPeerId, PeerHeaderState>,
    pub(super) seen: HeaderHashDedup,
    pub(super) pending_new_blocks: HashSet<block::Hash>,
    pub(super) schedule: RangeScheduler,
    pub(super) pending_commits: HashMap<PendingCommitKey, RangeRequest>,
}

impl HeaderSyncState {
    pub(super) fn new(startup: &HeaderSyncStartup) -> Result<Self, HeaderSyncStartError> {
        validate_anchor(&startup.network, startup.anchor)?;
        let (best_header_tip, best_header_hash) = startup.best_header_tip.unwrap_or(startup.anchor);

        Ok(Self {
            anchor: startup.anchor,
            finalized_height: startup.frontiers.finalized_height,
            verified_block_tip: startup.frontiers.verified_block_tip,
            best_header_tip,
            best_header_hash,
            peers: HashMap::new(),
            seen: HeaderHashDedup::default(),
            pending_new_blocks: HashSet::new(),
            schedule: RangeScheduler::new(),
            pending_commits: HashMap::new(),
        })
    }

    pub(super) fn refresh_forward_range(&mut self, startup: &HeaderSyncStartup) {
        let best_peer_tip = self
            .peers
            .values()
            .filter(|peer| peer.received_status)
            .map(|peer| peer.advertised_tip)
            .max()
            .unwrap_or(self.best_header_tip);
        if best_peer_tip <= self.best_header_tip {
            return;
        }

        let checkpoints = startup.network.checkpoint_list();
        let Some(start) = next_height(self.best_header_tip) else {
            return;
        };
        let mut end = best_peer_tip;
        let mut finalized = false;
        if let Some(first_checkpoint) = checkpoints.min_height_in_range(block::Height(1)..) {
            if self.best_header_tip < first_checkpoint {
                if best_peer_tip < first_checkpoint {
                    return;
                }
                end = first_checkpoint;
                finalized = true;
            }
        }

        let count = count_between(start, end);
        if count == 0 {
            return;
        }
        self.schedule.ensure_forward(RangeRequest {
            start_height: start,
            count,
            anchor_hash: self.best_header_hash,
            finalized,
            priority: RangePriority::Forward,
        });
    }

    pub(super) fn refresh_backward_range(&mut self, startup: &HeaderSyncStartup) {
        if self.anchor.0 == block::Height(0) {
            return;
        }
        let checkpoints = startup.network.checkpoint_list();
        // v1 backfill schedules one checkpoint bracket below the configured anchor.
        // Iterating all deeper brackets is left to final node wiring/backfill policy.
        let Some(previous_checkpoint) = checkpoints.max_height_in_range(..self.anchor.0) else {
            return;
        };
        let Some(previous_hash) = checkpoints.hash(previous_checkpoint) else {
            return;
        };
        let Some(start) = next_height(previous_checkpoint) else {
            return;
        };
        let count = count_between(start, self.anchor.0);
        if count == 0 {
            return;
        }
        self.schedule.ensure_backward(RangeRequest {
            start_height: start,
            count,
            anchor_hash: previous_hash,
            finalized: true,
            priority: RangePriority::Backward,
        });
    }
}

#[derive(Clone, Debug)]
pub(super) struct PeerHeaderState {
    pub(super) advertised_tip: block::Height,
    pub(super) anchor: block::Height,
    pub(super) max_headers_per_response: u32,
    pub(super) max_inflight_requests: u16,
    pub(super) received_status: bool,
    pub(super) outstanding: Vec<OutstandingRange>,
    pub(super) late_covered_responses: usize,
    pub(super) unsolicited: RateMeter,
    pub(super) inbound_status: RateMeter,
    pub(super) inbound_new_block: RateMeter,
    pub(super) served_headers_inflight: u16,
    pub(super) misbehavior: u32,
}

impl PeerHeaderState {
    pub(super) fn new(
        anchor: block::Height,
        local_range: u32,
        local_inflight: u16,
        status_refresh_interval: Duration,
        inbound_status_min_interval: Duration,
        inbound_new_block_min_interval: Duration,
    ) -> Self {
        Self {
            advertised_tip: anchor,
            anchor,
            max_headers_per_response: clamp_advertised_range(local_range),
            max_inflight_requests: local_inflight.clamp(1, LOCAL_MAX_HS_INFLIGHT_PER_PEER),
            received_status: false,
            outstanding: Vec::new(),
            late_covered_responses: 0,
            unsolicited: RateMeter::new(status_refresh_interval),
            inbound_status: RateMeter::new(inbound_status_min_interval),
            inbound_new_block: RateMeter::new(inbound_new_block_min_interval),
            served_headers_inflight: 0,
            misbehavior: 0,
        }
    }

    pub(super) fn available_slots(&self) -> usize {
        usize::from(self.max_inflight_requests)
            .min(EFFECTIVE_HS_OUTBOUND_INFLIGHT_PER_PEER)
            .saturating_sub(self.outstanding.len())
    }

    pub(super) fn pop_oldest_outstanding(&mut self) -> Option<OutstandingRange> {
        (!self.outstanding.is_empty()).then(|| self.outstanding.remove(0))
    }

    pub(super) fn take_late_covered_response(&mut self) -> bool {
        if self.late_covered_responses == 0 {
            return false;
        }
        self.late_covered_responses -= 1;
        true
    }

    pub(super) fn try_start_serving_headers(&mut self, local_inflight_cap: u16) -> bool {
        if self.served_headers_inflight >= local_inflight_cap {
            return false;
        }
        self.served_headers_inflight = self.served_headers_inflight.saturating_add(1);
        true
    }

    pub(super) fn finish_serving_headers(&mut self) {
        self.served_headers_inflight = self.served_headers_inflight.saturating_sub(1);
    }
}

#[derive(Copy, Clone, Debug)]
pub(super) struct OutstandingRange {
    pub(super) range: RangeRequest,
    pub(super) deadline: Instant,
    pub(super) expected_max_count: u32,
    pub(super) clear_assignment_on_timeout: bool,
}

#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct RangeRequest {
    pub(super) start_height: block::Height,
    pub(super) count: u32,
    pub(super) anchor_hash: block::Hash,
    pub(super) finalized: bool,
    pub(super) priority: RangePriority,
}

impl RangeRequest {
    pub(super) fn end_height(self) -> block::Height {
        height_after_count(self.start_height, self.count)
            .and_then(previous_height)
            .expect("range request count is non-zero")
    }

    pub(super) fn is_within(self, start: block::Height, end: block::Height) -> bool {
        self.start_height >= start && self.end_height() <= end
    }
}

#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub(super) enum RangePriority {
    Forward,
    Backward,
}
