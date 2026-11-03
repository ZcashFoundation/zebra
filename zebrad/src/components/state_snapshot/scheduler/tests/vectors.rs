//! The scheduler against fake peers, in paused time (design, §10 "the
//! scheduler against mock peers").

use std::{
    collections::{HashMap, HashSet},
    io,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
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

    /// Later delays before every answer: from each time since the test
    /// started, the delay for the requests that start then.
    phases: Vec<(Duration, Duration)>,

    /// An extra delay before answering a request at offset 0.
    first_range_latency: Duration,

    /// The rate of the peer's link, in bytes per second, which its requests
    /// in flight share as a real connection's streams do: an answer's bytes
    /// are sent after those of the answers before it, `n` bytes in `n / rate`
    /// seconds. `None` delivers them at once.
    rate: Option<u64>,

    /// The most bytes one answer carries, as a responder that finishes early.
    cap: Option<u32>,

    /// Never answers.
    hang: bool,

    /// Fails every request after its latency, as the network layer does
    /// when its request deadline passes before the peer answered.
    time_out: bool,

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

    /// When the peer's link has sent every answer so far, in microseconds
    /// since the test started.
    link_free_at: AtomicU64,
}

impl Counters {
    /// The most requests that were in flight at once.
    fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }

    /// Queues an answer that takes `send` to send on the peer's link at `now`,
    /// both since the test started, and returns when it will have been sent.
    fn send_on_link(&self, now: Duration, send: Duration) -> Duration {
        let now = u64::try_from(now.as_micros()).expect("tests run for less than u64::MAX µs");
        let send = u64::try_from(send.as_micros()).expect("answers take less than u64::MAX µs");
        let sent = |free: u64| free.max(now).saturating_add(send);
        let free = self
            .link_free_at
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |free| Some(sent(free)))
            .expect("the update always returns a value");
        Duration::from_micros(sent(free))
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

        let started = self.epoch.elapsed();
        let latency = behavior
            .phases
            .iter()
            .rev()
            .find(|(from, _)| started >= *from)
            .map_or(behavior.latency, |(_, latency)| *latency);
        tokio::time::sleep(latency).await;
        if offset == 0 {
            tokio::time::sleep(behavior.first_range_latency).await;
        }
        if behavior.hang {
            futures::future::pending::<()>().await;
        }
        if behavior.time_out {
            guard.answered(None);
            return Err(zn::SharedPeerError::from(zn::PeerError::ConnectionReceiveTimeout).into());
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
            let send = Duration::from_micros(bytes * 1_000_000 / rate.max(1));
            let sent = self.counters.send_on_link(self.epoch.elapsed(), send);
            tokio::time::sleep_until(self.epoch + sent).await;
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
/// of the piece. The rate is the peer's, which its sub-ranges in flight
/// share, not one sub-range's.
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

/// A sub-range in flight behind another one has time for both: a peer above
/// the rate floor delivers it in full, though it takes longer than a lone
/// sub-range's deadline.
#[tokio::test(start_paused = true)]
async fn a_pipelined_sub_range_has_time_for_the_one_ahead() {
    let _init_guard = zebra_test::init();

    let peer = Behavior {
        rate: Some((100 * KIB).into()),
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects(&[2 * MIB]), vec![peer]).run().await;

    outcome.assert_complete();
    // Two 1 MiB sub-ranges: the second is sent after the first, from 10.24 s to 20.48 s, past the
    // 16 s a lone 1 MiB sub-range gets.
    let calls = outcome.calls_of(0);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls
        .iter()
        .all(|call| call.delivered == Some(usize::try_from(MIB).expect("fits"))));
    assert_eq!(outcome.elapsed, ms(20_480));
}

/// A peer whose connection delivers above the 64 KiB/s floor is not stalled,
/// though each of its two sub-ranges in flight gets half of it.
#[tokio::test(start_paused = true)]
async fn a_pipelining_peer_above_the_rate_floor_is_not_stalled() {
    let _init_guard = zebra_test::init();

    let peer = Behavior {
        rate: Some((96 * KIB).into()),
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects(&[8 * MIB]), vec![peer]).run().await;

    outcome.assert_complete();
    assert!(outcome.calls.iter().all(|call| !call.dropped));
    // 8 MiB at 96 KiB/s takes 85.3 s, with the connection always busy.
    assert!(outcome.elapsed < secs(86), "{:?}", outcome.elapsed);
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
    // Above the rate floor, and busy for longer than the backoffs below.
    let honest = Behavior {
        latency: secs(2),
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects(&[256 * KIB; 50]), vec![refusing, honest])
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

/// A peer that finishes every answer early with fewer bytes than the
/// short-answer threshold is stalled after three of them: its piece goes,
/// whole, to another peer, and it rests for the stall cooldown.
#[tokio::test(start_paused = true)]
async fn short_answers_stall_a_peer() {
    let _init_guard = zebra_test::init();

    let early_fin = Behavior {
        latency: ms(100),
        cap: Some(32 * KIB),
        ..Behavior::default()
    };
    let honest = Behavior {
        latency: secs(1),
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects(&[2 * MIB; 6]), vec![early_fin, honest])
        .run()
        .await;

    outcome.assert_complete();
    assert!(outcome.blamed.is_empty());

    // Two short answers at 0.1 s, the third at 0.2 s, then nothing for the rest of the run.
    let calls = outcome.calls_of(0);
    assert_eq!(calls.len(), 4, "{calls:?}");
    assert!(calls.iter().all(|call| call.hash == outcome.hash(0)));
    assert!(calls.iter().all(|call| call.start <= ms(100)));
    assert!(outcome.elapsed < secs(60), "{:?}", outcome.elapsed);

    // The other peer downloaded the piece from its start.
    let taken_over = outcome.calls_of_for(1, 0);
    assert_eq!(taken_over[0].offset, 0);
    let requested: u64 = taken_over.iter().map(|call| u64::from(call.length)).sum();
    assert_eq!(requested, u64::from(2 * MIB));
}

/// A peer that trickles one byte per answer is stalled after three answers
/// each time it is given the piece, rests 60 seconds after each stall, and is
/// excluded after the third, which stops the scheduler.
#[tokio::test(start_paused = true)]
async fn a_trickling_peer_is_stalled_then_excluded() {
    let _init_guard = zebra_test::init();

    let trickle = Behavior {
        latency: secs(5),
        cap: Some(1),
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects(&[MIB]), vec![trickle]).run().await;

    assert!(
        matches!(outcome.result, Err(SchedulerError::NoPeers)),
        "{:?}",
        outcome.result
    );
    // Stalls at 10 s, 80 s and 150 s, each 60 s of rest after the one before.
    assert_eq!(outcome.elapsed, secs(150));
    let starts: Vec<Duration> = outcome.calls.iter().map(|call| call.start).collect();
    assert_eq!(
        starts,
        [0, 5, 5, 70, 70, 75, 75, 140, 140, 145, 145].map(secs),
        "{:?}",
        outcome.calls
    );
    // Each time, the piece starts again from nothing.
    for start in [70, 140].map(secs) {
        assert!(outcome
            .calls
            .iter()
            .any(|call| call.start == start && call.offset == 0));
    }
}

/// A peer that never answers misses each sub-range's deadline, which counts
/// as a short answer of no bytes: it is stalled after three. Its rate is then
/// below the floor, so after each rest it is stalled again 30 seconds into
/// its next download, and it is excluded after its third stall.
#[tokio::test(start_paused = true)]
async fn a_peer_that_never_answers_is_stalled_then_excluded() {
    let _init_guard = zebra_test::init();

    let hung = Behavior {
        hang: true,
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects(&[MIB]), vec![hung]).run().await;

    assert!(
        matches!(outcome.result, Err(SchedulerError::NoPeers)),
        "{:?}",
        outcome.result
    );
    // A 1 MiB sub-range gets 16 s, and so do the 256 KiB ones after it. Three missed deadlines
    // stall the peer at 32 s; then the rate floor stalls it at 122 s and 212 s.
    assert_eq!(outcome.elapsed, secs(212));
    assert!(outcome.calls.iter().all(|call| call.dropped));
    let times: Vec<(Duration, Duration)> = outcome
        .calls
        .iter()
        .map(|call| (call.start, call.end))
        .collect();
    let expected = [
        (0, 16),
        (16, 32),
        (16, 32),
        (92, 108),
        (92, 108),
        (108, 122),
        (108, 122),
        (182, 198),
        (182, 198),
        (198, 212),
        (198, 212),
    ]
    .map(|(start, end)| (secs(start), secs(end)));
    assert_eq!(times, expected);
}

/// A request the network layer times out, because the peer sent nothing
/// before its deadline, is a short answer of no bytes, not a refusal: the
/// peer is stalled after three, and excluded after its third stall.
#[tokio::test(start_paused = true)]
async fn a_network_timeout_is_a_short_answer() {
    let _init_guard = zebra_test::init();

    let timing_out = Behavior {
        latency: secs(10),
        time_out: true,
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects(&[2 * MIB]), vec![timing_out])
        .run()
        .await;

    assert!(
        matches!(outcome.result, Err(SchedulerError::NoPeers)),
        "{:?}",
        outcome.result
    );
    assert!(outcome.blamed.is_empty());
    // Two timeouts at 10 s and a third at 20 s stall the peer, which rests for 60 s, three times.
    let starts: Vec<Duration> = outcome.calls.iter().map(|call| call.start).collect();
    assert_eq!(
        starts,
        [0, 0, 10, 10, 80, 80, 90, 90, 160, 160, 170, 170].map(secs),
        "{:?}",
        outcome.calls
    );
    assert_eq!(outcome.elapsed, secs(180));
}

/// A peer whose rate stays below the floor for 30 seconds of downloading is
/// stalled, though it finishes each of its small pieces in less.
#[tokio::test(start_paused = true)]
async fn a_slow_peer_is_stalled_across_small_pieces() {
    let _init_guard = zebra_test::init();

    // 100 KiB in 4 s is 25 KiB/s.
    let slow = Behavior {
        latency: secs(4),
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects(&[100 * KIB; 10]), vec![slow])
        .run()
        .await;

    outcome.assert_complete();
    // Below the floor from its first answer at 4 s, so stalled at 34 s, during the ninth piece,
    // which it downloads again after resting until 94 s.
    let starts: Vec<Duration> = outcome.calls.iter().map(|call| call.start).collect();
    assert_eq!(starts, [0, 4, 8, 12, 16, 20, 24, 28, 32, 94, 98].map(secs));
    assert!(outcome.calls[8].dropped);
    assert_eq!(outcome.elapsed, secs(102));
}

/// The time a slow peer spends without a download, here waiting to ask again
/// for an object it did not have, does not count towards the slow window.
#[tokio::test(start_paused = true)]
async fn time_without_a_download_does_not_count_as_slow() {
    let _init_guard = zebra_test::init();

    let objects = objects(&[100 * KIB; 2]);
    let slow = Behavior {
        latency: secs(4),
        missing_until: [(ObjectHash::of(&objects[1]), secs(50))].into(),
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects, vec![slow]).run().await;

    outcome.assert_complete();
    // The second object is not found at 8 s and 42 s, then found at 76 s, with no stall: the peer
    // downloaded below the floor for 16 s in all.
    let starts: Vec<Duration> = outcome.calls.iter().map(|call| call.start).collect();
    assert_eq!(starts, [0, 4, 38, 72].map(secs));
    assert_eq!(outcome.elapsed, secs(76));
}

/// When a peer's rate recovers to the floor, its slow window starts again:
/// the time it was slow before does not count.
#[tokio::test(start_paused = true)]
async fn a_recovered_rate_restarts_the_slow_window() {
    let _init_guard = zebra_test::init();

    // 100 KiB in 4 s is 25 KiB/s, below the floor; in 0.5 s it is 200 KiB/s.
    let peer = Behavior {
        phases: vec![(secs(0), secs(4)), (secs(20), ms(500)), (secs(40), secs(4))],
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects(&[100 * KIB; 52]), vec![peer])
        .run()
        .await;

    outcome.assert_complete();
    // Five pieces in 4 s each, forty in 0.5 s, then seven in 4 s. The rate is below the floor for
    // 18 s of downloading until it recovers at 22.5 s, and below it again from 48 s, so the run
    // ends at 68 s without a stall.
    assert_eq!(outcome.elapsed, secs(68));
}

/// A peer whose rate stays below the 64 KiB/s floor for 30 seconds is
/// stalled, though it answers every sub-range in full and in time: its piece
/// starts again from nothing after its rest, and it is excluded after its
/// third stall.
#[tokio::test(start_paused = true)]
async fn a_slow_peer_is_stalled_below_the_rate_floor() {
    let _init_guard = zebra_test::init();

    let slow = Behavior {
        rate: Some((32 * KIB).into()),
        ..Behavior::default()
    };
    let outcome = Scenario {
        // Small first sub-ranges, which a peer at 32 KiB/s delivers within their deadline.
        config: SchedulerConfig {
            initial_range_bytes: 256 * KIB,
            ..SchedulerConfig::default()
        },
        ..Scenario::new(objects(&[8 * MIB]), vec![slow])
    }
    .run()
    .await;

    assert!(
        matches!(outcome.result, Err(SchedulerError::NoPeers)),
        "{:?}",
        outcome.result
    );
    assert!(outcome.calls.iter().all(
        |call| call.delivered.is_none() || call.delivered == usize::try_from(call.length).ok()
    ));

    // 256 KiB, which is also 8 s of the rate, takes 8 s at 32 KiB/s. The rate is below the floor
    // from the first answers, at 8 s, so the first stall is at 38 s; after each 60 s rest, the
    // rate is still below the floor, so the next download is stalled 30 s after it starts, at
    // 128 s and 218 s.
    assert_eq!(outcome.elapsed, secs(218));
    for rest_end in [98, 188].map(secs) {
        let restart: Vec<&Call> = outcome
            .calls
            .iter()
            .filter(|call| call.start == rest_end)
            .collect();
        assert_eq!(restart.len(), 2, "{:?}", outcome.calls);
        assert_eq!(restart[0].offset, 0, "the partial piece was discarded");
    }
}

/// In the end-game, the piece of a peer below a quarter of the median rate
/// is downloaded again, whole and independently, by an idle fast peer; the
/// first download to verify wins and the slow one is cancelled, with no
/// penalty.
#[tokio::test(start_paused = true)]
async fn the_end_game_duplicates_the_piece_of_a_slow_peer() {
    let _init_guard = zebra_test::init();

    let fast = Behavior {
        rate: Some((100 * MIB).into()),
        ..Behavior::default()
    };
    // Above the rate floor, so it is never stalled.
    let slow = Behavior {
        rate: Some((200 * KIB).into()),
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects(&[4 * MIB; 3]), vec![fast.clone(), fast, slow])
        .run()
        .await;

    outcome.assert_complete();
    assert!(outcome.blamed.is_empty());

    // The slow peer's first answers, at 5.12 s, give its rate; the duplicate takes 40 ms.
    assert!(outcome.elapsed < secs(6), "{:?}", outcome.elapsed);
    let duplicates: Vec<&Call> = [0, 1]
        .into_iter()
        .flat_map(|peer| outcome.calls_of_for(peer, 2))
        .collect();
    assert_eq!(duplicates.len(), 1, "one duplicate: {duplicates:?}");
    assert_eq!(duplicates[0].offset, 0);
    assert_eq!(duplicates[0].length, 4 * MIB);
    assert_eq!(duplicates[0].start, Duration::from_millis(5120));

    // The slow peer was cancelled, not stalled.
    let slow_calls = outcome.calls_of(2);
    assert!(slow_calls.iter().all(|call| call.hash == outcome.hash(2)));
    assert!(slow_calls.iter().any(|call| call.dropped));
}

/// In the end-game, a peer slower than the others, but above a quarter of the
/// median rate, keeps its piece to itself.
#[tokio::test(start_paused = true)]
async fn the_end_game_leaves_a_peer_above_a_quarter_of_the_median() {
    let _init_guard = zebra_test::init();

    let fast = Behavior {
        rate: Some((10 * MIB).into()),
        ..Behavior::default()
    };
    // 40% of the median rate.
    let slower = Behavior {
        rate: Some((4 * MIB).into()),
        ..Behavior::default()
    };
    let outcome = Scenario::new(objects(&[8 * MIB; 3]), vec![fast.clone(), fast, slower])
        .run()
        .await;

    outcome.assert_complete();
    // The fast peers are idle from 0.8 s, and the slower one finishes its piece at 2 s.
    assert!(outcome.calls_of_for(0, 2).is_empty());
    assert!(outcome.calls_of_for(1, 2).is_empty());
    assert!(outcome.calls.iter().all(|call| !call.dropped));
    assert_eq!(outcome.elapsed, secs(2));
}
