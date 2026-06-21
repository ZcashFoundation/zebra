//! Local-only Zakura stream-6 block-sync throughput harness.

use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};

use tokio::{sync::watch, task::JoinHandle};
use zebra_chain::{
    block,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transaction::Transaction,
    transparent,
};
use zebra_jsonl_trace::{JsonlTraceConfig, JsonlTraceGuard, JsonlTracer};
use zebra_test::vectors::{BLOCK_MAINNET_1_BYTES, BLOCK_MAINNET_GENESIS_BYTES};

use super::{await_until, ZakuraTestCluster, ZakuraTestNode};
use crate::{
    zakura::{
        BlockApplyResult, BlockSizeEstimate, BlockSyncAction, BlockSyncBlockMeta, BlockSyncEvent,
        BlockSyncFrontiers, HeaderSyncAction, HeaderSyncFrontiers, ServicePeerLimits,
        ZakuraBlockSyncConfig, ZakuraLocalLimits,
    },
    BoxError, Config,
};

const DEFAULT_SEEDS: usize = 4;
const DEFAULT_BLOCKS: u32 = 100_000;
const DEFAULT_MAX_BLOCKS_PER_RESPONSE: u32 = 128;
const DEFAULT_MAX_INFLIGHT: u16 = 512;
const DEFAULT_FANOUT: usize = 1;
const SYNTHETIC_CORPUS_SEED: u64 = 0x5eed_5eed_b10c_0006;
const MIN_SYNTHETIC_TXS: usize = 1;
const MAX_SYNTHETIC_TXS: usize = 16;

#[derive(Clone)]
struct SyntheticBlockCorpus {
    blocks: Arc<Vec<Arc<block::Block>>>,
    sizes: Arc<Vec<usize>>,
    by_hash: Arc<HashMap<block::Hash, block::Height>>,
}

impl SyntheticBlockCorpus {
    fn generate(count: u32, seed: u64) -> Self {
        let template = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let mut blocks = Vec::with_capacity(usize::try_from(count).expect("u32 fits usize"));
        let mut sizes = Vec::with_capacity(usize::try_from(count).expect("u32 fits usize"));
        let mut by_hash = HashMap::new();
        let mut previous_hash = mainnet_genesis_hash();

        for height in 1..=count {
            let block = synthetic_block_at_height(
                &template,
                block::Height(height),
                previous_hash,
                splitmix64(seed ^ u64::from(height)),
            );
            previous_hash = block.hash();
            sizes.push(block_size(&block));
            by_hash.insert(block.hash(), block::Height(height));
            blocks.push(block);
        }

        Self {
            blocks: Arc::new(blocks),
            sizes: Arc::new(sizes),
            by_hash: Arc::new(by_hash),
        }
    }

    fn target_height(&self) -> block::Height {
        block::Height(
            u32::try_from(self.blocks.len()).expect("synthetic corpus length came from u32"),
        )
    }

    fn tip_hash(&self) -> block::Hash {
        self.block_at(self.target_height())
            .map(|block| block.hash())
            .unwrap_or_else(mainnet_genesis_hash)
    }

    fn block_at(&self, height: block::Height) -> Option<Arc<block::Block>> {
        let index = height.0.checked_sub(1)?;
        self.blocks
            .get(usize::try_from(index).expect("u32 fits usize"))
            .cloned()
    }

    fn size_at(&self, height: block::Height) -> Option<usize> {
        let index = height.0.checked_sub(1)?;
        self.sizes
            .get(usize::try_from(index).expect("u32 fits usize"))
            .copied()
    }

    fn height_for_hash(&self, hash: block::Hash) -> Option<block::Height> {
        self.by_hash.get(&hash).copied()
    }

    fn metas_between(
        &self,
        start: block::Height,
        end: block::Height,
    ) -> Vec<BlockSyncBlockMeta> {
        (start.0..=end.0)
            .filter_map(|height| {
                let height = block::Height(height);
                let block = self.block_at(height)?;
                let size = u32::try_from(self.size_at(height)?).ok()?;
                Some(BlockSyncBlockMeta {
                    height,
                    hash: block.hash(),
                    size: BlockSizeEstimate::Advertised(size),
                })
            })
            .collect()
    }

    fn blocks_in_range(
        &self,
        start: block::Height,
        count: u32,
        max_height: block::Height,
    ) -> Vec<(block::Height, Arc<block::Block>, usize)> {
        let end = start
            .0
            .checked_add(count.saturating_sub(1))
            .map(block::Height)
            .unwrap_or(max_height)
            .min(max_height);

        if start > end {
            return Vec::new();
        }

        (start.0..=end.0)
            .filter_map(|height| {
                let height = block::Height(height);
                Some((height, self.block_at(height)?, self.size_at(height)?))
            })
            .collect()
    }
}

#[derive(Clone)]
struct MockApplyFrontier {
    inner: Arc<StdMutex<MockApplyFrontierState>>,
    corpus: SyntheticBlockCorpus,
}

#[derive(Debug)]
struct MockApplyFrontierState {
    frontier: block::Height,
    frontier_hash: block::Hash,
}

#[derive(Copy, Clone, Debug)]
struct MockApplyOutcome {
    result: BlockApplyResult,
    frontiers: BlockSyncFrontiers,
}

impl MockApplyFrontier {
    fn new(corpus: SyntheticBlockCorpus) -> Self {
        Self {
            inner: Arc::new(StdMutex::new(MockApplyFrontierState {
                frontier: block::Height(0),
                frontier_hash: mainnet_genesis_hash(),
            })),
            corpus,
        }
    }

    fn apply(&self, block: &block::Block) -> MockApplyOutcome {
        let height = block
            .coinbase_height()
            .expect("synthetic block has a coinbase height");
        let hash = block.hash();
        let mut state = self
            .inner
            .lock()
            .expect("mock apply frontier mutex is not poisoned");

        let result = if height <= state.frontier {
            if self.corpus.height_for_hash(hash) == Some(height) {
                BlockApplyResult::Duplicate
            } else {
                BlockApplyResult::Rejected
            }
        } else if state.frontier.next().ok() != Some(height) {
            BlockApplyResult::Rejected
        } else if self.corpus.height_for_hash(hash) != Some(height) {
            BlockApplyResult::Rejected
        } else {
            state.frontier = height;
            state.frontier_hash = hash;
            BlockApplyResult::Committed
        };

        MockApplyOutcome {
            result,
            frontiers: BlockSyncFrontiers {
                finalized_height: state.frontier,
                verified_block_tip: state.frontier,
                verified_block_hash: state.frontier_hash,
            },
        }
    }
}

#[derive(Clone, Default)]
struct ThroughputStats {
    inner: Arc<StdMutex<ThroughputStatsState>>,
}

struct ThroughputStatsState {
    started: Instant,
    committed_blocks: u64,
    committed_bytes: u64,
    request_blocks: Vec<usize>,
    request_bytes: Vec<usize>,
    final_frontier: block::Height,
}

impl Default for ThroughputStatsState {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            committed_blocks: 0,
            committed_bytes: 0,
            request_blocks: Vec::new(),
            request_bytes: Vec::new(),
            final_frontier: block::Height(0),
        }
    }
}

#[derive(Debug)]
struct ThroughputSummary {
    elapsed: Duration,
    committed_blocks: u64,
    committed_bytes: u64,
    request_count: usize,
    request_blocks_p50: usize,
    request_blocks_p95: usize,
    request_bytes_p50: usize,
    request_bytes_p95: usize,
    final_frontier: block::Height,
}

impl ThroughputStats {
    fn record_commit(&self, height: block::Height, bytes: usize) {
        let mut state = self
            .inner
            .lock()
            .expect("throughput stats mutex is not poisoned");
        state.committed_blocks = state.committed_blocks.saturating_add(1);
        state.committed_bytes = state
            .committed_bytes
            .saturating_add(u64::try_from(bytes).expect("usize fits u64"));
        state.final_frontier = state.final_frontier.max(height);
    }

    fn record_request(&self, blocks: usize, bytes: usize) {
        let mut state = self
            .inner
            .lock()
            .expect("throughput stats mutex is not poisoned");
        state.request_blocks.push(blocks);
        state.request_bytes.push(bytes);
    }

    fn final_frontier(&self) -> block::Height {
        self.inner
            .lock()
            .expect("throughput stats mutex is not poisoned")
            .final_frontier
    }

    fn summary(&self) -> ThroughputSummary {
        let state = self
            .inner
            .lock()
            .expect("throughput stats mutex is not poisoned");
        ThroughputSummary {
            elapsed: state.started.elapsed(),
            committed_blocks: state.committed_blocks,
            committed_bytes: state.committed_bytes,
            request_count: state.request_blocks.len(),
            request_blocks_p50: percentile(state.request_blocks.clone(), 50),
            request_blocks_p95: percentile(state.request_blocks.clone(), 95),
            request_bytes_p50: percentile(state.request_bytes.clone(), 50),
            request_bytes_p95: percentile(state.request_bytes.clone(), 95),
            final_frontier: state.final_frontier,
        }
    }
}

#[derive(Clone, Debug)]
struct HarnessConfig {
    seeds: usize,
    blocks: u32,
    max_blocks_per_response: u32,
    max_inflight: u16,
    fanout: usize,
    trace_dir: Option<PathBuf>,
}

impl HarnessConfig {
    fn from_env() -> Self {
        Self {
            seeds: env_usize("ZAKURA_MOCK_BS_SEEDS", DEFAULT_SEEDS).max(1),
            blocks: env_u32("ZAKURA_MOCK_BS_BLOCKS", DEFAULT_BLOCKS).max(1),
            max_blocks_per_response: env_u32(
                "ZAKURA_MOCK_BS_MAX_BLOCKS_PER_RESPONSE",
                DEFAULT_MAX_BLOCKS_PER_RESPONSE,
            )
            .max(1),
            max_inflight: env_u16("ZAKURA_MOCK_BS_MAX_INFLIGHT", DEFAULT_MAX_INFLIGHT).max(1),
            fanout: env_usize("ZAKURA_MOCK_BS_FANOUT", DEFAULT_FANOUT).max(1),
            trace_dir: env::var_os("ZAKURA_MOCK_BS_TRACE_DIR")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
        }
    }

    fn block_sync_config(&self) -> ZakuraBlockSyncConfig {
        ZakuraBlockSyncConfig {
            max_blocks_per_response: self.max_blocks_per_response,
            max_inflight_requests: self.max_inflight,
            max_inflight_block_bytes: u64::MAX,
            max_submitted_block_applies: usize::from(self.max_inflight)
                .saturating_mul(
                    usize::try_from(self.max_blocks_per_response).expect("u32 fits usize"),
                )
                .max(1),
            request_timeout: Duration::from_secs(60),
            status_refresh_interval: Duration::from_millis(200),
            fanout: self.fanout,
            peer_limits: ServicePeerLimits {
                max_inbound_peers: self.seeds.saturating_add(1),
                max_outbound_peers: self.seeds.saturating_add(1),
                inbound_queue_depth: usize::from(self.max_inflight).saturating_mul(2).max(128),
                outbound_queue_depth: usize::from(self.max_inflight).saturating_mul(2).max(128),
                ..ServicePeerLimits::default()
            },
            ..ZakuraBlockSyncConfig::default()
        }
    }

    fn limits(&self) -> ZakuraLocalLimits {
        let mut limits = ZakuraLocalLimits::from_config(&Config::default());
        limits.max_connections = self.seeds.saturating_add(1).max(16);
        limits.max_pending_handshakes = self.seeds.saturating_add(1).max(16);
        limits.max_open_streams = 64;
        limits.max_inbound_queue_depth = 4096;
        limits.message_rate_per_second = 10_000;
        limits.stream_open_rate_per_second = 10_000;
        limits
    }
}

struct HarnessTrace {
    root: Option<PathBuf>,
    guards: Vec<JsonlTraceGuard>,
}

impl HarnessTrace {
    fn new(root: Option<PathBuf>) -> Self {
        Self {
            root,
            guards: Vec::new(),
        }
    }

    fn tracer_for_node(&mut self, seed: u64) -> JsonlTracer {
        let Some(root) = &self.root else {
            return JsonlTracer::noop();
        };
        let guard = JsonlTracer::spawn_guard_with_config(
            root.join(format!("node-{seed:02}")),
            JsonlTraceConfig {
                channel_capacity: 262_144,
                file_flush_interval: Duration::from_millis(100),
                ..JsonlTraceConfig::default()
            },
        );
        let tracer = guard.tracer();
        self.guards.push(guard);
        tracer
    }

    async fn shutdown(self) {
        for guard in self.guards {
            guard.shutdown().await;
        }
    }

    fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }
}

async fn spawn_mock_node(
    cluster: &mut ZakuraTestCluster,
    seed: u64,
    initial_frontiers: BlockSyncFrontiers,
    corpus: &SyntheticBlockCorpus,
    config: &HarnessConfig,
    trace: &mut HarnessTrace,
) -> Result<usize, BoxError> {
    let anchor = (block::Height(0), mainnet_genesis_hash());
    let builder = ZakuraTestNode::builder(seed)
        .limits(config.limits())
        .max_connections_per_ip(config.seeds.saturating_add(1))
        .tracer(trace.tracer_for_node(seed))
        .header_sync_driver(
            Config::default().network,
            anchor,
            HeaderSyncFrontiers {
                finalized_height: initial_frontiers.finalized_height,
                verified_block_tip: initial_frontiers.verified_block_tip,
                verified_block_hash: initial_frontiers.verified_block_hash,
            },
            Some((corpus.target_height(), corpus.tip_hash())),
        )
        .block_sync_config(config.block_sync_config());

    cluster.spawn_node_with_builder(builder).await
}

async fn drain_header_sync_actions(node: &ZakuraTestNode) -> JoinHandle<()> {
    let supervisor = node.supervisor();
    let mut actions = node
        .take_header_sync_actions()
        .await
        .expect("header-sync action receiver is enabled");

    tokio::spawn(async move {
        while let Some(action) = actions.recv().await {
            if let HeaderSyncAction::Misbehavior { peer, .. } = action {
                let _ = supervisor.disconnect_peer(&peer).await;
            }
        }
    })
}

async fn drive_mock_block_sync_actions(
    node: &ZakuraTestNode,
    corpus: SyntheticBlockCorpus,
    apply: Option<MockApplyFrontier>,
    servable_high: block::Height,
    stats: ThroughputStats,
    mut needed_blocks_gate: Option<watch::Receiver<bool>>,
) -> JoinHandle<()> {
    let endpoint = node.endpoint();
    let supervisor = node.supervisor();
    let mut actions = node
        .take_block_sync_actions()
        .await
        .expect("block-sync action receiver is enabled");

    tokio::spawn(async move {
        while let Some(action) = actions.recv().await {
            let Some(handle) = endpoint.block_sync() else {
                continue;
            };
            match action {
                BlockSyncAction::QueryNeededBlocks {
                    verified_block_tip,
                    best_header_tip,
                } => {
                    if let Some(gate) = needed_blocks_gate.as_mut() {
                        while !*gate.borrow_and_update() {
                            if gate.changed().await.is_err() {
                                return;
                            }
                        }
                    }
                    let start = verified_block_tip.next().unwrap_or(verified_block_tip);
                    let end = best_header_tip.min(corpus.target_height());
                    let metas = if start <= end {
                        corpus.metas_between(start, end)
                    } else {
                        Vec::new()
                    };
                    let _ = handle.send(BlockSyncEvent::NeededBlocks(metas)).await;
                }
                BlockSyncAction::QueryBlocksByHeightRange { peer, start, count } => {
                    let blocks = corpus.blocks_in_range(start, count, servable_high);
                    let response_bytes = blocks
                        .iter()
                        .fold(0usize, |sum, (_, _, size)| sum.saturating_add(*size));
                    stats.record_request(blocks.len(), response_bytes);
                    let _ = handle
                        .send(BlockSyncEvent::BlockRangeResponseReady {
                            peer,
                            start_height: start,
                            requested_count: count,
                            blocks,
                        })
                        .await;
                }
                BlockSyncAction::SubmitBlock { token, block } => {
                    let Some(apply) = &apply else {
                        continue;
                    };
                    let height = block
                        .coinbase_height()
                        .expect("synthetic submitted block has height");
                    let outcome = apply.apply(&block);
                    if outcome.result == BlockApplyResult::Committed {
                        if let Some(size) = corpus.size_at(height) {
                            stats.record_commit(height, size);
                        }
                    }
                    let _ = handle
                        .send(BlockSyncEvent::BlockApplyFinished {
                            token,
                            height,
                            hash: block.hash(),
                            result: outcome.result,
                            local_frontier: Some(outcome.frontiers),
                        })
                        .await;
                }
                BlockSyncAction::Misbehavior { peer, .. } => {
                    let _ = supervisor.disconnect_peer(&peer).await;
                }
            }
        }
    })
}

async fn connect_leecher_to_seeds(
    cluster: &ZakuraTestCluster,
    seed_count: usize,
    leecher_index: usize,
) -> Result<(), BoxError> {
    let leecher = cluster.node(leecher_index);
    for seed_index in 0..seed_count {
        leecher
            .connect_native(cluster.node(seed_index), Duration::from_secs(10))
            .await?;
    }

    let leecher_id = leecher.node_addr().await.node_id.as_bytes().to_vec();
    let seed_ids = seed_peer_ids(cluster, seed_count).await;
    let leecher_peers = leecher.supervisor().subscribe();
    await_until("leecher connected to all seeds", Duration::from_secs(10), || {
        seed_ids
            .iter()
            .all(|id| leecher_peers.borrow().iter().any(|peer| peer.as_bytes() == id))
    })
    .await?;

    for seed_index in 0..seed_count {
        let seed_peers = cluster.node(seed_index).supervisor().subscribe();
        await_until("seed connected to leecher", Duration::from_secs(10), || {
            seed_peers
                .borrow()
                .iter()
                .any(|peer| peer.as_bytes() == leecher_id)
        })
        .await?;
    }

    Ok(())
}

async fn seed_peer_ids(cluster: &ZakuraTestCluster, seed_count: usize) -> Vec<Vec<u8>> {
    let mut ids = Vec::with_capacity(seed_count);
    for seed_index in 0..seed_count {
        ids.push(
            cluster
                .node(seed_index)
                .node_addr()
                .await
                .node_id
                .as_bytes()
                .to_vec(),
        );
    }
    ids
}

fn synthetic_block_at_height(
    template: &Arc<block::Block>,
    height: block::Height,
    previous_hash: block::Hash,
    random: u64,
) -> Arc<block::Block> {
    let mut block = template.as_ref().clone();
    let tx_count = synthetic_tx_count(random);

    let mut coinbase = block.transactions[0].clone();
    let input = match Arc::make_mut(&mut coinbase) {
        Transaction::V1 { inputs, .. }
        | Transaction::V2 { inputs, .. }
        | Transaction::V3 { inputs, .. }
        | Transaction::V4 { inputs, .. }
        | Transaction::V5 { inputs, .. } => &mut inputs[0],
    };
    match input {
        transparent::Input::Coinbase {
            height: coinbase_height,
            ..
        } => *coinbase_height = height,
        _ => panic!("template block must start with a coinbase input"),
    }

    block.transactions.clear();
    for _ in 0..tx_count {
        block.transactions.push(coinbase.clone());
    }

    let merkle_root = block.transactions.iter().collect::<block::merkle::Root>();
    let mut header = *block.header;
    header.previous_block_hash = previous_hash;
    header.merkle_root = merkle_root;
    header.nonce = zebra_chain::fmt::HexDebug(nonce_bytes(height, random));
    block.header = Arc::new(header);

    Arc::new(block)
}

fn synthetic_tx_count(random: u64) -> usize {
    let span = MAX_SYNTHETIC_TXS
        .checked_sub(MIN_SYNTHETIC_TXS)
        .and_then(|span| span.checked_add(1))
        .expect("synthetic tx count bounds are valid");
    let slot =
        usize::try_from(random % u64::try_from(span).expect("usize fits u64")).expect("fits usize");
    MIN_SYNTHETIC_TXS.saturating_add(slot)
}

fn nonce_bytes(height: block::Height, random: u64) -> [u8; 32] {
    let mut nonce = [0u8; 32];
    nonce[0..4].copy_from_slice(&height.0.to_le_bytes());
    nonce[4..12].copy_from_slice(&random.to_le_bytes());
    nonce[12..20].copy_from_slice(&splitmix64(random).to_le_bytes());
    nonce[20..28].copy_from_slice(&splitmix64(random ^ u64::from(height.0)).to_le_bytes());
    nonce[28..32].copy_from_slice(&height.0.wrapping_mul(0x9e37_79b9).to_le_bytes());
    nonce
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn mainnet_block(bytes: &[u8]) -> Arc<block::Block> {
    Arc::new(bytes.zcash_deserialize_into().expect("block vector parses"))
}

fn mainnet_genesis_hash() -> block::Hash {
    mainnet_block(&BLOCK_MAINNET_GENESIS_BYTES).hash()
}

fn block_size(block: &block::Block) -> usize {
    block
        .zcash_serialize_to_vec()
        .expect("test block serializes")
        .len()
}

fn percentile(mut values: Vec<usize>, percentile: usize) -> usize {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let max_index = values.len().saturating_sub(1);
    let index = max_index.saturating_mul(percentile).saturating_add(99) / 100;
    values[index.min(max_index)]
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_u32(name: &str, default: u32) -> u32 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_u16(name: &str, default: u16) -> u16 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn print_summary(config: &HarnessConfig, summary: &ThroughputSummary, trace_root: Option<&Path>) {
    let elapsed_secs = summary.elapsed.as_secs_f64().max(f64::EPSILON);
    // These casts are for approximate human-readable throughput output only.
    let blocks_per_second = summary.committed_blocks as f64 / elapsed_secs;
    // These casts are for approximate human-readable throughput output only.
    let mib_per_second = summary.committed_bytes as f64 / (1024.0 * 1024.0) / elapsed_secs;

    println!(
        "zakura mock blocksync: seeds={} blocks={} max_blocks_per_response={} max_inflight={} fanout={}",
        config.seeds,
        config.blocks,
        config.max_blocks_per_response,
        config.max_inflight,
        config.fanout,
    );
    println!(
        "throughput: {:.2} blocks/sec, {:.2} MiB/sec, elapsed={:.3}s",
        blocks_per_second, mib_per_second, elapsed_secs,
    );
    println!(
        "requests: count={} p50={} blocks/{} bytes p95={} blocks/{} bytes",
        summary.request_count,
        summary.request_blocks_p50,
        summary.request_bytes_p50,
        summary.request_blocks_p95,
        summary.request_bytes_p95,
    );
    println!("final frontier: {}", summary.final_frontier.0);
    if let Some(trace_root) = trace_root {
        println!("trace dir: {}", trace_root.display());
    }
}

#[test]
fn synthetic_block_generation_is_stable_and_serializable() {
    let corpus = SyntheticBlockCorpus::generate(64, SYNTHETIC_CORPUS_SEED);
    let repeat = SyntheticBlockCorpus::generate(64, SYNTHETIC_CORPUS_SEED);
    let mut previous_hash = mainnet_genesis_hash();

    for height in 1..=64 {
        let height = block::Height(height);
        let block = corpus.block_at(height).expect("height exists");
        let bytes = block
            .zcash_serialize_to_vec()
            .expect("synthetic block serializes");
        let roundtrip: block::Block = bytes
            .zcash_deserialize_into()
            .expect("synthetic block deserializes");

        assert_eq!(block.coinbase_height(), Some(height));
        assert_eq!(block.header.previous_block_hash, previous_hash);
        assert_eq!(roundtrip.hash(), block.hash());
        assert_eq!(repeat.block_at(height).expect("repeat height exists").hash(), block.hash());
        assert_eq!(repeat.size_at(height), corpus.size_at(height));
        assert!(!bytes.is_empty());
        assert!(bytes.len() <= usize::try_from(block::MAX_BLOCK_BYTES).expect("u32 fits usize"));

        previous_hash = block.hash();
    }
}

#[test]
fn mock_apply_frontier_commits_duplicates_and_rejects_gaps() {
    let corpus = SyntheticBlockCorpus::generate(3, SYNTHETIC_CORPUS_SEED);
    let apply = MockApplyFrontier::new(corpus.clone());
    let block_1 = corpus.block_at(block::Height(1)).expect("height 1 exists");
    let block_2 = corpus.block_at(block::Height(2)).expect("height 2 exists");
    let block_3 = corpus.block_at(block::Height(3)).expect("height 3 exists");

    let gap = apply.apply(&block_2);
    assert_eq!(gap.result, BlockApplyResult::Rejected);
    assert_eq!(gap.frontiers.verified_block_tip, block::Height(0));

    let first = apply.apply(&block_1);
    assert_eq!(first.result, BlockApplyResult::Committed);
    assert_eq!(first.frontiers.verified_block_tip, block::Height(1));

    let duplicate = apply.apply(&block_1);
    assert_eq!(duplicate.result, BlockApplyResult::Duplicate);
    assert_eq!(duplicate.frontiers.verified_block_tip, block::Height(1));

    let second = apply.apply(&block_2);
    assert_eq!(second.result, BlockApplyResult::Committed);
    assert_eq!(second.frontiers.verified_block_tip, block::Height(2));

    let third = apply.apply(&block_3);
    assert_eq!(third.result, BlockApplyResult::Committed);
    assert_eq!(third.frontiers.verified_block_tip, block::Height(3));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "local-only throughput harness; run with ZAKURA_MOCK_BS_* env vars and --nocapture"]
async fn zakura_mock_blocksync_throughput() -> Result<(), BoxError> {
    let _guard = zebra_test::init();
    let config = HarnessConfig::from_env();
    let corpus = SyntheticBlockCorpus::generate(config.blocks, SYNTHETIC_CORPUS_SEED);
    let stats = ThroughputStats::default();
    let apply = MockApplyFrontier::new(corpus.clone());
    let mut trace = HarnessTrace::new(config.trace_dir.clone());
    let mut cluster = ZakuraTestCluster::new();
    let mut tasks = Vec::new();

    let seed_frontiers = BlockSyncFrontiers {
        finalized_height: corpus.target_height(),
        verified_block_tip: corpus.target_height(),
        verified_block_hash: corpus.tip_hash(),
    };
    for offset in 0..config.seeds {
        let seed = u64::try_from(offset)
            .expect("seed index fits u64")
            .saturating_add(1);
        let index = spawn_mock_node(
            &mut cluster,
            seed,
            seed_frontiers,
            &corpus,
            &config,
            &mut trace,
        )
        .await?;
        tasks.push(drain_header_sync_actions(cluster.node(index)).await);
        tasks.push(
            drive_mock_block_sync_actions(
                cluster.node(index),
                corpus.clone(),
                None,
                corpus.target_height(),
                stats.clone(),
                None,
            )
            .await,
        );
    }

    let leecher_frontiers = BlockSyncFrontiers {
        finalized_height: block::Height(0),
        verified_block_tip: block::Height(0),
        verified_block_hash: mainnet_genesis_hash(),
    };
    let leecher_seed = u64::try_from(config.seeds)
        .expect("seed count fits u64")
        .saturating_add(1);
    let (needed_blocks_gate_tx, needed_blocks_gate_rx) = watch::channel(false);
    let leecher_index = spawn_mock_node(
        &mut cluster,
        leecher_seed,
        leecher_frontiers,
        &corpus,
        &config,
        &mut trace,
    )
    .await?;
    tasks.push(drain_header_sync_actions(cluster.node(leecher_index)).await);
    tasks.push(
        drive_mock_block_sync_actions(
            cluster.node(leecher_index),
            corpus.clone(),
            Some(apply),
            block::Height(0),
            stats.clone(),
            Some(needed_blocks_gate_rx),
        )
        .await,
    );

    connect_leecher_to_seeds(&cluster, config.seeds, leecher_index).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = needed_blocks_gate_tx.send(true);

    await_until("leecher reaches mock block-sync target", Duration::from_secs(300), || {
        stats.final_frontier() >= corpus.target_height()
    })
    .await?;

    let summary = stats.summary();
    print_summary(&config, &summary, trace.root());
    assert_eq!(summary.final_frontier, corpus.target_height());

    cluster.shutdown().await;
    for task in tasks {
        task.abort();
    }
    trace.shutdown().await;

    Ok(())
}
