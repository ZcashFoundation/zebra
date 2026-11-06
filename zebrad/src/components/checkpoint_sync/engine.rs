//! The engine task: one task owns the window
//! of slots, the [`Scheduler`], and the commit futures; one stream task per
//! assigned unit opens a `get-block-range` stream on its `SyncPeer` and
//! forwards the items; conversion runs on rayon. No locks: `charged` is the
//! only shared counter, an atomic.
//!
//! Commits go through `zs::Request::CommitCheckpointVerifiedBlock`, for the
//! contiguous frontier only, a whole checkpoint interval at a time, at most
//! 1,024 blocks or 64 MiB outstanding, each future timed (120 s, retried
//! once).
//!
//! The window and its commit-reset recovery are ported from
//! `ibd:zebrad/src/components/ibd/engine.rs` (`Slot`, `advance_base`,
//! `on_commit_stage_failure`), with the hash-batch slot states replaced by
//! unit states, and without the disk cache.

use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    net::IpAddr,
    sync::{atomic::AtomicU64, Arc},
    time::Duration,
};

use bytes::Bytes;
use futures::{future::BoxFuture, stream::FuturesUnordered, FutureExt, StreamExt};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::{self, Instant},
};
use tower::{Service, ServiceExt};

use zebra_chain::{
    block::{self, Header},
    parameters::Network,
};
use zebra_network::{
    sync_peer::{BlockRangeStream, ConnectionId, GetBlockRange, RangeItem, BULK_STREAMS_PER_PEER},
    types::PeerServices,
    SyncPeer, SyncPeerError,
};
use zebra_state::{self as zs, CheckpointVerifiedBlock};

use super::{
    anchors::Anchors,
    convert::{spawn_convert, spawn_rebuild, ConvertError},
    scheduler::{
        Assignment, Scheduler, StreamFailure, StreamId, MIN_ENGINE_PEERS, SAMPLE_INTERVAL,
    },
};
use crate::BoxError;

/// The engine's parameters, mapped from `[sync]` config
/// (`v2_lookahead_bytes`, `v2_min_peer_rate`, `v2_target_peers`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineConfig {
    /// The byte lookahead bound (S:506-510). Default 512 MiB.
    pub lookahead_bytes: u64,

    /// The stall floor rate, bytes per second (stall rule 2). Default
    /// 256 KiB/s.
    pub min_peer_rate: u64,

    /// The number of peers to stream from at once. Default 8; the engine
    /// starts with at least 2.
    pub target_peers: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            lookahead_bytes: 512 * 1024 * 1024,
            min_peer_rate: 256 * 1024,
            target_peers: 8,
        }
    }
}

/// The lookahead charge: `Σ assigned-but-undelivered estimates + Σ
/// delivered-not-committed raw bytes × 3`. Shared with
/// #12192's memory budget, which reads it.
pub type Charged = Arc<AtomicU64>;

/// The most blocks submitted to the state and not yet committed
/// (`ibd:…/engine.rs:116-119`).
pub const COMMIT_PIPELINE_BLOCKS: usize = 1_024;

/// The most serialized bytes submitted to the state and not yet committed.
pub const COMMIT_PIPELINE_BYTES: u64 = 64 * 1024 * 1024;

/// How long one commit may take before it is retried (once), then the
/// engine drains.
pub const COMMIT_TIMEOUT: Duration = Duration::from_secs(120);

/// How long the engine runs with fewer than [`MIN_ENGINE_PEERS`] selected
/// peers before it drains.
pub const TOO_FEW_PEERS_FOR: Duration = Duration::from_secs(60);

/// The frontier-unit failures without progress after which the engine
/// drains.
pub const FRONTIER_FAILURE_LIMIT: u32 = 5;

/// The receive window of a selected peer's connection.
pub const SELECTED_RECEIVE_WINDOW: u32 = 64 * 1024 * 1024;

/// The receive window of a deselected peer's connection.
pub const DESELECTED_RECEIVE_WINDOW: u32 = 8 * 1024 * 1024;

/// The capacity of the channel from the stream tasks: 256 events per
/// stream.
const EVENTS_PER_STREAM: usize = 256;

/// The consecutive reset drops of the frontier block, with no commit in
/// between, after which the engine treats the state as shut down.
///
/// The state answers `WriteTaskExited` both when it shuts down and when a
/// reset after a failed commit drops the blocks queued above the failure. A
/// reset drops the frontier block at most once, because the state accepts the
/// block at the reset's next height again, so repeated drops of the same
/// frontier block mean the write task is gone.
const SHUTDOWN_FRONTIER_DROPS: u32 = 3;

/// The misbehavior points for a block whose authorizing data does not match
/// its header's commitment (zips#1344 P:1404-1407).
const AUTH_COMMITMENT_PENALTY: u32 = 100;

// Peers

/// A peer the engine can stream block ranges from. Implemented by
/// [`SyncPeer`]; the tests implement it with simulated peers.
pub trait RangePeer: Clone + Send + 'static {
    /// The stream [`RangePeer::get_block_range`] opens.
    type Stream: RangeStream;

    /// The connection's ID.
    fn id(&self) -> ConnectionId;

    /// The peer's IP address. Blame that outlives a run is keyed by it, so a
    /// peer that reconnects does not get a clean slate.
    fn ip(&self) -> IpAddr;

    /// The services in the peer's `init` record.
    fn services(&self) -> PeerServices;

    /// The peer's advertised tip height (untrusted).
    fn remote_tip_height(&self) -> block::Height;

    /// Whether the connection has closed.
    fn is_closed(&self) -> bool;

    /// Opens a `get-block-range` stream (see [`SyncPeer::get_block_range`]).
    fn get_block_range(
        &self,
        request: GetBlockRange,
    ) -> BoxFuture<'static, Result<Self::Stream, SyncPeerError>>;

    /// Closes the connection for a violation the stream reader cannot see.
    fn report_violation(&self, reason: String);

    /// Adds misbehavior points.
    fn penalize(&self, points: u32, reason: String) -> BoxFuture<'static, ()>;

    /// Sets the connection's receive window.
    fn set_bulk_receive_window(&self, bytes: u32);
}

/// An open `get-block-range` response (see [`BlockRangeStream`]).
pub trait RangeStream: Send + 'static {
    /// The next item, or `None` after the end or an error. Cancellation-safe.
    fn next(&mut self) -> BoxFuture<'_, Option<Result<RangeItem, SyncPeerError>>>;

    /// The response bytes received so far, a partly read block included
    /// (see [`BlockRangeStream::bytes_received`]).
    fn bytes_received(&self) -> u64;
}

impl RangePeer for SyncPeer {
    type Stream = BlockRangeStream;

    fn id(&self) -> ConnectionId {
        SyncPeer::id(self)
    }

    fn ip(&self) -> IpAddr {
        SyncPeer::addr(self).ip()
    }

    fn services(&self) -> PeerServices {
        SyncPeer::services(self)
    }

    fn remote_tip_height(&self) -> block::Height {
        SyncPeer::remote_tip_height(self)
    }

    fn is_closed(&self) -> bool {
        SyncPeer::is_closed(self)
    }

    fn get_block_range(
        &self,
        request: GetBlockRange,
    ) -> BoxFuture<'static, Result<Self::Stream, SyncPeerError>> {
        let peer = self.clone();
        async move { SyncPeer::get_block_range(&peer, request).await }.boxed()
    }

    fn report_violation(&self, reason: String) {
        SyncPeer::report_violation(self, reason)
    }

    fn penalize(&self, points: u32, reason: String) -> BoxFuture<'static, ()> {
        let peer = self.clone();
        async move { SyncPeer::penalize(&peer, points, reason).await }.boxed()
    }

    fn set_bulk_receive_window(&self, bytes: u32) {
        SyncPeer::set_bulk_receive_window(self, bytes)
    }
}

impl RangeStream for BlockRangeStream {
    fn next(&mut self) -> BoxFuture<'_, Option<Result<RangeItem, SyncPeerError>>> {
        BlockRangeStream::next(self).boxed()
    }

    fn bytes_received(&self) -> u64 {
        BlockRangeStream::bytes_received(self)
    }
}

// The window

/// A converted block the window holds.
///
/// A submitted block moves into its commit request, and the window keeps
/// only a cheap [`WindowBlock::Retained`] handle until the commit resolves:
/// cloning a [`CheckpointVerifiedBlock`] deep-copies its `new_outputs`. The
/// rare resubmission rebuilds the block from the handle, on rayon.
pub trait WindowBlock {
    /// What the window keeps of a submitted block.
    type Retained: Clone + std::fmt::Debug + Send + 'static;

    /// The handle to keep while the block is committed.
    fn retain(&self) -> Self::Retained;
}

/// What the window keeps of a submitted [`CheckpointVerifiedBlock`]: the
/// block it was converted from, which [`super::convert::rebuild`] turns back into
/// the same checkpoint-verified block.
#[derive(Clone, Debug)]
pub struct RetainedBlock {
    /// The block.
    pub block: Arc<zebra_chain::block::Block>,
    /// The hash the stream reader verified.
    pub hash: block::Hash,
}

impl WindowBlock for CheckpointVerifiedBlock {
    type Retained = RetainedBlock;

    fn retain(&self) -> RetainedBlock {
        RetainedBlock {
            block: self.block.clone(),
            hash: self.hash,
        }
    }
}

/// The state of one height in the engine's window (ported from
/// `ibd:…/engine.rs:559-616`, with unit states).
///
/// Every uncommitted height in `[base, base + window.len())` has exactly one
/// slot, so duplicate deliveries and end-game losers collapse onto a single
/// accounting point.
#[derive(Clone, Debug)]
pub enum Slot<B: WindowBlock> {
    /// Not delivered yet: in a queued or streaming unit.
    Pending,
    /// Delivered; waiting for a conversion slot.
    Fetched {
        /// The block's serialization.
        raw: Bytes,
        /// The hash the stream reader verified.
        hash: block::Hash,
        /// The delivering connection.
        from: ConnectionId,
    },
    /// Being converted on rayon.
    Converting {
        /// The hash the stream reader verified.
        hash: block::Hash,
        /// The delivering connection.
        from: ConnectionId,
        /// The serialized size.
        bytes: u64,
    },
    /// Converted; waiting for its checkpoint interval to be complete.
    Converted {
        /// The converted block.
        block: B,
        /// The hash the stream reader verified.
        hash: block::Hash,
        /// The delivering connection.
        from: ConnectionId,
        /// The serialized size.
        bytes: u64,
    },
    /// Submitted to the state. The block is retained until its commit
    /// resolves, for the commit-reset recovery.
    Committing {
        /// The submitted block's retained handle.
        retained: B::Retained,
        /// The hash the stream reader verified.
        hash: block::Hash,
        /// The delivering connection.
        from: ConnectionId,
        /// The serialized size.
        bytes: u64,
        /// Whether the commit already timed out once.
        timed_out: bool,
    },
    /// Committed; popped once the contiguous committed prefix reaches it.
    Committed {
        /// The serialized size.
        bytes: u64,
    },
}

/// What the window releases when a held copy is discarded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Discarded {
    /// The hash the stream reader verified, which a refetch anchors at.
    pub hash: block::Hash,
    /// The delivering connection.
    pub from: ConnectionId,
    /// The serialized size.
    pub bytes: u64,
}

/// What to do about a commit timeout.
#[derive(Clone, Debug)]
pub enum Timeout<R> {
    /// Submit the retained block again.
    Retry(R),
    /// The second consecutive timeout: drain.
    GiveUp,
    /// The slot changed meanwhile.
    Ignore,
}

/// The window of uncommitted heights, from the commit frontier `base` up.
#[derive(Debug)]
pub struct Window<B: WindowBlock> {
    base: block::Height,
    slots: VecDeque<Slot<B>>,
    /// The lowest height not yet submitted, or resubmitted after a reset.
    next_submit: block::Height,
    /// The number of [`Slot::Committing`] blocks and their bytes.
    outstanding: (usize, u64),
}

impl<B: WindowBlock> Window<B> {
    /// An empty window whose frontier is `base`.
    pub fn new(base: block::Height) -> Self {
        Window {
            base,
            slots: VecDeque::new(),
            next_submit: base,
            outstanding: (0, 0),
        }
    }

    /// The lowest uncommitted height.
    pub fn base(&self) -> block::Height {
        self.base
    }

    /// The slot at `height`, if it is in the window.
    pub fn slot(&self, height: block::Height) -> Option<&Slot<B>> {
        self.index(height).and_then(|index| self.slots.get(index))
    }

    fn index(&self, height: block::Height) -> Option<usize> {
        // Heights differ by less than 2^31: the cast is exact.
        height
            .0
            .checked_sub(self.base.0)
            .map(|offset| offset as usize)
    }

    fn slot_mut(&mut self, height: block::Height) -> Option<&mut Slot<B>> {
        let index = self.index(height)?;
        self.slots.get_mut(index)
    }

    /// Takes a delivered block. Returns `false` if the height is below the
    /// frontier or already delivered.
    pub fn deliver(
        &mut self,
        height: block::Height,
        raw: Bytes,
        hash: block::Hash,
        from: ConnectionId,
    ) -> bool {
        let Some(index) = self.index(height) else {
            return false;
        };
        if index >= self.slots.len() {
            self.slots.resize_with(index + 1, || Slot::Pending);
        }

        let slot = &mut self.slots[index];
        if !matches!(slot, Slot::Pending) {
            return false;
        }
        *slot = Slot::Fetched { raw, hash, from };
        true
    }

    /// Moves a delivered block to conversion, returning its bytes and hash.
    pub fn start_convert(&mut self, height: block::Height) -> Option<(Bytes, block::Hash)> {
        let slot = self.slot_mut(height)?;
        let Slot::Fetched { raw, hash, from } = slot else {
            return None;
        };
        let (raw, hash, from) = (raw.clone(), *hash, *from);
        *slot = Slot::Converting {
            hash,
            from,
            // `usize` fits in `u64`.
            bytes: raw.len() as u64,
        };
        Some((raw, hash))
    }

    /// Stores a conversion result.
    pub fn converted(&mut self, height: block::Height, block: B) {
        if let Some(slot) = self.slot_mut(height) {
            if let Slot::Converting { hash, from, bytes } = slot {
                *slot = Slot::Converted {
                    block,
                    hash: *hash,
                    from: *from,
                    bytes: *bytes,
                };
            }
        }
    }

    /// Discards the copy held at `height` for a refetch.
    pub fn discard(&mut self, height: block::Height) -> Option<Discarded> {
        let slot = self.slot_mut(height)?;
        let discarded = match slot {
            Slot::Fetched { raw, hash, from } => Discarded {
                hash: *hash,
                from: *from,
                bytes: raw.len() as u64,
            },
            Slot::Converting { hash, from, bytes }
            | Slot::Converted {
                hash, from, bytes, ..
            }
            | Slot::Committing {
                hash, from, bytes, ..
            } => Discarded {
                hash: *hash,
                from: *from,
                bytes: *bytes,
            },
            Slot::Pending | Slot::Committed { .. } => return None,
        };
        let was_committing = matches!(slot, Slot::Committing { .. });
        *slot = Slot::Pending;
        if was_committing {
            self.outstanding.0 -= 1;
            self.outstanding.1 -= discarded.bytes;
        }
        self.next_submit = self.next_submit.min(height);
        Some(discarded)
    }

    /// The highest height through which the window may submit: the end of
    /// the last complete checkpoint interval above the submitted prefix that
    /// fits the commit pipeline. `next_checkpoint` gives
    /// the checkpoint at or above a height, `None` above the target.
    pub fn submittable_through(
        &self,
        next_checkpoint: impl Fn(block::Height) -> Option<block::Height>,
        mut outstanding_blocks: usize,
        mut outstanding_bytes: u64,
    ) -> Option<block::Height> {
        let mut through = None;
        let mut low = self.next_submit;

        while let Some(end) = next_checkpoint(low) {
            let (mut blocks, mut bytes) = (0, 0);
            let mut height = low;
            loop {
                match self.slot(height) {
                    Some(Slot::Converted { bytes: size, .. }) => {
                        blocks += 1;
                        bytes += size;
                    }
                    Some(Slot::Committing { .. } | Slot::Committed { .. }) => {}
                    // Below the frontier: committed.
                    None if height < self.base => {}
                    _ => return through,
                }
                if height == end {
                    break;
                }
                height = height.next().expect("below a checkpoint");
            }

            let fits = (outstanding_blocks + blocks <= COMMIT_PIPELINE_BLOCKS
                && outstanding_bytes + bytes <= COMMIT_PIPELINE_BYTES)
                || outstanding_blocks == 0;
            if !fits {
                return through;
            }
            outstanding_blocks += blocks;
            outstanding_bytes += bytes;
            through = Some(end);

            match end.next() {
                Ok(next) => low = next,
                Err(_) => break,
            }
        }

        through
    }

    /// Marks the converted blocks through `through` as committing and returns
    /// them, in height order.
    pub fn take_submissions(
        &mut self,
        through: block::Height,
    ) -> Vec<(block::Height, block::Hash, B)> {
        let mut submissions = Vec::new();
        let mut height = self.next_submit.max(self.base);
        while height <= through {
            let index = self.index(height).expect("at or above the frontier");
            if let Some(slot) = self.slots.get_mut(index) {
                if matches!(slot, Slot::Converted { .. }) {
                    let Slot::Converted {
                        block,
                        hash,
                        from,
                        bytes,
                    } = std::mem::replace(slot, Slot::Pending)
                    else {
                        unreachable!("just matched");
                    };
                    self.outstanding.0 += 1;
                    self.outstanding.1 += bytes;
                    *slot = Slot::Committing {
                        retained: block.retain(),
                        hash,
                        from,
                        bytes,
                        timed_out: false,
                    };
                    submissions.push((height, hash, block));
                }
            }
            height = height.next().expect("at most the target");
        }
        self.next_submit = self.next_submit.max(height);
        submissions
    }

    /// The submitted block at `height` committed.
    pub fn committed(&mut self, height: block::Height) {
        if let Some(slot) = self.slot_mut(height) {
            if let Slot::Committing { bytes, .. } = slot {
                let bytes = *bytes;
                *slot = Slot::Committed { bytes };
                self.outstanding.0 -= 1;
                self.outstanding.1 -= bytes;
            }
        }
    }

    /// The submitted block at `height` was dropped by a state reset: submit
    /// the retained copy again (a re-verification, never a re-download).
    ///
    /// Returns the retained handle: the slot is converting again, and the
    /// caller rebuilds the block from the handle and stores it with
    /// [`Window::converted`].
    pub fn resubmit(&mut self, height: block::Height) -> Option<B::Retained> {
        let slot = self.slot_mut(height)?;
        let Slot::Committing {
            retained,
            hash,
            from,
            bytes,
            ..
        } = slot
        else {
            return None;
        };
        let (retained, bytes) = (retained.clone(), *bytes);
        *slot = Slot::Converting {
            hash: *hash,
            from: *from,
            bytes,
        };
        self.outstanding.0 -= 1;
        self.outstanding.1 -= bytes;
        self.next_submit = self.next_submit.min(height);
        Some(retained)
    }

    /// The commit of `height` timed out: on its first timeout, returns the
    /// retained handle to rebuild and submit again, and on a second
    /// consecutive one [`Timeout::GiveUp`].
    pub fn timed_out(&mut self, height: block::Height) -> Timeout<B::Retained> {
        match self.slot_mut(height) {
            Some(Slot::Committing {
                retained,
                timed_out,
                ..
            }) => {
                if *timed_out {
                    Timeout::GiveUp
                } else {
                    *timed_out = true;
                    Timeout::Retry(retained.clone())
                }
            }
            // Discarded or committed meanwhile.
            _ => Timeout::Ignore,
        }
    }

    /// Pops the contiguous committed prefix, advancing the frontier. Returns
    /// the number of blocks popped and their serialized bytes.
    pub fn advance_base(&mut self) -> (u64, u64) {
        let (mut blocks, mut bytes) = (0, 0);
        while let Some(Slot::Committed { bytes: size }) = self.slots.front() {
            blocks += 1;
            bytes += size;
            self.slots.pop_front();
            self.base = self.base.next().expect("at most the target");
        }
        self.next_submit = self.next_submit.max(self.base);
        (blocks, bytes)
    }

    /// The number of submitted, uncommitted blocks and their bytes.
    pub fn outstanding(&self) -> (usize, u64) {
        self.outstanding
    }
}

// The engine

/// Why the engine stopped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineExit {
    /// Every height up to the target is committed.
    TargetReached,
    /// Fewer than [`MIN_ENGINE_PEERS`] selected peers for
    /// [`TOO_FEW_PEERS_FOR`].
    TooFewPeers,
    /// The frontier unit failed [`FRONTIER_FAILURE_LIMIT`] times without
    /// progress.
    NoProgress,
    /// A commit timed out twice in a row.
    CommitTimeout,
    /// A checkpoint-list defect or a deterministic commit failure: fatal.
    Defect(String),
    /// The state or the rayon pool is gone: Zebra is shutting down.
    Shutdown,
}

/// The result of one engine run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineOutcome {
    /// Why the engine stopped.
    pub exit: EngineExit,
    /// The lowest height the engine did not commit.
    pub base: block::Height,
    /// The IP addresses of the peers whose streams of the frontier unit
    /// failed since it last advanced, when the run ended with
    /// [`EngineExit::NoProgress`]; otherwise empty.
    pub blamed: BTreeSet<IpAddr>,
}

/// The inputs of one engine run.
pub struct EngineRun<P, ZS> {
    /// The engine parameters.
    pub config: EngineConfig,
    /// The network, for the merkle check.
    pub network: Network,
    /// The checkpoint anchors.
    pub anchors: Anchors,
    /// The first height to download: the state tip plus one.
    pub start: block::Height,
    /// The checkpoint to stop at.
    pub target: block::Height,
    /// The connected version 2 peers.
    pub peers: watch::Receiver<Arc<[P]>>,
    /// The state service.
    pub state: ZS,
    /// Commits wait until this is `true` or its sender is dropped.
    pub hints_gate: Option<watch::Receiver<bool>>,
    /// Receives the block count of each committed unit.
    pub unit_lengths: mpsc::Sender<usize>,
    /// The shared lookahead charge.
    pub charged: Charged,
    /// The IP addresses of the peers the last run blamed for its lack of
    /// progress: this run does not download from them, whatever their
    /// connection.
    pub blamed: BTreeSet<IpAddr>,
}

enum StreamItem {
    Block {
        raw: Bytes,
        header: Arc<Header>,
        hash: block::Hash,
    },
    /// The stream's response bytes so far, sent every [`SAMPLE_INTERVAL`]
    /// while they grow, so that the stall rules see a partly read block.
    Progress(u64),
    End,
    Failed(SyncPeerError),
}

struct StreamEvent {
    stream: StreamId,
    item: StreamItem,
}

struct EngineStream {
    peer: ConnectionId,
    start: block::Height,
    next_height: Option<block::Height>,
    task: JoinHandle<()>,
}

/// The engine's stream tasks, aborted on drop so that an aborted engine
/// leaves no stream open.
#[derive(Default)]
struct Streams(HashMap<StreamId, EngineStream>);

impl Drop for Streams {
    fn drop(&mut self) {
        for stream in self.0.values() {
            stream.task.abort();
        }
    }
}

type ConvertFuture =
    BoxFuture<'static, (block::Height, Result<CheckpointVerifiedBlock, ConvertError>)>;

type CommitResult = Result<Result<zs::Response, BoxError>, time::error::Elapsed>;
type CommitFuture = BoxFuture<'static, (block::Height, CommitResult)>;

/// How a commit attempt ended.
enum CommitVerdict {
    Committed,
    /// `WriteTaskExited`: a reset after a failed commit dropped the block, or
    /// the state is shutting down.
    Dropped,
    /// The state service is gone.
    Shutdown,
    AuthCommitmentMismatch(BoxError),
    Failed(BoxError),
}

fn commit_verdict(result: Result<zs::Response, BoxError>) -> CommitVerdict {
    let error = match result {
        Ok(zs::Response::Committed(_)) => return CommitVerdict::Committed,
        Ok(response) => {
            return CommitVerdict::Failed(format!("unexpected response {response:?}").into())
        }
        Err(error) => error,
    };

    match error.downcast_ref::<zs::CommitCheckpointVerifiedError>() {
        // A legacy verifier's leftover, or a retried timeout, already queued
        // or committed this block.
        Some(commit) if commit.inner().is_duplicate_request() => CommitVerdict::Committed,
        Some(commit) if matches!(commit.inner(), zs::CommitBlockError::WriteTaskExited { .. }) => {
            CommitVerdict::Dropped
        }
        None if error.is::<tower::buffer::error::Closed>() || error.is::<ConvertError>() => {
            // The state's buffer is closed, or a retry's rebuild lost the
            // rayon pool.
            CommitVerdict::Shutdown
        }
        Some(commit) if commit.inner().is_auth_commitment_mismatch() => {
            CommitVerdict::AuthCommitmentMismatch(error)
        }
        _ => CommitVerdict::Failed(error),
    }
}

impl<P, ZS> EngineRun<P, ZS>
where
    P: RangePeer,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send,
{
    /// Runs the engine until it reaches its target or must hand back, then
    /// drains: stops issuing, cancels its streams, and awaits every submitted
    /// commit.
    pub async fn run(self) -> EngineOutcome {
        let EngineRun {
            config,
            network,
            anchors,
            start,
            target,
            mut peers,
            state,
            mut hints_gate,
            unit_lengths,
            charged,
            blamed: excluded,
        } = self;

        let now = Instant::now();
        let shared_charge = charged.clone();
        let mut scheduler = Scheduler::new(&config, anchors.clone(), start, target, charged, now);
        let mut window: Window<CheckpointVerifiedBlock> = Window::new(start);
        let (events_tx, mut events) = mpsc::channel(
            EVENTS_PER_STREAM * BULK_STREAMS_PER_PEER * config.target_peers.max(MIN_ENGINE_PEERS),
        );
        let mut streams = Streams::default();
        let mut known_peers: HashMap<ConnectionId, P> = HashMap::new();
        // Every candidate's IP address during the run, departed ones included,
        // so that the run's blame can name a peer that has since disconnected.
        let mut peer_ips: HashMap<ConnectionId, IpAddr> = HashMap::new();
        let mut converts: FuturesUnordered<ConvertFuture> = FuturesUnordered::new();
        let mut to_convert: VecDeque<block::Height> = VecDeque::new();
        let max_converts = rayon::current_num_threads().max(1) * 2;
        let mut commits: FuturesUnordered<CommitFuture> = FuturesUnordered::new();
        let mut commit_failures: HashMap<block::Height, u32> = HashMap::new();
        let mut auth_mismatches: HashMap<block::Height, u32> = HashMap::new();
        let mut frontier_drops: u32 = 0;
        let mut gate_open = hints_gate
            .as_mut()
            .is_none_or(|gate| *gate.borrow_and_update());
        let mut too_few_since: Option<Instant> = None;
        let mut tick = time::interval(SAMPLE_INTERVAL);
        tick.set_missed_tick_behavior(time::MissedTickBehavior::Delay);

        let next_checkpoint = |height: block::Height| {
            anchors
                .at_or_above(height)
                .map(|(checkpoint, _)| checkpoint)
                .filter(|checkpoint| *checkpoint <= target)
        };

        update_peers(
            &peers,
            &excluded,
            &mut known_peers,
            &mut peer_ips,
            &mut scheduler,
            &mut streams,
            now,
        );

        let exit = loop {
            let now = Instant::now();

            apply_window_changes(&mut scheduler, &known_peers);

            // Assign units to peers with free pull budgets.
            while let Some(assignment) = scheduler.next_assignment(now) {
                let Some(peer) = known_peers.get(&assignment.peer).cloned() else {
                    scheduler.on_failure(assignment.stream, StreamFailure::Transport, now);
                    continue;
                };
                let task = tokio::spawn(stream_task(peer, assignment, events_tx.clone()));
                streams.0.insert(
                    assignment.stream,
                    EngineStream {
                        peer: assignment.peer,
                        start: assignment.unit.start,
                        next_height: Some(assignment.unit.end),
                        task,
                    },
                );
            }

            // Convert delivered blocks on rayon.
            while converts.len() < max_converts {
                let Some(height) = to_convert.pop_front() else {
                    break;
                };
                if let Some((raw, hash)) = window.start_convert(height) {
                    let network = network.clone();
                    converts.push(
                        async move { (height, spawn_convert(network, height, hash, raw).await) }
                            .boxed(),
                    );
                }
            }

            // Submit complete checkpoint intervals at the frontier.
            if gate_open {
                let (blocks, bytes) = window.outstanding();
                if let Some(through) = window.submittable_through(next_checkpoint, blocks, bytes) {
                    for (height, _hash, block) in window.take_submissions(through) {
                        commits.push(commit(state.clone(), height, block));
                    }
                }
            }

            // Exit checks.
            if window.base() > target {
                break EngineExit::TargetReached;
            }
            // While the hints gate is closed the engine keeps its claim, so
            // that nothing commits before hint acquisition finishes: the
            // frontier cannot advance, so its failures do not end the run.
            if gate_open && scheduler.frontier_stuck(FRONTIER_FAILURE_LIMIT, now) {
                break EngineExit::NoProgress;
            }
            if gate_open && scheduler.selected_peers() < MIN_ENGINE_PEERS {
                let since = *too_few_since.get_or_insert(now);
                if now.saturating_duration_since(since) >= TOO_FEW_PEERS_FOR {
                    break EngineExit::TooFewPeers;
                }
            } else {
                too_few_since = None;
            }

            tokio::select! {
                Some(event) = events.recv() => {
                    on_stream_event(
                        event,
                        &mut scheduler,
                        &mut window,
                        &mut streams,
                        &mut to_convert,
                    );
                }

                Some((height, result)) = converts.next() => match result {
                    Ok(block) => window.converted(height, block),
                    Err(ConvertError::RayonShutdown) => break EngineExit::Shutdown,
                    Err(error @ ConvertError::WrongHeight { .. }) => {
                        break EngineExit::Defect(error.to_string());
                    }
                    Err(error) => {
                        warn!(%error, "a peer delivered an invalid block; refetching it");
                        if let Some(discarded) = window.discard(height) {
                            if let Some(peer) = known_peers.get(&discarded.from) {
                                peer.report_violation(error.to_string());
                            }
                            scheduler.on_discarded(height, discarded.hash, discarded.bytes);
                        }
                    }
                },

                Some((height, result)) = commits.next() => {
                    let verdict = match result {
                        Err(_elapsed) => match window.timed_out(height) {
                            Timeout::Retry(retained) => {
                                warn!(height = height.0, "a commit timed out; retrying it once");
                                commits.push(commit_retained(state.clone(), height, retained));
                                continue;
                            }
                            Timeout::GiveUp => break EngineExit::CommitTimeout,
                            Timeout::Ignore => continue,
                        },
                        Ok(result) => commit_verdict(result),
                    };

                    match verdict {
                        CommitVerdict::Committed => {
                            window.committed(height);
                            let (blocks, bytes) = window.advance_base();
                            if blocks > 0 {
                                scheduler.on_committed(bytes);
                                for length in scheduler.set_frontier(window.base()) {
                                    // The legacy syncer drains this channel; a full channel
                                    // only loses a sync-length sample.
                                    let _ = unit_lengths.try_send(length);
                                }
                                commit_failures.retain(|failed, _| *failed >= window.base());
                                auth_mismatches.retain(|failed, _| *failed >= window.base());
                                frontier_drops = 0;
                            }
                        }
                        CommitVerdict::Shutdown => break EngineExit::Shutdown,
                        CommitVerdict::Dropped => {
                            // A block dropped by the write task's reset after
                            // a failed commit below it (`ibd:…/engine.rs:2127-2160`),
                            // or a shutdown: submit the retained copy again,
                            // once the interval below it is submittable.
                            if height == window.base() {
                                frontier_drops += 1;
                                if frontier_drops >= SHUTDOWN_FRONTIER_DROPS {
                                    break EngineExit::Shutdown;
                                }
                            }
                            debug!(height = height.0, "resubmitting a commit dropped by a reset");
                            if let Some(retained) = window.resubmit(height) {
                                converts.push(rebuild(height, retained));
                            }
                        }
                        CommitVerdict::AuthCommitmentMismatch(error)
                        | CommitVerdict::Failed(error)
                            if height > window.base() =>
                        {
                            // The failure arrived before the commits below
                            // it: submit it again, it is judged at the
                            // frontier.
                            debug!(%error, height = height.0, "resubmitting a commit that failed above the frontier");
                            if let Some(retained) = window.resubmit(height) {
                                converts.push(rebuild(height, retained));
                            }
                        }
                        CommitVerdict::AuthCommitmentMismatch(error) => {
                            // Checkpointed blocks are valid, so a mismatch
                            // always means the delivering peer lied, never a
                            // list defect, however often it repeats: penalize
                            // the peer (a ban), back it off, and refetch. If
                            // lying peers keep the frontier from committing,
                            // hand back to the legacy syncer, which is not
                            // fatal.
                            let mismatches = auth_mismatches.entry(height).or_default();
                            *mismatches += 1;
                            if *mismatches >= FRONTIER_FAILURE_LIMIT {
                                warn!(%error, height = height.0, "peers keep sending mismatched authorizing data; handing back");
                                break EngineExit::NoProgress;
                            }
                            warn!(%error, height = height.0, "a peer sent mismatched authorizing data; refetching");
                            if let Some(discarded) = window.discard(height) {
                                if let Some(peer) = known_peers.get(&discarded.from) {
                                    tokio::spawn(peer.penalize(AUTH_COMMITMENT_PENALTY, error.to_string()));
                                }
                                scheduler.back_off(discarded.from, Instant::now());
                                scheduler.on_discarded(height, discarded.hash, discarded.bytes);
                            }
                        }
                        CommitVerdict::Failed(error) => {
                            let failures = commit_failures.entry(height).or_default();
                            *failures += 1;
                            if *failures >= 2 {
                                // A copy from a second peer failed too: a
                                // deterministic failure (`ibd:…/engine.rs:212-282`).
                                break EngineExit::Defect(format!(
                                    "block at {height:?} failed its commit twice: {error}"
                                ));
                            }
                            warn!(%error, height = height.0, "a commit failed; retrying with another copy");
                            if let Some(discarded) = window.discard(height) {
                                scheduler.on_discarded(height, discarded.hash, discarded.bytes);
                            }
                        }
                    }
                }

                _ = tick.tick() => {
                    let now = Instant::now();
                    for stream in scheduler.tick(now) {
                        if let Some(stream) = streams.0.remove(&stream) {
                            stream.task.abort();
                        }
                    }
                }

                changed = peers.changed() => {
                    if changed.is_err() {
                        // The registry is gone: no peers.
                        peers = watch::channel(Arc::from(Vec::new())).1;
                    }
                    update_peers(
                        &peers,
                        &excluded,
                        &mut known_peers,
                        &mut peer_ips,
                        &mut scheduler,
                        &mut streams,
                        Instant::now(),
                    );
                }

                open = wait_for_gate(&mut hints_gate), if !gate_open => {
                    gate_open = open;
                    // Count the failures from here: the frontier could not
                    // advance while the gate was closed.
                    scheduler.reset_frontier_failures();
                }
            }
        };

        // Drain: stop issuing, cancel the streams, await the submitted
        // commits (each is timed).
        for (_, stream) in streams.0.drain() {
            stream.task.abort();
        }
        drop(converts);
        while let Some((height, result)) = commits.next().await {
            if let Ok(result) = result {
                if matches!(commit_verdict(result), CommitVerdict::Committed) {
                    window.committed(height);
                }
            }
        }
        window.advance_base();
        let blamed = if exit == EngineExit::NoProgress {
            scheduler
                .frontier_failed_peers()
                .iter()
                .filter_map(|id| peer_ips.get(id))
                .copied()
                .collect()
        } else {
            BTreeSet::new()
        };

        // The window's blocks are dropped with the run: nothing stays
        // charged while the legacy syncer runs.
        drop(scheduler);
        shared_charge.store(0, std::sync::atomic::Ordering::Relaxed);

        for peer in known_peers.values() {
            peer.set_bulk_receive_window(DESELECTED_RECEIVE_WINDOW);
        }

        EngineOutcome {
            exit,
            base: window.base(),
            blamed,
        }
    }
}

/// Waits for the hints gate to open. Returns `true` once it is open or its
/// sender is dropped.
async fn wait_for_gate(gate: &mut Option<watch::Receiver<bool>>) -> bool {
    let Some(receiver) = gate.as_mut() else {
        return true;
    };
    let dropped = receiver.wait_for(|open| *open).await.is_err();
    if dropped {
        *gate = None;
    }
    true
}

/// Rebuilds the block at `height` from its retained handle, on rayon.
fn rebuild(height: block::Height, retained: RetainedBlock) -> ConvertFuture {
    async move { (height, spawn_rebuild(retained.block, retained.hash).await) }.boxed()
}

/// Rebuilds a timed-out block from its retained handle, on rayon, and
/// starts its commit again, timed by [`COMMIT_TIMEOUT`].
fn commit_retained<ZS>(state: ZS, height: block::Height, retained: RetainedBlock) -> CommitFuture
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + 'static,
    ZS::Future: Send,
{
    async move {
        match spawn_rebuild(retained.block, retained.hash).await {
            Ok(block) => commit(state, height, block).await,
            Err(error) => (height, Ok(Err(Box::new(error) as BoxError))),
        }
    }
    .boxed()
}

/// Starts the commit of `block`, timed by [`COMMIT_TIMEOUT`].
fn commit<ZS>(state: ZS, height: block::Height, block: CheckpointVerifiedBlock) -> CommitFuture
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + 'static,
    ZS::Future: Send,
{
    async move {
        let result = time::timeout(
            COMMIT_TIMEOUT,
            state.oneshot(zs::Request::CommitCheckpointVerifiedBlock(block)),
        )
        .await;
        (height, result)
    }
    .boxed()
}

/// Opens the stream for `assignment` and forwards its items to the engine.
async fn stream_task<P: RangePeer>(
    peer: P,
    assignment: Assignment,
    events: mpsc::Sender<StreamEvent>,
) {
    let stream = assignment.stream;
    let unit = assignment.unit;
    let request = GetBlockRange {
        final_hash: unit.end_hash,
        count: unit.end.0 - unit.start.0 + 1,
        max_bytes: unit.max_bytes,
    };

    let send = |item| events.send(StreamEvent { stream, item });

    let mut range = match peer.get_block_range(request).await {
        Ok(range) => range,
        Err(error) => {
            let _ = send(StreamItem::Failed(error)).await;
            return;
        }
    };

    let mut progress = time::interval(SAMPLE_INTERVAL);
    progress.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
    let mut reported = 0;

    loop {
        let next = tokio::select! {
            next = range.next() => next,
            _ = progress.tick() => {
                // `next` is cancellation-safe, so dropping it loses nothing.
                let received = range.bytes_received();
                if received > reported {
                    reported = received;
                    if send(StreamItem::Progress(received)).await.is_err() {
                        return;
                    }
                }
                continue;
            }
        };
        let item = match next {
            Some(Ok(RangeItem::Block { raw, header, hash })) => {
                StreamItem::Block { raw, header, hash }
            }
            Some(Ok(RangeItem::End { .. })) => StreamItem::End,
            Some(Err(error)) => StreamItem::Failed(error),
            None => StreamItem::Failed(SyncPeerError::Transport(
                "the stream ended without an end marker".to_string(),
            )),
        };
        let last = !matches!(item, StreamItem::Block { .. } | StreamItem::Progress(_));
        if send(item).await.is_err() || last {
            return;
        }
    }
}

fn on_stream_event(
    event: StreamEvent,
    scheduler: &mut Scheduler,
    window: &mut Window<CheckpointVerifiedBlock>,
    streams: &mut Streams,
    to_convert: &mut VecDeque<block::Height>,
) {
    let now = Instant::now();
    if !scheduler.is_live(event.stream) {
        // A cancelled stream's buffered events, or the end marker of the
        // stream that completed its unit.
        if !matches!(
            event.item,
            StreamItem::Block { .. } | StreamItem::Progress(_)
        ) {
            streams.0.remove(&event.stream);
        }
        return;
    }
    let Some(stream) = streams.0.get_mut(&event.stream) else {
        return;
    };

    match event.item {
        StreamItem::Block { raw, header, hash } => {
            let Some(height) = stream.next_height.filter(|height| *height >= stream.start) else {
                // The stream reader bounds the count: this is unreachable for a
                // correct reader, so the peer is not blamed.
                warn!(stream = ?event.stream, "a stream delivered more blocks than requested");
                scheduler.on_failure(event.stream, StreamFailure::Transport, now);
                if let Some(stream) = streams.0.remove(&event.stream) {
                    stream.task.abort();
                }
                return;
            };
            stream.next_height = height.previous().ok();

            // `usize` fits in `u64`.
            let bytes = raw.len() as u64;
            let is_new = window.deliver(height, raw, hash, stream.peer);
            if is_new {
                to_convert.push_back(height);
            }
            for cancelled in scheduler.on_block(
                event.stream,
                height,
                bytes,
                header.previous_block_hash,
                is_new,
                now,
            ) {
                // The unit is complete. The delivering stream's end marker is
                // next; the others lost the end-game race.
                if cancelled != event.stream {
                    if let Some(stream) = streams.0.remove(&cancelled) {
                        stream.task.abort();
                    }
                }
            }
        }
        StreamItem::Progress(received) => scheduler.on_bytes(event.stream, received, now),
        StreamItem::End => {
            scheduler.on_end(event.stream, now);
            streams.0.remove(&event.stream);
        }
        StreamItem::Failed(error) => {
            let failure = match &error {
                SyncPeerError::NotFound => StreamFailure::NotFound,
                SyncPeerError::Refused => StreamFailure::Refused,
                SyncPeerError::Violation { .. } => StreamFailure::Violation,
                SyncPeerError::LocalRequest(reason) => {
                    warn!(%reason, "the engine built an invalid get-block-range request");
                    StreamFailure::Transport
                }
                _ => StreamFailure::Transport,
            };
            debug!(%error, peer = ?stream.peer, "a block-range stream failed");
            scheduler.on_failure(event.stream, failure, now);
            streams.0.remove(&event.stream);
        }
    }
}

/// Refreshes the candidate peers from the registry, leaving out the
/// peers whose IP address is `excluded`, and records each candidate's IP
/// address in `peer_ips`.
fn update_peers<P: RangePeer>(
    peers: &watch::Receiver<Arc<[P]>>,
    excluded: &BTreeSet<IpAddr>,
    known_peers: &mut HashMap<ConnectionId, P>,
    peer_ips: &mut HashMap<ConnectionId, IpAddr>,
    scheduler: &mut Scheduler,
    streams: &mut Streams,
    now: Instant,
) {
    let current = peers.borrow().clone();
    known_peers.clear();
    let mut candidates = Vec::new();
    for peer in current.iter() {
        if peer.services().contains(PeerServices::NODE_NETWORK)
            && !peer.is_closed()
            && !excluded.contains(&peer.ip())
        {
            candidates.push((peer.id(), peer.remote_tip_height()));
            known_peers.insert(peer.id(), peer.clone());
            peer_ips.insert(peer.id(), peer.ip());
        }
    }

    for stream in scheduler.set_peers(&candidates, now) {
        if let Some(stream) = streams.0.remove(&stream) {
            stream.task.abort();
        }
    }
}

fn apply_window_changes<P: RangePeer>(
    scheduler: &mut Scheduler,
    known_peers: &HashMap<ConnectionId, P>,
) {
    for change in scheduler.take_window_changes() {
        if let Some(peer) = known_peers.get(&change.peer) {
            peer.set_bulk_receive_window(if change.selected {
                SELECTED_RECEIVE_WINDOW
            } else {
                DESELECTED_RECEIVE_WINDOW
            });
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
