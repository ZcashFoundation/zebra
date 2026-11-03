//! The piece download scheduler (design, §6.1).
//!
//! One task owns every download: it assigns each piece to one peer, requests
//! it in sub-ranges sized from the peer's measured rate, hashes the bytes as
//! they arrive, stores each verified piece, and reports it on a channel. It
//! detects stalled peers, duplicates the slowest pieces in the end-game,
//! backs off from refusals and missing objects, and blames a peer only for a
//! piece it delivered entirely.
//!
//! The scheduler is generic over the peer service, so tests drive it with
//! mock peers, and it holds its state in the task alone: no locks.

use std::{
    collections::{hash_map::Entry, BTreeMap, HashMap, HashSet, VecDeque},
    convert::Infallible,
    fmt,
    path::PathBuf,
    time::Duration,
};

use futures::{future::BoxFuture, stream::FuturesUnordered, FutureExt, StreamExt};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{
    sync::{
        mpsc::{self, error::TrySendError},
        oneshot,
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

/// The limits of the scheduler (design, §6.1), with the design's values as
/// the default.
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
    /// sub-range (design, §6.1 "Sub-ranges").
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

    /// Returns how long a sub-range of `length` bytes may take: as long as a
    /// peer at the rate floor needs to deliver it, and at least twice the
    /// duration sub-ranges are sized for.
    ///
    /// With the defaults, a first 1 MiB sub-range gets 16 seconds, inside the
    /// network layer's own 20-second request timeout, so a peer that never
    /// answers is caught here rather than surfacing as a failed request.
    fn sub_range_deadline(&self, length: u32) -> Duration {
        let at_floor_micros =
            u64::from(length).saturating_mul(1_000_000) / self.min_rate_bytes_per_sec.max(1);
        Duration::from_micros(at_floor_micros).max(self.range_duration.saturating_mul(2))
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

    /// No piece verified for the piece-phase stall deadline.
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
    /// scheduler runs: a piece counts as done once it is reported. Each peer
    /// is blamed at most once, so a `blame` channel with room for one report
    /// per peer never makes the scheduler wait.
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

    /// The rate the peer delivers one sub-range at, once it has answered.
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

    /// The sub-range requests in flight, by offset. Dropping a sender cancels
    /// its request, so dropping the download cancels them all.
    in_flight: HashMap<u64, oneshot::Sender<Infallible>>,

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
        /// How long the request took.
        elapsed: Duration,
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
    /// stream reset like any other failed request stream, so every failure is
    /// handled as a refusal (design, §6.1).
    Failed(BoxError),

    /// The request took longer than its deadline.
    TimedOut,

    /// The download was dropped, which cancelled the request.
    Cancelled,
}

/// Returns `instant` plus `duration`, or a year after `instant` if that
/// overflows.
fn after(instant: Instant, duration: Duration) -> Instant {
    instant
        .checked_add(duration)
        .unwrap_or_else(|| instant + Duration::from_secs(365 * 24 * 60 * 60))
}

/// A peer's rate: moving averages of the bytes it delivers per sub-range and
/// of the time a sub-range takes, each the first answer's, then moved a
/// quarter of the way to each new answer's. Their ratio is the rate, which a
/// small answer, such as the last few bytes of a piece, barely moves.
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
    let started = Instant::now();
    async move {
        let call = async {
            match service.ready().await {
                Err(error) => Answer::Unready(error),
                Ok(service) => match service.call(request).await {
                    Ok(response) => Answer::Response(response),
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
            elapsed: started.elapsed(),
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

    /// When the last piece was reported, or the run started.
    last_progress: Instant,

    /// Everything the run waits for: requests, hashing, storing, and reports.
    futures: FuturesUnordered<BoxFuture<'static, Event>>,

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

        // Design, §6.1: at most 8 peers.
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
            pieces,
            store,
            verified,
            blamed: blame,
        }
    }

    /// Downloads every piece.
    async fn run(mut self) -> Result<(), SchedulerError> {
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
        // Design, §6 "Failure handling": abort the piece phase if no piece verifies for 5 minutes.
        if now >= after(self.last_progress, self.config.piece_phase_stall) {
            return Err(SchedulerError::Stalled(self.config.piece_phase_stall));
        }

        for peer in &mut self.peers {
            if peer.resting_until.is_some_and(|until| until <= now) {
                peer.resting_until = None;
            }
            peer.not_found.retain(|_, until| *until > now);
        }

        // Design, §6.1 "Stalls": a peer whose rate stays below the floor for the slow window is
        // stalled.
        for peer in 0..self.peers.len() {
            let slow_deadline = self.peers[peer].slow_deadline(self.config.slow_window);
            if slow_deadline.is_some_and(|deadline| now >= deadline) {
                self.stall(peer, now, "its rate stayed below the floor");
            }
        }

        // Design, §6 "Failure handling": abort if every peer is excluded.
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

    /// Gives each idle peer the next pending piece it can serve, while the
    /// budget allows (design, §6.1: each piece goes to one peer).
    fn assign(&mut self) {
        for peer in 0..self.peers.len() {
            if self.pending.is_empty() {
                break;
            }
            if !self.is_idle(peer) {
                continue;
            }
            let Some(position) = self
                .pending
                .iter()
                .position(|&piece| self.can_serve(peer, piece))
            else {
                continue;
            };
            if !self.budget_allows(self.pieces[self.pending[position]].size) {
                break;
            }
            if let Some(piece) = self.pending.remove(position) {
                self.start_download(peer, piece);
            }
        }

        if self.pending.is_empty() {
            self.duplicate_slow_pieces();
        }
    }

    /// In the end-game, when no piece is waiting for a peer, gives the piece
    /// of each peer below a quarter of the median rate to the fastest idle
    /// peer as well, as an independent whole-piece download: the first to
    /// verify wins, and the other is cancelled (design, §6.1 "Stalls"; sync
    /// draft, "End-game duplication"). Each piece gets at most one duplicate,
    /// and each download is by one peer, so blame stays exact.
    fn duplicate_slow_pieces(&mut self) {
        let Some(median) = self.median_rate() else {
            return;
        };
        let threshold = median / u64::from(self.config.endgame_slow_divisor.max(1));

        let mut slow: Vec<(u64, usize)> = self
            .peers
            .iter()
            .filter_map(|peer| {
                let rate = peer.rate?.bytes_per_sec();
                let piece = self.downloads.get(&peer.download?)?.piece;
                (rate < threshold && self.pieces[piece].downloads.len() == 1)
                    .then_some((rate, piece))
            })
            .collect();
        slow.sort_unstable();

        for (_, piece) in slow {
            let fastest = (0..self.peers.len())
                .filter(|&peer| self.is_idle(peer) && self.can_serve(peer, piece))
                .max_by_key(|&peer| self.peers[peer].rate.map(Rate::bytes_per_sec));
            let Some(peer) = fastest else {
                continue;
            };
            if !self.budget_allows(self.pieces[piece].size) {
                break;
            }
            tracing::debug!(
                peer = %self.peers[peer].addr,
                hash = %self.pieces[piece].hash,
                "duplicating the piece of a slow state snapshot peer",
            );
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
    /// have queued.
    fn dispatch(&mut self) {
        for peer in 0..self.peers.len() {
            if let Some(id) = self.peers[peer].download {
                self.request_sub_ranges(peer, id);
                self.hash_queued(id);
            }
        }
    }

    /// Requests sub-ranges of download `id` from `peer`, up to the in-flight
    /// limit (design, §6.1 "Sub-ranges").
    fn request_sub_ranges(&mut self, peer: usize, id: DownloadId) {
        let range_len = self
            .config
            .range_len(self.peers[peer].rate.map(Rate::bytes_per_sec));
        let Some(download) = self.downloads.get_mut(&id) else {
            return;
        };
        let hash = self.pieces[download.piece].hash;

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

            let (cancel, cancelled) = oneshot::channel();
            download.in_flight.insert(start, cancel);
            self.futures.push(request(
                self.peers[peer].service.clone(),
                id,
                zn::Request::Object {
                    hash,
                    offset: start,
                    length,
                },
                (start, length),
                self.config.sub_range_deadline(length),
                cancelled,
            ));
        }
    }

    /// Hashes the bytes download `id` has queued on a blocking thread, unless
    /// one already has its hasher (design, §6.1: each piece is hashed
    /// incrementally as it arrives).
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

        self.futures.push(
            tokio::task::spawn_blocking(move || {
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
                elapsed,
                answer,
            } => self.answered(download, (offset, length), elapsed, answer),
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
    fn answered(
        &mut self,
        id: DownloadId,
        (offset, length): (u64, u32),
        elapsed: Duration,
        answer: Answer,
    ) {
        // An abandoned download's answers are dropped.
        let Some(download) = self.downloads.get_mut(&id) else {
            return;
        };
        download.in_flight.remove(&offset);
        let peer = download.peer;
        let piece = download.piece;
        let now = Instant::now();

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
            Answer::Failed(error) => self.refused(peer, now, &error),
            Answer::TimedOut => {
                download.give_back(offset, offset + u64::from(length));
                // Nothing arrived in time, so the sub-range is a short answer of no bytes.
                self.measure(peer, 0, length, elapsed, now);
            }
            Answer::Response(zn::Response::ObjectNotFound) => self.not_found(peer, piece, now),
            Answer::Response(zn::Response::Object { size, data }) => {
                self.delivered(id, (offset, length), elapsed, size, data)
            }
            Answer::Response(response) => {
                self.refused(peer, now, &format!("unexpected response {response}"))
            }
        }
    }

    /// Handles `data` delivered for the sub-range of download `id` at
    /// `offset`, from an object the peer says has `size` bytes.
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

        // Design, §6.1: a size that differs from the manifest cancels that peer's request, with no
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
            // The piece's reservation is kept until the piece is stored.
            self.detach(id);
            for other in std::mem::take(&mut self.pieces[piece].downloads) {
                self.release(other);
            }
            self.store_piece(piece, bytes);
        } else {
            // Design, §6 "Failure handling": the peer delivered every byte of the piece, so the
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

    /// Updates `peer`'s rate with an answer of `delivered` bytes to a
    /// sub-range of `length` bytes, which took `elapsed`, and stalls the peer
    /// after too many short answers in a row (design, §6.1 "Stalls"). Returns
    /// whether the peer is still downloading.
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
    /// or excludes it once it has stalled too often (design, §6.1 "Stalls").
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

    /// Rests `peer` after a refused or failed request, for a backoff that
    /// doubles with each refusal in a row, with no penalty (design, §6.1).
    fn refused(&mut self, peer: usize, now: Instant, reason: &dyn fmt::Display) {
        self.release_peer(peer);
        let state = &mut self.peers[peer];
        state.refusals = state.refusals.saturating_add(1);
        let backoff = self.config.refused_backoff(state.refusals);
        state.resting_until = Some(after(now, backoff));
        tracing::debug!(
            peer = %state.addr,
            %reason,
            ?backoff,
            "a state snapshot peer refused a request; backing off",
        );
    }

    /// Handles `peer` not having `piece`: it is not asked for the piece's
    /// object again until the not-found backoff has passed (design, §6
    /// "Failure handling").
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
    /// wrong hash (design, §6 "Failure handling").
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
        self.futures.push(
            tokio::task::spawn_blocking(move || store.put(hash, &bytes))
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
