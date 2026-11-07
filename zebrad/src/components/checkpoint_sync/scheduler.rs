//! Download scheduling (zips#1346 S:482-531): byte-sized units, pull
//! budgets, a byte lookahead, double stall detection, end-game duplication
//! and exact blame.
//!
//! The [`Scheduler`] is a plain state machine: it does no I/O and reads no
//! clock, every method takes `now`. The engine task owns it and drives it
//! from stream events and a [`SAMPLE_INTERVAL`] tick, and the scheduler
//! simulation in the tests drives it from a deterministic event queue.
//!
//! Units are planned lazily from the commit frontier upwards. A unit that
//! starts at a height whose verified known-hash entry is resident (see
//! [`HashSource`]) is anchored at a known hash: it has an exact size, and
//! ends at any height, at [`FRONTIER_UNIT_BYTES`] for the frontier unit and
//! [`DEEP_UNIT_BYTES`] deeper. Otherwise the unit is anchored at a
//! checkpoint: the frontier unit is exactly one checkpoint interval, deeper
//! units coalesce consecutive intervals while the estimate stays below
//! [`MAX_UNIT_BYTES`]. A truncated, stalled or failed unit keeps its
//! delivered top part; the remainder is planned again, at known hashes or at
//! the checkpoints inside it, and above the highest of them at the verified
//! `next_expected` hash ([`Anchor::Resumed`]).

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    future::Future,
    pin::Pin,
    sync::atomic::Ordering,
    time::Duration,
};

use tokio::time::Instant;

use zebra_chain::{
    block::{self, SyncHashEntry, MAX_BLOCK_BYTES},
    BoxError,
};
use zebra_network::sync_peer::{ConnectionId, BULK_STREAMS_PER_PEER};

use super::{
    anchors::{Anchors, SizeEstimator},
    engine::{Charged, EngineConfig},
};

/// Identifies one unit for the life of the engine.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct UnitId(pub u64);

/// What a unit's top block is trusted through.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Anchor {
    /// A hard-coded checkpoint (IBD-1).
    Checkpoint,
    /// A verified known-hash list entry (IBD-2).
    KnownHash,
    /// The verified `next_expected` hash of a truncated or stalled unit's
    /// delivered prefix.
    Resumed,
}

/// One unit of download work: the blocks `start..=end`, requested as one
/// `get-block-range` with `final_hash = end_hash` and `count = end - start + 1`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Unit {
    /// The unit's ID.
    pub id: UnitId,
    /// The lowest height.
    pub start: block::Height,
    /// The highest height: the anchor.
    pub end: block::Height,
    /// The hash at `end`.
    pub end_hash: block::Hash,
    /// How `end_hash` is trusted.
    pub anchor: Anchor,
    /// The request's `max_bytes`: the estimate, at least [`MAX_BLOCK_BYTES`],
    /// so that a response stays within the charge admission made for it.
    pub max_bytes: u32,
    /// The estimated (IBD-1) or exact (IBD-2) serialized size of the unit.
    pub est_bytes: u64,
}

/// The most bytes one unit requests (`MAX_GET_BLOCK_RANGE_BYTES`).
pub const MAX_UNIT_BYTES: u32 = 64 * 1024 * 1024;

/// The most blocks one unit requests (`MAX_GET_BLOCK_RANGE_COUNT`).
pub const MAX_UNIT_COUNT: u32 = 65_536;

/// The exact size of a known-hash frontier unit: small, so the lowest blocks
/// arrive and commit soon.
pub const FRONTIER_UNIT_BYTES: u64 = 16 * 1024 * 1024;

/// The exact size of a deeper known-hash unit: [`MAX_UNIT_BYTES`].
// A `u32` always fits in a `u64`.
pub const DEEP_UNIT_BYTES: u64 = MAX_UNIT_BYTES as u64;

/// A source of verified per-height entries that makes every height an
/// anchor.
///
/// Every entry a source returns is verified (its chunk matched its pin).
/// Units whose entries are not resident fall back to checkpoint anchors, so
/// a slow source slows the engine but never blocks it.
pub trait HashSource: fmt::Debug + Send + 'static {
    /// The highest height the list covers, verified or not.
    fn max_height(&self) -> block::Height;

    /// The verified entry at `height`, or `None` if its chunk is not
    /// verified and resident.
    fn entry(&self, height: block::Height) -> Option<SyncHashEntry>;

    /// Asks the source to make `start..=end` resident, fetching at most 4
    /// chunks ahead of `start`. Returns at once; progress is reported through
    /// [`HashSource::changed`].
    fn request(&mut self, start: block::Height, end: block::Height);

    /// Resolves when new entries became resident, or with an error if the
    /// source has failed for good (the engine then continues with checkpoint
    /// anchors only).
    fn changed(&mut self) -> Pin<Box<dyn Future<Output = Result<(), BoxError>> + Send + '_>>;

    /// Drops resident entries below `height` (the committed frontier).
    fn release_below(&mut self, height: block::Height);
}

/// Plans one known-hash unit from the verified entries of `source`: the
/// blocks `start..=end`, where `end` is the highest height at most `limit`
/// that keeps the exact size within `budget` bytes ([`FRONTIER_UNIT_BYTES`]
/// for the frontier unit, [`DEEP_UNIT_BYTES`] for the others).
///
/// Every height is an anchor, so the unit can end anywhere: it ends at the
/// budget, at `limit`, after [`MAX_UNIT_COUNT`] blocks, or below the first
/// height whose entry is not resident. It always has at least one block,
/// even one larger than `budget`.
///
/// Returns `None` if the entry at `start` is not resident: the caller then
/// plans the unit with a checkpoint anchor, so a slow known-hash source slows
/// the engine but never blocks it.
pub fn known_hash_unit(
    source: &(impl HashSource + ?Sized),
    id: UnitId,
    start: block::Height,
    limit: block::Height,
    budget: u64,
) -> Option<Unit> {
    if limit < start {
        return None;
    }

    let mut end_entry = source.entry(start)?;
    let mut end = start;
    let mut est_bytes = end_entry.size;
    let mut blocks = 1;

    while end < limit && blocks < MAX_UNIT_COUNT {
        let next = block::Height(end.0 + 1);
        let Some(entry) = source.entry(next) else {
            break;
        };
        let Some(total) = est_bytes
            .checked_add(entry.size)
            .filter(|&total| total <= budget)
        else {
            break;
        };
        est_bytes = total;
        end = next;
        end_entry = entry;
        blocks += 1;
    }

    Some(Unit {
        id,
        start,
        end,
        end_hash: end_entry.hash,
        anchor: Anchor::KnownHash,
        max_bytes: MAX_UNIT_BYTES,
        est_bytes,
    })
}

/// The charge per delivered byte: a deserialized `Arc<Block>` retains about
/// three times its serialized size (#12192's
/// `block_retained_charge` replaces it).
pub const RETAINED_FACTOR: u64 = 3;

/// How often rates are sampled and the stall rules evaluated.
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

/// The EWMA weight of one rate sample.
pub const RATE_ALPHA: f64 = 0.2;

/// The rate a new peer or stream starts with, in bytes per second.
pub const INITIAL_RATE: f64 = 2.0 * 1024.0 * 1024.0;

/// A peer pulls a new unit while its in-flight estimate is below this much
/// of its rate.
pub const PULL_BUDGET: Duration = Duration::from_millis(2_500);

/// Stall rule 1: no bytes for this long.
pub const STALL_NO_BYTES: Duration = Duration::from_secs(5);

/// Stall rule 2: a rate below `min_peer_rate` for this long.
pub const STALL_MIN_RATE_FOR: Duration = Duration::from_secs(10);

/// Stall rule 3: a rate below a quarter of the median for this long.
pub const STALL_SLOW_FOR: Duration = Duration::from_secs(15);

/// Stall rule 3 applies once this many peers are active.
pub const STALL_SLOW_MIN_PEERS: usize = 3;

/// How long a stalled or failed peer waits before its next pull.
pub const STALL_BACKOFF: Duration = Duration::from_secs(30);

/// How long a peer that answered `REFUSED` again, without delivering a
/// block since its last refusal, or that did not find a checkpointed range
/// below its own tip, is not selected.
///
/// A first refusal excludes the peer for [`STALL_BACKOFF`] only: `REFUSED`
/// also means that the server is at its stream limit, or still starting,
/// which says nothing about later requests.
pub const REFUSED_EXCLUSION: Duration = Duration::from_secs(10 * 60);

/// How often the probe slot rotates in an unselected peer.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(60);

/// The fewest selected peers the engine runs with.
pub const MIN_ENGINE_PEERS: usize = 2;

/// Identifies one `get-block-range` stream for the life of the engine.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct StreamId(pub u64);

/// A unit given to a peer: the engine opens one stream for it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Assignment {
    /// The stream to open.
    pub stream: StreamId,
    /// The peer to open it on.
    pub peer: ConnectionId,
    /// The unit to request.
    pub unit: Unit,
    /// Whether this is an end-game duplicate of a unit another peer streams.
    pub duplicate: bool,
}

/// Why a stream ended without completing its unit.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StreamFailure {
    /// The peer does not have the range.
    NotFound,
    /// The peer refused `get-block-range` before sending data.
    Refused,
    /// A transport error, a cancellation by the peer, or a closed connection.
    Transport,
    /// The peer violated the protocol, and the connection is closed.
    Violation,
}

/// Why the scheduler released a stream.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Release {
    /// The stream ended: the unit was complete, or truncated.
    End,
    /// The stream ended without delivering a block. A response MAY finish
    /// early, but one that never delivers makes no progress, so it is
    /// treated like a failure.
    EmptyEnd,
    /// The stream failed.
    Failure(StreamFailure),
    /// A stall rule fired.
    Stall,
    /// The peer left the registry.
    Gone,
}

/// A peer's receive-window change for the engine to apply.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct WindowChange {
    /// The peer.
    pub peer: ConnectionId,
    /// `true` when the peer was selected (64 MiB), `false` when deselected
    /// (8 MiB).
    pub selected: bool,
}

#[derive(Clone, Debug)]
struct UnitState {
    unit: Unit,
    /// The live streams of this unit: one, or two in the end-game.
    streams: Vec<StreamId>,
    /// The lowest delivered height and its `previous_block_hash`, across
    /// every stream of the unit. Streams deliver descending prefixes from
    /// `end`, so `low..=end` is all delivered.
    low: Option<(block::Height, block::Hash)>,
    /// The serialized bytes of newly delivered blocks.
    delivered: u64,
    /// The undelivered charge this unit currently contributes.
    charge: u64,
    /// When the unit's first live stream was assigned.
    assigned_at: Option<Instant>,
    /// Whether an end-game duplicate was ever given for this unit.
    duplicated: bool,
}

#[derive(Clone, Debug)]
struct StreamState {
    unit: UnitId,
    peer: ConnectionId,
    /// When the stream last received bytes, of a block or of a partly read
    /// one.
    last_bytes_at: Instant,
    /// Whether the stream delivered any block.
    delivered_any: bool,
    /// The bytes received since the last rate sample.
    sample_bytes: u64,
    /// The bytes counted into rate samples so far: the larger of the
    /// stream's reported received bytes and its delivered blocks' bytes, so
    /// that bytes reported by both are counted once.
    counted_bytes: u64,
    /// The serialized bytes of the blocks the stream delivered.
    block_bytes: u64,
    rate: f64,
    below_min_since: Option<Instant>,
    slow_since: Option<Instant>,
}

impl StreamState {
    /// Counts the bytes above [`StreamState::counted_bytes`] of a running
    /// total of `bytes` as received now.
    fn count_bytes(&mut self, bytes: u64, now: Instant) {
        if bytes > self.counted_bytes {
            self.sample_bytes = self.sample_bytes.saturating_add(bytes - self.counted_bytes);
            self.counted_bytes = bytes;
            self.last_bytes_at = now;
        }
    }
}

#[derive(Clone, Debug)]
struct PeerState {
    remote_tip: block::Height,
    rate: f64,
    streams: usize,
    selected: bool,
    unselected_since: Instant,
    backoff_until: Option<Instant>,
    refused_until: Option<Instant>,
    duplicates: u32,
    /// Whether `rate` has a sample with bytes in it, rather than only the
    /// rate the peer started with.
    measured: bool,
    /// Whether the peer refused a stream, and delivered no block since.
    refused: bool,
}

impl PeerState {
    fn excluded(&self, now: Instant) -> bool {
        self.refused_until.is_some_and(|until| now < until)
    }

    fn backing_off(&self, now: Instant) -> bool {
        self.backoff_until.is_some_and(|until| now < until)
    }
}

/// The download scheduler.
#[derive(Debug)]
pub struct Scheduler {
    lookahead_bytes: u64,
    min_peer_rate: f64,
    target_peers: usize,

    anchors: Anchors,
    estimator: SizeEstimator,
    target: block::Height,
    /// The highest height planned for now: the target, or with headers-first
    /// sync the highest checkpoint at or below the verified header chain tip.
    ceiling: block::Height,

    /// The lowest uncommitted height.
    frontier: block::Height,
    /// The highest height in a planned unit; `start - 1` before planning.
    planned_through: Option<block::Height>,
    /// The planned units' spans, `end -> start`, for the per-unit block
    /// counts sent to the legacy syncer.
    planned_spans: BTreeMap<block::Height, block::Height>,

    next_unit: u64,
    next_stream: u64,
    units: BTreeMap<UnitId, UnitState>,
    /// The unassigned units, by start height.
    queue: BTreeSet<(block::Height, UnitId)>,
    streams: BTreeMap<StreamId, StreamState>,
    peers: HashMap<ConnectionId, PeerState>,

    /// The Σ of the units' undelivered charges.
    undelivered: u64,
    /// `RETAINED_FACTOR ×` the delivered, uncommitted bytes.
    buffered: u64,
    charged: Charged,

    last_tick: Instant,
    last_probe: Instant,
    /// The frontier unit's failures since the frontier last advanced.
    frontier_failures: u32,
    /// The peers whose streams of the frontier unit failed since the
    /// frontier last advanced.
    frontier_failed_peers: BTreeSet<ConnectionId>,
    window_changes: Vec<WindowChange>,

    /// The known-hash list, if this network has one and it has not failed.
    hashes: Option<Box<dyn HashSource>>,
    /// The number of units planned at a known hash.
    known_hash_units: u64,
}

impl Scheduler {
    /// A scheduler for the heights `start..=target`, where `target` is a
    /// checkpoint.
    pub fn new(
        config: &EngineConfig,
        anchors: Anchors,
        start: block::Height,
        target: block::Height,
        charged: Charged,
        now: Instant,
    ) -> Self {
        debug_assert!(anchors.contains(target), "targets are checkpoints");

        Scheduler {
            lookahead_bytes: config.lookahead_bytes,
            // A lossy cast is fine: it is a rate threshold.
            min_peer_rate: config.min_peer_rate as f64,
            target_peers: config.target_peers.max(MIN_ENGINE_PEERS),
            anchors,
            estimator: SizeEstimator::default(),
            target,
            ceiling: target,
            frontier: start,
            planned_through: start.previous().ok(),
            planned_spans: BTreeMap::new(),
            next_unit: 0,
            next_stream: 0,
            units: BTreeMap::new(),
            queue: BTreeSet::new(),
            streams: BTreeMap::new(),
            peers: HashMap::new(),
            undelivered: 0,
            buffered: 0,
            charged,
            last_tick: now,
            last_probe: now,
            frontier_failures: 0,
            frontier_failed_peers: BTreeSet::new(),
            window_changes: Vec::new(),
            hashes: None,
            known_hash_units: 0,
        }
    }

    /// Plans no unit above `ceiling`, a checkpoint, until it is raised: with
    /// headers-first sync, no body is requested before its header is
    /// verified. Units already planned stay planned, and the ceiling never
    /// rises above the target.
    pub fn set_ceiling(&mut self, ceiling: block::Height) {
        debug_assert!(self.anchors.contains(ceiling), "ceilings are checkpoints");
        self.ceiling = ceiling.min(self.target);
    }

    /// Plans units at the verified entries of `source` where they are
    /// resident, and asks it for the entries from the commit frontier up.
    pub fn with_hash_source(mut self, mut source: Box<dyn HashSource>) -> Self {
        source.request(self.frontier, self.target);
        self.hashes = Some(source);
        self
    }

    /// Resolves when the known-hash source has new entries, or with an error
    /// if it has failed for good. Never resolves without a source.
    pub fn hashes_changed(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<(), BoxError>> + Send + '_>> {
        match self.hashes.as_mut() {
            Some(source) => source.changed(),
            None => Box::pin(std::future::pending()),
        }
    }

    /// Stops using the known-hash source, which has failed for good: later
    /// units are anchored at checkpoints only.
    pub fn drop_hash_source(&mut self) {
        self.hashes = None;
    }

    /// The verified known-hash entry at `height`, if it is resident.
    pub fn known_hash(&self, height: block::Height) -> Option<SyncHashEntry> {
        self.hashes.as_ref()?.entry(height)
    }

    /// The number of units planned at a known hash so far.
    pub fn known_hash_units(&self) -> u64 {
        self.known_hash_units
    }

    // Queries

    /// The current lookahead charge.
    pub fn charged(&self) -> u64 {
        self.undelivered + self.buffered
    }

    /// The charge of the frontier unit, which is always admitted.
    #[cfg(test)]
    pub fn frontier_charge(&self) -> u64 {
        self.units
            .values()
            .filter(|state| self.is_frontier(&state.unit))
            .map(|state| state.charge)
            .sum()
    }

    /// Whether `unit` is in the checkpoint interval that holds the commit
    /// frontier, the lowest uncommitted height. The engine submits whole
    /// intervals, so every block of that interval must stay fetchable: a
    /// block discarded in the middle of it would otherwise wait forever
    /// behind a full lookahead.
    fn is_frontier(&self, unit: &Unit) -> bool {
        unit.start <= self.frontier_interval_end()
    }

    /// The checkpoint that ends the interval holding the commit frontier.
    fn frontier_interval_end(&self) -> block::Height {
        self.anchors
            .at_or_above(self.frontier)
            .map_or(self.frontier, |(checkpoint, _)| checkpoint)
    }

    /// The number of selected peers.
    pub fn selected_peers(&self) -> usize {
        self.peers.values().filter(|peer| peer.selected).count()
    }

    /// Whether every height up to the target was planned and delivered.
    #[cfg(test)]
    pub fn is_done(&self) -> bool {
        self.planned_through
            .is_some_and(|planned| planned >= self.target)
            && self.units.is_empty()
    }

    /// Whether `stream` is live: events of other streams are stale.
    pub fn is_live(&self, stream: StreamId) -> bool {
        self.streams.contains_key(&stream)
    }

    /// The failures of the frontier unit's streams since the frontier last
    /// advanced.
    pub fn frontier_failures(&self) -> u32 {
        self.frontier_failures
    }

    /// The peers whose frontier streams failed since the frontier last
    /// advanced.
    pub fn frontier_failed_peers(&self) -> &BTreeSet<ConnectionId> {
        &self.frontier_failed_peers
    }

    /// The peers excluded at `now` for refusing ranges: `REFUSED` again
    /// without a block in between, or `NOT_FOUND` for a checkpointed range
    /// below their tip.
    pub fn refused_peers(&self, now: Instant) -> impl Iterator<Item = ConnectionId> + '_ {
        self.peers
            .iter()
            .filter(move |(_, peer)| peer.excluded(now))
            .map(|(id, _)| *id)
    }

    /// Whether the frontier has failed `limit` times without progress (the
    /// "5 restarts without progress" fallback), counted so that one peer
    /// cannot end the run alone: either `limit` distinct peers failed it, or
    /// it failed `limit` times and every eligible peer is among those that
    /// failed it.
    pub fn frontier_stuck(&self, limit: u32, now: Instant) -> bool {
        // A peer count fits in `u32`.
        if self.frontier_failed_peers.len() as u32 >= limit {
            return true;
        }
        self.frontier_failures >= limit
            && self
                .peers
                .iter()
                .filter(|(_, peer)| self.eligible(peer, now))
                .all(|(id, _)| self.frontier_failed_peers.contains(id))
    }

    /// Starts counting [`Scheduler::frontier_failures`] again, as if the
    /// frontier had advanced.
    pub fn reset_frontier_failures(&mut self) {
        self.frontier_failures = 0;
        self.frontier_failed_peers.clear();
    }

    /// The bytes-per-block estimate, for tests and metrics.
    pub fn bytes_per_block(&self) -> f64 {
        self.estimator.bytes_per_block()
    }

    /// Takes the receive-window changes since the last call.
    pub fn take_window_changes(&mut self) -> Vec<WindowChange> {
        std::mem::take(&mut self.window_changes)
    }

    // Peers

    /// Replaces the candidate peers: `(id, remote tip height)` of each
    /// connected peer that may serve ranges. Returns the streams of departed
    /// peers, which the scheduler has released and the engine must cancel.
    pub fn set_peers(
        &mut self,
        candidates: &[(ConnectionId, block::Height)],
        now: Instant,
    ) -> Vec<StreamId> {
        let ids: BTreeSet<ConnectionId> = candidates.iter().map(|(id, _)| *id).collect();

        let gone: Vec<StreamId> = self
            .streams
            .iter()
            .filter(|(_, stream)| !ids.contains(&stream.peer))
            .map(|(id, _)| *id)
            .collect();
        for stream in &gone {
            self.release(*stream, Release::Gone, now);
        }
        self.peers.retain(|id, _| ids.contains(id));

        let new_peer_rate = self.new_peer_rate();
        for (id, remote_tip) in candidates {
            self.peers
                .entry(*id)
                .and_modify(|peer| peer.remote_tip = *remote_tip)
                .or_insert(PeerState {
                    remote_tip: *remote_tip,
                    rate: new_peer_rate,
                    streams: 0,
                    selected: false,
                    unselected_since: now,
                    backoff_until: None,
                    refused_until: None,
                    duplicates: 0,
                    measured: false,
                    refused: false,
                });
        }

        self.reselect(now);
        gone
    }

    /// The rate a peer that connects now starts with: [`INITIAL_RATE`], or
    /// half the median measured peer rate once a peer has been measured.
    ///
    /// Peers are selected and pull units fastest first, so a new peer that
    /// started above the measured peers would be given the frontier unit
    /// before them, on nothing but its own word.
    fn new_peer_rate(&self) -> f64 {
        let mut rates: Vec<f64> = self
            .peers
            .values()
            .filter(|peer| peer.measured)
            .map(|peer| peer.rate)
            .collect();
        if rates.is_empty() {
            return INITIAL_RATE;
        }
        rates.sort_by(f64::total_cmp);
        (rates[rates.len() / 2] / 2.0).min(INITIAL_RATE)
    }

    /// Whether `peer` may be selected: it is not excluded, and its
    /// advertised tip reaches the checkpoint interval that holds the commit
    /// frontier. A peer below it can serve none of the units the engine
    /// needs next, and must not count toward [`MIN_ENGINE_PEERS`].
    fn eligible(&self, peer: &PeerState, now: Instant) -> bool {
        !peer.excluded(now) && peer.remote_tip >= self.frontier_interval_end()
    }

    /// Fills the selection up to the target with the best eligible peers,
    /// and drops ineligible peers from it.
    fn reselect(&mut self, now: Instant) {
        let mut deselect = Vec::new();
        for (id, peer) in &self.peers {
            if peer.selected && !self.eligible(peer, now) {
                deselect.push(*id);
            }
        }
        for id in deselect {
            self.set_selected(id, false, now);
        }

        while self.selected_peers() < self.target_peers {
            let Some(best) =
                self.best_unselected(now, |a, b| b.1.rate.total_cmp(&a.1.rate).then(a.0.cmp(b.0)))
            else {
                break;
            };
            self.set_selected(best, true, now);
        }
    }

    /// The probe slot: every [`PROBE_INTERVAL`], when more peers are
    /// available than selected, replaces the slowest selected peer with the
    /// peer that has waited longest.
    fn probe(&mut self, now: Instant) {
        if now.saturating_duration_since(self.last_probe) < PROBE_INTERVAL {
            return;
        }
        self.last_probe = now;

        if self.selected_peers() < self.target_peers {
            return;
        }
        let Some(waiting) = self.best_unselected(now, |a, b| {
            a.1.unselected_since
                .cmp(&b.1.unselected_since)
                .then(a.0.cmp(b.0))
        }) else {
            return;
        };
        let slowest = self
            .peers
            .iter()
            .filter(|(_, peer)| peer.selected)
            .min_by(|a, b| a.1.rate.total_cmp(&b.1.rate).then(b.0.cmp(a.0)))
            .map(|(id, _)| *id)
            .expect("the selection is full, so it is not empty");

        self.set_selected(slowest, false, now);
        self.set_selected(waiting, true, now);
    }

    fn best_unselected(
        &self,
        now: Instant,
        order: impl Fn(&(&ConnectionId, &PeerState), &(&ConnectionId, &PeerState)) -> std::cmp::Ordering,
    ) -> Option<ConnectionId> {
        self.peers
            .iter()
            .filter(|(_, peer)| !peer.selected && self.eligible(peer, now))
            .min_by(|a, b| order(a, b))
            .map(|(id, _)| *id)
    }

    fn set_selected(&mut self, id: ConnectionId, selected: bool, now: Instant) {
        let peer = self.peers.get_mut(&id).expect("callers pass known peers");
        if peer.selected == selected {
            return;
        }
        peer.selected = selected;
        if !selected {
            peer.unselected_since = now;
        }
        self.window_changes
            .push(WindowChange { peer: id, selected });
    }

    // Assignment

    /// The next unit for a peer with a free pull budget, or
    /// `None` if no peer can pull now. The engine calls this until it returns
    /// `None` after every event.
    pub fn next_assignment(&mut self, now: Instant) -> Option<Assignment> {
        let mut pullers: Vec<(ConnectionId, f64)> = self
            .peers
            .iter()
            .filter(|(id, peer)| self.can_pull(**id, peer, now))
            .map(|(id, peer)| (*id, peer.rate))
            .collect();
        pullers.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));

        for (peer, _) in pullers {
            if let Some(assignment) = self.assign_to(peer, now) {
                return Some(assignment);
            }
        }

        None
    }

    fn can_pull(&self, id: ConnectionId, peer: &PeerState, now: Instant) -> bool {
        if !peer.selected
            || peer.excluded(now)
            || peer.backing_off(now)
            || peer.streams >= BULK_STREAMS_PER_PEER
        {
            return false;
        }

        let inflight: u64 = self
            .streams
            .values()
            .filter(|stream| stream.peer == id)
            .filter_map(|stream| self.units.get(&stream.unit))
            .map(|state| state.unit.est_bytes.saturating_sub(state.delivered))
            .sum();

        // A lossy cast is fine: it is a budget.
        (inflight as f64) < PULL_BUDGET.as_secs_f64() * peer.rate
    }

    fn assign_to(&mut self, peer: ConnectionId, now: Instant) -> Option<Assignment> {
        let remote_tip = self.peers[&peer].remote_tip;

        loop {
            let candidate = self
                .queue
                .iter()
                .map(|(_, id)| &self.units[id])
                .find(|state| state.unit.end <= remote_tip)
                .map(|state| state.unit);

            match candidate {
                Some(unit) if self.admissible(&unit) => {
                    return Some(self.start_stream(peer, unit.id, false, now));
                }
                // The lowest unit this peer can serve does not fit: neither
                // does anything above it.
                Some(_) => return None,
                None => {
                    if self
                        .planned_through
                        .is_some_and(|planned| planned >= remote_tip && planned < self.target)
                    {
                        // A new unit would end above this peer's tip: do not
                        // plan ahead with estimates for a peer that cannot
                        // serve it.
                        return None;
                    }
                    if !self.plan_unit() {
                        break;
                    }
                }
            }
        }

        self.end_game(peer, now)
    }

    /// Whether `unit` fits the lookahead, `charged + est ≤ lookahead_bytes`:
    /// an undelivered estimate is charged once, its delivered
    /// bytes at [`RETAINED_FACTOR`]. The frontier unit always fits: the commit
    /// frontier must always be fetchable, or delivered-but-uncommitted blocks
    /// above it could fill the lookahead forever (memory-budget §4.2).
    fn admissible(&self, unit: &Unit) -> bool {
        self.is_frontier(unit)
            || self.charged().saturating_add(unit.est_bytes) <= self.lookahead_bytes
    }

    /// End-game: with nothing left to assign, a peer is given a
    /// duplicate of the unit with the oldest assignment. At most one
    /// duplicate exists per unit, and each peer gets at most one.
    fn end_game(&mut self, peer: ConnectionId, now: Instant) -> Option<Assignment> {
        let planned_all = self
            .planned_through
            .is_some_and(|planned| planned >= self.target);
        if !planned_all || !self.queue.is_empty() || self.peers[&peer].duplicates > 0 {
            return None;
        }

        let remote_tip = self.peers[&peer].remote_tip;
        let oldest = self
            .units
            .values()
            .filter(|state| {
                !state.duplicated
                    && state.streams.len() == 1
                    && state.unit.end <= remote_tip
                    && self.streams[&state.streams[0]].peer != peer
            })
            .min_by_key(|state| (state.assigned_at, state.unit.start))
            .map(|state| state.unit.id)?;

        if let Some(puller) = self.peers.get_mut(&peer) {
            puller.duplicates += 1;
        }
        if let Some(state) = self.units.get_mut(&oldest) {
            state.duplicated = true;
        }

        Some(self.start_stream(peer, oldest, true, now))
    }

    fn start_stream(
        &mut self,
        peer: ConnectionId,
        unit: UnitId,
        duplicate: bool,
        now: Instant,
    ) -> Assignment {
        let stream = StreamId(self.next_stream);
        self.next_stream += 1;

        let state = self.units.get_mut(&unit).expect("callers pass known units");
        self.queue.remove(&(state.unit.start, unit));
        state.streams.push(stream);
        state.assigned_at.get_or_insert(now);
        let assigned = state.unit;

        self.streams.insert(
            stream,
            StreamState {
                unit,
                peer,
                last_bytes_at: now,
                delivered_any: false,
                sample_bytes: 0,
                counted_bytes: 0,
                block_bytes: 0,
                rate: INITIAL_RATE,
                below_min_since: None,
                slow_since: None,
            },
        );
        self.peers
            .get_mut(&peer)
            .expect("callers pass known peers")
            .streams += 1;

        self.recharge(unit);

        Assignment {
            stream,
            peer,
            unit: assigned,
            duplicate,
        }
    }

    // Planning

    /// Plans the next unit above `planned_through` into the queue. Returns
    /// `false` when everything up to the ceiling is planned.
    fn plan_unit(&mut self) -> bool {
        let limit = self.target.min(self.ceiling);
        let start = match self.planned_through {
            Some(planned) if planned >= limit => return false,
            Some(planned) => planned.next().expect("below the target"),
            None => block::Height(0),
        };

        let (end, end_hash, anchor, est) = self.plan_piece(start, limit, None);
        self.planned_through = Some(end);
        self.planned_spans.insert(end, start);
        self.enqueue(start, end, end_hash, anchor, est);
        true
    }

    /// Queues `start..=end`, whose top hash `end_hash` is verified, again:
    /// the remainder of a truncated, stalled or failed unit, or a discarded
    /// block. It is planned like new work, anchored at the checkpoints inside
    /// it, so the part holding the frontier is again a single interval and
    /// the rest can stream from other peers at once.
    fn requeue_range(&mut self, start: block::Height, end: block::Height, end_hash: block::Hash) {
        let mut low = start;
        loop {
            let (high, hash, anchor, est) = self.plan_piece(low, end, Some(end_hash));
            self.enqueue(low, high, hash, anchor, est);
            if high >= end {
                break;
            }
            low = high.next().expect("below end");
        }
    }

    /// Plans one unit from `start`, ending at or below `limit`.
    ///
    /// If the known-hash entry at `start` is resident, the unit is anchored
    /// at a known hash, with an exact size of at most [`FRONTIER_UNIT_BYTES`]
    /// for the frontier unit or [`DEEP_UNIT_BYTES`] deeper (see
    /// [`known_hash_unit`]).
    ///
    /// Otherwise, the frontier unit is exactly one checkpoint interval, a
    /// deeper unit coalesces consecutive intervals while the estimate stays
    /// below [`MAX_UNIT_BYTES`]. Above the last checkpoint below `limit`, the
    /// unit is anchored at `limit_hash`.
    fn plan_piece(
        &self,
        start: block::Height,
        limit: block::Height,
        limit_hash: Option<block::Hash>,
    ) -> (block::Height, block::Hash, Anchor, u64) {
        let is_frontier = start <= self.frontier_interval_end();

        if let Some(source) = self.hashes.as_deref() {
            let budget = if is_frontier {
                FRONTIER_UNIT_BYTES
            } else {
                DEEP_UNIT_BYTES
            };
            // The ID is assigned when the unit is queued.
            if let Some(unit) =
                known_hash_unit(source, UnitId(self.next_unit), start, limit, budget)
            {
                return (unit.end, unit.end_hash, unit.anchor, unit.est_bytes);
            }
        }

        let Some((mut end, mut end_hash)) = self
            .anchors
            .at_or_above(start)
            .filter(|(checkpoint, _)| *checkpoint <= limit)
        else {
            let hash = limit_hash.expect("a range without a checkpoint is a verified remainder");
            return (
                limit,
                hash,
                Anchor::Resumed,
                self.estimate_span(start, limit),
            );
        };
        let mut est = self.estimator.estimate(&self.anchors, start, end);

        // The frontier unit is exactly one interval: delivery is descending,
        // so it must be small to reach the frontier quickly.
        while !is_frontier && end < limit {
            let next_start = end.next().expect("below the limit");
            let Some((next_end, next_hash)) = self
                .anchors
                .at_or_above(next_start)
                .filter(|(checkpoint, _)| *checkpoint <= limit)
            else {
                break;
            };
            let next_est = self.estimator.estimate(&self.anchors, next_start, next_end);

            // Strictly below: two 32 MiB intervals hold a little more than
            // 64 MiB, and would always truncate.
            if est + next_est >= u64::from(MAX_UNIT_BYTES)
                || next_end.0 - start.0 + 1 > MAX_UNIT_COUNT
            {
                break;
            }
            (end, end_hash) = (next_end, next_hash);
            est += next_est;
        }

        (end, end_hash, Anchor::Checkpoint, est)
    }

    fn enqueue(
        &mut self,
        start: block::Height,
        end: block::Height,
        end_hash: block::Hash,
        anchor: Anchor,
        est_bytes: u64,
    ) {
        let id = UnitId(self.next_unit);
        self.next_unit += 1;
        if anchor == Anchor::KnownHash {
            self.known_hash_units += 1;
        }

        // A response holds at most `max_bytes` after its first block, so a
        // larger estimate only means the unit will be truncated.
        let est_bytes = est_bytes.min(u64::from(MAX_UNIT_BYTES));
        // Admission charges only the estimate, so asking for no more than it
        // keeps a peer's bodies, inflated or not, within the lookahead charge,
        // up to two blocks per stream. At least one maximum-size block, so
        // that a small unit is not truncated after every block. An honest
        // response that exceeds its estimate is truncated and resumed.
        let max_bytes = u32::try_from(est_bytes.max(MAX_BLOCK_BYTES)).expect(
            "the estimate is capped at MAX_UNIT_BYTES, a u32, which exceeds MAX_BLOCK_BYTES",
        );

        let unit = Unit {
            id,
            start,
            end,
            end_hash,
            anchor,
            max_bytes,
            est_bytes,
        };
        self.units.insert(
            id,
            UnitState {
                unit,
                streams: Vec::new(),
                low: None,
                delivered: 0,
                charge: 0,
                assigned_at: None,
                duplicated: false,
            },
        );
        self.queue.insert((start, id));
    }

    /// The estimate of `start..=end`, summed over the intervals it overlaps.
    fn estimate_span(&self, start: block::Height, end: block::Height) -> u64 {
        let mut est = 0;
        let mut low = start;
        while low <= end {
            let (interval_end, _) = self
                .anchors
                .at_or_above(low)
                .expect("spans end at or below a checkpoint");
            let high = interval_end.min(end);

            // `high` is in the interval ending at `interval_end`: estimate the
            // part of it.
            let part = self.estimator.estimate(&self.anchors, low, interval_end);
            let part_blocks = u64::from(interval_end.0 - low.0) + 1;
            let blocks = u64::from(high.0 - low.0) + 1;
            est += part.saturating_mul(blocks) / part_blocks;

            match high.next() {
                Ok(next) => low = next,
                Err(_) => break,
            }
        }
        est
    }

    // Stream events

    /// A block of `bytes` serialized bytes at `height`, whose header's
    /// `previous_block_hash` is `prev_hash`, arrived on `stream`. `is_new` is
    /// whether the engine's window took it (it is `false` for a block another
    /// stream already delivered).
    ///
    /// Returns the streams to cancel: all of the unit's streams, once the
    /// unit is complete.
    pub fn on_block(
        &mut self,
        stream: StreamId,
        height: block::Height,
        bytes: u64,
        prev_hash: block::Hash,
        is_new: bool,
        now: Instant,
    ) -> Vec<StreamId> {
        let Some(state) = self.streams.get_mut(&stream) else {
            return Vec::new();
        };
        state.delivered_any = true;
        state.block_bytes = state.block_bytes.saturating_add(bytes);
        let block_bytes = state.block_bytes;
        state.count_bytes(block_bytes, now);
        let unit_id = state.unit;
        if let Some(peer) = self.peers.get_mut(&state.peer) {
            peer.refused = false;
        }

        if is_new {
            self.buffered = self
                .buffered
                .saturating_add(RETAINED_FACTOR.saturating_mul(bytes));
            // Deliveries are a timelier size sample than commits: a deep unit
            // that holds the frontier commits nothing until its bottom block
            // arrives.
            self.estimator.observe(1, bytes);
        }

        let unit = self
            .units
            .get_mut(&unit_id)
            .expect("live streams have units");
        if is_new {
            unit.delivered = unit.delivered.saturating_add(bytes);
        }
        if unit.low.is_none_or(|(low, _)| height < low) {
            unit.low = Some((height, prev_hash));
        }

        let complete = unit.low.is_some_and(|(low, _)| low <= unit.unit.start);
        if !complete {
            self.recharge(unit_id);
            return Vec::new();
        }

        // Complete: every height of the unit is delivered.
        let unit = self.units.remove(&unit_id).expect("just found");
        // A lossy cast is fine: it is a metric.
        metrics::histogram!("sync.v2.unit.bytes").record(unit.delivered as f64);
        self.undelivered -= unit.charge;
        for stream in &unit.streams {
            self.forget_stream(*stream);
        }
        self.publish_charge();

        unit.streams
    }

    /// `stream` has received `received` response bytes in all, a partly read
    /// block included. The stall rules and rates count bytes as they arrive,
    /// not only once a whole block has been read.
    pub fn on_bytes(&mut self, stream: StreamId, received: u64, now: Instant) {
        if let Some(state) = self.streams.get_mut(&stream) {
            state.count_bytes(received, now);
        }
    }

    /// `stream` finished: its unit is complete or was truncated.
    pub fn on_end(&mut self, stream: StreamId, now: Instant) {
        self.release(stream, Release::End, now);
    }

    /// `stream` failed.
    pub fn on_failure(&mut self, stream: StreamId, failure: StreamFailure, now: Instant) {
        self.release(stream, Release::Failure(failure), now);
    }

    /// Samples rates and applies the stall rules. Returns the
    /// stalled streams, which the scheduler has released and the engine must
    /// cancel.
    pub fn tick(&mut self, now: Instant) -> Vec<StreamId> {
        let elapsed = now.saturating_duration_since(self.last_tick).as_secs_f64();
        self.last_tick = now;

        if elapsed > 0.0 {
            let mut peer_samples: HashMap<ConnectionId, u64> = HashMap::new();
            for stream in self.streams.values_mut() {
                // Lossy casts are fine: these are rates.
                let sample = stream.sample_bytes as f64 / elapsed;
                stream.rate = stream.rate * (1.0 - RATE_ALPHA) + sample * RATE_ALPHA;
                *peer_samples.entry(stream.peer).or_default() += stream.sample_bytes;
                stream.sample_bytes = 0;
            }
            for (id, bytes) in peer_samples {
                if let Some(peer) = self.peers.get_mut(&id) {
                    let sample = bytes as f64 / elapsed;
                    peer.rate = peer.rate * (1.0 - RATE_ALPHA) + sample * RATE_ALPHA;
                    peer.measured |= bytes > 0;
                    metrics::histogram!("sync.v2.peer.rate").record(peer.rate);
                }
            }
        }

        let median = self.median_stream_rate();
        let mut stalled = Vec::new();
        for (id, stream) in self.streams.iter_mut() {
            if stream.rate < self.min_peer_rate {
                stream.below_min_since.get_or_insert(now);
            } else {
                stream.below_min_since = None;
            }
            match median {
                Some(median) if stream.rate < median / 4.0 => {
                    stream.slow_since.get_or_insert(now);
                }
                _ => stream.slow_since = None,
            }

            let silent = now.saturating_duration_since(stream.last_bytes_at) >= STALL_NO_BYTES;
            let below_min = stream
                .below_min_since
                .is_some_and(|since| now.saturating_duration_since(since) >= STALL_MIN_RATE_FOR);
            let slow = stream
                .slow_since
                .is_some_and(|since| now.saturating_duration_since(since) >= STALL_SLOW_FOR);

            let rule = if silent {
                "no_bytes"
            } else if below_min {
                "min_rate"
            } else if slow {
                "slow"
            } else {
                continue;
            };
            metrics::counter!("sync.v2.stalls", "rule" => rule).increment(1);
            stalled.push(*id);
        }

        for stream in &stalled {
            self.release(*stream, Release::Stall, now);
        }

        self.reselect(now);
        self.probe(now);
        stalled
    }

    /// The median stream rate, once [`STALL_SLOW_MIN_PEERS`] peers are
    /// active.
    fn median_stream_rate(&self) -> Option<f64> {
        let active: BTreeSet<ConnectionId> = self.streams.values().map(|s| s.peer).collect();
        if active.len() < STALL_SLOW_MIN_PEERS {
            return None;
        }

        let mut rates: Vec<f64> = self.streams.values().map(|s| s.rate).collect();
        rates.sort_by(f64::total_cmp);
        Some(rates[rates.len() / 2])
    }

    fn release(&mut self, stream: StreamId, reason: Release, now: Instant) {
        let Some(state) = self.forget_stream(stream) else {
            return;
        };
        let reason = if reason == Release::End && !state.delivered_any {
            Release::EmptyEnd
        } else {
            reason
        };

        let unit = self.units.get(&state.unit).map(|unit| unit.unit);
        if let Some(peer) = self.peers.get_mut(&state.peer) {
            match reason {
                Release::End | Release::Gone => {}
                Release::Failure(StreamFailure::Refused) => {
                    let exclusion = if peer.refused {
                        REFUSED_EXCLUSION
                    } else {
                        STALL_BACKOFF
                    };
                    peer.refused = true;
                    peer.refused_until = Some(now + exclusion);
                }
                // A peer whose advertised tip covers a checkpoint has every
                // block below it: not finding it is as good as a refusal.
                // A known hash is a best-chain hash below the final checkpoint,
                // so it is the same.
                Release::Failure(StreamFailure::NotFound)
                    if unit.is_some_and(|unit| {
                        matches!(unit.anchor, Anchor::Checkpoint | Anchor::KnownHash)
                            && unit.end <= peer.remote_tip
                    }) =>
                {
                    peer.refused_until = Some(now + REFUSED_EXCLUSION);
                }
                Release::Failure(_) | Release::Stall | Release::EmptyEnd => {
                    peer.backoff_until = Some(now + STALL_BACKOFF);
                }
            }
        }

        let unit_id = state.unit;
        let Some(unit) = self.units.get(&unit_id) else {
            return;
        };
        // A closed connection is the registry's business: its peer is gone,
        // and it cannot fail the frontier again.
        if !matches!(reason, Release::End | Release::Gone)
            && unit.streams.is_empty()
            && self.is_frontier(&unit.unit)
        {
            self.frontier_failures += 1;
            self.frontier_failed_peers.insert(state.peer);
        }

        if !unit.streams.is_empty() {
            // Another stream of the unit is still delivering.
            self.recharge(unit_id);
            self.reselect(now);
            return;
        }

        match unit.low {
            Some((low, next_expected)) if low > unit.unit.start => {
                // Keep the delivered top part; re-unit the rest at the
                // verified `next_expected` hash.
                let unit = self.units.remove(&unit_id).expect("just found");
                self.undelivered -= unit.charge;
                let end = low.previous().expect("above start");
                self.requeue_range(unit.unit.start, end, next_expected);
            }
            _ => {
                // Nothing delivered: back to the queue unchanged.
                let unit = self.units.get_mut(&unit_id).expect("just found");
                unit.assigned_at = None;
                self.queue.insert((unit.unit.start, unit_id));
                self.recharge(unit_id);
            }
        }

        self.reselect(now);
        self.publish_charge();
    }

    /// Removes `stream` from the scheduler and its unit, without requeuing.
    fn forget_stream(&mut self, stream: StreamId) -> Option<StreamState> {
        let state = self.streams.remove(&stream)?;
        if let Some(peer) = self.peers.get_mut(&state.peer) {
            peer.streams -= 1;
        }
        if let Some(unit) = self.units.get_mut(&state.unit) {
            unit.streams.retain(|id| *id != stream);
        }
        Some(state)
    }

    // Commit-side events

    /// `peer` delivered a block that failed its commit through its own
    /// fault: its next pull waits [`STALL_BACKOFF`], so another peer
    /// refetches the block.
    pub fn back_off(&mut self, peer: ConnectionId, now: Instant) {
        if let Some(peer) = self.peers.get_mut(&peer) {
            peer.backoff_until = Some(now + STALL_BACKOFF);
        }
    }

    /// The commit frontier advanced to `base`, the lowest uncommitted
    /// height. Returns the block counts of the planned units now fully
    /// committed, for the legacy syncer's `RecentSyncLengths`.
    pub fn set_frontier(&mut self, base: block::Height) -> Vec<usize> {
        if base > self.frontier {
            self.frontier = base;
            self.reset_frontier_failures();

            // Every unit below the frontier is committed, and the units above
            // it hold their anchors, so only the entries from the frontier up
            // can still be planned.
            if let Some(source) = self.hashes.as_mut() {
                source.release_below(base);
                source.request(base, self.target);
            }
        }

        let mut lengths = Vec::new();
        while let Some((&end, &start)) = self.planned_spans.first_key_value() {
            if end >= base {
                break;
            }
            self.planned_spans.pop_first();
            // A unit's span is at most `MAX_UNIT_COUNT`: the cast is exact.
            lengths.push((end.0 - start.0 + 1) as usize);
        }
        lengths
    }

    /// Blocks of `bytes` serialized bytes in all were committed.
    pub fn on_committed(&mut self, bytes: u64) {
        self.buffered = self
            .buffered
            .saturating_sub(RETAINED_FACTOR.saturating_mul(bytes));
        self.publish_charge();
    }

    /// The delivered block at `height` with hash `hash` and `bytes` serialized
    /// bytes was discarded (a convert or commit failure). Queues the single
    /// block again, anchored at its hash, which the hash chain verified.
    pub fn on_discarded(&mut self, height: block::Height, hash: block::Hash, bytes: u64) {
        self.buffered = self
            .buffered
            .saturating_sub(RETAINED_FACTOR.saturating_mul(bytes));
        self.requeue_range(height, height, hash);
        self.publish_charge();
    }

    // Accounting

    fn recharge(&mut self, unit: UnitId) {
        let Some(state) = self.units.get_mut(&unit) else {
            return;
        };
        let charge = if state.streams.is_empty() {
            0
        } else {
            state.unit.est_bytes.saturating_sub(state.delivered)
        };
        self.undelivered = self.undelivered - state.charge + charge;
        state.charge = charge;
        self.publish_charge();
    }

    fn publish_charge(&self) {
        let charged = self.charged();
        self.charged.store(charged, Ordering::Relaxed);
        // A lossy cast is fine: it is a metric.
        metrics::gauge!("sync.v2.lookahead.bytes").set(charged as f64);
    }
}

#[cfg(test)]
mod tests;
