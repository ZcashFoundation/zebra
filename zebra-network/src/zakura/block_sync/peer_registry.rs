//! Shared per-peer fact table for Zakura block sync (S4).
//!
//! S4 moves all per-peer *download* state and the take-work decision off the
//! reactor's single loop into a spawned [`PeerRoutine`](super::peer_routine) per
//! connected peer. The [`PeerRegistry`] is the small shared table the reactor
//! still needs for *global* decisions — admission counting, the producer's
//! `!has_outstanding_request` filter, the low-water `total_unreceived` gate, and
//! candidate publication — plus the per-peer servable range / caps the routine
//! reads back when it runs its want-work loop.
//!
//! Field ownership is disjoint so the brief `std::sync::Mutex` is never a
//! contention point and is **never held across `.await`** (the anti-block rule).
//! After the S4 inverted data flow the **routine** is authoritative for its own
//! per-peer facts and writes them all (generation-gated): servable/caps/
//! `received_status` (when it decodes a `Status` frame in its own task),
//! `outstanding` (on issue/finish/timeout/disconnect — per *request*, never per
//! *body*), slot diagnostics, and download-side misbehavior. The **reactor** owns
//! only entry insert/remove (admission/teardown) and reports serving-side
//! misbehavior. The aggregate soft-misbehavior count lives here so the routine
//! (download offenses) and reactor (serving offenses) share one threshold.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Mutex as StdMutex,
};

use zebra_chain::block;

use super::{
    config::{clamp_advertised_blocks, clamp_advertised_inflight, clamp_advertised_response_bytes},
    state::EFFECTIVE_BS_OUTBOUND_INFLIGHT_PER_PEER,
    BlockSyncStatus, ServicePeerDirection, ZakuraPeerId,
};

/// Soft-misbehavior disconnect threshold shared by the routine and the reactor.
///
/// Kept identical to the reactor's pre-S4 `SOFT_MISBEHAVIOR_DISCONNECT_THRESHOLD`
/// so the aggregate count behaves exactly as before, now summed across the two
/// reporters (routine download offenses + reactor serving offenses).
pub(super) const SOFT_MISBEHAVIOR_DISCONNECT_THRESHOLD: u32 = 3;

/// Per-peer facts the reactor needs globally and the routine reads back.
#[derive(Clone, Debug)]
pub(super) struct Entry {
    pub(super) direction: ServicePeerDirection,
    pub(super) servable_low: block::Height,
    pub(super) servable_high: block::Height,
    pub(super) received_status: bool,
    pub(super) max_blocks_per_response: u32,
    pub(super) max_inflight_requests: u16,
    pub(super) max_response_bytes: u32,
    /// The height→hash set of this peer's *unreceived* in-flight request heights.
    /// Per-*request* granularity (each outstanding `BlockRangeRequest` contributes
    /// its still-unreceived expected heights), never per-body. This is the S3b
    /// producer filter's `!has_outstanding_request` home, now routine-owned and
    /// independent of `work.in_flight`, so it structurally closes the
    /// reject-rollback window.
    pub(super) outstanding: BTreeMap<block::Height, block::Hash>,
    pub(super) misbehavior: u32,
    /// Routine-published slot diagnostics (trace only): the per-peer download
    /// window state the reactor used to read off `PeerBlockState` for the periodic
    /// `BLOCK_SYNC_STATE` row. Updated whenever the routine issues/finishes/times
    /// out a request.
    pub(super) slots: SlotDiagnostics,
    /// Monotonic generation bumped each time a routine is (re)spawned for this
    /// peer. A cancelled routine's async `Drop` only clears outstanding when the
    /// generation still matches, so an old Drop racing a reset respawn cannot wipe
    /// the live routine's published outstanding.
    pub(super) generation: u64,
}

impl Entry {
    fn new(
        direction: ServicePeerDirection,
        config: &super::ZakuraBlockSyncConfig,
        generation: u64,
    ) -> Self {
        Self {
            direction,
            servable_low: block::Height::MIN,
            servable_high: block::Height::MIN,
            received_status: false,
            max_blocks_per_response: config.advertised_max_blocks_per_response(),
            max_inflight_requests: config.advertised_max_inflight_requests(),
            max_response_bytes: config.advertised_max_response_bytes(),
            outstanding: BTreeMap::new(),
            misbehavior: 0,
            slots: SlotDiagnostics::default(),
            generation,
        }
    }
}

/// Per-peer download window slot diagnostics published by the routine for the
/// reactor's periodic `BLOCK_SYNC_STATE` trace row.
#[derive(Copy, Clone, Debug, Default)]
pub(super) struct SlotDiagnostics {
    pub(super) hard_capacity: usize,
    pub(super) effective_window: usize,
    pub(super) available_slots: usize,
    pub(super) timeout_recovery_slots: usize,
    pub(super) outstanding_requests: usize,
}

/// The shared per-peer fact table. `Arc`-wrapped at the construction site so the
/// reactor and every routine share one table.
#[derive(Debug)]
pub(super) struct PeerRegistry {
    peers: StdMutex<HashMap<ZakuraPeerId, Entry>>,
    /// Source of monotonically-increasing routine generations.
    next_generation: std::sync::atomic::AtomicU64,
}

impl Default for PeerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PeerRegistry {
    pub(super) fn new() -> Self {
        Self {
            peers: StdMutex::new(HashMap::new()),
            next_generation: std::sync::atomic::AtomicU64::new(1),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ZakuraPeerId, Entry>> {
        self.peers
            .lock()
            .expect("peer registry mutex is never poisoned")
    }

    /// Admit (or re-admit) a peer and allocate a fresh routine generation.
    ///
    /// On a genuinely new peer this inserts a default entry; on a respawn (reset)
    /// the existing entry's servable/caps/`received_status` are preserved (the
    /// peer stays connected) but its outstanding set is cleared and its generation
    /// bumped, so the new routine owns the entry. Returns the generation the new
    /// routine must carry for its `Drop` guard.
    pub(super) fn admit(
        &self,
        peer: &ZakuraPeerId,
        direction: ServicePeerDirection,
        config: &super::ZakuraBlockSyncConfig,
    ) -> u64 {
        let generation = self
            .next_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut peers = self.lock();
        peers
            .entry(peer.clone())
            .and_modify(|entry| {
                entry.direction = direction;
                entry.outstanding.clear();
                entry.generation = generation;
            })
            .or_insert_with(|| Entry::new(direction, config, generation));
        generation
    }

    /// Remove a peer's entry entirely (disconnect/teardown/admission-reject).
    pub(super) fn remove(&self, peer: &ZakuraPeerId) {
        self.lock().remove(peer);
    }

    /// Publish a freshly-applied `Status` (routine-side, S4 inverted flow): grow
    /// servable range, clamp the advertised caps, and mark the peer as having sent
    /// a status. Generation-gated like the other routine writers so a superseded
    /// routine cannot clobber the live entry. No-op if the peer is gone.
    pub(super) fn upsert_status(
        &self,
        peer: &ZakuraPeerId,
        generation: u64,
        status: BlockSyncStatus,
    ) {
        let mut peers = self.lock();
        let Some(entry) = peers.get_mut(peer) else {
            return;
        };
        if entry.generation != generation {
            return;
        }
        entry.servable_low = status.servable_low;
        entry.servable_high = status.servable_high;
        entry.max_blocks_per_response = clamp_advertised_blocks(status.max_blocks_per_response);
        entry.max_inflight_requests = clamp_advertised_inflight(status.max_inflight_requests);
        entry.max_response_bytes = clamp_advertised_response_bytes(status.max_response_bytes);
        entry.received_status = true;
    }

    /// Replace the peer's outstanding height→hash set (routine-owned), but only if
    /// the routine's `generation` still owns the entry. A write from a routine
    /// that has been superseded by a respawn is dropped.
    pub(super) fn set_outstanding(
        &self,
        peer: &ZakuraPeerId,
        generation: u64,
        outstanding: BTreeMap<block::Height, block::Hash>,
    ) {
        let mut peers = self.lock();
        if let Some(entry) = peers.get_mut(peer) {
            if entry.generation == generation {
                entry.outstanding = outstanding;
            }
        }
    }

    /// Clear the peer's outstanding set (it has no live requests), generation-gated
    /// as in [`set_outstanding`](Self::set_outstanding).
    pub(super) fn clear_outstanding(&self, peer: &ZakuraPeerId, generation: u64) {
        let mut peers = self.lock();
        if let Some(entry) = peers.get_mut(peer) {
            if entry.generation == generation {
                entry.outstanding.clear();
            }
        }
    }

    /// Publish the routine's download-window slot diagnostics (trace only),
    /// generation-gated like the outstanding writers.
    pub(super) fn publish_slots(
        &self,
        peer: &ZakuraPeerId,
        generation: u64,
        slots: SlotDiagnostics,
    ) {
        let mut peers = self.lock();
        if let Some(entry) = peers.get_mut(peer) {
            if entry.generation == generation {
                entry.slots = slots;
            }
        }
    }

    /// Aggregate the routines' slot diagnostics for the periodic trace row.
    pub(super) fn slot_summary(&self) -> SlotSummary {
        let peers = self.lock();
        let mut summary = SlotSummary::default();
        for entry in peers.values() {
            summary.outstanding_requests = summary
                .outstanding_requests
                .saturating_add(entry.slots.outstanding_requests);
            if !entry.received_status {
                continue;
            }
            summary.capacity = summary.capacity.saturating_add(entry.slots.hard_capacity);
            summary.effective_window = summary
                .effective_window
                .saturating_add(entry.slots.effective_window);
            summary.available = summary
                .available
                .saturating_add(entry.slots.available_slots);
            summary.timeout_recovery = summary
                .timeout_recovery
                .saturating_add(entry.slots.timeout_recovery_slots);
            if entry.slots.available_slots == 0 {
                summary.saturated_peers = summary.saturated_peers.saturating_add(1);
            }
        }
        summary
    }

    /// Whether any connected peer has an outstanding request for `height`
    /// expecting `hash` (the producer's `!has_outstanding_request` filter and the
    /// `ignore_unmatched_active` fallthrough).
    pub(super) fn has_outstanding_request(&self, height: block::Height, hash: block::Hash) -> bool {
        let peers = self.lock();
        peers
            .values()
            .any(|entry| entry.outstanding.get(&height) == Some(&hash))
    }

    /// Whether any connected peer has an outstanding request covering `height`
    /// (regardless of hash). Used by the routine's terminator-dedup fallthrough
    /// (`ignore_unmatched_active_terminator_response`): a `BlocksDone` for a range
    /// another peer is actively requesting is dropped quietly, not scored.
    pub(super) fn has_outstanding_height(&self, height: block::Height) -> bool {
        let peers = self.lock();
        peers
            .values()
            .any(|entry| entry.outstanding.contains_key(&height))
    }

    /// Total unreceived in-flight heights summed across peers — *per request*,
    /// never per body (an `outstanding` entry is one requested height). Feeds the
    /// producer's low-water refill gate.
    pub(super) fn total_unreceived(&self) -> usize {
        let peers = self.lock();
        peers.values().map(|entry| entry.outstanding.len()).sum()
    }

    /// Whether any peer has an outstanding request reaching height `at_or_above`
    /// (the `peer_has_successor_after` half of the reset decision). Reads the
    /// registry's per-height outstanding set across peers.
    pub(super) fn any_outstanding_at_or_above(&self, at_or_above: block::Height) -> bool {
        let peers = self.lock();
        peers.values().any(|entry| {
            entry
                .outstanding
                .keys()
                .any(|height| *height >= at_or_above)
        })
    }

    /// Whether any peer has an outstanding request whose expected hash at `height`
    /// differs from `hash` (the peer-outstanding clause of
    /// `reset_tip_conflicts_with_local_work`).
    pub(super) fn any_outstanding_conflicts_at(
        &self,
        height: block::Height,
        hash: block::Hash,
    ) -> bool {
        let peers = self.lock();
        peers.values().any(|entry| {
            entry
                .outstanding
                .get(&height)
                .is_some_and(|expected| *expected != hash)
        })
    }

    /// Whether the peer has sent a `Status` (the reactor's serving-admission and
    /// disconnect-trace read). The routine owns the rest of the serving caps
    /// locally now (S4 inverted flow); only `received_status` is read reactor-side.
    pub(super) fn has_received_status(&self, peer: &ZakuraPeerId) -> bool {
        let peers = self.lock();
        peers.get(peer).is_some_and(|entry| entry.received_status)
    }

    /// Record one misbehavior offense and return whether the soft-disconnect
    /// threshold is now reached (so the reporter cancels the session). The count
    /// always increments; the returned `should_cancel` is only set for a soft
    /// reason at threshold, matching the pre-S4 reactor behavior.
    pub(super) fn record_misbehavior(&self, peer: &ZakuraPeerId, is_soft: bool) -> bool {
        let mut peers = self.lock();
        let Some(entry) = peers.get_mut(peer) else {
            return false;
        };
        entry.misbehavior = entry.misbehavior.saturating_add(1);
        is_soft && entry.misbehavior >= SOFT_MISBEHAVIOR_DISCONNECT_THRESHOLD
    }

    /// Count of peers that have sent a status (low-water refill + trace).
    pub(super) fn peers_with_status(&self) -> usize {
        let peers = self.lock();
        peers.values().filter(|entry| entry.received_status).count()
    }

    /// Candidate snapshot: node-id-servable hint per peer, used to publish the
    /// block-sync candidate set. Returns `(received_status, servable_low,
    /// servable_high)` per peer so the reactor can compute `can_serve_any`.
    pub(super) fn candidate_snapshot(
        &self,
    ) -> Vec<(ZakuraPeerId, bool, block::Height, block::Height)> {
        let peers = self.lock();
        peers
            .iter()
            .map(|(peer, entry)| {
                (
                    peer.clone(),
                    entry.received_status,
                    entry.servable_low,
                    entry.servable_high,
                )
            })
            .collect()
    }

    /// Per-direction peer / with-status counts for the periodic trace tick.
    pub(super) fn direction_status_counts(&self) -> DirectionStatusCounts {
        let peers = self.lock();
        let mut counts = DirectionStatusCounts::default();
        for entry in peers.values() {
            match entry.direction {
                ServicePeerDirection::Inbound => {
                    counts.inbound += 1;
                    if entry.received_status {
                        counts.inbound_with_status += 1;
                    }
                }
                ServicePeerDirection::Outbound => {
                    counts.outbound += 1;
                    if entry.received_status {
                        counts.outbound_with_status += 1;
                    }
                }
            }
        }
        counts
    }

    /// Snapshot for the `floor_gap_diagnostics` trace: for a target `height`,
    /// how many peers are servable and how many of those have an outstanding
    /// request covering it.
    pub(super) fn floor_gap_servable(&self, height: block::Height) -> (usize, usize) {
        let peers = self.lock();
        let mut servable = 0usize;
        let mut outstanding = 0usize;
        for entry in peers.values() {
            if entry.received_status
                && entry.servable_low <= height
                && height <= entry.servable_high
            {
                servable = servable.saturating_add(1);
            }
            if entry.outstanding.contains_key(&height) {
                outstanding = outstanding.saturating_add(1);
            }
        }
        (servable, outstanding)
    }
}

/// Aggregated slot diagnostics across peers for the periodic trace row.
#[derive(Copy, Clone, Debug, Default)]
pub(super) struct SlotSummary {
    pub(super) capacity: usize,
    pub(super) effective_window: usize,
    pub(super) available: usize,
    pub(super) timeout_recovery: usize,
    pub(super) saturated_peers: usize,
    pub(super) outstanding_requests: usize,
}

/// Per-direction peer counts for the periodic trace tick.
#[derive(Copy, Clone, Debug, Default)]
pub(super) struct DirectionStatusCounts {
    pub(super) inbound: usize,
    pub(super) outbound: usize,
    pub(super) inbound_with_status: usize,
    pub(super) outbound_with_status: usize,
}

/// Hard outbound concurrency ceiling for a peer with the given advertised
/// in-flight cap (the routine's slot bound; mirrors `PeerBlockState`).
pub(super) fn hard_outbound_capacity(max_inflight_requests: u16) -> usize {
    usize::from(max_inflight_requests).min(EFFECTIVE_BS_OUTBOUND_INFLIGHT_PER_PEER)
}
