//! The scheduler against fake peers, in paused time (design, §10 "the
//! scheduler against mock peers").

use std::{
    collections::{HashMap, HashSet},
    io,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    task::{Context, Poll},
    time::Duration,
};

use futures::{future::BoxFuture, FutureExt};
use tokio::{sync::mpsc, time::Instant};
use tower::Service;

use zebra_chain::state_snapshot::ObjectHash;
use zebra_network as zn;
use zebra_state::state_snapshot::{format::ComponentId, store::ArtifactStore, SnapshotError};

use crate::{
    components::state_snapshot::scheduler::{
        BlameReason, PeerBlame, PieceDescriptor, PieceStore, Scheduler, SchedulerConfig,
        SchedulerError, VerifiedPiece,
    },
    BoxError,
};

const KIB: u32 = 1 << 10;
const MIB: u32 = 1 << 20;

/// Returns `millis` milliseconds.
fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

/// Returns `secs` seconds.
fn secs(secs: u64) -> Duration {
    Duration::from_secs(secs)
}

/// Returns objects of `sizes` bytes, each with its own pseudo-random contents.
fn objects(sizes: &[u32]) -> Vec<Vec<u8>> {
    sizes
        .iter()
        .zip(1u32..)
        .map(|(&size, seed)| {
            let mut state = seed.wrapping_mul(2_654_435_761);
            (0..size)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    state.to_le_bytes()[3]
                })
                .collect()
        })
        .collect()
}

/// How a fake peer answers `get-object` requests.
#[derive(Clone, Debug, Default)]
struct Behavior {
    /// The delay before every answer.
    latency: Duration,

    /// An extra delay before answering a request at offset 0.
    first_range_latency: Duration,

    /// The rate one answer's bytes arrive at, in bytes per second: an answer
    /// of `n` bytes takes `n / rate` seconds more. `None` delivers them at
    /// once.
    rate: Option<u64>,

    /// The most bytes one answer carries, as a responder that finishes early.
    cap: Option<u32>,

    /// Never answers.
    hang: bool,

    /// Refuses every request.
    refuse: bool,

    /// Refuses the requests that start in these windows of time since the
    /// test started.
    refusing: Vec<(Duration, Duration)>,

    /// Objects the peer does not have, until this long after the test
    /// started.
    missing_until: HashMap<ObjectHash, Duration>,

    /// Objects the peer serves as other bytes, of another size.
    other_size: HashSet<ObjectHash>,

    /// Flips a bit of every answer.
    corrupt: bool,

    /// Answers with one byte more than was asked for.
    overdeliver: bool,

    /// The service fails, as a closed connection's does.
    broken: bool,
}

/// A request a fake peer received, logged when it ended.
#[derive(Clone, Debug)]
struct Call {
    /// The peer's index.
    peer: usize,

    /// The requested object.
    hash: ObjectHash,

    /// The requested offset.
    offset: u64,

    /// The requested length.
    length: u32,

    /// When the request started, since the test started.
    start: Duration,

    /// When the request ended, since the test started.
    end: Duration,

    /// How many bytes it was answered with, if it was answered with data.
    delivered: Option<usize>,

    /// The scheduler dropped the request before it was answered.
    dropped: bool,
}

impl Call {
    /// Returns whether this request and `other` were in flight at the same
    /// time.
    fn overlaps(&self, other: &Call) -> bool {
        self.start < other.end && other.start < self.end
    }
}

/// Counters shared by every clone of a fake peer, or by every peer.
#[derive(Debug, Default)]
struct Counters {
    /// The requests in flight.
    in_flight: AtomicUsize,

    /// The most requests that were in flight at once.
    max_in_flight: AtomicUsize,

    /// How many times the service was checked for readiness.
    readiness_checks: AtomicUsize,
}

impl Counters {
    /// The most requests that were in flight at once.
    fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }
}

/// A fake bootstrap peer.
#[derive(Clone)]
struct FakePeer {
    /// The peer's index in the scenario.
    index: usize,

    /// How it answers.
    behavior: Arc<Behavior>,

    /// The objects every honest peer has.
    objects: Arc<HashMap<ObjectHash, Vec<u8>>>,

    /// What this peer serves instead, for the objects it gives another size.
    lies: Arc<HashMap<ObjectHash, Vec<u8>>>,

    /// When the test started.
    epoch: Instant,

    /// Where requests are logged when they end.
    calls: mpsc::UnboundedSender<Call>,

    /// This peer's counters.
    counters: Arc<Counters>,

    /// The counters of every peer together.
    all: Arc<Counters>,
}

/// Logs a request when it ends, and keeps the in-flight counts.
struct CallGuard {
    /// The peer the request went to.
    peer: FakePeer,

    /// The request, as it will be logged.
    call: Call,
}

impl CallGuard {
    /// Starts logging a request to `peer`.
    fn start(peer: &FakePeer, hash: ObjectHash, offset: u64, length: u32) -> Self {
        for counters in [&peer.counters, &peer.all] {
            let in_flight = counters.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            counters
                .max_in_flight
                .fetch_max(in_flight, Ordering::SeqCst);
        }
        CallGuard {
            peer: peer.clone(),
            call: Call {
                peer: peer.index,
                hash,
                offset,
                length,
                start: peer.epoch.elapsed(),
                end: Duration::ZERO,
                delivered: None,
                dropped: true,
            },
        }
    }

    /// Marks the request answered, with `delivered` bytes of data if it had
    /// data.
    fn answered(&mut self, delivered: Option<usize>) {
        self.call.dropped = false;
        self.call.delivered = delivered;
    }
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        for counters in [&self.peer.counters, &self.peer.all] {
            counters.in_flight.fetch_sub(1, Ordering::SeqCst);
        }
        self.call.end = self.peer.epoch.elapsed();
        let _ = self.peer.calls.send(self.call.clone());
    }
}

impl FakePeer {
    /// Answers `request` as the peer's behavior says.
    async fn answer(self, request: zn::Request) -> Result<zn::Response, BoxError> {
        let zn::Request::Object {
            hash,
            offset,
            length,
        } = request
        else {
            return Err("the scheduler only sends object requests".into());
        };
        let mut guard = CallGuard::start(&self, hash, offset, length);
        let behavior = &self.behavior;

        tokio::time::sleep(behavior.latency).await;
        if offset == 0 {
            tokio::time::sleep(behavior.first_range_latency).await;
        }
        if behavior.hang {
            futures::future::pending::<()>().await;
        }
        let call_start = guard.call.start;
        if behavior.refuse
            || behavior
                .refusing
                .iter()
                .any(|(from, until)| (*from..*until).contains(&call_start))
        {
            guard.answered(None);
            // How the network layer reports a peer's `REFUSED` stream reset.
            let refusal = zn::PeerError::RequestStreamFailed(
                "stream I/O error: stream reset by peer: error 8".to_string(),
            );
            return Err(zn::SharedPeerError::from(refusal).into());
        }

        let missing = behavior
            .missing_until
            .get(&hash)
            .is_some_and(|until| self.epoch.elapsed() < *until);
        let object = self.lies.get(&hash).or_else(|| self.objects.get(&hash));
        let Some(object) = object.filter(|_| !missing) else {
            guard.answered(None);
            return Ok(zn::Response::ObjectNotFound);
        };

        let size = object.len();
        let start = usize::try_from(offset).unwrap_or(usize::MAX).min(size);
        let length = usize::try_from(length).expect("a u32 fits in a usize on supported targets");
        let mut end = start.saturating_add(length).min(size);
        if let Some(cap) = behavior.cap {
            let cap = usize::try_from(cap).expect("a u32 fits in a usize on supported targets");
            end = end.min(start.saturating_add(cap));
        }
        let mut data = object[start..end].to_vec();
        if behavior.overdeliver {
            data.push(0);
        }
        if behavior.corrupt {
            if let Some(byte) = data.first_mut() {
                *byte ^= 1;
            }
        }
        if let Some(rate) = behavior.rate {
            let bytes = u64::try_from(data.len()).expect("a usize fits in a u64");
            tokio::time::sleep(Duration::from_micros(bytes * 1_000_000 / rate.max(1))).await;
        }

        guard.answered(Some(data.len()));
        Ok(zn::Response::Object {
            size: u64::try_from(size).expect("a usize fits in a u64"),
            data,
        })
    }
}

impl Service<zn::Request> for FakePeer {
    type Response = zn::Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<zn::Response, BoxError>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        self.counters
            .readiness_checks
            .fetch_add(1, Ordering::SeqCst);
        if self.behavior.broken {
            Poll::Ready(Err("the connection was closed".into()))
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn call(&mut self, request: zn::Request) -> Self::Future {
        self.clone().answer(request).boxed()
    }
}

/// A piece store that hands the stored pieces to the test, or always fails.
#[derive(Clone)]
struct MemoryStore {
    /// Where stored pieces go.
    stored: mpsc::UnboundedSender<(ObjectHash, Vec<u8>)>,

    /// Every write fails.
    fail: bool,
}

impl PieceStore for MemoryStore {
    fn put(&self, hash: ObjectHash, object: &[u8]) -> Result<PathBuf, SnapshotError> {
        let path = PathBuf::from("objects").join(hash.to_string());
        if self.fail {
            return Err(SnapshotError::Io {
                path,
                source: io::Error::other("the test store always fails"),
            });
        }
        let _ = self.stored.send((hash, object.to_vec()));
        Ok(path)
    }
}

/// The objects to download, and the peers to download them from.
struct Scenario {
    /// The scheduler's limits.
    config: SchedulerConfig,

    /// The objects, one manifest piece each.
    objects: Vec<Vec<u8>>,

    /// More manifest pieces with the same contents as these objects.
    duplicates: Vec<usize>,

    /// How each peer answers.
    peers: Vec<Behavior>,

    /// Every store write fails.
    fail_store: bool,

    /// The receiver of verified pieces is dropped before the run.
    drop_verified: bool,
}

/// What a scenario's run did.
struct Outcome {
    /// How the run ended.
    result: Result<(), SchedulerError>,

    /// How long the run took.
    elapsed: Duration,

    /// The objects.
    objects: Vec<Vec<u8>>,

    /// The manifest pieces: one per object, then the duplicates.
    descriptors: Vec<PieceDescriptor>,

    /// The pieces the scheduler reported.
    verified: Vec<VerifiedPiece>,

    /// The pieces the scheduler stored.
    stored: Vec<(ObjectHash, Vec<u8>)>,

    /// The peers the scheduler blamed.
    blamed: Vec<PeerBlame>,

    /// Every request, in the order they started.
    calls: Vec<Call>,

    /// Each peer's counters.
    counters: Vec<Arc<Counters>>,

    /// The counters of every peer together.
    all: Arc<Counters>,

    /// Each peer's address.
    addrs: Vec<zn::PeerSocketAddr>,
}

impl Scenario {
    /// Returns a scenario with the default limits.
    fn new(objects: Vec<Vec<u8>>, peers: Vec<Behavior>) -> Self {
        Scenario {
            config: SchedulerConfig::default(),
            objects,
            duplicates: Vec::new(),
            peers,
            fail_store: false,
            drop_verified: false,
        }
    }

    /// Runs the scheduler on the scenario.
    async fn run(self) -> Outcome {
        let epoch = Instant::now();

        let mut descriptors: Vec<PieceDescriptor> = self
            .objects
            .iter()
            .zip(0u32..)
            .map(|(object, index)| PieceDescriptor {
                component: ComponentId::ALL[usize::try_from(index).expect("small") % 28],
                index,
                size: u32::try_from(object.len()).expect("test objects are small"),
                hash: ObjectHash::of(object),
            })
            .collect();
        for (&object, index) in self.duplicates.iter().zip(1000u32..) {
            descriptors.push(PieceDescriptor {
                component: ComponentId::ValuePools,
                index,
                ..descriptors[object].clone()
            });
        }
        let honest: Arc<HashMap<ObjectHash, Vec<u8>>> = Arc::new(
            self.objects
                .iter()
                .map(|object| (ObjectHash::of(object), object.clone()))
                .collect(),
        );

        let (calls_tx, mut calls_rx) = mpsc::unbounded_channel();
        let all = Arc::new(Counters::default());
        let mut peers = Vec::new();
        let mut counters = Vec::new();
        let mut addrs = Vec::new();
        for (index, behavior) in self.peers.into_iter().enumerate() {
            let lies = behavior
                .other_size
                .iter()
                .filter_map(|hash| {
                    let object = honest.get(hash)?;
                    let lie = object.iter().map(|byte| !byte).chain([0; 1000]).collect();
                    Some((*hash, lie))
                })
                .collect();
            let peer = FakePeer {
                index,
                behavior: Arc::new(behavior),
                objects: honest.clone(),
                lies: Arc::new(lies),
                epoch,
                calls: calls_tx.clone(),
                counters: Arc::new(Counters::default()),
                all: all.clone(),
            };
            let port = u16::try_from(index)
                .ok()
                .and_then(|index| index.checked_add(8233))
                .expect("tests have few peers");
            let addr = zn::PeerSocketAddr::from(SocketAddr::from(([127, 0, 0, 1], port)));
            counters.push(peer.counters.clone());
            addrs.push(addr);
            peers.push((addr, peer));
        }
        drop(calls_tx);

        let (verified_tx, verified_rx) = mpsc::channel(descriptors.len().max(1));
        let mut verified_rx = (!self.drop_verified).then_some(verified_rx);
        let (blame_tx, mut blame_rx) = mpsc::channel(peers.len().max(1));
        let (stored_tx, mut stored_rx) = mpsc::unbounded_channel();
        let store = MemoryStore {
            stored: stored_tx,
            fail: self.fail_store,
        };

        let scheduler = Scheduler::new(
            self.config,
            descriptors.clone(),
            peers,
            ArtifactStore::at(PathBuf::from("unused")),
            verified_tx,
            blame_tx,
        );
        let result = tokio::time::timeout(
            Duration::from_secs(24 * 60 * 60),
            scheduler.run_storing_in(store),
        )
        .await
        .expect("the scheduler finishes within a day of paused time");
        let elapsed = epoch.elapsed();

        let mut verified = Vec::new();
        if let Some(receiver) = verified_rx.as_mut() {
            while let Ok(piece) = receiver.try_recv() {
                verified.push(piece);
            }
        }
        let mut blamed = Vec::new();
        while let Ok(blame) = blame_rx.try_recv() {
            blamed.push(blame);
        }
        let mut stored = Vec::new();
        while let Ok(piece) = stored_rx.try_recv() {
            stored.push(piece);
        }
        let mut calls = Vec::new();
        while let Ok(call) = calls_rx.try_recv() {
            calls.push(call);
        }
        calls.sort_by_key(|call| (call.start, call.peer, call.offset));

        Outcome {
            result,
            elapsed,
            objects: self.objects,
            descriptors,
            verified,
            stored,
            blamed,
            calls,
            counters,
            all,
            addrs,
        }
    }
}

impl Outcome {
    /// Asserts that the run succeeded: each manifest piece was reported once,
    /// at its object's path, and each object was stored once with its
    /// contents.
    fn assert_complete(&self) {
        assert!(self.result.is_ok(), "{:?}", self.result);

        let mut reported: Vec<&PieceDescriptor> = self
            .verified
            .iter()
            .map(|piece| &piece.descriptor)
            .collect();
        reported.sort_by_key(|descriptor| (descriptor.index, descriptor.hash));
        let mut expected: Vec<&PieceDescriptor> = self.descriptors.iter().collect();
        expected.sort_by_key(|descriptor| (descriptor.index, descriptor.hash));
        assert_eq!(reported, expected, "each piece is reported once");

        for piece in &self.verified {
            assert_eq!(
                piece.path,
                PathBuf::from("objects").join(piece.descriptor.hash.to_string()),
            );
        }

        let mut stored: Vec<(ObjectHash, &Vec<u8>)> = self
            .stored
            .iter()
            .map(|(hash, bytes)| (*hash, bytes))
            .collect();
        stored.sort_by_key(|(hash, _)| *hash);
        let mut objects: Vec<(ObjectHash, &Vec<u8>)> = self
            .objects
            .iter()
            .map(|object| (ObjectHash::of(object), object))
            .collect();
        objects.sort_by_key(|(hash, _)| *hash);
        objects.dedup_by_key(|(hash, _)| *hash);
        assert!(stored == objects, "each object is stored once, intact");
    }

    /// Returns the hash of object `object`.
    fn hash(&self, object: usize) -> ObjectHash {
        self.descriptors[object].hash
    }

    /// Returns the requests to `peer`, in the order they started.
    fn calls_of(&self, peer: usize) -> Vec<&Call> {
        self.calls.iter().filter(|call| call.peer == peer).collect()
    }

    /// Returns the requests to `peer` for object `object`, in the order they
    /// started.
    fn calls_of_for(&self, peer: usize, object: usize) -> Vec<&Call> {
        let hash = self.hash(object);
        self.calls
            .iter()
            .filter(|call| call.peer == peer && call.hash == hash)
            .collect()
    }

    /// Returns the size of the object `hash`.
    fn size_of(&self, hash: ObjectHash) -> u64 {
        let size = self
            .descriptors
            .iter()
            .find(|descriptor| descriptor.hash == hash)
            .expect("requests are for manifest pieces")
            .size;
        u64::from(size)
    }
}

/// Three peers at different rates download pieces of many sizes: each piece
/// is reported once and stored intact, each peer works on one piece at a
/// time with at most two sub-ranges in flight, starts with 1 MiB sub-ranges,
/// and never asks past the end of a piece.
#[tokio::test(start_paused = true)]
async fn three_peers_download_every_piece() {
    let _init_guard = zebra_test::init();

    let sizes = [
        1,
        1000,
        256 * KIB,
        MIB,
        MIB + 1,
        3 * MIB + 7,
        5 * MIB,
        8 * MIB - 3,
        2 * MIB,
        700 * KIB,
        4 * MIB,
        6 * MIB + 17,
    ];
    let peers = [4, 6, 8]
        .map(|mib_per_sec: u64| Behavior {
            latency: ms(50),
            rate: Some(mib_per_sec * u64::from(MIB)),
            ..Behavior::default()
        })
        .to_vec();
    let outcome = Scenario::new(objects(&sizes), peers).run().await;

    outcome.assert_complete();
    assert!(outcome.blamed.is_empty());
    assert!(outcome.calls.iter().all(|call| !call.dropped));

    for peer in 0..3 {
        let calls = outcome.calls_of(peer);
        assert!(!calls.is_empty(), "peer {peer} took part");

        // One piece per peer at a time.
        for (index, call) in calls.iter().enumerate() {
            for other in &calls[index + 1..] {
                if call.overlaps(other) {
                    assert_eq!(call.hash, other.hash, "peer {peer} works on one piece");
                }
            }
        }

        // At most two sub-ranges in flight, and pipelined.
        assert!(outcome.counters[peer].max_in_flight() <= 2);

        // A peer's rate is unknown at first, so it starts with 1 MiB.
        let first = calls[0];
        assert_eq!(
            u64::from(first.length),
            outcome.size_of(first.hash).min(MIB.into())
        );
    }
    assert!((0..3).any(|peer| outcome.counters[peer].max_in_flight() == 2));

    for call in &outcome.calls {
        assert!(
            call.offset + u64::from(call.length) <= outcome.size_of(call.hash),
            "{call:?} is inside its piece",
        );
        assert!(call.length <= 4 * MIB);
    }
}

/// After its first answers, a peer is asked for 8 seconds of its rate at a
/// time, between 256 KiB and 4 MiB, and the last sub-range ends at the end
/// of the piece.
#[tokio::test(start_paused = true)]
async fn sub_ranges_follow_the_peer_rate() {
    let _init_guard = zebra_test::init();

    for (rate, expected) in [
        (64 * KIB, 512 * KIB),
        (100 * KIB, 800 * KIB),
        (MIB, 4 * MIB),
        (16 * KIB, 256 * KIB),
    ] {
        let size = 12 * MIB;
        let peer = Behavior {
            rate: Some(rate.into()),
            ..Behavior::default()
        };
        let outcome = Scenario {
            // Keep the slowest peer above the rate floor, and inside the deadline.
            config: SchedulerConfig {
                min_rate_bytes_per_sec: KIB.into(),
                piece_phase_stall: secs(60 * 60),
                ..SchedulerConfig::default()
            },
            ..Scenario::new(objects(&[size]), vec![peer])
        }
        .run()
        .await;

        outcome.assert_complete();
        let calls = outcome.calls_of(0);
        let lengths: Vec<u32> = calls.iter().map(|call| call.length).collect();
        assert_eq!(
            lengths[..4],
            [MIB, MIB, expected, expected],
            "at {rate} B/s"
        );

        let last = calls
            .iter()
            .max_by_key(|call| call.offset)
            .expect("the piece was requested");
        assert_eq!(last.offset + u64::from(last.length), u64::from(size));
    }
}

/// A peer that finishes each answer early, with more than the short-answer
/// threshold, delivers the whole piece: the rest of each sub-range is asked
/// for again, from the same peer.
#[tokio::test(start_paused = true)]
async fn partial_answers_are_resumed_from_the_same_peer() {
    let _init_guard = zebra_test::init();

    let size = MIB + 5;
    let peer = Behavior {
        latency: ms(100),
        cap: Some(100 * KIB),
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects(&[size]), vec![peer]).run().await;

    outcome.assert_complete();
    let delivered: usize = outcome
        .calls
        .iter()
        .map(|call| call.delivered.expect("every request is answered with data"))
        .sum();
    assert_eq!(
        delivered,
        usize::try_from(size).expect("fits"),
        "nothing is asked for twice"
    );
    // Twelve answers, two at a time: the peer was never rested.
    assert_eq!(outcome.calls.len(), 12);
    assert!(outcome.elapsed < secs(2), "{:?}", outcome.elapsed);
}

/// A sub-range answered before the one in front of it waits for it: the
/// piece is hashed in order and verifies.
#[tokio::test(start_paused = true)]
async fn out_of_order_answers_are_hashed_in_order() {
    let _init_guard = zebra_test::init();

    let peer = Behavior {
        first_range_latency: secs(2),
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects(&[3 * MIB]), vec![peer]).run().await;

    outcome.assert_complete();
    assert!(outcome.blamed.is_empty());
    let first = &outcome.calls_of(0)[0];
    assert_eq!(first.offset, 0);
    assert!(
        outcome.calls.iter().any(|call| call.end < first.end),
        "a later sub-range was answered first",
    );
}

/// Two manifest pieces with the same contents are downloaded once and both
/// reported.
#[tokio::test(start_paused = true)]
async fn identical_pieces_are_downloaded_once() {
    let _init_guard = zebra_test::init();

    let outcome = Scenario {
        duplicates: vec![0],
        ..Scenario::new(
            objects(&[2 * MIB, MIB]),
            vec![Behavior::default(), Behavior::default()],
        )
    }
    .run()
    .await;

    outcome.assert_complete();
    let hash = outcome.hash(0);
    let requested: u64 = outcome
        .calls
        .iter()
        .filter(|call| call.hash == hash)
        .map(|call| u64::from(call.length))
        .sum();
    assert_eq!(requested, u64::from(2 * MIB), "the object is fetched once");
    assert_eq!(outcome.stored.len(), 2);
}

/// Pieces in flight never exceed the byte budget.
#[tokio::test(start_paused = true)]
async fn the_budget_bounds_the_pieces_in_flight() {
    let _init_guard = zebra_test::init();

    let peer = Behavior {
        latency: secs(1),
        ..Behavior::default()
    };
    let outcome = Scenario {
        config: SchedulerConfig {
            budget_bytes: (2 * MIB).into(),
            ..SchedulerConfig::default()
        },
        // One 1 MiB sub-range per piece, so requests in flight are pieces in flight.
        ..Scenario::new(objects(&[MIB; 6]), vec![peer; 3])
    }
    .run()
    .await;

    outcome.assert_complete();
    assert_eq!(outcome.all.max_in_flight(), 2);
}

/// A piece larger than the whole budget is still downloaded, alone.
#[tokio::test(start_paused = true)]
async fn a_piece_larger_than_the_budget_is_downloaded() {
    let _init_guard = zebra_test::init();

    let outcome = Scenario {
        config: SchedulerConfig {
            budget_bytes: (512 * KIB).into(),
            ..SchedulerConfig::default()
        },
        ..Scenario::new(objects(&[MIB, MIB]), vec![Behavior::default(); 2])
    }
    .run()
    .await;

    outcome.assert_complete();
}

/// Only the first `max_peers` peers are used.
#[tokio::test(start_paused = true)]
async fn at_most_max_peers_are_used() {
    let _init_guard = zebra_test::init();

    let peer = Behavior {
        latency: ms(100),
        ..Behavior::default()
    };
    let outcome = Scenario {
        config: SchedulerConfig {
            max_peers: 2,
            ..SchedulerConfig::default()
        },
        ..Scenario::new(objects(&[MIB; 6]), vec![peer; 3])
    }
    .run()
    .await;

    outcome.assert_complete();
    assert!(!outcome.calls_of(0).is_empty());
    assert!(!outcome.calls_of(1).is_empty());
    assert!(outcome.calls_of(2).is_empty());
    assert_eq!(
        outcome.counters[2].readiness_checks.load(Ordering::SeqCst),
        0
    );
}

/// A peer whose service fails is excluded at once, without blame.
#[tokio::test(start_paused = true)]
async fn a_failed_connection_excludes_the_peer() {
    let _init_guard = zebra_test::init();

    let broken = Behavior {
        broken: true,
        ..Behavior::default()
    };
    let honest = Behavior {
        latency: ms(100),
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects(&[MIB; 4]), vec![broken, honest])
        .run()
        .await;

    outcome.assert_complete();
    assert!(outcome.blamed.is_empty());
    assert!(outcome.calls_of(0).is_empty());
    assert_eq!(
        outcome.counters[0].readiness_checks.load(Ordering::SeqCst),
        1,
        "the failed peer is not asked again",
    );
}

/// A peer that refuses every request backs off for 1 second, doubling up to
/// 30 seconds, and is never excluded or blamed.
#[tokio::test(start_paused = true)]
async fn a_refusing_peer_backs_off_without_penalty() {
    let _init_guard = zebra_test::init();

    let refusing = Behavior {
        refuse: true,
        ..Behavior::default()
    };
    let slow = Behavior {
        latency: secs(8),
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects(&[256 * KIB; 20]), vec![refusing, slow])
        .run()
        .await;

    outcome.assert_complete();
    assert!(outcome.blamed.is_empty());
    let starts: Vec<Duration> = outcome.calls_of(0).iter().map(|call| call.start).collect();
    let gaps: Vec<Duration> = starts.windows(2).map(|pair| pair[1] - pair[0]).collect();
    assert!(gaps.len() >= 7, "{starts:?}");
    assert_eq!(gaps[..7], [1, 2, 4, 8, 16, 30, 30].map(secs), "{starts:?}");
}

/// An answer ends a peer's run of refusals, so its next refusal backs it off
/// for the shortest time again.
#[tokio::test(start_paused = true)]
async fn an_answer_resets_the_refusal_backoff() {
    let _init_guard = zebra_test::init();

    let peer = Behavior {
        latency: secs(1),
        refusing: vec![(secs(0), secs(2)), (secs(10), secs(12))],
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects(&[256 * KIB; 12]), vec![peer])
        .run()
        .await;

    outcome.assert_complete();
    // Refused at 1 s and at 11 s, each followed by a 1 s rest.
    let starts: Vec<Duration> = outcome.calls.iter().map(|call| call.start).collect();
    assert_eq!(
        starts,
        [0, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 13, 14, 15].map(secs)
    );
    assert_eq!(outcome.elapsed, secs(16));
}

/// A peer that answers with more bytes than were asked for breaks the
/// protocol, which the network layer closes the connection for, so it is
/// excluded, without blame.
#[tokio::test(start_paused = true)]
async fn an_answer_longer_than_its_request_excludes_the_peer() {
    let _init_guard = zebra_test::init();

    let flooding = Behavior {
        latency: ms(100),
        overdeliver: true,
        ..Behavior::default()
    };
    let honest = Behavior {
        latency: ms(100),
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects(&[MIB; 4]), vec![flooding, honest])
        .run()
        .await;

    outcome.assert_complete();
    assert!(outcome.blamed.is_empty());
    assert_eq!(outcome.calls_of(0).len(), 1, "the peer is not asked again");
}

/// A peer that does not have an object is not asked for it again for 30
/// seconds, and keeps serving the objects it has.
#[tokio::test(start_paused = true)]
async fn not_found_backs_off_for_that_peer_and_object() {
    let _init_guard = zebra_test::init();

    let objects = objects(&[256 * KIB; 3]);
    let peer = Behavior {
        latency: ms(10),
        missing_until: [(ObjectHash::of(&objects[0]), secs(40))].into(),
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects, vec![peer]).run().await;

    outcome.assert_complete();
    // Asked at once, then 30 seconds after each not-found answer, and found after 40 seconds.
    let calls = outcome.calls_of_for(0, 0);
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert_eq!(calls[0].start, Duration::ZERO);
    assert_eq!(calls[1].start, calls[0].end + secs(30));
    assert_eq!(calls[2].start, calls[1].end + secs(30));
    assert_eq!(calls[2].delivered, Some(256 * 1024));
    for object in [1, 2] {
        assert!(outcome.calls_of_for(0, object)[0].start < secs(1));
    }
}

/// The not-found backoff is for one peer and object: another peer is asked
/// for the object at once.
#[tokio::test(start_paused = true)]
async fn not_found_backs_off_only_that_peer() {
    let _init_guard = zebra_test::init();

    let objects = objects(&[256 * KIB; 2]);
    let lacking = Behavior {
        latency: ms(10),
        missing_until: [(ObjectHash::of(&objects[0]), Duration::MAX)].into(),
        ..Behavior::default()
    };
    let honest = Behavior {
        latency: ms(10),
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects, vec![lacking, honest]).run().await;

    outcome.assert_complete();
    assert!(outcome.blamed.is_empty());
    assert_eq!(
        outcome.calls_of_for(0, 0).len(),
        1,
        "the lacking peer is asked once"
    );
    assert_eq!(outcome.calls_of_for(1, 0).len(), 1);
    assert_eq!(outcome.elapsed, ms(20));
}

/// A peer that gives a piece another size than the manifest is not asked
/// for that piece again, is not blamed, and keeps serving other pieces.
#[tokio::test(start_paused = true)]
async fn a_peer_with_another_size_is_not_blamed() {
    let _init_guard = zebra_test::init();

    let objects = objects(&[2 * MIB; 4]);
    let lying = Behavior {
        latency: ms(100),
        other_size: [ObjectHash::of(&objects[0])].into(),
        ..Behavior::default()
    };
    let honest = Behavior {
        latency: ms(100),
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects, vec![lying, honest]).run().await;

    outcome.assert_complete();
    assert!(outcome.blamed.is_empty());
    assert!(outcome
        .calls_of_for(0, 0)
        .iter()
        .all(|call| call.start == Duration::ZERO));
    let other_pieces = outcome
        .calls_of(0)
        .iter()
        .filter(|call| call.hash != outcome.hash(0))
        .count();
    assert!(other_pieces > 0, "the peer is not excluded");
}

/// A peer that delivers a whole piece with the wrong hash is blamed and
/// excluded, and the piece is downloaded again from another peer.
#[tokio::test(start_paused = true)]
async fn a_corrupt_piece_blames_and_excludes_its_peer() {
    let _init_guard = zebra_test::init();

    let corrupt = Behavior {
        latency: ms(100),
        corrupt: true,
        ..Behavior::default()
    };
    let honest = Behavior {
        latency: ms(100),
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects(&[MIB; 4]), vec![corrupt, honest])
        .run()
        .await;

    outcome.assert_complete();
    assert_eq!(
        outcome.blamed,
        [PeerBlame {
            peer: outcome.addrs[0],
            reason: BlameReason::HashMismatch {
                hash: outcome.hash(0)
            },
        }],
    );
    assert!(
        outcome
            .calls_of(0)
            .iter()
            .all(|call| call.hash == outcome.hash(0)),
        "the blamed peer is not asked again",
    );
}

/// The scheduler stops when every peer is excluded.
#[tokio::test(start_paused = true)]
async fn the_scheduler_stops_when_every_peer_is_excluded() {
    let _init_guard = zebra_test::init();

    let corrupt = Behavior {
        latency: ms(100),
        corrupt: true,
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects(&[MIB; 3]), vec![corrupt; 2])
        .run()
        .await;

    assert!(
        matches!(outcome.result, Err(SchedulerError::NoPeers)),
        "{:?}",
        outcome.result
    );
    assert_eq!(outcome.blamed.len(), 2);
    assert!(outcome.elapsed < secs(1), "{:?}", outcome.elapsed);
}

/// The scheduler stops when no piece verifies for the piece-phase deadline.
#[tokio::test(start_paused = true)]
async fn the_scheduler_stops_when_no_piece_verifies() {
    let _init_guard = zebra_test::init();

    let refusing = Behavior {
        refuse: true,
        ..Behavior::default()
    };
    let outcome = Scenario {
        config: SchedulerConfig {
            piece_phase_stall: secs(60),
            ..SchedulerConfig::default()
        },
        ..Scenario::new(objects(&[MIB; 2]), vec![refusing; 2])
    }
    .run()
    .await;

    assert!(
        matches!(outcome.result, Err(SchedulerError::Stalled(stall)) if stall == secs(60)),
        "{:?}",
        outcome.result
    );
    assert_eq!(outcome.elapsed, secs(60));
}

/// The scheduler stops when the receiver of verified pieces is dropped.
#[tokio::test(start_paused = true)]
async fn the_scheduler_stops_when_the_verified_receiver_is_dropped() {
    let _init_guard = zebra_test::init();

    let peer = Behavior {
        latency: secs(1),
        ..Behavior::default()
    };
    let outcome = Scenario {
        drop_verified: true,
        ..Scenario::new(objects(&[MIB; 2]), vec![peer])
    }
    .run()
    .await;

    assert!(
        matches!(outcome.result, Err(SchedulerError::Closed)),
        "{:?}",
        outcome.result
    );
    assert_eq!(outcome.elapsed, Duration::ZERO);
}

/// The scheduler stops when a verified piece can not be stored.
#[tokio::test(start_paused = true)]
async fn the_scheduler_stops_when_a_piece_can_not_be_stored() {
    let _init_guard = zebra_test::init();

    let outcome = Scenario {
        fail_store: true,
        ..Scenario::new(objects(&[MIB; 2]), vec![Behavior::default()])
    }
    .run()
    .await;

    assert!(
        matches!(outcome.result, Err(SchedulerError::Store(_))),
        "{:?}",
        outcome.result
    );
}

/// With nothing to download, the scheduler is done at once; with pieces and
/// no peers, it stops at once.
#[tokio::test(start_paused = true)]
async fn the_scheduler_needs_peers_only_for_pieces() {
    let _init_guard = zebra_test::init();

    let outcome = Scenario::new(Vec::new(), Vec::new()).run().await;
    outcome.assert_complete();
    assert_eq!(outcome.elapsed, Duration::ZERO);

    let outcome = Scenario::new(objects(&[MIB]), Vec::new()).run().await;
    assert!(
        matches!(outcome.result, Err(SchedulerError::NoPeers)),
        "{:?}",
        outcome.result
    );
}

/// An empty piece is stored and reported without asking a peer for it.
#[tokio::test(start_paused = true)]
async fn an_empty_piece_needs_no_peer() {
    let _init_guard = zebra_test::init();

    let outcome = Scenario::new(objects(&[0, MIB]), vec![Behavior::default()])
        .run()
        .await;

    outcome.assert_complete();
    assert!(outcome
        .calls
        .iter()
        .all(|call| call.hash != outcome.hash(0)));
}
