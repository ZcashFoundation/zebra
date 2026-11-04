//! The piece download scheduler.
//!
//! One task owns every download: it assigns each piece to one peer, requests
//! it in sub-ranges sized from the peer's measured rate, hashes the bytes as
//! they arrive, stores each verified piece, and reports it on a channel. It
//! detects stalled peers, takes the slowest peers' pieces away in the
//! end-game, backs off from refusals and missing objects, and blames a peer
//! only for a piece it delivered entirely.
//!
//! The scheduler is generic over the peer service, so tests drive it with
//! mock peers, and it holds its state in the task alone: no locks.

use std::{
    collections::{hash_map::Entry, BTreeMap, HashMap, HashSet, VecDeque},
    convert::Infallible,
    fmt,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use futures::{future::BoxFuture, stream::FuturesUnordered, FutureExt, StreamExt};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{
    sync::{
        mpsc::{self, error::TrySendError},
        oneshot, Notify,
    },
    time::Instant,
};
use tower::{Service, ServiceExt};

use zebra_chain::{diagnostic::task::WaitForPanics, state_snapshot::ObjectHash};
use zebra_network as zn;
use zebra_state::state_snapshot::{format::ComponentId, store::ArtifactStore, SnapshotError};

use crate::BoxError;

#[cfg(test)]
mod tests;

/// How much longer than the network layer's request timeout the scheduler
/// waits for a sub-range. The scheduler's clock starts before the request is
/// sent to the peer's service, the network layer's when the request is
/// dispatched on the connection, so the grace covers the wait between them:
/// normally nothing, since the bootstrap clients serve only the scheduler,
/// which keeps fewer requests in flight than the connection has streams for.
const SUB_RANGE_DEADLINE_GRACE: Duration = Duration::from_secs(5);

/// The limits of the scheduler, with their defaults.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchedulerConfig {
    /// The length of the first sub-range requested from a peer, before its
    /// rate is known: 1 MiB.
    pub initial_range_bytes: u32,

    /// The shortest sub-range: 256 KiB.
    pub min_range_bytes: u32,

    /// The longest sub-range: 4 MiB.
    pub max_range_bytes: u32,

    /// How much of a peer's measured rate one sub-range asks for: 8 seconds'
    /// worth, clamped between the shortest and the longest sub-range.
    pub range_duration: Duration,

    /// The most sub-ranges in flight to one peer: 2.
    pub max_in_flight_per_peer: usize,

    /// A sub-range answer shorter than this, before the end of the object, is
    /// a short answer: 64 KiB.
    pub short_answer_bytes: u32,

    /// Short answers in a row before a peer counts as stalled: 3.
    pub max_short_answers: u32,

    /// A peer whose rate stays below this for [`Self::slow_window`] counts
    /// as stalled: 64 KiB/s.
    pub min_rate_bytes_per_sec: u64,

    /// How long a peer may stay below [`Self::min_rate_bytes_per_sec`]: 30
    /// seconds.
    pub slow_window: Duration,

    /// In the end-game, a peer below the median rate divided by this loses its
    /// piece to an independent whole-piece duplicate: 4.
    pub endgame_slow_divisor: u32,

    /// How long a stalled peer rests before it is assigned a piece again: 60
    /// seconds.
    pub stall_cooldown: Duration,

    /// Stalls before a peer is excluded for the rest of the bootstrap: 3.
    pub max_stalls: u32,

    /// The first backoff after a peer refuses a request: 1 second; it doubles
    /// up to [`Self::max_refused_backoff`], with no penalty.
    pub min_refused_backoff: Duration,

    /// The longest backoff after refusals: 30 seconds.
    pub max_refused_backoff: Duration,

    /// How long a peer that answered not-found for a hash is not asked for
    /// that hash again: 30 seconds.
    pub not_found_backoff: Duration,

    /// The most bytes in flight plus downloaded but not yet verified: 256
    /// MiB.
    pub budget_bytes: u64,

    /// The most peers downloaded from at once: 8.
    pub max_peers: usize,

    /// How long the piece phase may go without a piece verifying before the
    /// bootstrap is aborted: 5 minutes.
    pub piece_phase_stall: Duration,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        SchedulerConfig {
            initial_range_bytes: 1 << 20,
            min_range_bytes: 256 << 10,
            max_range_bytes: 4 << 20,
            range_duration: Duration::from_secs(8),
            max_in_flight_per_peer: 2,
            short_answer_bytes: 64 << 10,
            max_short_answers: 3,
            min_rate_bytes_per_sec: 64 << 10,
            slow_window: Duration::from_secs(30),
            endgame_slow_divisor: 4,
            stall_cooldown: Duration::from_secs(60),
            max_stalls: 3,
            min_refused_backoff: Duration::from_secs(1),
            max_refused_backoff: Duration::from_secs(30),
            not_found_backoff: Duration::from_secs(30),
            budget_bytes: 256 << 20,
            max_peers: 8,
            piece_phase_stall: Duration::from_secs(5 * 60),
        }
    }
}

impl SchedulerConfig {
    /// Returns the length of the next sub-range for a peer that delivers one
    /// sub-range at `rate` bytes per second, or has not answered yet: its rate
    /// times [`Self::range_duration`], clamped to the shortest and longest
    /// sub-range.
    fn range_len(&self, rate: Option<u64>) -> u32 {
        let len = match rate {
            None => self.initial_range_bytes,
            Some(rate) => {
                let len = u128::from(rate) * self.range_duration.as_micros() / 1_000_000;
                u32::try_from(len)
                    .unwrap_or(u32::MAX)
                    .max(self.min_range_bytes)
                    .min(self.max_range_bytes)
            }
        };
        // A zero-length request would never make progress.
        len.max(1)
    }

    /// Returns how long the scheduler waits for a sub-range when its peer has
    /// `bytes` to deliver with it, its own and those of the sub-ranges in
    /// flight with it, which share the peer's connection: longer than the
    /// network layer's own request timeout, and as long as a peer at the rate
    /// floor needs to deliver the bytes, if that is longer.
    ///
    /// The network layer's timeout always comes first, and gives back the
    /// bytes received so far as a short answer, as
    /// does the serving node's early FIN before it. This deadline only catches
    /// a service that never resolves; it must not cancel a stream the network
    /// layer is still reading, which would discard every byte received.
    fn sub_range_deadline(&self, bytes: u64) -> Duration {
        let at_floor_micros = bytes.saturating_mul(1_000_000) / self.min_rate_bytes_per_sec.max(1);
        Duration::from_micros(at_floor_micros)
            .max(zn::constants::REQUEST_TIMEOUT.saturating_add(SUB_RANGE_DEADLINE_GRACE))
    }

    /// Returns how long a peer rests after `refusals` refusals in a row:
    /// [`Self::min_refused_backoff`], doubling up to
    /// [`Self::max_refused_backoff`].
    fn refused_backoff(&self, refusals: u32) -> Duration {
        let doublings = refusals.saturating_sub(1).min(31);
        self.min_refused_backoff
            .saturating_mul(1 << doublings)
            .min(self.max_refused_backoff)
    }
}

/// A piece to download, from the manifest.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct PieceDescriptor {
    /// The component the piece belongs to.
    pub component: ComponentId,

    /// The piece's index in its component.
    pub index: u32,

    /// The piece's size in bytes.
    pub size: u32,

    /// The piece's hash.
    pub hash: ObjectHash,
}

/// A piece that hashed to its manifest entry and was stored.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedPiece {
    /// The piece.
    pub descriptor: PieceDescriptor,

    /// The stored piece's path.
    pub path: PathBuf,
}

/// A peer the scheduler blames for a piece it delivered entirely.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerBlame {
    /// The peer.
    pub peer: zn::PeerSocketAddr,

    /// What it delivered.
    pub reason: BlameReason,
}

/// Why a peer is blamed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BlameReason {
    /// The peer delivered every byte of a piece that does not hash to its
    /// manifest entry.
    HashMismatch {
        /// The hash the piece should have had.
        hash: ObjectHash,
    },
}

/// Why the scheduler stopped before every piece was verified.
#[derive(Debug, Error)]
pub enum SchedulerError {
    /// Every peer was excluded.
    #[error("every bootstrap peer was excluded")]
    NoPeers,

    /// No piece verified or was reported for the piece-phase stall deadline.
    #[error("no piece verified for {0:?}")]
    Stalled(Duration),

    /// The receiver of verified pieces was dropped.
    #[error("the verified piece channel was closed")]
    Closed,

    /// Storing a verified piece failed.
    #[error(transparent)]
    Store(#[from] SnapshotError),
}

/// Downloads a manifest's pieces from the bootstrap peers.
pub struct Scheduler<S> {
    config: SchedulerConfig,
    pieces: Vec<PieceDescriptor>,
    peers: Vec<(zn::PeerSocketAddr, S)>,
    store: ArtifactStore,
    verified: mpsc::Sender<VerifiedPiece>,
    blame: mpsc::Sender<PeerBlame>,
}

impl<S> Scheduler<S>
where
    S: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    /// Returns a scheduler that downloads `pieces` from `peers` within
    /// `config`'s limits, stores each verified piece in `store`, reports it on
    /// `verified`, and reports blamed peers on `blame`.
    ///
    /// Only the first [`SchedulerConfig::max_peers`] peers are used. Pieces
    /// with the same hash and size are downloaded once, and each of them is
    /// reported. The receiver of `verified` must be drained while the
    /// scheduler runs: a piece counts as done once it is reported, and the
    /// piece-phase deadline passes once no piece has verified or been reported
    /// for [`SchedulerConfig::piece_phase_stall`]. Each peer is blamed at most
    /// once, so a `blame` channel with room for one report per peer never
    /// makes the scheduler wait.
    pub fn new(
        config: SchedulerConfig,
        pieces: Vec<PieceDescriptor>,
        peers: Vec<(zn::PeerSocketAddr, S)>,
        store: ArtifactStore,
        verified: mpsc::Sender<VerifiedPiece>,
        blame: mpsc::Sender<PeerBlame>,
    ) -> Self {
        Scheduler {
            config,
            pieces,
            peers,
            store,
            verified,
            blame,
        }
    }

    /// Downloads every piece, and returns once all of them are verified and
    /// reported.
    pub async fn run(self) -> Result<(), SchedulerError> {
        let store = self.store.clone();
        self.run_storing_in(store).await
    }

    /// Downloads every piece like [`Self::run`], storing the verified pieces
    /// in `store`.
    async fn run_storing_in<P: PieceStore>(self, store: P) -> Result<(), SchedulerError> {
        Run::new(self, store).run().await
    }
}

/// Where the scheduler stores verified pieces: the artifact store, or a test
/// double.
trait PieceStore: Clone + Send + Sync + 'static {
    /// Stores `object` as `hash`, which is its hash, and returns its path.
    /// Blocks, so it runs on a blocking thread.
    fn put(&self, hash: ObjectHash, object: &[u8]) -> Result<PathBuf, SnapshotError>;
}

impl PieceStore for ArtifactStore {
    fn put(&self, hash: ObjectHash, object: &[u8]) -> Result<PathBuf, SnapshotError> {
        ArtifactStore::put(self, hash, object)
    }
}

/// Identifies one peer's download of one piece. A piece that is reassigned
/// gets a new download, so answers to an abandoned download are recognized
/// and dropped.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
struct DownloadId(u64);

/// A distinct object to download: the manifest's pieces with the same hash
/// and size.
struct Piece {
    /// The object's hash.
    hash: ObjectHash,

    /// The object's size, from the manifest.
    size: u32,

    /// The manifest's pieces this object is, each reported once it is stored.
    descriptors: Vec<PieceDescriptor>,

    /// How far the piece has got.
    status: PieceStatus,

    /// The downloads of the piece in progress.
    downloads: Vec<DownloadId>,

    /// The piece was taken from a slow peer in the end-game, which happens to
    /// a piece at most once.
    reassigned: bool,
}

/// How far a piece has got.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PieceStatus {
    /// Not verified yet: waiting for a peer, or downloading.
    Wanted,

    /// Verified, and being stored and reported.
    Verified,

    /// Stored and reported.
    Done,
}

/// A bootstrap peer, and what the scheduler has learned about it.
struct Peer<S> {
    /// The peer's address, for blame and logs.
    addr: zn::PeerSocketAddr,

    /// The peer's service.
    service: S,

    /// The download the peer is working on: one piece at a time.
    download: Option<DownloadId>,

    /// The rate the peer delivers at, once it has answered.
    rate: Option<Rate>,

    /// Refusals in a row.
    refusals: u32,

    /// Short answers in a row.
    short_answers: u32,

    /// While the peer is downloading with its rate below the rate floor, when
    /// that started.
    slow_since: Option<Instant>,

    /// How long the peer downloaded with its rate below the rate floor in its
    /// earlier downloads, since its rate was last at or above the floor. The
    /// time it spends without a download does not count.
    slow_for: Duration,

    /// How many times the peer has stalled.
    stalls: u32,

    /// The peer is given no work until then.
    resting_until: Option<Instant>,

    /// The peer is not used again.
    excluded: bool,

    /// Objects the peer did not have, and when it may be asked for them again.
    not_found: HashMap<ObjectHash, Instant>,

    /// Pieces the peer gave a different size for, which it is not asked for
    /// again.
    wrong_size: HashSet<usize>,
}

impl<S> Peer<S> {
    /// Returns when the peer will have downloaded below the rate floor for
    /// `slow_window`, if it is downloading below the floor now.
    fn slow_deadline(&self, slow_window: Duration) -> Option<Instant> {
        let since = self.slow_since?;
        Some(after(since, slow_window.saturating_sub(self.slow_for)))
    }
}

/// One peer's download of one piece.
struct Download {
    /// The peer, an index into the run's peers.
    peer: usize,

    /// The piece, an index into the run's pieces.
    piece: usize,

    /// The byte ranges neither received nor requested, as start → end.
    unrequested: BTreeMap<u64, u64>,

    /// The sub-range requests in flight, by offset.
    in_flight: HashMap<u64, InFlight>,

    /// Received bytes after a gap, by offset.
    ahead: BTreeMap<u64, Vec<u8>>,

    /// Received bytes that continue the hashed bytes, in order, waiting for
    /// the hasher.
    queued: Vec<Vec<u8>>,

    /// The end of the bytes received without a gap from the start of the
    /// piece, hashed or queued.
    contiguous: u64,

    /// The hasher and the bytes it has hashed, unless a blocking task has
    /// them.
    assembly: Option<Assembly>,
}

impl Download {
    /// Returns a download of a whole piece of `size` bytes.
    fn new(peer: usize, piece: usize, size: u32) -> Self {
        let mut unrequested = BTreeMap::new();
        if size > 0 {
            unrequested.insert(0, u64::from(size));
        }
        let capacity =
            usize::try_from(size).expect("a u32 size fits in a usize on supported targets");
        Download {
            peer,
            piece,
            unrequested,
            in_flight: HashMap::new(),
            ahead: BTreeMap::new(),
            queued: Vec::new(),
            contiguous: 0,
            assembly: Some(Assembly {
                hasher: Sha256::new(),
                bytes: Vec::with_capacity(capacity),
            }),
        }
    }

    /// Returns the bytes from `start` to `end` to the unrequested ranges,
    /// merged with their neighbours.
    fn give_back(&mut self, mut start: u64, mut end: u64) {
        if start >= end {
            return;
        }
        if let Some(next_end) = self.unrequested.remove(&end) {
            end = next_end;
        }
        if let Some((&before, &before_end)) = self.unrequested.range(..start).next_back() {
            if before_end == start {
                self.unrequested.remove(&before);
                start = before;
            }
        }
        self.unrequested.insert(start, end);
    }

    /// Records the request for `length` bytes at `offset` as in flight from
    /// `now`.
    fn start_request(
        &mut self,
        offset: u64,
        length: u32,
        cancel: oneshot::Sender<Infallible>,
        now: Instant,
    ) {
        self.settle(now);
        self.in_flight.insert(
            offset,
            InFlight {
                length,
                effective: Duration::ZERO,
                since: now,
                _cancel: cancel,
            },
        );
    }

    /// Ends the request at `offset` at `now`, and returns how long it had the
    /// peer's connection to itself, if it was in flight.
    fn end_request(&mut self, offset: u64, now: Instant) -> Option<Duration> {
        self.settle(now);
        self.in_flight
            .remove(&offset)
            .map(|in_flight| in_flight.effective)
    }

    /// Brings every in-flight request's time with the connection to itself up
    /// to `now`. The requests in flight share the peer's connection equally, as
    /// quinn's streams of one priority do, so each gets the time since the
    /// last change divided by their number.
    fn settle(&mut self, now: Instant) {
        let shares = u32::try_from(self.in_flight.len())
            .expect("fewer requests are in flight than fit in a u32")
            .max(1);
        for in_flight in self.in_flight.values_mut() {
            let share = now.saturating_duration_since(in_flight.since) / shares;
            in_flight.effective = in_flight.effective.saturating_add(share);
            in_flight.since = now;
        }
    }

    /// Adds `data`, received at `offset`, to the piece.
    fn accept(&mut self, offset: u64, data: Vec<u8>) {
        if data.is_empty() {
            return;
        }
        if offset != self.contiguous {
            self.ahead.insert(offset, data);
            return;
        }
        self.contiguous = self.contiguous.saturating_add(len_u64(&data));
        self.queued.push(data);
        while let Some(next) = self.ahead.remove(&self.contiguous) {
            self.contiguous = self.contiguous.saturating_add(len_u64(&next));
            self.queued.push(next);
        }
    }
}

/// A sub-range request in flight.
struct InFlight {
    /// The requested length.
    length: u32,

    /// How long the request has had the peer's connection to itself: each
    /// moment it was in flight, divided by the requests in flight then.
    effective: Duration,

    /// When `effective` was last brought up to date.
    since: Instant,

    /// Dropping it cancels the request, so dropping the download cancels them
    /// all.
    _cancel: oneshot::Sender<Infallible>,
}

/// Returns the length of `data` as a `u64`.
fn len_u64(data: &[u8]) -> u64 {
    u64::try_from(data.len()).expect("a usize length fits in a u64 on supported targets")
}

/// A piece's bytes so far, and their running hash.
struct Assembly {
    /// The hash of `bytes`.
    hasher: Sha256,

    /// The piece's first bytes.
    bytes: Vec<u8>,
}

/// Accounts for blocking work even after its join handle is dropped during
/// scheduler shutdown.
struct BlockingTaskGuard {
    active: Arc<AtomicUsize>,
    done: Arc<Notify>,
}

impl BlockingTaskGuard {
    fn new(active: Arc<AtomicUsize>, done: Arc<Notify>) -> Self {
        active.fetch_add(1, Ordering::SeqCst);
        BlockingTaskGuard { active, done }
    }
}

impl Drop for BlockingTaskGuard {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
        self.done.notify_one();
    }
}

/// Something the scheduler waited for.
enum Event {
    /// A sub-range request ended.
    Answered {
        /// The download it is part of.
        download: DownloadId,
        /// The requested offset.
        offset: u64,
        /// The requested length.
        length: u32,
        /// How it ended.
        answer: Answer,
    },

    /// A blocking task hashed a download's queued bytes.
    Hashed {
        /// The download.
        download: DownloadId,
        /// The download's hasher and bytes.
        assembly: Assembly,
    },

    /// A verified piece was written to the store.
    Stored {
        /// The piece.
        piece: usize,
        /// Its path, or why it was not stored.
        path: Result<PathBuf, SnapshotError>,
    },

    /// A stored piece was reported on the verified channel.
    Reported {
        /// The piece.
        piece: usize,
        /// Whether the receiver took it, rather than having been dropped.
        sent: bool,
    },

    /// A blame report that waited for room was sent, or its receiver was
    /// dropped.
    Blamed,
}

/// How a sub-range request ended.
enum Answer {
    /// The peer answered.
    Response(zn::Response),

    /// The peer's service failed before taking the request, as the service
    /// of a closed connection does.
    Unready(BoxError),

    /// The request failed. The network layer reports a peer's `REFUSED`
    /// stream reset like any other failed request stream, so every failure but
    /// a timeout is handled as a refusal.
    Failed(BoxError),

    /// The request took longer than its deadline, or than the network
    /// layer's: either way, the peer delivered nothing in time.
    TimedOut,

    /// The download was dropped, which cancelled the request.
    Cancelled,
}

/// Returns whether `error` is the network layer's request timeout: the peer
/// sent no answer before the connection's request deadline. That is a short
/// answer of no bytes, not a refusal.
fn is_receive_timeout(error: &BoxError) -> bool {
    // A `SharedPeerError` hides its `PeerError`, but formats it as the error's own `Debug`, which
    // is how the peer set tells its errors apart too.
    error
        .downcast_ref::<zn::SharedPeerError>()
        .is_some_and(|error| error.inner_debug() == "ConnectionReceiveTimeout")
}

/// Returns `instant` plus `duration`, or a year after `instant` if that
/// overflows.
fn after(instant: Instant, duration: Duration) -> Instant {
    instant
        .checked_add(duration)
        .unwrap_or_else(|| instant + Duration::from_secs(365 * 24 * 60 * 60))
}

/// A peer's rate: moving averages of the bytes it delivers per answer and of
/// the time each answer had the peer's connection to itself, each the first
/// answer's, then moved a quarter of the way to each new answer's. Their ratio
/// is the rate, which a small answer, such as the last few bytes of a piece,
/// barely moves.
#[derive(Clone, Copy, Debug)]
struct Rate {
    /// The average bytes per answer.
    bytes: u64,

    /// The average microseconds per answer, at least 1.
    micros: u64,
}

impl Rate {
    /// Returns `old` moved by an answer of `bytes` bytes that took `elapsed`.
    fn update(old: Option<Rate>, bytes: u32, elapsed: Duration) -> Rate {
        let bytes = u64::from(bytes);
        let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        let (bytes, micros) = match old {
            None => (bytes, micros),
            Some(old) => (average(old.bytes, bytes), average(old.micros, micros)),
        };
        Rate {
            bytes,
            micros: micros.max(1),
        }
    }

    /// Returns the rate in bytes per second.
    fn bytes_per_sec(self) -> u64 {
        let rate = u128::from(self.bytes) * 1_000_000 / u128::from(self.micros.max(1));
        u64::try_from(rate).unwrap_or(u64::MAX)
    }
}

/// Returns `old` moved a quarter of the way to `new`.
fn average(old: u64, new: u64) -> u64 {
    u64::try_from((u128::from(old) * 3 + u128::from(new)) / 4).unwrap_or(u64::MAX)
}

/// Sends `request` to `service` once it is ready, within `deadline`, unless
/// `cancelled` resolves first: its sender is dropped with the download.
fn request<S>(
    mut service: S,
    download: DownloadId,
    request: zn::Request,
    (offset, length): (u64, u32),
    deadline: Duration,
    cancelled: oneshot::Receiver<Infallible>,
) -> BoxFuture<'static, Event>
where
    S: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + 'static,
    S::Future: Send + 'static,
{
    async move {
        let call = async {
            match service.ready().await {
                Err(error) => Answer::Unready(error),
                Ok(service) => match service.call(request).await {
                    Ok(response) => Answer::Response(response),
                    Err(error) if is_receive_timeout(&error) => Answer::TimedOut,
                    Err(error) => Answer::Failed(error),
                },
            }
        };
        let answer = tokio::select! {
            biased;
            _ = cancelled => Answer::Cancelled,
            answer = tokio::time::timeout(deadline, call) => answer.unwrap_or(Answer::TimedOut),
        };
        Event::Answered {
            download,
            offset,
            length,
            answer,
        }
    }
    .boxed()
}

/// The state of one run of the scheduler, owned by the task that runs it.
struct Run<S, P> {
    /// The scheduler's limits.
    config: SchedulerConfig,

    /// The distinct objects to download.
    pieces: Vec<Piece>,

    /// The wanted pieces no peer is downloading, in the order they are
    /// assigned.
    pending: VecDeque<usize>,

    /// The peers.
    peers: Vec<Peer<S>>,

    /// The downloads in progress.
    downloads: HashMap<DownloadId, Download>,

    /// The identifier of the next download.
    next_download: u64,

    /// The bytes of the pieces being downloaded, and of the verified pieces
    /// not yet stored: each download reserves its whole piece.
    reserved: u64,

    /// The pieces not verified yet.
    unverified: usize,

    /// The pieces not stored and reported yet.
    remaining: usize,

    /// When the last piece verified or was reported, or the run started.
    last_progress: Instant,

    /// Everything the run waits for: requests, hashing, storing, and reports.
    futures: FuturesUnordered<BoxFuture<'static, Event>>,

    /// Blocking hashing and storing work that must finish before shutdown.
    active_blocking: Arc<AtomicUsize>,

    /// Wakes shutdown when a blocking task finishes.
    blocking_done: Arc<Notify>,

    /// Where verified pieces are stored.
    store: P,

    /// Where stored pieces are reported.
    verified: mpsc::Sender<VerifiedPiece>,

    /// Where blamed peers are reported.
    blamed: mpsc::Sender<PeerBlame>,
}

impl<S, P> Run<S, P>
where
    S: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
    P: PieceStore,
{
    /// Returns the state of a run of `scheduler`, storing pieces in `store`.
    fn new(scheduler: Scheduler<S>, store: P) -> Self {
        let Scheduler {
            config,
            pieces: descriptors,
            mut peers,
            store: _,
            verified,
            blame,
        } = scheduler;

        // at most 8 peers.
        peers.truncate(config.max_peers);
        let peers = peers
            .into_iter()
            .map(|(addr, service)| Peer {
                addr,
                service,
                download: None,
                rate: None,
                refusals: 0,
                short_answers: 0,
                slow_since: None,
                slow_for: Duration::ZERO,
                stalls: 0,
                resting_until: None,
                excluded: false,
                not_found: HashMap::new(),
                wrong_size: HashSet::new(),
            })
            .collect();

        let mut pieces: Vec<Piece> = Vec::new();
        let mut by_object: HashMap<(ObjectHash, u32), usize> = HashMap::new();
        for descriptor in descriptors {
            match by_object.entry((descriptor.hash, descriptor.size)) {
                Entry::Occupied(entry) => pieces[*entry.get()].descriptors.push(descriptor),
                Entry::Vacant(entry) => {
                    entry.insert(pieces.len());
                    pieces.push(Piece {
                        hash: descriptor.hash,
                        size: descriptor.size,
                        descriptors: vec![descriptor],
                        status: PieceStatus::Wanted,
                        downloads: Vec::new(),
                        reassigned: false,
                    });
                }
            }
        }

        Run {
            config,
            pending: VecDeque::new(),
            peers,
            downloads: HashMap::new(),
            next_download: 0,
            reserved: 0,
            unverified: pieces.len(),
            remaining: pieces.len(),
            last_progress: Instant::now(),
            futures: FuturesUnordered::new(),
            active_blocking: Arc::new(AtomicUsize::new(0)),
            blocking_done: Arc::new(Notify::new()),
            pieces,
            store,
            verified,
            blamed: blame,
        }
    }

    /// Downloads every piece, then waits for any blocking work left by a
    /// closed receiver or another early return.
    async fn run(mut self) -> Result<(), SchedulerError> {
        let outcome = self.run_inner().await;
        let active_blocking = self.active_blocking.clone();
        let blocking_done = self.blocking_done.clone();
        drop(self);
        Self::wait_for_blocking_tasks(active_blocking, blocking_done).await;
        outcome
    }

    async fn wait_for_blocking_tasks(
        active_blocking: Arc<AtomicUsize>,
        blocking_done: Arc<Notify>,
    ) {
        Self::wait_for_blocking_tasks_with_hook(active_blocking, blocking_done, || {}).await;
    }

    async fn wait_for_blocking_tasks_with_hook<F>(
        active_blocking: Arc<AtomicUsize>,
        blocking_done: Arc<Notify>,
        mut after_active_load: F,
    ) where
        F: FnMut(),
    {
        loop {
            let notified = blocking_done.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if active_blocking.load(Ordering::SeqCst) == 0 {
                return;
            }
            after_active_load();
            notified.await;
        }
    }

    async fn run_inner(&mut self) -> Result<(), SchedulerError> {
        for piece in 0..self.pieces.len() {
            if self.pieces[piece].size == 0 {
                self.verify_empty(piece);
            } else {
                self.pending.push_back(piece);
            }
        }

        loop {
            if self.remaining == 0 {
                return Ok(());
            }
            let now = Instant::now();
            self.maintain(now)?;
            self.assign();
            self.dispatch();

            let wake = self.next_wakeup();
            let event = tokio::select! {
                biased;
                () = self.verified.closed() => return Err(SchedulerError::Closed),
                Some(event) = self.futures.next(), if !self.futures.is_empty() => Some(event),
                () = tokio::time::sleep_until(wake) => None,
            };
            if let Some(event) = event {
                self.handle(event)?;
            }
        }
    }

    /// Stores an empty piece without downloading it, if its hash is the empty
    /// object's. Any other empty piece can never verify, so no peer is asked
    /// for it, and the run ends when the piece-phase deadline passes.
    fn verify_empty(&mut self, piece: usize) {
        let hash = self.pieces[piece].hash;
        if hash == ObjectHash::of(&[]) {
            self.pieces[piece].status = PieceStatus::Verified;
            self.unverified = self.unverified.saturating_sub(1);
            self.store_piece(piece, Vec::new());
        } else {
            tracing::warn!(
                %hash,
                "a state snapshot piece is empty but has the hash of a non-empty object"
            );
        }
    }

    /// Applies the rules that depend on time alone, and returns an error if
    /// the run must stop.
    fn maintain(&mut self, now: Instant) -> Result<(), SchedulerError> {
        // abort the piece phase if no piece verifies for 5 minutes.
        // A report counts too, so a slow receiver does not end a run whose pieces are all verified.
        if now >= after(self.last_progress, self.config.piece_phase_stall) {
            return Err(SchedulerError::Stalled(self.config.piece_phase_stall));
        }

        let floor = self.config.min_rate_bytes_per_sec;
        for peer in &mut self.peers {
            if peer.resting_until.is_some_and(|until| until <= now) {
                peer.resting_until = None;
                // A peer that kept its download through a refusal's rest goes on downloading below
                // the floor, if that is where its rate was.
                if peer.download.is_some()
                    && peer.rate.is_some_and(|rate| rate.bytes_per_sec() < floor)
                {
                    peer.slow_since = Some(now);
                }
            }
            peer.not_found.retain(|_, until| *until > now);
        }

        // A peer whose rate stays below the floor for the slow window is
        // stalled.
        for peer in 0..self.peers.len() {
            let slow_deadline = self.peers[peer].slow_deadline(self.config.slow_window);
            if slow_deadline.is_some_and(|deadline| now >= deadline) {
                self.stall(peer, now, "its rate stayed below the floor");
            }
        }

        // abort if every peer is excluded.
        if self.unverified > 0 && self.peers.iter().all(|peer| peer.excluded) {
            return Err(SchedulerError::NoPeers);
        }
        Ok(())
    }

    /// Returns when the next rule that depends on time alone applies.
    fn next_wakeup(&self) -> Instant {
        let mut wake = after(self.last_progress, self.config.piece_phase_stall);
        for peer in &self.peers {
            if peer.excluded {
                continue;
            }
            if let Some(until) = peer.resting_until {
                wake = wake.min(until);
            } else if peer.download.is_none() {
                if let Some(until) = peer.not_found.values().min() {
                    wake = wake.min(*until);
                }
            }
            if let Some(deadline) = peer.slow_deadline(self.config.slow_window) {
                wake = wake.min(deadline);
            }
        }
        wake
    }

    /// Returns whether `peer` can be given a piece now.
    fn is_idle(&self, peer: usize) -> bool {
        let peer = &self.peers[peer];
        !peer.excluded && peer.resting_until.is_none() && peer.download.is_none()
    }

    /// Returns whether `peer` may be asked for `piece`.
    fn can_serve(&self, peer: usize, piece: usize) -> bool {
        let peer = &self.peers[peer];
        !peer.wrong_size.contains(&piece) && !peer.not_found.contains_key(&self.pieces[piece].hash)
    }

    /// Returns whether a download of `size` bytes fits in the budget. One
    /// download always fits, so a budget smaller than a piece still makes
    /// progress.
    fn budget_allows(&self, size: u32) -> bool {
        self.reserved == 0
            || self.reserved.saturating_add(u64::from(size)) <= self.config.budget_bytes
    }

    /// Gives each idle peer the first pending piece it can serve that fits in
    /// the budget. Each piece goes to one peer.
    fn assign(&mut self) {
        for peer in 0..self.peers.len() {
            if self.pending.is_empty() {
                break;
            }
            if !self.is_idle(peer) {
                continue;
            }
            let Some(position) = self.pending.iter().position(|&piece| {
                self.can_serve(peer, piece) && self.budget_allows(self.pieces[piece].size)
            }) else {
                continue;
            };
            if let Some(piece) = self.pending.remove(position) {
                self.start_download(peer, piece);
            }
        }

        if self.pending.is_empty() {
            self.reassign_slow_pieces();
        }
    }

    /// In the end-game, when no piece is waiting for a peer, takes the piece of
    /// each peer below a quarter of the median rate away from it, with no
    /// penalty, and gives it whole to the fastest idle peer that is not below
    /// that itself, as an independent end-game duplicate. The slow peer's partial data is
    /// discarded, so each download is by one peer and blame stays exact, and a
    /// piece changes hands this way at most once.
    fn reassign_slow_pieces(&mut self) {
        let Some(median) = self.median_rate() else {
            return;
        };
        let threshold = median / u64::from(self.config.endgame_slow_divisor.max(1));

        let mut slow: Vec<(u64, usize, DownloadId)> = self
            .peers
            .iter()
            .filter_map(|peer| {
                let rate = peer.rate?.bytes_per_sec();
                let id = peer.download?;
                let piece = self.downloads.get(&id)?.piece;
                (rate < threshold && !self.pieces[piece].reassigned).then_some((rate, piece, id))
            })
            .collect();
        slow.sort_unstable_by_key(|&(rate, piece, _)| (rate, piece));

        for (_, piece, id) in slow {
            let fastest = (0..self.peers.len())
                .filter(|&peer| self.is_idle(peer) && self.can_serve(peer, piece))
                .filter_map(|peer| Some((self.peers[peer].rate?.bytes_per_sec(), peer)))
                .filter(|&(rate, _)| rate >= threshold)
                .max();
            let Some((_, peer)) = fastest else {
                continue;
            };
            let Some(slow_peer) = self.downloads.get(&id).map(|download| download.peer) else {
                continue;
            };
            tracing::debug!(
                slow_peer = %self.peers[slow_peer].addr,
                peer = %self.peers[peer].addr,
                hash = %self.pieces[piece].hash,
                "taking the piece of a slow state snapshot peer away in the end-game",
            );
            // The slow peer loses the piece, and the piece's reservation moves to the new download,
            // so the budget always allows it.
            self.release(id);
            self.pending.retain(|&pending| pending != piece);
            self.pieces[piece].reassigned = true;
            self.start_download(peer, piece);
        }
    }

    /// Returns the median rate of the peers in use that have answered.
    fn median_rate(&self) -> Option<u64> {
        let mut rates: Vec<u64> = self
            .peers
            .iter()
            .filter(|peer| !peer.excluded)
            .filter_map(|peer| peer.rate.map(Rate::bytes_per_sec))
            .collect();
        rates.sort_unstable();
        let middle = rates.len() / 2;
        match rates.len() {
            0 => None,
            len if len % 2 == 1 => Some(rates[middle]),
            _ => {
                let sum = u128::from(rates[middle - 1]) + u128::from(rates[middle]);
                Some(u64::try_from(sum / 2).unwrap_or(u64::MAX))
            }
        }
    }

    /// Starts `peer`'s download of the whole of `piece`.
    fn start_download(&mut self, peer: usize, piece: usize) {
        let id = DownloadId(self.next_download);
        self.next_download += 1;

        let size = self.pieces[piece].size;
        self.downloads.insert(id, Download::new(peer, piece, size));
        self.pieces[piece].downloads.push(id);
        self.reserved = self.reserved.saturating_add(u64::from(size));

        // A peer already below the rate floor is measured from the start of its download.
        let floor = self.config.min_rate_bytes_per_sec;
        let state = &mut self.peers[peer];
        state.download = Some(id);
        if state.rate.is_some_and(|rate| rate.bytes_per_sec() < floor) {
            state.slow_since = Some(Instant::now());
        }

        tracing::debug!(
            peer = %self.peers[peer].addr,
            hash = %self.pieces[piece].hash,
            size,
            "downloading a state snapshot piece",
        );
    }

    /// Requests every download's next sub-ranges, and hashes the bytes they
    /// have queued. A resting peer keeps its download, but is not asked for
    /// more until its rest ends.
    fn dispatch(&mut self) {
        for peer in 0..self.peers.len() {
            if let Some(id) = self.peers[peer].download {
                if self.peers[peer].resting_until.is_none() {
                    self.request_sub_ranges(peer, id);
                }
                self.hash_queued(id);
            }
        }
    }

    /// Requests sub-ranges of download `id` from `peer`, up to the in-flight
    /// limit.
    fn request_sub_ranges(&mut self, peer: usize, id: DownloadId) {
        let range_len = self
            .config
            .range_len(self.peers[peer].rate.map(Rate::bytes_per_sec));
        let Some(download) = self.downloads.get_mut(&id) else {
            return;
        };
        let hash = self.pieces[download.piece].hash;
        let now = Instant::now();

        while download.in_flight.len() < self.config.max_in_flight_per_peer {
            let Some((start, end)) = download.unrequested.pop_first() else {
                break;
            };
            // Never past the end of the range, so never past the piece's size.
            let length = range_len.min(u32::try_from(end - start).unwrap_or(u32::MAX));
            let request_end = start + u64::from(length);
            if request_end < end {
                download.unrequested.insert(request_end, end);
            }

            // The sub-ranges in flight share the peer's connection, so this one is delivered with,
            // or after, those ahead of it.
            let ahead: u64 = download
                .in_flight
                .values()
                .map(|in_flight| u64::from(in_flight.length))
                .sum();
            let deadline = self
                .config
                .sub_range_deadline(ahead.saturating_add(u64::from(length)));

            let (cancel, cancelled) = oneshot::channel();
            download.start_request(start, length, cancel, now);
            self.futures.push(request(
                self.peers[peer].service.clone(),
                id,
                zn::Request::Object {
                    hash,
                    offset: start,
                    length,
                },
                (start, length),
                deadline,
                cancelled,
            ));
        }
    }

    /// Hashes the bytes download `id` has queued on a blocking thread, unless
    /// one already has its hasher. Each piece is hashed incrementally as it arrives.
    fn hash_queued(&mut self, id: DownloadId) {
        let Some(download) = self.downloads.get_mut(&id) else {
            return;
        };
        if download.queued.is_empty() {
            return;
        }
        let Some(mut assembly) = download.assembly.take() else {
            return;
        };
        let chunks = std::mem::take(&mut download.queued);
        let guard =
            BlockingTaskGuard::new(self.active_blocking.clone(), self.blocking_done.clone());

        self.futures.push(
            tokio::task::spawn_blocking(move || {
                let _guard = guard;
                for chunk in chunks {
                    assembly.hasher.update(&chunk);
                    assembly.bytes.extend_from_slice(&chunk);
                }
                assembly
            })
            .wait_for_panics()
            .map(move |assembly| Event::Hashed {
                download: id,
                assembly,
            })
            .boxed(),
        );
    }

    /// Handles something the run waited for.
    fn handle(&mut self, event: Event) -> Result<(), SchedulerError> {
        match event {
            Event::Answered {
                download,
                offset,
                length,
                answer,
            } => self.answered(download, (offset, length), answer),
            Event::Hashed { download, assembly } => {
                if let Some(state) = self.downloads.get_mut(&download) {
                    state.assembly = Some(assembly);
                    self.try_finish(download);
                }
            }
            Event::Stored { piece, path } => self.stored(piece, path?),
            Event::Reported { piece, sent } => {
                if !sent {
                    return Err(SchedulerError::Closed);
                }
                self.pieces[piece].status = PieceStatus::Done;
                self.remaining = self.remaining.saturating_sub(1);
                self.last_progress = Instant::now();
            }
            Event::Blamed => {}
        }
        Ok(())
    }

    /// Handles the answer to the sub-range of download `id` at `offset`.
    fn answered(&mut self, id: DownloadId, (offset, length): (u64, u32), answer: Answer) {
        // An abandoned download's answers are dropped.
        let Some(download) = self.downloads.get_mut(&id) else {
            return;
        };
        let now = Instant::now();
        // So is an answer to a request the download no longer has in flight.
        let Some(elapsed) = download.end_request(offset, now) else {
            return;
        };
        let peer = download.peer;
        let piece = download.piece;

        match answer {
            Answer::Cancelled => {}
            Answer::Unready(error) => {
                tracing::info!(
                    peer = %self.peers[peer].addr,
                    %error,
                    "excluding a state snapshot peer whose connection failed",
                );
                self.exclude(peer);
            }
            Answer::Failed(error) => self.refused(peer, (offset, length), now, &error),
            Answer::TimedOut => {
                download.give_back(offset, offset + u64::from(length));
                // Nothing arrived in time, so the sub-range is a short answer of no bytes.
                self.measure(peer, 0, length, elapsed, now);
            }
            Answer::Response(zn::Response::ObjectNotFound) => self.not_found(peer, piece, now),
            Answer::Response(zn::Response::Object { size, data }) => {
                self.delivered(id, (offset, length), elapsed, size, data)
            }
            Answer::Response(response) => self.refused(
                peer,
                (offset, length),
                now,
                &format!("unexpected response {response}"),
            ),
        }
    }

    /// Handles `data` delivered for the sub-range of download `id` at
    /// `offset`, from an object the peer says has `size` bytes, by a request
    /// that had the peer's connection to itself for `elapsed`.
    fn delivered(
        &mut self,
        id: DownloadId,
        (offset, length): (u64, u32),
        elapsed: Duration,
        size: u64,
        data: Vec<u8>,
    ) {
        let Some(download) = self.downloads.get(&id) else {
            return;
        };
        let (peer, piece) = (download.peer, download.piece);

        // a size that differs from the manifest cancels that peer's request, with no
        // penalty, since only the hash check settles which size is right (draft, "get-object").
        if size != u64::from(self.pieces[piece].size) {
            tracing::info!(
                peer = %self.peers[peer].addr,
                hash = %self.pieces[piece].hash,
                size,
                expected = self.pieces[piece].size,
                "a state snapshot peer gave another size for a piece; asking other peers for it",
            );
            self.peers[peer].wrong_size.insert(piece);
            self.release(id);
            return;
        }

        // The network layer closes a connection that delivers more than was asked for (draft,
        // "get-object": `FLOOD`), so only a service that breaks its contract gets here.
        let delivered = match u32::try_from(data.len()) {
            Ok(delivered) if delivered <= length => delivered,
            _ => {
                tracing::info!(
                    peer = %self.peers[peer].addr,
                    delivered = data.len(),
                    length,
                    "excluding a state snapshot peer that delivered more than was asked for",
                );
                self.exclude(peer);
                return;
            }
        };

        self.peers[peer].refusals = 0;
        if !self.measure(peer, delivered, length, elapsed, Instant::now()) {
            // The peer stalled, so its partial piece was discarded.
            return;
        }

        let Some(download) = self.downloads.get_mut(&id) else {
            return;
        };
        let request_end = offset + u64::from(length);
        download.give_back(offset + u64::from(delivered), request_end);
        download.accept(offset, data);
        self.try_finish(id);
    }

    /// Verifies download `id`'s piece once all its bytes are hashed.
    fn try_finish(&mut self, id: DownloadId) {
        let Some(download) = self.downloads.get(&id) else {
            return;
        };
        let size = u64::from(self.pieces[download.piece].size);
        if download.contiguous != size || !download.queued.is_empty() || download.assembly.is_none()
        {
            return;
        }

        let piece = download.piece;
        let peer = download.peer;
        let hash = self.pieces[piece].hash;
        let delivered = self
            .downloads
            .get_mut(&id)
            .and_then(|download| download.assembly.take())
            .map(|Assembly { hasher, bytes }| (ObjectHash(hasher.finalize().into()), bytes));
        let Some((delivered_hash, bytes)) = delivered else {
            return;
        };

        if delivered_hash == hash {
            tracing::debug!(peer = %self.peers[peer].addr, %hash, "verified a state snapshot piece");
            self.pieces[piece].status = PieceStatus::Verified;
            self.unverified = self.unverified.saturating_sub(1);
            self.last_progress = Instant::now();
            // The piece's reservation is kept until the piece is stored.
            self.detach(id);
            for other in std::mem::take(&mut self.pieces[piece].downloads) {
                self.release(other);
            }
            self.store_piece(piece, bytes);
        } else {
            // the peer delivered every byte of the piece, so the
            // mismatch is its fault (draft, "get-object").
            self.release(id);
            self.blame(peer, piece);
        }
    }

    /// Removes download `id` from its peer and piece, and returns its piece to
    /// the pending pieces if it is still wanted and no other peer is
    /// downloading it. Its reservation is not released.
    fn detach(&mut self, id: DownloadId) -> Option<Download> {
        let download = self.downloads.remove(&id)?;
        let peer = &mut self.peers[download.peer];
        if peer.download == Some(id) {
            peer.download = None;
            // Only the time a peer spends downloading counts towards the slow window.
            if let Some(since) = peer.slow_since.take() {
                peer.slow_for = peer.slow_for.saturating_add(since.elapsed());
            }
        }
        let piece = &mut self.pieces[download.piece];
        piece.downloads.retain(|other| *other != id);
        if piece.status == PieceStatus::Wanted && piece.downloads.is_empty() {
            self.pending.push_front(download.piece);
        }
        Some(download)
    }

    /// Abandons download `id`: its requests are cancelled, its bytes are
    /// discarded, and its reservation is released.
    fn release(&mut self, id: DownloadId) {
        if let Some(download) = self.detach(id) {
            let size = u64::from(self.pieces[download.piece].size);
            self.reserved = self.reserved.saturating_sub(size);
        }
    }

    /// Abandons `peer`'s download, if it has one.
    fn release_peer(&mut self, peer: usize) {
        if let Some(id) = self.peers[peer].download {
            self.release(id);
        }
    }

    /// Stops using `peer` for the rest of the run.
    fn exclude(&mut self, peer: usize) {
        self.release_peer(peer);
        self.peers[peer].excluded = true;
    }

    /// Updates `peer`'s rate with an answer of `delivered` bytes, at `now`, to
    /// a sub-range of `length` bytes whose request had the peer's connection to
    /// itself for `elapsed`, and stalls the peer after too many short answers
    /// in a row. Returns whether the peer is still
    /// downloading.
    fn measure(
        &mut self,
        peer: usize,
        delivered: u32,
        length: u32,
        elapsed: Duration,
        now: Instant,
    ) -> bool {
        let config = &self.config;
        let state = &mut self.peers[peer];

        // The rate is the peer's, not one sub-range's: its sub-ranges in
        // flight share its connection, so each is measured over the time it had the connection
        // to itself.
        let rate = Rate::update(state.rate, delivered, elapsed);
        state.rate = Some(rate);

        // Sub-ranges never go past the end of the piece, so a short answer is always one that
        // ends before the end of the piece.
        if delivered < config.short_answer_bytes.min(length) {
            state.short_answers = state.short_answers.saturating_add(1);
        } else {
            state.short_answers = 0;
        }

        if rate.bytes_per_sec() < config.min_rate_bytes_per_sec {
            state.slow_since.get_or_insert(now);
        } else {
            state.slow_since = None;
            state.slow_for = Duration::ZERO;
        }

        if state.short_answers >= config.max_short_answers {
            self.stall(peer, now, "it gave too many short answers in a row");
            return false;
        }
        true
    }

    /// Takes `peer`'s piece away, to be downloaded whole by another peer with
    /// the partial data discarded, and rests the peer for the stall cooldown,
    /// or excludes it once it has stalled too often.
    fn stall(&mut self, peer: usize, now: Instant, reason: &str) {
        self.release_peer(peer);
        let state = &mut self.peers[peer];
        state.stalls = state.stalls.saturating_add(1);
        state.short_answers = 0;
        state.slow_since = None;
        state.slow_for = Duration::ZERO;
        if state.stalls >= self.config.max_stalls {
            state.excluded = true;
            tracing::info!(
                peer = %state.addr,
                reason,
                stalls = state.stalls,
                "excluding a stalled state snapshot peer",
            );
        } else {
            state.resting_until = Some(after(now, self.config.stall_cooldown));
            tracing::info!(
                peer = %state.addr,
                reason,
                stalls = state.stalls,
                "a state snapshot peer stalled; resting it and downloading its piece elsewhere",
            );
        }
    }

    /// Rests `peer` after a refused or failed request for the sub-range of
    /// `length` bytes at `offset`, for a backoff that doubles with each refusal
    /// in a row, with no penalty: the peer keeps its piece and
    /// its other request in flight, its slow window pauses, and it is asked for
    /// the refused sub-range again after the rest, since `REFUSED` is how a
    /// server that has enough requests already says it is busy. Once the
    /// backoff reaches its longest, the refusals persist, and the piece goes to
    /// another peer.
    fn refused(
        &mut self,
        peer: usize,
        (offset, length): (u64, u32),
        now: Instant,
        reason: &dyn fmt::Display,
    ) {
        let state = &mut self.peers[peer];
        state.refusals = state.refusals.saturating_add(1);
        let backoff = self.config.refused_backoff(state.refusals);
        state.resting_until = Some(after(now, backoff));
        if let Some(since) = state.slow_since.take() {
            state.slow_for = state
                .slow_for
                .saturating_add(now.saturating_duration_since(since));
        }
        tracing::debug!(
            peer = %state.addr,
            %reason,
            ?backoff,
            "a state snapshot peer refused a request; backing off",
        );

        let download = state.download;
        if backoff >= self.config.max_refused_backoff {
            self.release_peer(peer);
        } else if let Some(download) = download.and_then(|id| self.downloads.get_mut(&id)) {
            download.give_back(offset, offset + u64::from(length));
        }
    }

    /// Handles `peer` not having `piece`: it is not asked for the piece's
    /// object again until the not-found backoff has passed.
    fn not_found(&mut self, peer: usize, piece: usize, now: Instant) {
        self.release_peer(peer);
        let hash = self.pieces[piece].hash;
        let state = &mut self.peers[peer];
        state.refusals = 0;
        state
            .not_found
            .insert(hash, after(now, self.config.not_found_backoff));
        tracing::debug!(peer = %state.addr, %hash, "a state snapshot peer does not have a piece");
    }

    /// Blames and excludes `peer`, which delivered all of `piece` with the
    /// wrong hash.
    fn blame(&mut self, peer: usize, piece: usize) {
        let hash = self.pieces[piece].hash;
        let addr = self.peers[peer].addr;
        tracing::warn!(
            peer = %addr,
            %hash,
            "a state snapshot peer delivered a piece with the wrong hash; excluding it",
        );
        self.exclude(peer);

        let report = PeerBlame {
            peer: addr,
            reason: BlameReason::HashMismatch { hash },
        };
        match self.blamed.try_send(report) {
            Ok(()) => {}
            Err(TrySendError::Full(report)) => {
                let blamed = self.blamed.clone();
                self.futures.push(
                    async move {
                        let _ = blamed.send(report).await;
                        Event::Blamed
                    }
                    .boxed(),
                );
            }
            Err(TrySendError::Closed(_)) => {
                tracing::debug!(peer = %addr, "the blame receiver was dropped");
            }
        }
    }

    /// Writes verified `piece`, made of `bytes`, to the store on a blocking
    /// thread.
    fn store_piece(&mut self, piece: usize, bytes: Vec<u8>) {
        let store = self.store.clone();
        let hash = self.pieces[piece].hash;
        let guard =
            BlockingTaskGuard::new(self.active_blocking.clone(), self.blocking_done.clone());
        self.futures.push(
            tokio::task::spawn_blocking(move || {
                let _guard = guard;
                store.put(hash, &bytes)
            })
            .wait_for_panics()
            .map(move |path| Event::Stored { piece, path })
            .boxed(),
        );
    }

    /// Releases stored `piece`'s reservation, and reports each of its manifest
    /// pieces as stored at `path`.
    fn stored(&mut self, piece: usize, path: PathBuf) {
        let size = u64::from(self.pieces[piece].size);
        self.reserved = self.reserved.saturating_sub(size);

        let reports: Vec<VerifiedPiece> = self.pieces[piece]
            .descriptors
            .iter()
            .map(|descriptor| VerifiedPiece {
                descriptor: descriptor.clone(),
                path: path.clone(),
            })
            .collect();
        let verified = self.verified.clone();
        self.futures.push(
            async move {
                for report in reports {
                    if verified.send(report).await.is_err() {
                        return Event::Reported { piece, sent: false };
                    }
                }
                Event::Reported { piece, sent: true }
            }
            .boxed(),
        );
    }
}
