//! Scheduler tests: unit planning over the real Mainnet checkpoint list,
//! unit-level behaviour, and a deterministic simulation of a whole run.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    time::Duration,
};

use tokio::time::Instant;

use zebra_chain::{block, parameters::Network};
use zebra_network::sync_peer::ConnectionId;

use crate::components::checkpoint_sync::{
    anchors::{Anchors, FULL_INTERVAL_BYTES},
    engine::{Charged, EngineConfig},
};

use super::*;

const MIB: u64 = 1024 * 1024;

fn mainnet_anchors() -> Anchors {
    Anchors::new(&Network::Mainnet.checkpoint_list())
}

fn scheduler(
    config: &EngineConfig,
    start: block::Height,
    target: block::Height,
    now: Instant,
) -> Scheduler {
    Scheduler::new(
        config,
        mainnet_anchors(),
        start,
        target,
        Charged::default(),
        now,
    )
}

/// The Mainnet checkpoint heights.
fn checkpoints() -> Vec<block::Height> {
    Network::Mainnet
        .checkpoint_list()
        .iter_cloned()
        .map(|(height, _)| height)
        .collect()
}

/// The index of the first Mainnet interval with a gap below 400.
fn first_short_interval(checkpoints: &[block::Height]) -> usize {
    checkpoints
        .windows(2)
        .position(|w| ((w[1].0 - w[0].0) as usize) < 400)
        .expect("Mainnet has 32 MiB intervals")
}

fn peer(id: u64) -> ConnectionId {
    ConnectionId(id)
}

/// Units cover `start..=target` contiguously, end at checkpoints, and stay
/// within the protocol's bounds; the frontier unit is exactly one interval,
/// and deep units coalesce intervals while the estimate fits 64 MiB.
#[test]
fn mainnet_unit_planning() {
    let _init_guard = zebra_test::init();

    let anchors = mainnet_anchors();
    let config = EngineConfig::default();
    let target = anchors.final_height();
    let now = Instant::now();
    let mut scheduler = scheduler(&config, block::Height(1), target, now);

    while scheduler.plan_unit() {}

    let units: Vec<Unit> = scheduler
        .queue
        .iter()
        .map(|(_, id)| scheduler.units[id].unit)
        .collect();

    let first = units[0];
    assert_eq!(first.start, block::Height(1));
    assert_eq!(
        anchors.intervals_between(block::Height(0), first.end),
        1,
        "the frontier unit is one interval"
    );

    let mut expected_start = block::Height(1);
    let mut coalesced = 0;
    for unit in &units {
        assert_eq!(unit.start, expected_start, "units are contiguous");
        assert_eq!(
            anchors.hash(unit.end),
            Some(unit.end_hash),
            "units are anchored at checkpoints"
        );
        assert_eq!(unit.anchor, Anchor::Checkpoint);
        assert_eq!(
            u64::from(unit.max_bytes),
            unit.est_bytes.max(MAX_BLOCK_BYTES),
            "units request their charge"
        );
        assert!(unit.end.0 - unit.start.0 < MAX_UNIT_COUNT);

        let intervals = anchors.intervals_between(unit.start.previous().unwrap(), unit.end);
        if intervals > 1 {
            coalesced += 1;
            assert!(
                unit.est_bytes < u64::from(MAX_UNIT_BYTES),
                "coalesced units fit 64 MiB"
            );
        }

        expected_start = unit.end.next().unwrap();
    }
    assert_eq!(
        expected_start,
        target.next().unwrap(),
        "units reach the target"
    );
    assert!(
        coalesced > 100,
        "small-block intervals coalesce: {coalesced}"
    );

    // Two 32 MiB intervals never share a unit: they would truncate.
    let full = units
        .iter()
        .filter(|unit| unit.est_bytes >= FULL_INTERVAL_BYTES)
        .count();
    assert!(full > 6_000, "{full} units of at least 32 MiB");
    assert!(units
        .iter()
        .all(|unit| unit.est_bytes < 2 * FULL_INTERVAL_BYTES));
}

/// A peer can never deliver much more than admission charged: each
/// request's `max_bytes` is at most the unit's estimate, or one
/// maximum-size block.
#[test]
fn requests_ask_for_no_more_than_their_charge() {
    let _init_guard = zebra_test::init();

    let checkpoints = checkpoints();
    let config = EngineConfig::default();
    let now = Instant::now();
    // Early Mainnet: tiny blocks, so estimates are far below 64 MiB.
    let start = checkpoints[1].next().unwrap();
    let mut scheduler = scheduler(&config, start, checkpoints[40], now);
    scheduler.set_peers(
        &[(peer(1), block::Height::MAX), (peer(2), block::Height::MAX)],
        now,
    );

    let mut seen = 0;
    while let Some(assignment) = scheduler.next_assignment(now) {
        let unit = assignment.unit;
        assert!(
            u64::from(unit.max_bytes) <= unit.est_bytes.max(MAX_BLOCK_BYTES),
            "unit {:?} requests {} bytes but is charged {}",
            unit.id,
            unit.max_bytes,
            unit.est_bytes,
        );
        seen += 1;
        if seen >= 4 {
            break;
        }
    }
    assert!(seen > 0, "the scheduler assigns units");
}

/// A truncated unit keeps its delivered top part, and the remainder is
/// anchored at the verified `next_expected` hash.
#[test]
fn truncation_resumes_at_next_expected() {
    let _init_guard = zebra_test::init();

    let checkpoints = checkpoints();
    let config = EngineConfig::default();
    let now = Instant::now();
    let start = checkpoints[1].next().unwrap();
    let mut scheduler = scheduler(&config, start, checkpoints[3], now);
    scheduler.set_peers(&[(peer(1), block::Height::MAX)], now);

    let assignment = scheduler.next_assignment(now).expect("a unit");
    let unit = assignment.unit;
    assert_eq!(
        unit.end, checkpoints[2],
        "the frontier unit is one interval"
    );

    // Deliver the top two blocks, then the stream ends.
    let resume_hash = block::Hash([7; 32]);
    scheduler.on_block(
        assignment.stream,
        unit.end,
        1_000,
        block::Hash([6; 32]),
        true,
        now,
    );
    let below_top = unit.end.previous().unwrap();
    scheduler.on_block(assignment.stream, below_top, 1_000, resume_hash, true, now);
    scheduler.on_end(assignment.stream, now);

    let (_, resumed) = scheduler.queue.first().copied().unwrap();
    let resumed = scheduler.units[&resumed].unit;
    assert_eq!(resumed.start, unit.start);
    assert_eq!(resumed.end, below_top.previous().unwrap());
    assert_eq!(resumed.end_hash, resume_hash);
    assert_eq!(resumed.anchor, Anchor::Resumed);
    assert_eq!(
        scheduler.frontier_failures(),
        0,
        "truncation is not a failure"
    );

    // The same peer resumes it.
    let again = scheduler.next_assignment(now).unwrap();
    assert_eq!(again.unit, resumed);
}

/// A 2 MB block that takes longer than [`STALL_NO_BYTES`] to arrive, at a
/// rate above `min_peer_rate`, does not stall its stream: the stall rules
/// count bytes as they arrive, not whole blocks, and the completed block's
/// bytes are not counted twice.
#[test]
fn a_slow_large_block_above_the_minimum_rate_does_not_stall() {
    let _init_guard = zebra_test::init();

    let checkpoints = checkpoints();
    let config = EngineConfig::default();
    // 2 MB at 300 KiB/s takes about 6.7 s.
    let block_bytes: u64 = 2_000_000;
    let rate = 300 * 1024;
    assert!(rate > config.min_peer_rate);
    let mut now = Instant::now();
    let mut scheduler = scheduler(&config, block::Height(1), checkpoints[50], now);
    scheduler.set_peers(&[(peer(1), block::Height::MAX)], now);
    let assignment = scheduler.next_assignment(now).unwrap();

    let per_sample = rate * SAMPLE_INTERVAL.as_millis() as u64 / 1000;
    let started = now;
    let mut received = 0;
    while received < block_bytes {
        now += SAMPLE_INTERVAL;
        received = (received + per_sample).min(block_bytes);
        scheduler.on_bytes(assignment.stream, received, now);
        assert_eq!(
            scheduler.tick(now),
            Vec::new(),
            "a stream receiving {received} bytes of a block is not stalled"
        );
    }
    assert!(now.saturating_duration_since(started) > STALL_NO_BYTES);
    assert!(scheduler.is_live(assignment.stream));

    // The whole block arrives: its bytes were already counted.
    scheduler.on_block(
        assignment.stream,
        assignment.unit.end,
        block_bytes,
        block::Hash([6; 32]),
        true,
        now,
    );
    assert_eq!(scheduler.streams[&assignment.stream].sample_bytes, 0);

    // Without bytes, the stream stalls after `STALL_NO_BYTES`.
    let stalled_at = now + STALL_NO_BYTES;
    while now < stalled_at {
        now += SAMPLE_INTERVAL;
        let stalled = scheduler.tick(now);
        if now < stalled_at {
            assert_eq!(stalled, Vec::new());
        } else {
            assert_eq!(stalled, vec![assignment.stream]);
        }
    }
}

/// A peer that refused is not selected for 30 s, and for 10 minutes if it
/// refuses again without delivering in between, and a failed peer backs off
/// for 30 s.
#[test]
fn refused_peers_are_excluded_and_failed_peers_back_off() {
    let _init_guard = zebra_test::init();

    let checkpoints = checkpoints();
    let config = EngineConfig {
        target_peers: 2,
        ..EngineConfig::default()
    };
    let now = Instant::now();
    let mut scheduler = scheduler(&config, block::Height(1), checkpoints[100], now);
    scheduler.set_peers(
        &[
            (peer(1), block::Height::MAX),
            (peer(2), block::Height::MAX),
            (peer(3), block::Height::MAX),
        ],
        now,
    );
    assert_eq!(scheduler.selected_peers(), 2);
    let changes = scheduler.take_window_changes();
    assert_eq!(changes.len(), 2);
    assert!(changes.iter().all(|change| change.selected));

    let first = scheduler.next_assignment(now).unwrap();
    scheduler.on_failure(first.stream, StreamFailure::Refused, now);
    assert!(!scheduler.peers[&first.peer].selected);
    assert_eq!(scheduler.selected_peers(), 2, "the third peer replaces it");
    assert_eq!(scheduler.frontier_failures(), 1);

    let second = scheduler.next_assignment(now).unwrap();
    assert_ne!(second.peer, first.peer);
    assert_eq!(
        second.unit, first.unit,
        "the failed unit is requeued unchanged"
    );
    scheduler.on_failure(second.stream, StreamFailure::Transport, now);
    assert!(scheduler.peers[&second.peer].backing_off(now));
    assert!(!scheduler.peers[&second.peer].backing_off(now + STALL_BACKOFF));
    assert!(scheduler.peers[&first.peer].excluded(now));
    assert!(!scheduler.peers[&first.peer].excluded(now + STALL_BACKOFF));

    // A peer that refuses again after its short exclusion, without
    // delivering in between, is excluded for `REFUSED_EXCLUSION`; one that
    // delivered in between only for `STALL_BACKOFF` again.
    let mut scheduler = scheduler_for_one_peer(&config, &checkpoints, now);
    let refused = scheduler.next_assignment(now).unwrap();
    scheduler.on_failure(refused.stream, StreamFailure::Refused, now);
    let later = now + STALL_BACKOFF;
    scheduler.tick(later);
    let again = scheduler.next_assignment(later).expect("selected again");
    scheduler.on_failure(again.stream, StreamFailure::Refused, later);
    assert!(scheduler.peers[&peer(1)].excluded(later + STALL_BACKOFF));
    assert!(!scheduler.peers[&peer(1)].excluded(later + REFUSED_EXCLUSION));

    let mut scheduler = scheduler_for_one_peer(&config, &checkpoints, now);
    let refused = scheduler.next_assignment(now).unwrap();
    scheduler.on_failure(refused.stream, StreamFailure::Refused, now);
    scheduler.tick(later);
    let delivering = scheduler.next_assignment(later).expect("selected again");
    scheduler.on_block(
        delivering.stream,
        delivering.unit.end,
        1_000,
        block::Hash([6; 32]),
        true,
        later,
    );
    scheduler.on_end(delivering.stream, later);
    let again = scheduler.next_assignment(later).expect("pulls again");
    scheduler.on_failure(again.stream, StreamFailure::Refused, later);
    assert!(!scheduler.peers[&peer(1)].excluded(later + STALL_BACKOFF));
}

fn scheduler_for_one_peer(
    config: &EngineConfig,
    checkpoints: &[block::Height],
    now: Instant,
) -> Scheduler {
    let mut scheduler = scheduler(config, block::Height(1), checkpoints[100], now);
    scheduler.set_peers(&[(peer(1), block::Height::MAX)], now);
    scheduler
}

/// Units above a peer's advertised tip are not given to it.
#[test]
fn units_respect_the_peer_tip() {
    let _init_guard = zebra_test::init();

    let checkpoints = checkpoints();
    let config = EngineConfig::default();
    let now = Instant::now();
    let mut scheduler = scheduler(&config, block::Height(1), checkpoints[50], now);
    scheduler.set_peers(&[(peer(1), checkpoints[1])], now);

    let first = scheduler.next_assignment(now).unwrap();
    assert_eq!(first.unit.end, checkpoints[1]);
    assert_eq!(scheduler.next_assignment(now), None);
    assert!(
        scheduler.planned_through.unwrap() < checkpoints[50],
        "planning stops above the peer's tip"
    );

    // Asking again plans nothing more for this peer.
    let planned = scheduler.planned_through;
    for _ in 0..10 {
        assert_eq!(scheduler.next_assignment(now), None);
    }
    assert_eq!(scheduler.planned_through, planned);
}

/// A peer whose tip is below the frontier's checkpoint interval is not
/// selected, does not count toward the engine's peers, and plans nothing.
#[test]
fn peers_below_the_frontier_interval_are_not_selected() {
    let _init_guard = zebra_test::init();

    let checkpoints = checkpoints();
    let config = EngineConfig::default();
    let now = Instant::now();
    let start = checkpoints[10].next().unwrap();
    let mut scheduler = scheduler(&config, start, checkpoints[50], now);
    scheduler.set_peers(
        &[(peer(1), checkpoints[10]), (peer(2), block::Height::MAX)],
        now,
    );

    assert_eq!(scheduler.selected_peers(), 1);
    assert!(scheduler
        .take_window_changes()
        .iter()
        .all(|change| change.peer == peer(2)));
    let assignment = scheduler.next_assignment(now).unwrap();
    assert_eq!(assignment.peer, peer(2));
    assert_eq!(assignment.unit.end, checkpoints[11]);

    // When the low peer's advertised tip rises, it is selected.
    scheduler.set_peers(
        &[(peer(1), checkpoints[11]), (peer(2), block::Height::MAX)],
        now,
    );
    assert_eq!(scheduler.selected_peers(), 2);
}

/// A stream that ends without delivering a block is a failure: its peer
/// backs off, and it counts against the frontier.
#[test]
fn empty_responses_back_off_and_count_against_the_frontier() {
    let _init_guard = zebra_test::init();

    let checkpoints = checkpoints();
    let config = EngineConfig::default();
    let now = Instant::now();
    let mut scheduler = scheduler(&config, block::Height(1), checkpoints[50], now);
    scheduler.set_peers(&[(peer(1), block::Height::MAX)], now);

    let frontier = scheduler.next_assignment(now).unwrap();
    assert_eq!(frontier.unit.start, block::Height(1));
    scheduler.on_end(frontier.stream, now);

    assert_eq!(scheduler.frontier_failures(), 1);
    assert_eq!(
        scheduler.next_assignment(now),
        None,
        "the peer backs off after an empty response"
    );
    let retry = scheduler
        .next_assignment(now + STALL_BACKOFF)
        .expect("the peer pulls again after its backoff");
    assert_eq!(retry.unit.start, block::Height(1));
}

/// The lookahead bound: only the frontier unit is admitted over it.
#[test]
fn lookahead_admits_only_the_frontier_over_the_bound() {
    let _init_guard = zebra_test::init();

    let checkpoints = checkpoints();
    let index = first_short_interval(&checkpoints);
    let start = checkpoints[index].next().unwrap();
    let target = checkpoints[index + 20];

    let config = EngineConfig {
        // Below one interval's estimate (32 MiB).
        lookahead_bytes: 16 * MIB,
        ..EngineConfig::default()
    };
    let now = Instant::now();
    let mut scheduler = scheduler(&config, start, target, now);
    scheduler.set_peers(
        &[(peer(1), block::Height::MAX), (peer(2), block::Height::MAX)],
        now,
    );

    let frontier = scheduler
        .next_assignment(now)
        .expect("the frontier is admitted");
    assert_eq!(frontier.unit.start, start);
    assert_eq!(scheduler.next_assignment(now), None, "nothing else fits");
    assert!(scheduler.charged() > config.lookahead_bytes);
    assert_eq!(scheduler.charged(), scheduler.frontier_charge());
    assert_eq!(
        scheduler.charged.load(Ordering::Relaxed),
        scheduler.charged()
    );
}

/// A block discarded inside the frontier's checkpoint interval is refetched
/// even when the lookahead is full: the interval cannot commit without it.
#[test]
fn discarded_frontier_interval_blocks_bypass_the_lookahead() {
    let _init_guard = zebra_test::init();

    let checkpoints = checkpoints();
    let config = EngineConfig {
        lookahead_bytes: 1,
        ..EngineConfig::default()
    };
    let now = Instant::now();
    let start = checkpoints[1].next().unwrap();
    let mut scheduler = scheduler(&config, start, checkpoints[3], now);
    scheduler.set_peers(&[(peer(1), block::Height::MAX)], now);

    let assignment = scheduler.next_assignment(now).unwrap();
    let unit = assignment.unit;
    let mut height = unit.end;
    loop {
        scheduler.on_block(
            assignment.stream,
            height,
            1_000,
            block::Hash([1; 32]),
            true,
            now,
        );
        if height == unit.start {
            break;
        }
        height = height.previous().unwrap();
    }
    scheduler.on_end(assignment.stream, now);
    // Nothing committed yet: the frontier is still `start`.
    let middle = block::Height(unit.start.0 + (unit.end.0 - unit.start.0) / 2);
    scheduler.on_discarded(middle, block::Hash([2; 32]), 1_000);

    let refetch = scheduler
        .next_assignment(now)
        .expect("the discarded block is admitted");
    assert_eq!((refetch.unit.start, refetch.unit.end), (middle, middle));
}

/// End-game: with nothing unassigned, a peer gets one duplicate of the
/// oldest assignment; the first complete delivery wins.
#[test]
fn end_game_duplicates_the_oldest_unit_once_per_peer() {
    let _init_guard = zebra_test::init();

    let checkpoints = checkpoints();
    let config = EngineConfig::default();
    let now = Instant::now();
    let start = checkpoints[1].next().unwrap();
    let mut scheduler = scheduler(&config, start, checkpoints[2], now);
    scheduler.set_peers(
        &[(peer(1), block::Height::MAX), (peer(2), block::Height::MAX)],
        now,
    );

    let original = scheduler.next_assignment(now).unwrap();
    let duplicate = scheduler
        .next_assignment(now + Duration::from_secs(1))
        .expect("an end-game duplicate");
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.unit, original.unit);
    assert_ne!(duplicate.peer, original.peer);
    assert_eq!(
        scheduler.next_assignment(now),
        None,
        "one duplicate per unit"
    );

    // The duplicate delivers the whole unit first.
    let unit = duplicate.unit;
    let mut height = unit.end;
    let cancelled = loop {
        let cancelled = scheduler.on_block(
            duplicate.stream,
            height,
            10,
            block::Hash([1; 32]),
            true,
            now,
        );
        if height == unit.start {
            break cancelled;
        }
        assert!(cancelled.is_empty());
        height = height.previous().unwrap();
    };
    assert_eq!(
        cancelled.iter().copied().collect::<BTreeSet<_>>(),
        [original.stream, duplicate.stream].into_iter().collect(),
        "the loser is cancelled"
    );
    assert!(scheduler.is_done());
    assert!(!scheduler.is_live(original.stream));
    scheduler.on_end(duplicate.stream, now);
    assert_eq!(
        scheduler.peers[&peer(1)].streams + scheduler.peers[&peer(2)].streams,
        0
    );
}

/// The probe slot rotates an unselected peer in every minute.
#[test]
fn probe_slot_rotates() {
    let _init_guard = zebra_test::init();

    let checkpoints = checkpoints();
    let config = EngineConfig {
        target_peers: 2,
        ..EngineConfig::default()
    };
    let now = Instant::now();
    let mut scheduler = scheduler(&config, block::Height(1), checkpoints[100], now);
    scheduler.set_peers(
        &[
            (peer(1), block::Height::MAX),
            (peer(2), block::Height::MAX),
            (peer(3), block::Height::MAX),
        ],
        now,
    );
    assert!(!scheduler.peers[&peer(3)].selected);
    scheduler.take_window_changes();

    scheduler.tick(now + PROBE_INTERVAL);
    assert!(scheduler.peers[&peer(3)].selected);
    assert_eq!(scheduler.selected_peers(), 2);
    let changes = scheduler.take_window_changes();
    assert_eq!(changes.len(), 2);
    assert_eq!(changes.iter().filter(|change| change.selected).count(), 1);
}

// The simulation

/// A deterministic pseudo-random generator (splitmix64).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn chance(&mut self, probability: f64) -> bool {
        (self.next() % 1_000_000) < (probability * 1_000_000.0) as u64
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound.max(1)
    }
}

/// A simulated peer: `(rate, rtt, stall_prob, truncation_prob)`.
#[derive(Clone, Debug)]
struct SimPeer {
    /// Bytes per second, shared by its streams.
    rate: u64,
    /// The delay to the first block of a stream.
    rtt: Duration,
    /// The probability that a stream goes silent part way.
    stall_prob: f64,
    /// The probability that a stream ends early.
    truncation_prob: f64,
    /// Whether the peer refuses `get-block-range`.
    refuses: bool,
    /// Whether the peer answers every `get-block-range` with an empty
    /// response (it MAY finish early).
    empty: bool,
}

fn healthy_peer(rate_mib: u64) -> SimPeer {
    SimPeer {
        rate: rate_mib * MIB,
        rtt: Duration::from_millis(100),
        stall_prob: 0.0,
        truncation_prob: 0.0,
        refuses: false,
        empty: false,
    }
}

#[derive(Debug)]
struct SimStream {
    peer: ConnectionId,
    unit: Unit,
    next: block::Height,
    first_at: Instant,
    credit: f64,
    sent_bytes: u64,
    sent_blocks: u32,
    /// The stream goes silent after this many blocks.
    stall_after: Option<u32>,
    /// The stream ends after this many blocks.
    truncate_after: Option<u32>,
    /// When the stream went silent.
    silent_since: Option<Instant>,
    duplicate: bool,
}

/// The chain: synthetic sizes over the real checkpoint intervals, and hashes
/// that agree with the checkpoints.
struct SimChain {
    anchors: Anchors,
    sizes: BTreeMap<block::Height, u64>,
}

impl SimChain {
    fn new(anchors: Anchors, start: block::Height, target: block::Height, rng: &mut Rng) -> Self {
        let mut sizes = BTreeMap::new();
        let mut low = start;
        while low <= target {
            let (end, _) = anchors.at_or_above(low).unwrap();
            let (previous, _) = anchors.at_or_below(low.previous().unwrap());
            let gap = u64::from(end.0 - previous.0);
            let mut height = low;
            while height <= end {
                let size = if gap < 400 {
                    // The generator's rule: just over 32 MiB in the interval.
                    FULL_INTERVAL_BYTES / gap + 1
                } else {
                    1_000 + rng.below(20_000)
                };
                sizes.insert(height, size);
                height = height.next().unwrap();
            }
            low = end.next().unwrap();
        }
        SimChain { anchors, sizes }
    }

    fn hash(&self, height: block::Height) -> block::Hash {
        self.anchors.hash(height).unwrap_or_else(|| {
            let mut bytes = [0xab; 32];
            bytes[..4].copy_from_slice(&height.0.to_le_bytes());
            block::Hash(bytes)
        })
    }
}

/// What the simulation measured.
#[derive(Debug, Default)]
struct SimReport {
    elapsed: Duration,
    duplicates_per_peer: HashMap<ConnectionId, u32>,
    max_concurrent_duplicates_per_peer: u32,
    stalls_injected: u32,
    max_stall_detection: Duration,
    max_charged: u64,
    deliveries: u64,
    new_deliveries: u64,
}

const STEP: Duration = Duration::from_millis(10);
const COMMIT_RATE: u64 = 400 * MIB;
const LOOKAHEAD: u64 = 512 * MIB;

/// The simulation's mutable state.
struct Sim<'a> {
    peers: &'a [SimPeer],
    chain: SimChain,
    rng: Rng,
    now: Instant,
    scheduler: Scheduler,
    streams: BTreeMap<StreamId, SimStream>,
    delivered: BTreeSet<block::Height>,
    new_count: BTreeMap<block::Height, u32>,
    /// Per unit: its estimate, the new bytes delivered for it, and its end.
    units: HashMap<UnitId, (u64, u64, block::Height)>,
    concurrent_duplicates: HashMap<ConnectionId, u32>,
    base: block::Height,
    report: SimReport,
}

impl Sim<'_> {
    fn remove_stream(&mut self, id: StreamId) -> Option<SimStream> {
        let stream = self.streams.remove(&id)?;
        if stream.duplicate {
            if let Some(count) = self.concurrent_duplicates.get_mut(&stream.peer) {
                *count -= 1;
            }
        }
        Some(stream)
    }

    fn assign(&mut self, target: block::Height) {
        while let Some(assignment) = self.scheduler.next_assignment(self.now) {
            let unit = assignment.unit;
            assert_eq!(
                unit.end_hash,
                self.chain.hash(unit.end),
                "units are anchored correctly"
            );
            assert!(unit.end <= target && unit.start >= self.base);
            assert!(unit.end.0 - unit.start.0 < MAX_UNIT_COUNT);

            // Admission: never over the bound, except by the frontier unit,
            // which is always admitted. Duplicates add no charge.
            if unit.start > self.base && !assignment.duplicate {
                assert!(
                    self.scheduler.charged() <= LOOKAHEAD,
                    "admission exceeded the lookahead: {} > {LOOKAHEAD}",
                    self.scheduler.charged(),
                );
            }

            self.units
                .entry(unit.id)
                .or_insert((unit.est_bytes, 0, unit.end));
            if assignment.duplicate {
                *self
                    .report
                    .duplicates_per_peer
                    .entry(assignment.peer)
                    .or_default() += 1;
                let concurrent = self
                    .concurrent_duplicates
                    .entry(assignment.peer)
                    .or_default();
                *concurrent += 1;
                self.report.max_concurrent_duplicates_per_peer = self
                    .report
                    .max_concurrent_duplicates_per_peer
                    .max(*concurrent);
            }

            let sim = &self.peers[assignment.peer.0 as usize];
            if sim.refuses {
                if assignment.duplicate {
                    *self
                        .concurrent_duplicates
                        .get_mut(&assignment.peer)
                        .unwrap() -= 1;
                }
                self.scheduler
                    .on_failure(assignment.stream, StreamFailure::Refused, self.now);
                continue;
            }
            let blocks = u64::from(unit.end.0 - unit.start.0 + 1);
            let stall_after = self
                .rng
                .chance(sim.stall_prob)
                .then(|| self.rng.below(blocks) as u32);
            let truncate_after = if sim.empty {
                Some(0)
            } else {
                self.rng
                    .chance(sim.truncation_prob)
                    .then(|| 1 + self.rng.below(blocks) as u32)
            };
            if stall_after.is_some() {
                self.report.stalls_injected += 1;
            }
            self.streams.insert(
                assignment.stream,
                SimStream {
                    peer: assignment.peer,
                    unit,
                    next: unit.end,
                    first_at: self.now + sim.rtt,
                    credit: 0.0,
                    sent_bytes: 0,
                    sent_blocks: 0,
                    stall_after,
                    truncate_after,
                    silent_since: None,
                    duplicate: assignment.duplicate,
                },
            );
        }
    }

    fn deliver(&mut self) {
        let now = self.now;
        let mut active: HashMap<ConnectionId, u32> = HashMap::new();
        for stream in self.streams.values() {
            if now >= stream.first_at && stream.silent_since.is_none() {
                *active.entry(stream.peer).or_default() += 1;
            }
        }

        let ids: Vec<StreamId> = self.streams.keys().copied().collect();
        for id in ids {
            let Some(stream) = self.streams.get_mut(&id) else {
                continue;
            };
            if now < stream.first_at || stream.silent_since.is_some() {
                continue;
            }
            let rate = self.peers[stream.peer.0 as usize].rate as f64;
            stream.credit += rate / f64::from(active[&stream.peer]) * STEP.as_secs_f64();

            while let Some(stream) = self.streams.get_mut(&id) {
                if stream.stall_after == Some(stream.sent_blocks) {
                    stream.silent_since = Some(now);
                    break;
                }
                // The responder stops at `max_bytes` after the first block.
                let truncated = stream.truncate_after == Some(stream.sent_blocks)
                    || (stream.sent_blocks > 0
                        && stream.sent_bytes > u64::from(stream.unit.max_bytes));
                if truncated {
                    self.remove_stream(id);
                    self.scheduler.on_end(id, now);
                    break;
                }

                let height = stream.next;
                let size = self.chain.sizes[&height];
                if stream.credit < size as f64 {
                    break;
                }
                stream.credit -= size as f64;
                stream.sent_bytes += size;
                stream.sent_blocks += 1;
                let unit_id = stream.unit.id;
                let is_last = height == stream.unit.start;
                if !is_last {
                    stream.next = height.previous().unwrap();
                }

                let is_new = height >= self.base && self.delivered.insert(height);
                self.report.deliveries += 1;
                if is_new {
                    self.report.new_deliveries += 1;
                    *self.new_count.entry(height).or_default() += 1;
                    if let Some(unit) = self.units.get_mut(&unit_id) {
                        unit.1 += size;
                    }
                }
                let prev_hash = self.chain.hash(height.previous().unwrap());
                let cancelled = self
                    .scheduler
                    .on_block(id, height, size, prev_hash, is_new, now);
                for cancelled in cancelled {
                    self.remove_stream(cancelled);
                }
                if is_last {
                    self.remove_stream(id);
                    self.scheduler.on_end(id, now);
                    break;
                }
            }
        }
    }

    fn tick(&mut self) {
        let now = self.now;
        for stalled in self.scheduler.tick(now) {
            if let Some(stream) = self.remove_stream(stalled) {
                if let Some(since) = stream.silent_since {
                    self.report.max_stall_detection = self
                        .report
                        .max_stall_detection
                        .max(now.duration_since(since));
                }
            }
        }
        for stream in self.streams.values() {
            if let Some(since) = stream.silent_since {
                assert!(
                    now.duration_since(since) <= STALL_NO_BYTES + SAMPLE_INTERVAL + STEP,
                    "a silent stream was not cancelled within 5 s plus one sample"
                );
            }
        }
    }
}

/// Runs the scheduler against simulated peers from `start` to `target`,
/// committing the delivered prefix at [`COMMIT_RATE`], and checks the
/// invariants at every step.
fn simulate(
    start: block::Height,
    target: block::Height,
    peers: &[SimPeer],
    target_peers: usize,
    seed: u64,
) -> SimReport {
    let mut rng = Rng(seed);
    let anchors = mainnet_anchors();
    let chain = SimChain::new(anchors.clone(), start, target, &mut rng);
    let config = EngineConfig {
        lookahead_bytes: LOOKAHEAD,
        min_peer_rate: 256 * 1024,
        target_peers,
    };

    let begin = Instant::now();
    let mut scheduler = Scheduler::new(&config, anchors, start, target, Charged::default(), begin);
    let ids: Vec<(ConnectionId, block::Height)> = (0..peers.len())
        .map(|index| (ConnectionId(index as u64), block::Height::MAX))
        .collect();
    scheduler.set_peers(&ids, begin);

    let mut sim = Sim {
        peers,
        chain,
        rng,
        now: begin,
        scheduler,
        streams: BTreeMap::new(),
        delivered: BTreeSet::new(),
        new_count: BTreeMap::new(),
        units: HashMap::new(),
        concurrent_duplicates: HashMap::new(),
        base: start,
        report: SimReport::default(),
    };
    let mut commit_credit = 0.0;
    let mut next_tick = begin + SAMPLE_INTERVAL;

    while sim.base <= target {
        assert!(
            sim.now.duration_since(begin) < Duration::from_secs(3_600),
            "the simulated run stalled at {:?}",
            sim.base
        );

        sim.assign(target);
        sim.deliver();

        // Commit the delivered prefix, in order.
        commit_credit += COMMIT_RATE as f64 * STEP.as_secs_f64();
        let (mut blocks, mut bytes) = (0, 0);
        while sim.delivered.first() == Some(&sim.base) {
            let size = sim.chain.sizes[&sim.base];
            if commit_credit < size as f64 {
                break;
            }
            commit_credit -= size as f64;
            sim.delivered.pop_first();
            blocks += 1;
            bytes += size;
            sim.base = sim.base.next().unwrap();
        }
        if blocks > 0 {
            sim.scheduler.on_committed(bytes);
            sim.scheduler.set_frontier(sim.base);
        } else {
            commit_credit = commit_credit.min(COMMIT_RATE as f64);
        }

        // The lookahead bound at all times. Admission keeps the undelivered
        // estimates plus the delivered bytes within the bound, plus the
        // frontier unit; delivered bytes are then charged at the retained
        // factor, and deliveries can exceed their estimates.
        let base = sim.base;
        let overshoot: u64 = sim
            .units
            .values()
            .filter(|(_, _, end)| *end >= base)
            .map(|(est, got, _)| RETAINED_FACTOR * got.saturating_sub(*est))
            .sum();
        let bound = RETAINED_FACTOR * (LOOKAHEAD + u64::from(MAX_UNIT_BYTES)) + overshoot;
        let charged = sim.scheduler.charged();
        sim.report.max_charged = sim.report.max_charged.max(charged);
        assert!(
            charged <= bound,
            "the lookahead charge {charged} exceeds {bound}"
        );

        sim.now += STEP;
        if sim.now >= next_tick {
            next_tick += SAMPLE_INTERVAL;
            sim.tick();
        }
    }

    // In-order, exactly-once commits: every height was taken as new exactly
    // once, and the commit loop pops the contiguous prefix in order.
    assert_eq!(sim.base, target.next().unwrap());
    let mut height = start;
    while height <= target {
        assert_eq!(
            sim.new_count.get(&height),
            Some(&1),
            "{height:?} taken exactly once"
        );
        height = height.next().unwrap();
    }
    assert!(sim.scheduler.is_done());
    assert_eq!(sim.scheduler.charged(), 0, "nothing is charged at the end");

    sim.report.elapsed = sim.now.duration_since(begin);
    tracing::info!(?start, ?target, report = ?sim.report, "simulated run");
    sim.report
}

/// A Mainnet span that crosses from small-block to 32 MiB intervals.
fn mixed_span() -> (block::Height, block::Height) {
    let checkpoints = checkpoints();
    // The first run of 20 consecutive 32 MiB intervals, after 150 others.
    let first_short = (150..checkpoints.len() - 170)
        .find(|index| {
            checkpoints[*index..*index + 21]
                .windows(2)
                .all(|w| ((w[1].0 - w[0].0) as usize) < 400)
        })
        .expect("Mainnet has runs of 32 MiB intervals");
    (
        checkpoints[first_short - 150].next().unwrap(),
        checkpoints[first_short + 150],
    )
}

/// The acceptance run: 8 peers reach the target with at
/// most one duplicate unit per peer, within the lookahead bound.
#[test]
fn simulated_eight_peer_run() {
    let _init_guard = zebra_test::init();

    let (start, target) = mixed_span();
    let peers: Vec<SimPeer> = [40, 30, 25, 20, 15, 10, 8, 5]
        .into_iter()
        .map(healthy_peer)
        .collect();

    let report = simulate(start, target, &peers, 8, 1);

    assert!(
        report.duplicates_per_peer.values().all(|count| *count <= 1),
        "{report:?}"
    );
    assert!(report.max_concurrent_duplicates_per_peer <= 1);
    assert!(report.max_charged <= RETAINED_FACTOR * (LOOKAHEAD + u64::from(MAX_UNIT_BYTES)));
}

/// Stalls, truncations, a refusing peer, a slow peer and a spare peer for the
/// probe slot: every height is still committed in order exactly once,
/// silent streams are cancelled within 5 s plus one sample, and the lookahead
/// bound holds.
#[test]
fn simulated_faulty_peers() {
    let _init_guard = zebra_test::init();

    let (start, target) = mixed_span();
    let mut peers: Vec<SimPeer> = [30, 25, 20, 15, 10, 10, 8]
        .into_iter()
        .map(healthy_peer)
        .collect();
    peers[1].stall_prob = 0.3;
    peers[2].truncation_prob = 0.3;
    peers[3].stall_prob = 0.1;
    peers[3].truncation_prob = 0.1;
    peers[4].rtt = Duration::from_millis(800);
    // A slow peer, below the floor rate.
    peers.push(SimPeer {
        rate: 128 * 1024,
        ..healthy_peer(1)
    });
    // A peer that refuses ranges.
    peers.push(SimPeer {
        refuses: true,
        ..healthy_peer(50)
    });
    // A fast peer that answers every request with an empty response.
    peers.push(SimPeer {
        empty: true,
        ..healthy_peer(60)
    });
    // A spare peer beyond the target, for the probe slot.
    peers.push(healthy_peer(12));

    for seed in 1..=3 {
        let report = simulate(start, target, &peers, 8, seed);
        assert!(report.stalls_injected > 0, "{report:?}");
        assert!(
            report.max_stall_detection <= STALL_NO_BYTES + SAMPLE_INTERVAL + STEP,
            "{report:?}"
        );
        assert!(
            report.duplicates_per_peer.values().all(|count| *count <= 1),
            "{report:?}"
        );
    }
}

/// With a single peer the run still completes, through truncations.
#[test]
fn simulated_single_truncating_peer() {
    let _init_guard = zebra_test::init();

    let (start, _) = mixed_span();
    let target = mainnet_anchors()
        .at_or_below(block::Height(start.0 + 30_000))
        .0;
    let mut peer = healthy_peer(20);
    peer.truncation_prob = 0.5;

    simulate(start, target, &[peer], 2, 7);
}

/// One peer that keeps failing the frontier cannot end the run alone: the
/// failures count toward the limit only from distinct peers, or once every
/// eligible peer has failed it.
#[test]
fn one_peer_cannot_exhaust_the_frontier_failures() {
    let _init_guard = zebra_test::init();

    let checkpoints = checkpoints();
    let config = EngineConfig {
        target_peers: 2,
        ..EngineConfig::default()
    };
    let limit = 5;
    let mut now = Instant::now();
    let mut scheduler = scheduler(&config, block::Height(1), checkpoints[50], now);
    scheduler.set_peers(
        &[(peer(1), block::Height::MAX), (peer(2), block::Height::MAX)],
        now,
    );

    // Peer 1 takes the frontier unit and fails it, again and again. Peer 2
    // is eligible, but does not pull, so it never fails.
    scheduler.peers.get_mut(&peer(2)).unwrap().backoff_until =
        Some(now + Duration::from_secs(3600));
    for _ in 0..(2 * limit) {
        let frontier = scheduler.next_assignment(now).expect("peer 1 pulls");
        assert_eq!(frontier.peer, peer(1));
        assert_eq!(frontier.unit.start, block::Height(1));
        scheduler.on_failure(frontier.stream, StreamFailure::Transport, now);
        now += STALL_BACKOFF;
    }
    assert!(scheduler.frontier_failures() >= limit);
    assert_eq!(scheduler.frontier_failed_peers().len(), 1);
    assert!(
        !scheduler.frontier_stuck(limit, now),
        "a peer that never failed is still eligible"
    );

    // Once the other peer fails it too, every eligible peer has failed.
    scheduler.frontier_failed_peers.insert(peer(2));
    assert!(scheduler.frontier_stuck(limit, now));

    // Failures from `limit` distinct peers are enough, whatever the others.
    scheduler.reset_frontier_failures();
    assert!(!scheduler.frontier_stuck(limit, now));
    for id in 10..(10 + u64::from(limit)) {
        scheduler.frontier_failed_peers.insert(peer(id));
    }
    assert!(scheduler.frontier_stuck(limit, now));
}

/// A closed connection does not count against the frontier.
#[test]
fn departed_peers_do_not_count_against_the_frontier() {
    let _init_guard = zebra_test::init();

    let checkpoints = checkpoints();
    let config = EngineConfig::default();
    let now = Instant::now();
    let mut scheduler = scheduler(&config, block::Height(1), checkpoints[50], now);
    scheduler.set_peers(&[(peer(1), block::Height::MAX)], now);

    let frontier = scheduler.next_assignment(now).unwrap();
    assert_eq!(frontier.unit.start, block::Height(1));
    let cancelled = scheduler.set_peers(&[], now);
    assert_eq!(cancelled, vec![frontier.stream]);
    assert_eq!(scheduler.frontier_failures(), 0);
    assert!(scheduler.frontier_failed_peers().is_empty());
}

/// A peer that does not find a checkpointed range at or below its own
/// advertised tip is excluded like a peer that refused, and replaced.
#[test]
fn not_found_below_the_advertised_tip_excludes_the_peer() {
    let _init_guard = zebra_test::init();

    let checkpoints = checkpoints();
    let config = EngineConfig {
        target_peers: 2,
        ..EngineConfig::default()
    };
    let now = Instant::now();
    let mut scheduler = scheduler(&config, block::Height(1), checkpoints[50], now);
    scheduler.set_peers(
        &[
            (peer(1), block::Height::MAX),
            (peer(2), block::Height::MAX),
            (peer(3), block::Height::MAX),
        ],
        now,
    );

    let first = scheduler.next_assignment(now).unwrap();
    assert_eq!(first.unit.anchor, Anchor::Checkpoint);
    scheduler.on_failure(first.stream, StreamFailure::NotFound, now);
    assert!(scheduler.peers[&first.peer].excluded(now + STALL_BACKOFF));
    assert!(!scheduler.peers[&first.peer].selected);
    assert_eq!(scheduler.selected_peers(), 2, "another peer replaces it");
}

/// A peer that connects once others have measured rates starts below their
/// median, so it is not given the frontier unit ahead of them.
#[test]
fn new_peers_start_below_the_measured_peers() {
    let _init_guard = zebra_test::init();

    let checkpoints = checkpoints();
    let config = EngineConfig {
        target_peers: 3,
        ..EngineConfig::default()
    };
    let mut now = Instant::now();
    let mut scheduler = scheduler(&config, block::Height(1), checkpoints[50], now);
    scheduler.set_peers(&[(peer(1), block::Height::MAX)], now);
    assert_eq!(scheduler.peers[&peer(1)].rate, INITIAL_RATE);

    // Peer 1 delivers at 1 MiB/s.
    let assignment = scheduler.next_assignment(now).unwrap();
    for _ in 0..40 {
        now += SAMPLE_INTERVAL;
        let received = scheduler.streams[&assignment.stream].counted_bytes + MIB / 4;
        scheduler.on_bytes(assignment.stream, received, now);
        scheduler.tick(now);
    }
    let measured = scheduler.peers[&peer(1)].rate;
    assert!(measured < INITIAL_RATE);

    scheduler.set_peers(
        &[(peer(1), block::Height::MAX), (peer(2), block::Height::MAX)],
        now,
    );
    assert!(scheduler.peers[&peer(2)].rate < measured);
    assert!(!scheduler.peers[&peer(2)].measured);
}

/// A known-hash source whose entries become resident when the test says so,
/// and which records what the scheduler asked of it.
#[derive(Debug, Default)]
struct SwitchedEntries {
    resident: std::sync::Arc<std::sync::atomic::AtomicBool>,
    requested_from: std::sync::Arc<std::sync::atomic::AtomicU32>,
    released_below: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

impl SwitchedEntries {
    /// Every height has a 1 MiB block.
    fn entry_at(height: block::Height) -> SyncHashEntry {
        let mut hash = [0; 32];
        hash[..4].copy_from_slice(&height.0.to_le_bytes());
        SyncHashEntry {
            hash: block::Hash(hash),
            size: MIB,
            txs: 1,
            txouts: 1,
            notes: 0,
        }
    }
}

impl HashSource for SwitchedEntries {
    fn max_height(&self) -> block::Height {
        block::Height(10_000_000)
    }

    fn entry(&self, height: block::Height) -> Option<SyncHashEntry> {
        self.resident
            .load(std::sync::atomic::Ordering::SeqCst)
            .then(|| Self::entry_at(height))
    }

    fn request(&mut self, start: block::Height, _end: block::Height) {
        self.requested_from
            .store(start.0, std::sync::atomic::Ordering::SeqCst);
    }

    fn changed(&mut self) -> Pin<Box<dyn Future<Output = Result<(), BoxError>> + Send + '_>> {
        Box::pin(std::future::pending())
    }

    fn release_below(&mut self, height: block::Height) {
        self.released_below
            .store(height.0, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Units start at checkpoint anchors while the known-hash entries are not
/// resident, and at known hashes, with exact sizes, once they are. The source
/// is asked for the entries from the commit frontier up, and releases those
/// below it.
#[test]
fn units_fall_back_to_checkpoints_until_known_hashes_are_resident() {
    use std::sync::atomic::Ordering;

    let _init_guard = zebra_test::init();

    let anchors = mainnet_anchors();
    let config = EngineConfig::default();
    let target = anchors.final_height();
    let source = SwitchedEntries::default();
    let resident = source.resident.clone();
    let requested_from = source.requested_from.clone();
    let released_below = source.released_below.clone();
    let mut scheduler = scheduler(&config, block::Height(1), target, Instant::now())
        .with_hash_source(Box::new(source));
    assert_eq!(requested_from.load(Ordering::SeqCst), 1);

    // Not resident: the frontier unit is a checkpoint interval.
    assert!(scheduler.plan_unit());
    let first = scheduler.units[&scheduler.queue.first().unwrap().1].unit;
    assert_eq!(first.anchor, Anchor::Checkpoint);
    assert_eq!(anchors.hash(first.end), Some(first.end_hash));
    assert_eq!(scheduler.known_hash_units(), 0);

    // Resident: the next unit is anchored at a known hash, deep, so 64 blocks of 1 MiB.
    resident.store(true, Ordering::SeqCst);
    assert!(scheduler.plan_unit());
    let second = scheduler
        .units
        .values()
        .map(|state| state.unit)
        .find(|unit| unit.start == first.end.next().unwrap())
        .expect("the next unit is planned");
    assert_eq!(second.anchor, Anchor::KnownHash);
    assert_eq!(second.est_bytes, DEEP_UNIT_BYTES);
    assert_eq!(second.end.0 - second.start.0 + 1, 64);
    assert_eq!(second.end_hash, SwitchedEntries::entry_at(second.end).hash);
    assert_eq!(scheduler.known_hash_units(), 1);
    assert_eq!(
        scheduler.known_hash(second.end),
        Some(SwitchedEntries::entry_at(second.end))
    );

    // The frontier moves: the entries below it are released, and the source is asked for the
    // entries above it.
    let base = first.end.next().unwrap();
    scheduler.set_frontier(base);
    assert_eq!(released_below.load(Ordering::SeqCst), base.0);
    assert_eq!(requested_from.load(Ordering::SeqCst), base.0);

    // A failed source is dropped: units fall back to checkpoint anchors.
    scheduler.drop_hash_source();
    assert!(scheduler.plan_unit());
    let third = scheduler
        .units
        .values()
        .map(|state| state.unit)
        .find(|unit| unit.start == second.end.next().unwrap())
        .expect("the next unit is planned");
    assert_eq!(third.anchor, Anchor::Checkpoint);
    assert_eq!(scheduler.known_hash(third.end), None);
}

/// With the entries resident from the start, the frontier unit is anchored
/// at a known hash and holds 16 MiB of blocks.
#[test]
fn the_frontier_unit_at_a_known_hash_is_small() {
    let _init_guard = zebra_test::init();

    let anchors = mainnet_anchors();
    let config = EngineConfig::default();
    let source = SwitchedEntries::default();
    source
        .resident
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mut scheduler = scheduler(
        &config,
        block::Height(1),
        anchors.final_height(),
        Instant::now(),
    )
    .with_hash_source(Box::new(source));

    assert!(scheduler.plan_unit());
    let frontier = scheduler.units[&scheduler.queue.first().unwrap().1].unit;
    assert_eq!(frontier.anchor, Anchor::KnownHash);
    assert_eq!(
        (frontier.start, frontier.end),
        (block::Height(1), block::Height(16))
    );
    assert_eq!(frontier.est_bytes, FRONTIER_UNIT_BYTES);
}
