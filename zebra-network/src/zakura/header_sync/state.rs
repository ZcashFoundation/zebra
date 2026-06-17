use super::{error::*, events::*, scheduler::*, validation::*, wire::*, *};
use crate::zakura::{
    HeaderSyncServiceSummary, ServicePeerDirection, DEFAULT_LIVE_SERVICE_SUMMARY_TTL,
};

pub(super) const HEADER_SYNC_ADVISORY_BACKOFF_FAILURES: u32 = 2;
pub(super) const HEADER_SYNC_ADVISORY_BACKOFF: Duration = Duration::from_secs(60);
pub(super) const HEADER_SYNC_ADVISORY_TTL: Duration = DEFAULT_LIVE_SERVICE_SUMMARY_TTL;
pub(super) const HEADER_SYNC_STALE_ANCHOR_LINK_FAILURES: u32 = 3;
pub(super) const HEADER_SYNC_STALE_ANCHOR_DISTINCT_PEERS: usize = 2;

#[derive(Clone, Debug)]
pub(super) struct HeaderSyncCore {
    pub(super) anchor: (block::Height, block::Hash),
    pub(super) finalized_height: block::Height,
    pub(super) verified_block_tip: block::Height,
    pub(super) verified_block_hash: block::Hash,
    pub(super) best_header_tip: block::Height,
    pub(super) best_header_hash: block::Hash,
    pub(super) peers: HashMap<ZakuraPeerId, PeerHeaderState>,
    pub(super) parked_peers: HashSet<ZakuraPeerId>,
    pub(super) seen: HeaderHashDedup,
    pub(super) pending_new_blocks: HashSet<block::Hash>,
    pub(super) schedule: RangeScheduler,
    pub(super) pending_commits: HashMap<PendingCommitKey, RangeRequest>,
    pub(super) advisory: HashMap<ZakuraPeerId, HeaderSyncAdvisoryPeerState>,
    pub(super) stale_anchor: StaleAnchorFailures,
}

impl HeaderSyncCore {
    pub(super) fn new(startup: &HeaderSyncStartup) -> Result<Self, HeaderSyncStartError> {
        validate_anchor(&startup.network, startup.anchor)?;
        let (best_header_tip, best_header_hash) = startup.best_header_tip.unwrap_or(startup.anchor);

        Ok(Self {
            anchor: startup.anchor,
            finalized_height: startup.frontiers.finalized_height,
            verified_block_tip: startup.frontiers.verified_block_tip,
            verified_block_hash: startup.frontiers.verified_block_hash,
            best_header_tip,
            best_header_hash,
            peers: HashMap::new(),
            parked_peers: HashSet::new(),
            seen: HeaderHashDedup::default(),
            pending_new_blocks: HashSet::new(),
            schedule: RangeScheduler::new(),
            pending_commits: HashMap::new(),
            advisory: HashMap::new(),
            stale_anchor: StaleAnchorFailures::default(),
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

#[derive(Clone, Debug, Default)]
pub(super) struct StaleAnchorFailures {
    pub(super) count: u32,
    pub(super) peers: HashSet<ZakuraPeerId>,
}

impl StaleAnchorFailures {
    pub(super) fn record(&mut self, peer: ZakuraPeerId) {
        self.count = self.count.saturating_add(1);
        self.peers.insert(peer);
    }

    pub(super) fn should_reanchor(&self) -> bool {
        self.count >= HEADER_SYNC_STALE_ANCHOR_LINK_FAILURES
            && self.peers.len() >= HEADER_SYNC_STALE_ANCHOR_DISTINCT_PEERS
    }

    pub(super) fn reset(&mut self) {
        self.count = 0;
        self.peers.clear();
    }
}

#[derive(Copy, Clone, Debug)]
pub(super) struct HeaderSyncAdvisoryPeerState {
    pub(super) summary: HeaderSyncServiceSummary,
    pub(super) observed_at: Instant,
    pub(super) failure_count: u32,
    pub(super) backoff_until: Option<Instant>,
}

impl HeaderSyncAdvisoryPeerState {
    pub(super) fn new(summary: HeaderSyncServiceSummary, observed_at: Instant) -> Self {
        Self {
            summary,
            observed_at,
            failure_count: 0,
            backoff_until: None,
        }
    }

    pub(super) fn refresh_summary(
        &mut self,
        summary: HeaderSyncServiceSummary,
        observed_at: Instant,
    ) {
        self.summary = summary;
        self.observed_at = observed_at;
    }

    pub(super) fn is_expired(&self, now: Instant) -> bool {
        now.duration_since(self.observed_at) >= HEADER_SYNC_ADVISORY_TTL
    }

    pub(super) fn is_backed_off(&self, now: Instant) -> bool {
        self.backoff_until.is_some_and(|until| until > now)
    }

    pub(super) fn record_confirmed(&mut self) {
        self.failure_count = 0;
        self.backoff_until = None;
    }

    pub(super) fn record_unconfirmed(&mut self, now: Instant) {
        self.failure_count = self.failure_count.saturating_add(1);
        if self.failure_count >= HEADER_SYNC_ADVISORY_BACKOFF_FAILURES {
            self.backoff_until = Some(now + HEADER_SYNC_ADVISORY_BACKOFF);
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct PeerHeaderState {
    pub(super) session: HeaderSyncPeerSession,
    pub(super) direction: ServicePeerDirection,
    pub(super) advertised_tip: block::Height,
    pub(super) advertised_hash: block::Hash,
    pub(super) anchor: block::Height,
    pub(super) max_headers_per_response: u32,
    pub(super) max_inflight_requests: u16,
    pub(super) received_status: bool,
    /// The most recent status sent to this peer over its current session, if
    /// any. Used to suppress re-sending an identical, non-tip-advancing status,
    /// which the peer's inbound rate limiter would otherwise treat as spam.
    pub(super) last_sent_status: Option<HeaderSyncStatus>,
    pub(super) outstanding: Vec<OutstandingRange>,
    pub(super) late_covered_responses: usize,
    pub(super) meters: HeaderSyncPeerMeters,
    pub(super) served_headers_inflight: u16,
    pub(super) misbehavior: u32,
}

impl PeerHeaderState {
    pub(super) fn new(
        session: HeaderSyncPeerSession,
        anchor: (block::Height, block::Hash),
        local_range: u32,
        local_inflight: u16,
        status_refresh_interval: Duration,
        inbound_status_min_interval: Duration,
        inbound_new_block_min_interval: Duration,
    ) -> Self {
        Self {
            direction: session.direction(),
            session,
            advertised_tip: anchor.0,
            advertised_hash: anchor.1,
            anchor: anchor.0,
            max_headers_per_response: clamp_advertised_range(local_range),
            max_inflight_requests: local_inflight.clamp(1, LOCAL_MAX_HS_INFLIGHT_PER_PEER),
            received_status: false,
            last_sent_status: None,
            outstanding: Vec::new(),
            late_covered_responses: 0,
            meters: HeaderSyncPeerMeters::new(
                status_refresh_interval,
                inbound_status_min_interval,
                inbound_new_block_min_interval,
            ),
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

    pub(super) fn restore_oldest_outstanding(&mut self, outstanding: OutstandingRange) {
        self.outstanding.insert(0, outstanding);
    }

    pub(super) fn take_late_covered_response(&mut self) -> bool {
        if self.late_covered_responses == 0 {
            return false;
        }
        self.late_covered_responses -= 1;
        true
    }

    /// Whether `status` differs from the most recent status sent to this peer
    /// over its current session. A status identical to the last one we sent is
    /// redundant — the peer cannot learn anything from it and its inbound status
    /// rate limiter would treat it as spam — so callers suppress it.
    pub(super) fn status_differs_from_last_sent(&self, status: HeaderSyncStatus) -> bool {
        self.last_sent_status != Some(status)
    }

    /// Records `status` as the most recent status sent to this peer, so a later
    /// identical status can be suppressed by [`Self::status_differs_from_last_sent`].
    pub(super) fn record_sent_status(&mut self, status: HeaderSyncStatus) {
        self.last_sent_status = Some(status);
    }

    /// Forgets the last status sent to this peer so the next one is always sent.
    /// Called when a fresh session replaces the peer's transport: the new
    /// channel's remote has received no status yet and gates serving us on it,
    /// so the initial status must go out regardless of its contents.
    pub(super) fn reset_sent_status(&mut self) {
        self.last_sent_status = None;
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

#[derive(Clone, Debug)]
pub(super) struct HeaderSyncPeerMeters {
    pub(super) unsolicited: RateMeter,
    pub(super) inbound_status: RateMeter,
    pub(super) inbound_new_block: RateMeter,
}

impl HeaderSyncPeerMeters {
    pub(super) fn new(
        status_refresh_interval: Duration,
        inbound_status_min_interval: Duration,
        inbound_new_block_min_interval: Duration,
    ) -> Self {
        Self {
            unsolicited: RateMeter::new(status_refresh_interval),
            inbound_status: RateMeter::new(inbound_status_min_interval),
            inbound_new_block: RateMeter::new(inbound_new_block_min_interval),
        }
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
