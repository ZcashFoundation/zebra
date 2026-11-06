//! Engine tests: the window under arbitrary interleavings (a property test),
//! and whole engine runs against simulated peers and a mock state.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    net::{IpAddr, Ipv4Addr},
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use bytes::Bytes;
use futures::{future::BoxFuture, FutureExt};
use proptest::prelude::*;
use tokio::sync::{mpsc, watch};

use zebra_chain::{
    block::{self, Block, Header, SyncHashEntry},
    parameters::{
        checkpoint::list::CheckpointList,
        known_hashes::{chunk_pin, KnownHashListSpec},
        Network,
    },
    serialization::{ZcashDeserializeInto, ZcashSerialize},
};
use zebra_network::{
    sync_peer::{ConnectionId, GetBlockRange, RangeItem},
    types::PeerServices,
    SyncPeerError,
};
use zebra_state as zs;

use super::*;
use crate::{
    components::checkpoint_sync::{
        anchors::Anchors,
        known_hashes::{ChunkPins, HashPeer, KnownHashes, PinnedList},
    },
    BoxError,
};

// The window

impl WindowBlock for u32 {
    type Retained = u32;

    fn retain(&self) -> u32 {
        *self
    }
}

/// One step of the window property test.
#[derive(Clone, Debug)]
enum Op {
    /// Deliver the block at the given offset (possibly again).
    Deliver(u32),
    /// Finish a conversion, picked by index.
    Convert(usize),
    /// Discard a held copy at the given offset (a convert failure).
    Discard(u32),
    /// Finish an outstanding commit, picked by index.
    Commit(usize),
    /// Fail an outstanding commit with a reset, picked by index.
    Reset(usize),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..60u32).prop_map(Op::Deliver),
        3 => any::<usize>().prop_map(Op::Convert),
        1 => (0..60u32).prop_map(Op::Discard),
        3 => any::<usize>().prop_map(Op::Commit),
        1 => any::<usize>().prop_map(Op::Reset),
    ]
}

const WINDOW_TARGET: u32 = 60;

/// The window model: heights `1..=60` with a checkpoint every `gap` heights.
struct WindowModel {
    window: Window<u32>,
    gap: u32,
    converting: Vec<block::Height>,
    outstanding: Vec<block::Height>,
    popped: Vec<block::Height>,
}

impl WindowModel {
    fn hash(height: block::Height) -> block::Hash {
        block::Hash([height.0 as u8; 32])
    }

    fn submit(&mut self) {
        let counted = self
            .outstanding
            .iter()
            .fold((0, 0), |(blocks, bytes), _| (blocks + 1, bytes + 10));
        assert_eq!(
            self.window.outstanding(),
            counted,
            "the outstanding counters agree"
        );

        let gap = self.gap;
        let next_checkpoint = move |height: block::Height| {
            (height.0 <= WINDOW_TARGET)
                .then(|| block::Height((height.0.div_ceil(gap) * gap).min(WINDOW_TARGET)))
        };
        let (blocks, bytes) = self.window.outstanding();
        let Some(through) = self
            .window
            .submittable_through(next_checkpoint, blocks, bytes)
        else {
            return;
        };

        let submissions = self.window.take_submissions(through);
        for pair in submissions.windows(2) {
            assert!(pair[0].0 < pair[1].0, "submissions are in height order");
        }
        for (height, _, block) in submissions {
            assert_eq!(block, height.0, "the converted block is submitted");
            assert!(
                !self.outstanding.contains(&height),
                "at most one commit per height"
            );
            self.outstanding.push(height);
        }
    }

    fn apply(&mut self, op: &Op) {
        match op {
            Op::Deliver(offset) => {
                let height = block::Height(1 + offset);
                let raw = Bytes::from_static(&[0; 10]);
                if self
                    .window
                    .deliver(height, raw, Self::hash(height), ConnectionId(0))
                {
                    assert!(self.window.start_convert(height).is_some());
                    self.converting.push(height);
                }
            }
            Op::Convert(index) if !self.converting.is_empty() => {
                let height = self.converting.swap_remove(index % self.converting.len());
                self.window.converted(height, height.0);
            }
            Op::Discard(offset) => {
                let height = block::Height(1 + offset);
                if let Some(discarded) = self.window.discard(height) {
                    assert_eq!(discarded.hash, Self::hash(height));
                    self.converting.retain(|converting| *converting != height);
                    self.outstanding
                        .retain(|outstanding| *outstanding != height);
                }
            }
            Op::Commit(index) if !self.outstanding.is_empty() => {
                let height = self.outstanding.swap_remove(index % self.outstanding.len());
                self.window.committed(height);
                let mut height = self.window.base();
                let (blocks, _) = self.window.advance_base();
                for _ in 0..blocks {
                    self.popped.push(height);
                    height = height.next().unwrap();
                }
            }
            Op::Reset(index) if !self.outstanding.is_empty() => {
                let height = self.outstanding.swap_remove(index % self.outstanding.len());
                assert_eq!(self.window.resubmit(height), Some(height.0));
                // The resubmitted block is rebuilt from its retained copy.
                self.converting.push(height);
            }
            _ => {}
        }
        self.submit();
    }

    /// Runs `ops`, then drives the window to the end. Returns the heights in
    /// the order the window popped them.
    fn run(ops: &[Op], gap: u32) -> Vec<block::Height> {
        let mut model = WindowModel {
            window: Window::new(block::Height(1)),
            gap,
            converting: Vec::new(),
            outstanding: Vec::new(),
            popped: Vec::new(),
        };

        for op in ops {
            model.apply(op);
        }

        for offset in 0..WINDOW_TARGET {
            model.apply(&Op::Deliver(offset));
        }
        while !model.converting.is_empty() || !model.outstanding.is_empty() {
            model.apply(&Op::Convert(0));
            model.apply(&Op::Commit(0));
        }

        assert_eq!(model.window.base(), block::Height(WINDOW_TARGET + 1));
        model.popped
    }
}

proptest! {
    /// Any interleaving of deliveries, duplicates, conversions, discards,
    /// commits and resets commits every height in order, exactly once.
    #[test]
    fn window_commits_in_order_exactly_once(
        ops in prop::collection::vec(op(), 0..400),
        gap in 1..20u32,
    ) {
        let _init_guard = zebra_test::init();

        let popped = WindowModel::run(&ops, gap);
        let expected: Vec<block::Height> = (1..=WINDOW_TARGET).map(block::Height).collect();
        prop_assert_eq!(popped, expected);
    }
}

/// Submission waits for a whole checkpoint interval and for
/// the commit pipeline bounds.
#[test]
fn submission_is_per_checkpoint_interval() {
    let _init_guard = zebra_test::init();

    let next_checkpoint = |height: block::Height| {
        (height.0 <= 10).then_some(block::Height(if height.0 <= 5 { 5 } else { 10 }))
    };
    let mut window: Window<u32> = Window::new(block::Height(1));
    let convert = |window: &mut Window<u32>, height: u32| {
        let height = block::Height(height);
        window.deliver(
            height,
            Bytes::from_static(&[0; 4]),
            block::Hash([0; 32]),
            ConnectionId(0),
        );
        window.start_convert(height);
        window.converted(height, height.0);
    };
    for height in (1..=4).chain(6..=10) {
        convert(&mut window, height);
    }
    assert_eq!(window.submittable_through(next_checkpoint, 0, 0), None);

    convert(&mut window, 5);
    assert_eq!(
        window.submittable_through(next_checkpoint, 0, 0),
        Some(block::Height(10))
    );
    assert_eq!(
        window.submittable_through(next_checkpoint, COMMIT_PIPELINE_BLOCKS - 5, 0),
        Some(block::Height(5)),
        "the pipeline bound stops at an interval"
    );
    assert_eq!(
        window.submittable_through(next_checkpoint, COMMIT_PIPELINE_BLOCKS, 0),
        None
    );

    let submitted = window.take_submissions(block::Height(10));
    assert_eq!(submitted.len(), 10);
    assert_eq!(window.outstanding(), (10, 40));
    assert!(matches!(
        window.timed_out(block::Height(3)),
        Timeout::Retry(3)
    ));
    assert!(matches!(
        window.timed_out(block::Height(3)),
        Timeout::GiveUp
    ));
}

// Engine runs

/// Mainnet blocks 0..=10 and a checkpoint list over them.
pub(crate) struct TestChain {
    blocks: BTreeMap<block::Height, (Bytes, Arc<Header>, block::Hash)>,
    heights: HashMap<block::Hash, block::Height>,
    pub(crate) list: CheckpointList,
}

impl TestChain {
    pub(crate) fn new() -> Arc<Self> {
        let mut blocks = BTreeMap::new();
        let mut heights = HashMap::new();
        for height in 0..=10 {
            let raw = zebra_test::vectors::MAINNET_BLOCKS[&height];
            let block: Block = raw.zcash_deserialize_into().unwrap();
            let hash = block.hash();
            blocks.insert(
                block::Height(height),
                (Bytes::from_static(raw), block.header.clone(), hash),
            );
            heights.insert(hash, block::Height(height));
        }
        let list = CheckpointList::from_list(
            [0, 5, 10].map(|height| (block::Height(height), blocks[&block::Height(height)].2)),
        )
        .unwrap();

        Arc::new(TestChain {
            blocks,
            heights,
            list,
        })
    }

    pub(crate) fn hash(&self, height: u32) -> block::Hash {
        self.blocks[&block::Height(height)].2
    }

    fn anchors(&self) -> Anchors {
        Anchors::new(&self.list)
    }
}

/// How a mock peer behaves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Behaviour {
    Honest,
    /// Opens streams and never sends.
    Silent,
    /// Sends a body that does not match its header at this height.
    CorruptAt(block::Height),
    /// Fails its first this many requests with a transport error, then is
    /// honest.
    FailFirst(u32),
    /// Answers every request with `REFUSED`.
    Refused,
}

/// A simulated version 2 peer serving [`TestChain`].
#[derive(Clone)]
pub(crate) struct MockPeer {
    id: ConnectionId,
    ip: IpAddr,
    chain: Arc<TestChain>,
    behaviour: Behaviour,
    remote_tip: block::Height,
    closed: Arc<AtomicBool>,
    violations: Arc<AtomicU32>,
    windows: Arc<AtomicU32>,
    dropped_streams: Arc<AtomicUsize>,
    requests: Arc<AtomicU32>,
    penalties: Arc<AtomicU32>,
}

impl MockPeer {
    /// The same peer, connecting from `ip`.
    pub(crate) fn with_ip(mut self, ip: IpAddr) -> Self {
        self.ip = ip;
        self
    }

    /// The peer's IP address.
    pub(crate) fn peer_ip(&self) -> IpAddr {
        self.ip
    }

    /// The same peer, advertising `tip`.
    pub(crate) fn with_tip(mut self, tip: block::Height) -> Self {
        self.remote_tip = tip;
        self
    }

    pub(crate) fn new(id: u64, chain: Arc<TestChain>, behaviour: Behaviour) -> Self {
        MockPeer {
            id: ConnectionId(id),
            // A distinct address for each test ID.
            ip: Ipv4Addr::from(u32::try_from(id).expect("test IDs are small")).into(),
            chain,
            behaviour,
            remote_tip: block::Height(1_000),
            closed: Arc::default(),
            violations: Arc::default(),
            windows: Arc::default(),
            dropped_streams: Arc::default(),
            requests: Arc::default(),
            penalties: Arc::default(),
        }
    }
}

pub(crate) struct MockStream {
    items: VecDeque<RangeItem>,
    silent: bool,
    dropped: Arc<AtomicUsize>,
    bytes_received: u64,
}

impl Drop for MockStream {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

impl RangeStream for MockStream {
    fn next(&mut self) -> BoxFuture<'_, Option<Result<RangeItem, SyncPeerError>>> {
        if self.silent {
            return futures::future::pending().boxed();
        }
        // Take the item only when the future is polled, so that the stream
        // is cancellation-safe like `BlockRangeStream`.
        async move {
            let item = self.items.pop_front();
            if let Some(RangeItem::Block { raw, .. }) = &item {
                // `usize` fits in `u64`.
                self.bytes_received += raw.len() as u64;
            }
            item.map(Ok)
        }
        .boxed()
    }

    fn bytes_received(&self) -> u64 {
        self.bytes_received
    }
}

impl TestChain {
    /// The `get-hashes` entries of the chain's blocks `start..start + count`.
    fn entries(&self, start: block::Height, count: u32) -> Vec<SyncHashEntry> {
        self.blocks
            .range(start..)
            .take(count as usize)
            .map(|(_, (raw, _, _))| {
                let block: Block = raw.zcash_deserialize_into().unwrap();
                SyncHashEntry::for_block(&block, raw.len())
            })
            .collect()
    }

    /// A known-hash list of the whole chain, in chunks of 4 heights.
    fn known_hash_list(&self) -> Arc<dyn ChunkPins> {
        let max_height = *self.blocks.keys().last().unwrap();
        let entries = self.entries(block::Height(0), max_height.0 + 1);
        let chunk_heights = 4;
        let spec = KnownHashListSpec {
            max_height,
            chunk_heights,
            chunk_hashes: entries
                .chunks(chunk_heights as usize)
                .map(chunk_pin)
                .collect::<Vec<_>>()
                .into(),
        };

        Arc::new(PinnedList {
            spec,
            network: Network::Mainnet,
        })
    }
}

impl HashPeer for MockPeer {
    type Id = ConnectionId;

    fn id(&self) -> ConnectionId {
        self.id
    }

    async fn get_hashes(
        &self,
        start: block::Height,
        count: u32,
    ) -> Result<Vec<SyncHashEntry>, SyncPeerError> {
        Ok(self.chain.entries(start, count))
    }
}

/// Serializes `raw` with its coinbase duplicated: the header and the coinbase
/// height are unchanged, but the body no longer matches the header.
fn corrupt(raw: &[u8]) -> Bytes {
    let mut block: Block = raw.zcash_deserialize_into().unwrap();
    let coinbase = block.transactions[0].clone();
    block.transactions.push(coinbase);
    block.zcash_serialize_to_vec().unwrap().into()
}

impl RangePeer for MockPeer {
    type Stream = MockStream;

    fn id(&self) -> ConnectionId {
        self.id
    }

    fn ip(&self) -> IpAddr {
        self.ip
    }

    fn services(&self) -> PeerServices {
        PeerServices::NODE_NETWORK
    }

    fn remote_tip_height(&self) -> block::Height {
        self.remote_tip
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn get_block_range(
        &self,
        request: GetBlockRange,
    ) -> BoxFuture<'static, Result<Self::Stream, SyncPeerError>> {
        let peer = self.clone();
        async move {
            if peer.is_closed() {
                return Err(SyncPeerError::Transport("closed".to_string()));
            }
            let request_number = peer.requests.fetch_add(1, Ordering::SeqCst);
            if peer.behaviour == Behaviour::Refused {
                return Err(SyncPeerError::Refused);
            }
            if matches!(peer.behaviour, Behaviour::FailFirst(count) if request_number < count) {
                return Err(SyncPeerError::Transport("failing first".to_string()));
            }
            let Some(&top) = peer.chain.heights.get(&request.final_hash) else {
                return Err(SyncPeerError::NotFound);
            };

            let mut items = VecDeque::new();
            let mut height = top;
            let mut delivered = 0;
            let mut next_expected = request.final_hash;
            while delivered < request.count {
                let (raw, header, hash) = peer.chain.blocks[&height].clone();
                let raw = match peer.behaviour {
                    Behaviour::CorruptAt(at) if at == height => corrupt(&raw),
                    _ => raw,
                };
                next_expected = header.previous_block_hash;
                items.push_back(RangeItem::Block { raw, header, hash });
                delivered += 1;
                match height.previous() {
                    Ok(previous) => height = previous,
                    Err(_) => break,
                }
            }
            items.push_back(RangeItem::End {
                delivered,
                next_expected,
            });

            Ok(MockStream {
                items,
                silent: peer.behaviour == Behaviour::Silent,
                dropped: peer.dropped_streams.clone(),
                bytes_received: 0,
            })
        }
        .boxed()
    }

    fn report_violation(&self, _reason: String) {
        self.violations.fetch_add(1, Ordering::SeqCst);
        self.closed.store(true, Ordering::SeqCst);
    }

    fn penalize(&self, points: u32, _reason: String) -> BoxFuture<'static, ()> {
        self.penalties.fetch_add(points, Ordering::SeqCst);
        async {}.boxed()
    }

    fn set_bulk_receive_window(&self, bytes: u32) {
        self.windows.store(bytes, Ordering::SeqCst);
    }
}

/// The mock state service type.
pub(crate) type MockState = tower::util::BoxCloneService<zs::Request, zs::Response, BoxError>;

/// How the mock state fails a commit.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum MockFailure {
    /// An ordinary commit error.
    Failed,
    /// The block's authorizing data does not match its header's commitment.
    AuthMismatch,
}

impl MockFailure {
    fn error(self) -> BoxError {
        match self {
            MockFailure::Failed => "injected commit failure".into(),
            MockFailure::AuthMismatch => {
                let error = zs::ValidateContextError::from(
                    block::CommitmentError::InvalidChainHistoryBlockTxAuthCommitment {
                        expected: [0; 32],
                        actual: [1; 32],
                    },
                );
                zs::CommitCheckpointVerifiedError::from(error).into()
            }
        }
    }
}

fn write_task_exited() -> BoxError {
    zs::CommitCheckpointVerifiedError::from(zs::CommitBlockError::new_write_task_exited()).into()
}

type MockCommit = (
    CheckpointVerifiedBlock,
    tokio::sync::oneshot::Sender<Result<zs::Response, BoxError>>,
);

/// A mock state: sends each committed height on `commits`, and fails a
/// commit when `fail(height, attempt)` says so.
pub(crate) fn mock_state(
    commits: mpsc::UnboundedSender<(block::Height, block::Hash)>,
    fail: impl Fn(block::Height, u32) -> bool + Send + 'static,
) -> MockState {
    mock_state_with(commits, move |height, attempt| {
        fail(height, attempt).then_some(MockFailure::Failed)
    })
}

/// A mock of the state's finalized commit queue, from a genesis tip: an
/// actor that commits blocks in height order, and fails a commit when
/// `fail(height, attempt)` says so, where `attempt` counts the submissions
/// of that height.
///
/// Like the state's reset (`service.rs` `drain_finalized_resets`, and the
/// write task dropping the blocks above a failure), a failure answers
/// `WriteTaskExited` to every queued block above it, and to every later
/// submission above it until the failed height is submitted again.
pub(crate) fn mock_state_with(
    commits: mpsc::UnboundedSender<(block::Height, block::Hash)>,
    fail: impl Fn(block::Height, u32) -> Option<MockFailure> + Send + 'static,
) -> MockState {
    mock_state_with_tip(commits, fail, None)
}

/// [`mock_state_with`], which also moves `tip` before answering a commit,
/// like the state's write task.
pub(crate) fn mock_state_with_tip(
    commits: mpsc::UnboundedSender<(block::Height, block::Hash)>,
    fail: impl Fn(block::Height, u32) -> Option<MockFailure> + Send + 'static,
    tip_sender: Option<zebra_chain::chain_tip::mock::MockChainTipSender>,
) -> MockState {
    mock_state_with_stall(commits, fail, tip_sender, |_, _| None)
}

/// [`mock_state_with_tip`], whose write task awaits `stall(height, attempt)`
/// before it commits that height: requests sent meanwhile stay queued, and
/// are committed once the stall ends, even if their senders stopped waiting.
pub(crate) fn mock_state_with_stall(
    commits: mpsc::UnboundedSender<(block::Height, block::Hash)>,
    fail: impl Fn(block::Height, u32) -> Option<MockFailure> + Send + 'static,
    tip_sender: Option<zebra_chain::chain_tip::mock::MockChainTipSender>,
    stall: impl Fn(block::Height, u32) -> Option<BoxFuture<'static, ()>> + Send + 'static,
) -> MockState {
    let (requests_tx, mut requests) = mpsc::unbounded_channel::<MockCommit>();

    tokio::spawn(async move {
        let mut tip = block::Height(0);
        // Queued blocks, with their attempt numbers.
        let mut queue: BTreeMap<block::Height, (MockCommit, u32)> = BTreeMap::new();
        let mut reset_next: Option<block::Height> = None;
        let mut attempts: HashMap<block::Height, u32> = HashMap::new();

        while let Some((block, rsp)) = requests.recv().await {
            let height = block.height;
            let attempt = attempts.entry(height).or_default();
            let this_attempt = *attempt;
            *attempt += 1;

            if height <= tip {
                let _ = rsp.send(Err(zs::CommitCheckpointVerifiedError::from(
                    zs::CommitBlockError::new_duplicate(
                        Some(block.hash.into()),
                        zs::KnownBlock::Finalized,
                    ),
                )
                .into()));
                continue;
            }
            match reset_next {
                Some(next) if height > next => {
                    let _ = rsp.send(Err(write_task_exited()));
                    continue;
                }
                Some(next) if height == next => reset_next = None,
                _ => {}
            }

            if let Some(((_, replaced), _)) = queue.insert(height, ((block, rsp), this_attempt)) {
                let _ = replaced.send(Err(zs::CommitCheckpointVerifiedError::from(
                    zs::CommitBlockError::new_duplicate(None, zs::KnownBlock::Queue),
                )
                .into()));
            }

            while let Some(((block, rsp), attempt)) =
                queue.remove(&tip.next().expect("test heights are small"))
            {
                if let Some(stall) = stall(block.height, attempt) {
                    stall.await;
                }
                if let Some(failure) = fail(block.height, attempt) {
                    reset_next = Some(block.height);
                    let _ = rsp.send(Err(failure.error()));
                    for ((_, dropped), _) in std::mem::take(&mut queue).into_values() {
                        let _ = dropped.send(Err(write_task_exited()));
                    }
                    break;
                }
                tip = block.height;
                if let Some(tip_sender) = &tip_sender {
                    tip_sender.send_best_tip_height(block.height);
                    tip_sender.send_best_tip_hash(block.hash);
                }
                // The receiver outlives the engine in these tests.
                let _ = commits.send((block.height, block.hash));
                let _ = rsp.send(Ok(zs::Response::Committed(block.hash)));
            }
        }
    });

    tower::util::BoxCloneService::new(tower::service_fn(move |request: zs::Request| {
        let requests = requests_tx.clone();
        async move {
            let zs::Request::CommitCheckpointVerifiedBlock(block) = request else {
                panic!("the engine only commits");
            };
            let (rsp_tx, rsp_rx) = tokio::sync::oneshot::channel();
            if requests.send((block, rsp_tx)).is_err() {
                return Err(write_task_exited());
            }
            rsp_rx.await.unwrap_or_else(|_| Err(write_task_exited()))
        }
    }))
}

struct Harness {
    outcome: EngineOutcome,
    /// The shared lookahead charge after the run.
    charged: u64,
    committed: Vec<block::Height>,
    unit_lengths: Vec<usize>,
}

async fn run_engine(
    peers: Vec<MockPeer>,
    fail: impl Fn(block::Height, u32) -> bool + Send + 'static,
    hints_gate: Option<watch::Receiver<bool>>,
) -> Harness {
    run_engine_with(
        peers,
        move |height, attempt| fail(height, attempt).then_some(MockFailure::Failed),
        hints_gate,
    )
    .await
}

async fn run_engine_with(
    peers: Vec<MockPeer>,
    fail: impl Fn(block::Height, u32) -> Option<MockFailure> + Send + 'static,
    hints_gate: Option<watch::Receiver<bool>>,
) -> Harness {
    let (_peers_tx, peers_rx) = watch::channel(Arc::<[MockPeer]>::from(peers));
    run_engine_on(peers_rx, fail, hints_gate, None).await
}

/// Runs the engine from height 1 to 10 on `peers`, with the known-hash
/// source `hashes`.
async fn run_engine_on(
    peers_rx: watch::Receiver<Arc<[MockPeer]>>,
    fail: impl Fn(block::Height, u32) -> Option<MockFailure> + Send + 'static,
    hints_gate: Option<watch::Receiver<bool>>,
    hashes: Option<Box<dyn HashSource>>,
) -> Harness {
    let chain = peers_rx.borrow()[0].chain.clone();
    let (commits_tx, mut commits_rx) = mpsc::unbounded_channel();
    let (lengths_tx, mut lengths_rx) = mpsc::channel(16);
    let charged = Charged::default();

    let outcome = tokio::time::timeout(
        Duration::from_secs(60 * 60),
        EngineRun {
            config: EngineConfig::default(),
            network: Network::Mainnet,
            anchors: chain.anchors(),
            start: block::Height(1),
            target: block::Height(10),
            peers: peers_rx,
            state: mock_state_with(commits_tx, fail),
            hints_gate,
            unit_lengths: lengths_tx,
            charged: charged.clone(),
            blamed: BTreeSet::new(),
            hashes,
        }
        .run(),
    )
    .await
    .expect("the engine finishes");

    let mut committed = Vec::new();
    while let Ok((height, _)) = commits_rx.try_recv() {
        committed.push(height);
    }
    let mut unit_lengths = Vec::new();
    while let Ok(length) = lengths_rx.try_recv() {
        unit_lengths.push(length);
    }

    Harness {
        outcome,
        charged: charged.load(Ordering::SeqCst),
        committed,
        unit_lengths,
    }
}

fn all_heights() -> BTreeSet<block::Height> {
    (1..=10).map(block::Height).collect()
}

fn honest_pair(chain: &Arc<TestChain>) -> Vec<MockPeer> {
    vec![
        MockPeer::new(1, chain.clone(), Behaviour::Honest),
        MockPeer::new(2, chain.clone(), Behaviour::Honest),
    ]
}

#[tokio::test]
async fn engine_syncs_to_the_target() {
    let _init_guard = zebra_test::init();

    let chain = TestChain::new();
    let peers = honest_pair(&chain);
    let harness = run_engine(peers.clone(), |_, _| false, None).await;

    assert_eq!(
        harness.outcome,
        EngineOutcome {
            exit: EngineExit::TargetReached,
            base: block::Height(11),
            blamed: BTreeSet::new(),
            known_hash_units: 0,
        }
    );
    assert_eq!(harness.committed.len(), 10, "each height is committed once");
    assert_eq!(
        harness.committed.iter().copied().collect::<BTreeSet<_>>(),
        all_heights()
    );
    assert_eq!(harness.unit_lengths, vec![5, 5]);
    for peer in &peers {
        assert_eq!(
            peer.windows.load(Ordering::SeqCst),
            DESELECTED_RECEIVE_WINDOW,
            "receive windows are restored after the run"
        );
    }
}

#[tokio::test]
async fn engine_waits_for_the_hints_gate() {
    let _init_guard = zebra_test::init();

    let chain = TestChain::new();
    let (gate_tx, gate_rx) = watch::channel(false);
    let commits = Arc::new(AtomicU32::new(0));

    let opener = {
        let commits = commits.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let before = commits.load(Ordering::SeqCst);
            gate_tx.send_replace(true);
            before
        })
    };
    let counted = commits.clone();
    let harness = run_engine(
        honest_pair(&chain),
        move |_, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            false
        },
        Some(gate_rx),
    )
    .await;

    assert_eq!(
        opener.await.unwrap(),
        0,
        "nothing commits while the gate is closed"
    );
    assert_eq!(harness.outcome.exit, EngineExit::TargetReached);
    assert_eq!(harness.committed.len(), 10);
}

#[tokio::test]
async fn engine_keeps_a_closed_gate_through_frontier_failures() {
    let _init_guard = zebra_test::init();
    tokio::time::pause();

    let chain = TestChain::new();
    // Each peer fails more requests than `FRONTIER_FAILURE_LIMIT` while the
    // gate is closed.
    let peers: Vec<MockPeer> = (1..=2)
        .map(|id| {
            MockPeer::new(
                id,
                chain.clone(),
                Behaviour::FailFirst(FRONTIER_FAILURE_LIMIT + 1),
            )
        })
        .collect();
    let (gate_tx, gate_rx) = watch::channel(false);
    let commits = Arc::new(AtomicU32::new(0));

    let opener = {
        let commits = commits.clone();
        let peers = peers.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(20 * 60)).await;
            let failed: u32 = peers
                .iter()
                .map(|peer| peer.requests.load(Ordering::SeqCst))
                .sum();
            let before = commits.load(Ordering::SeqCst);
            gate_tx.send_replace(true);
            (failed, before)
        })
    };
    let counted = commits.clone();
    let harness = run_engine(
        peers,
        move |_, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            false
        },
        Some(gate_rx),
    )
    .await;

    let (failed, before) = opener.await.unwrap();
    assert!(
        failed > FRONTIER_FAILURE_LIMIT,
        "the frontier failed {failed} times while the gate was closed"
    );
    assert_eq!(before, 0, "nothing commits while the gate is closed");
    assert_eq!(harness.outcome.exit, EngineExit::TargetReached);
    assert_eq!(harness.committed.len(), 10);
}

#[tokio::test]
async fn engine_treats_a_dropped_gate_as_open() {
    let _init_guard = zebra_test::init();

    let chain = TestChain::new();
    let (gate_tx, gate_rx) = watch::channel(false);
    drop(gate_tx);
    let harness = run_engine(honest_pair(&chain), |_, _| false, Some(gate_rx)).await;

    assert_eq!(harness.outcome.exit, EngineExit::TargetReached);
}

#[tokio::test]
async fn engine_refetches_a_corrupt_block_from_another_peer() {
    let _init_guard = zebra_test::init();

    let chain = TestChain::new();
    let corrupt = MockPeer::new(1, chain.clone(), Behaviour::CorruptAt(block::Height(3)));
    let honest = MockPeer::new(2, chain.clone(), Behaviour::Honest);
    let harness = run_engine(vec![corrupt.clone(), honest], |_, _| false, None).await;

    assert_eq!(harness.outcome.exit, EngineExit::TargetReached);
    assert_eq!(
        harness.committed.iter().copied().collect::<BTreeSet<_>>(),
        all_heights()
    );
    assert!(
        corrupt.violations.load(Ordering::SeqCst) >= 1,
        "the corrupt peer is reported"
    );
}

#[tokio::test]
async fn engine_end_game_beats_a_silent_peer() {
    let _init_guard = zebra_test::init();

    let chain = TestChain::new();
    let silent = MockPeer::new(1, chain.clone(), Behaviour::Silent);
    let honest = MockPeer::new(2, chain.clone(), Behaviour::Honest);
    let harness = run_engine(vec![silent.clone(), honest], |_, _| false, None).await;

    assert_eq!(harness.outcome.exit, EngineExit::TargetReached);
    assert_eq!(harness.committed.len(), 10);
    // Let the aborted stream tasks unwind.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        silent.dropped_streams.load(Ordering::SeqCst) >= 1,
        "the silent peer's stream is cancelled"
    );
}

#[tokio::test]
async fn engine_retries_a_failed_commit_with_another_copy() {
    let _init_guard = zebra_test::init();

    let chain = TestChain::new();
    // Height 3 fails its first commit: the state's reset drops 4 and 5.
    let attempts: Arc<Vec<AtomicU32>> = Arc::new((0..=10).map(|_| AtomicU32::new(0)).collect());
    let counted = attempts.clone();
    let harness = run_engine(
        honest_pair(&chain),
        move |height, attempt| {
            counted[height.0 as usize].fetch_add(1, Ordering::SeqCst);
            height.0 == 3 && attempt == 0
        },
        None,
    )
    .await;

    assert_eq!(harness.outcome.exit, EngineExit::TargetReached);
    assert_eq!(
        harness.committed.iter().copied().collect::<BTreeSet<_>>(),
        all_heights()
    );
    assert_eq!(
        attempts[3].load(Ordering::SeqCst),
        2,
        "one retry with a new copy"
    );
    for height in 4..=5 {
        assert!(
            attempts[height].load(Ordering::SeqCst) <= 3,
            "blocks dropped by the reset are resubmitted without spinning"
        );
    }
}

#[tokio::test]
async fn engine_refetches_through_repeated_auth_data_mismatches() {
    let _init_guard = zebra_test::init();
    // Each lying copy backs its peer off.
    tokio::time::pause();

    let chain = TestChain::new();
    let peers = honest_pair(&chain);
    // More mismatches at one height than any defect bound would allow.
    let harness = run_engine_with(
        peers.clone(),
        |height, attempt| (height.0 == 3 && attempt < 4).then_some(MockFailure::AuthMismatch),
        None,
    )
    .await;

    assert_eq!(harness.outcome.exit, EngineExit::TargetReached);
    assert_eq!(
        harness.committed.iter().copied().collect::<BTreeSet<_>>(),
        all_heights()
    );
    let penalties: u32 = peers
        .iter()
        .map(|peer| peer.penalties.load(Ordering::SeqCst))
        .sum();
    assert_eq!(
        penalties,
        4 * AUTH_COMMITMENT_PENALTY,
        "each delivering peer is penalized"
    );
}

#[tokio::test]
async fn engine_hands_back_when_auth_data_mismatches_persist() {
    let _init_guard = zebra_test::init();
    tokio::time::pause();

    let chain = TestChain::new();
    let peers = honest_pair(&chain);
    let harness = run_engine_with(
        peers.clone(),
        |height, _| (height.0 == 3).then_some(MockFailure::AuthMismatch),
        None,
    )
    .await;

    assert_eq!(
        harness.outcome,
        EngineOutcome {
            exit: EngineExit::NoProgress,
            base: block::Height(3),
            blamed: peers.iter().map(|peer| peer.peer_ip()).collect(),
            known_hash_units: 0,
        },
        "lying peers make the engine hand back, not fail, and are blamed"
    );
    let penalties: u32 = peers
        .iter()
        .map(|peer| peer.penalties.load(Ordering::SeqCst))
        .sum();
    assert_eq!(
        penalties,
        FRONTIER_FAILURE_LIMIT * AUTH_COMMITMENT_PENALTY,
        "every lying copy is penalized, the last one too"
    );
}

/// Peers that keep refusing ranges are blamed when the run hands back for
/// too few peers, so the supervisor does not claim again with them.
#[tokio::test]
async fn engine_blames_peers_that_refuse_ranges() {
    let _init_guard = zebra_test::init();
    tokio::time::pause();

    let chain = TestChain::new();
    let peers = vec![
        MockPeer::new(1, chain.clone(), Behaviour::Refused),
        MockPeer::new(2, chain.clone(), Behaviour::Refused),
    ];
    let harness = run_engine(peers.clone(), |_, _| false, None).await;

    assert_eq!(
        harness.outcome,
        EngineOutcome {
            exit: EngineExit::TooFewPeers,
            base: block::Height(1),
            blamed: peers.iter().map(|peer| peer.peer_ip()).collect(),
            known_hash_units: 0,
        },
        "both refusing peers are blamed"
    );
    assert!(
        peers
            .iter()
            .all(|peer| peer.requests.load(Ordering::SeqCst) >= 2),
        "each peer refused at least twice"
    );
}

#[tokio::test]
async fn engine_stops_when_the_state_write_task_is_gone() {
    let _init_guard = zebra_test::init();

    let chain = TestChain::new();
    let (lengths_tx, _lengths_rx) = mpsc::channel(16);
    let (_peers_tx, peers_rx) = watch::channel(Arc::<[MockPeer]>::from(honest_pair(&chain)));
    // Every commit answers `WriteTaskExited`, like a state whose write task
    // exited.
    let state: MockState =
        tower::util::BoxCloneService::new(tower::service_fn(|_: zs::Request| async {
            Err::<zs::Response, BoxError>(write_task_exited())
        }));

    let outcome = tokio::time::timeout(
        Duration::from_secs(60),
        EngineRun {
            config: EngineConfig::default(),
            network: Network::Mainnet,
            anchors: chain.anchors(),
            start: block::Height(1),
            target: block::Height(10),
            peers: peers_rx,
            state,
            hints_gate: None,
            unit_lengths: lengths_tx,
            charged: Charged::default(),
            blamed: BTreeSet::new(),
            hashes: None,
        }
        .run(),
    )
    .await
    .expect("the engine finishes");

    assert_eq!(
        outcome,
        EngineOutcome {
            exit: EngineExit::Shutdown,
            base: block::Height(1),
            blamed: BTreeSet::new(),
            known_hash_units: 0,
        }
    );
}

#[tokio::test]
async fn engine_stops_on_a_deterministic_commit_failure() {
    let _init_guard = zebra_test::init();

    let chain = TestChain::new();
    let harness = run_engine(honest_pair(&chain), |height, _| height.0 == 3, None).await;

    assert!(
        matches!(harness.outcome.exit, EngineExit::Defect(_)),
        "{:?}",
        harness.outcome
    );
    assert_eq!(
        harness.outcome.base,
        block::Height(3),
        "drained at the failure"
    );
    assert!(!harness.committed.contains(&block::Height(3)));
    assert_eq!(
        harness.charged, 0,
        "the delivered blocks above the failure are not charged after the run"
    );
}

#[tokio::test]
async fn engine_hands_back_without_peers() {
    let _init_guard = zebra_test::init();
    tokio::time::pause();

    let chain = TestChain::new();
    let (commits_tx, _commits_rx) = mpsc::unbounded_channel();
    let (lengths_tx, _lengths_rx) = mpsc::channel(16);
    let (_peers_tx, peers_rx) = watch::channel(Arc::<[MockPeer]>::from(Vec::new()));
    // A charge left by an earlier run.
    let charged = Charged::new(1_000.into());

    let outcome = EngineRun {
        config: EngineConfig::default(),
        network: Network::Mainnet,
        anchors: chain.anchors(),
        start: block::Height(1),
        target: block::Height(10),
        peers: peers_rx,
        state: mock_state(commits_tx, |_, _| false),
        hints_gate: None,
        unit_lengths: lengths_tx,
        charged: charged.clone(),
        blamed: BTreeSet::new(),
        hashes: None,
    }
    .run()
    .await;

    assert_eq!(
        outcome,
        EngineOutcome {
            exit: EngineExit::TooFewPeers,
            base: block::Height(1),
            blamed: BTreeSet::new(),
            known_hash_units: 0,
        }
    );
    assert_eq!(charged.load(Ordering::SeqCst), 0, "nothing stays charged");
}

/// A run does not download from the peers the last run blamed, even over a
/// new connection.
#[tokio::test]
async fn engine_skips_blamed_peers() {
    let _init_guard = zebra_test::init();

    let chain = TestChain::new();
    let peers: Vec<MockPeer> = (1..=3)
        .map(|id| MockPeer::new(id, chain.clone(), Behaviour::Honest))
        .collect();
    let blamed_peer = peers[0].clone();
    // The blamed peer reconnected: same address, new connection.
    let reconnected = MockPeer::new(4, chain.clone(), Behaviour::Honest).with_ip(blamed_peer.ip);
    let peers: Vec<MockPeer> = peers.into_iter().chain([reconnected.clone()]).collect();
    let (commits_tx, _commits_rx) = mpsc::unbounded_channel();
    let (lengths_tx, _lengths_rx) = mpsc::channel(16);
    let (_peers_tx, peers_rx) = watch::channel(Arc::<[MockPeer]>::from(peers));

    let outcome = tokio::time::timeout(
        Duration::from_secs(60),
        EngineRun {
            config: EngineConfig::default(),
            network: Network::Mainnet,
            anchors: chain.anchors(),
            start: block::Height(1),
            target: block::Height(10),
            peers: peers_rx,
            state: mock_state(commits_tx, |_, _| false),
            hints_gate: None,
            unit_lengths: lengths_tx,
            charged: Charged::default(),
            blamed: [blamed_peer.ip].into_iter().collect(),
            hashes: None,
        }
        .run(),
    )
    .await
    .expect("the engine finishes");

    assert_eq!(outcome.exit, EngineExit::TargetReached);
    assert_eq!(
        blamed_peer.requests.load(Ordering::SeqCst),
        0,
        "the blamed peer is not asked for blocks"
    );
    assert_eq!(
        reconnected.requests.load(Ordering::SeqCst),
        0,
        "the blamed peer's new connection is not asked for blocks"
    );
}

/// Known-hash entries for the test chain, resident from `from` up, with a
/// wrong hash at `wrong`.
#[derive(Debug)]
struct FixedEntries {
    entries: Vec<SyncHashEntry>,
    from: block::Height,
    wrong: Option<block::Height>,
}

impl HashSource for FixedEntries {
    fn max_height(&self) -> block::Height {
        block::Height(10)
    }

    fn entry(&self, height: block::Height) -> Option<SyncHashEntry> {
        if height < self.from {
            return None;
        }
        let mut entry = *self.entries.get(height.0 as usize)?;
        if Some(height) == self.wrong {
            entry.hash = block::Hash([0xab; 32]);
        }
        Some(entry)
    }

    fn request(&mut self, _start: block::Height, _end: block::Height) {}

    fn changed(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), BoxError>> + Send + '_>>
    {
        Box::pin(std::future::pending())
    }

    fn release_below(&mut self, _height: block::Height) {}
}

/// With the known-hash list resident, the engine plans its units at known
/// hashes, with exact sizes ending anywhere, and commits every block as with
/// checkpoint anchors.
#[tokio::test]
async fn engine_anchors_units_at_known_hashes() {
    let _init_guard = zebra_test::init();

    let chain = TestChain::new();
    let (_peers_tx, peers_rx) = watch::channel(Arc::<[MockPeer]>::from(honest_pair(&chain)));

    // The supervisor starts fetching before it claims block download: here the chunks are
    // fetched, verified against their pins, and resident before the run starts.
    let mut hashes = KnownHashes::with_pins(chain.known_hash_list(), peers_rx.clone());
    hashes.request(block::Height(1), block::Height(10));
    while hashes.entry(block::Height(10)).is_none() {
        tokio::time::timeout(Duration::from_secs(10), hashes.changed())
            .await
            .expect("the chunks are fetched")
            .expect("the source does not fail");
    }

    let harness = run_engine_on(peers_rx, |_, _| None, None, Some(Box::new(hashes))).await;

    assert_eq!(harness.outcome.exit, EngineExit::TargetReached);
    assert_eq!(harness.outcome.base, block::Height(11));
    assert!(
        harness.outcome.known_hash_units > 0,
        "units are anchored at known hashes: {:?}",
        harness.outcome
    );
    assert_eq!(
        harness.committed.iter().copied().collect::<BTreeSet<_>>(),
        all_heights()
    );
    assert_eq!(harness.committed.len(), 10, "each height is committed once");
}

/// Units whose known-hash entries are not resident are anchored at
/// checkpoints.
#[tokio::test]
async fn engine_falls_back_to_checkpoints_without_entries() {
    let _init_guard = zebra_test::init();

    let chain = TestChain::new();
    let (_peers_tx, peers_rx) = watch::channel(Arc::<[MockPeer]>::from(honest_pair(&chain)));
    let hashes = FixedEntries {
        entries: chain.entries(block::Height(0), 11),
        from: block::Height(11),
        wrong: None,
    };

    let harness = run_engine_on(peers_rx, |_, _| None, None, Some(Box::new(hashes))).await;

    assert_eq!(harness.outcome.exit, EngineExit::TargetReached);
    assert_eq!(harness.outcome.known_hash_units, 0);
    assert_eq!(harness.unit_lengths, vec![5, 5], "checkpoint intervals");
}

/// A block chained to a checkpoint that is not the known-hash list's entry
/// means the list and the checkpoints disagree: a defect.
#[tokio::test]
async fn engine_stops_when_the_list_disagrees_with_the_checkpoints() {
    let _init_guard = zebra_test::init();

    let chain = TestChain::new();
    let (_peers_tx, peers_rx) = watch::channel(Arc::<[MockPeer]>::from(honest_pair(&chain)));
    // No entry at the frontier, so the frontier unit is anchored at a checkpoint.
    let hashes = FixedEntries {
        entries: chain.entries(block::Height(0), 11),
        from: block::Height(2),
        wrong: Some(block::Height(3)),
    };

    let harness = run_engine_on(peers_rx, |_, _| None, None, Some(Box::new(hashes))).await;

    assert!(
        matches!(&harness.outcome.exit, EngineExit::Defect(defect) if defect.contains("known-hash list")),
        "{:?}",
        harness.outcome
    );
    assert!(harness.outcome.base <= block::Height(3));
}
