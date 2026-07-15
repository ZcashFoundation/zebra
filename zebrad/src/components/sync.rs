//! The syncer discovers tentative chain tips from peers and drives the
//! generic IBD engine over them, keeping Zebra synchronized with the network.
//!
//! The crawl (`ObtainTips`/`ExtendTips`) stays here; block fetch, verify, and
//! commit run inside [`crate::components::ibd::engine::Engine`] over a
//! [`DiscoverySource`] (design doc `known-hash-ibd.md` §17).

use std::{
    collections::HashSet,
    convert, iter,
    path::{Path, PathBuf},
    pin::pin,
    time::Duration,
};

use color_eyre::eyre::{eyre, Report};
use futures::stream::{FuturesUnordered, StreamExt};
use indexmap::IndexSet;
use serde::{Deserialize, Serialize};
use tokio::{
    sync::watch,
    task::JoinError,
    time::{sleep, timeout},
};
use tower::{timeout::Timeout, Service, ServiceExt};

use zebra_chain::{block, chain_tip::ChainTip};
use zebra_network as zn;
use zebra_state as zs;

use crate::{
    components::ibd::{self, cache::BlockCache, discovery::DiscoverySource, engine::Engine},
    config::ZebradConfig,
    BoxError,
};

pub mod end_of_support;
mod gossip;
mod progress;
mod recent_sync_lengths;
mod status;

#[cfg(test)]
mod tests;

pub use gossip::{gossip_best_tip_block_hashes, BlockGossipError};
pub use progress::show_block_chain_progress;
pub use recent_sync_lengths::RecentSyncLengths;
pub use status::SyncStatus;

/// Controls the number of peers used for each ObtainTips and ExtendTips request.
const FANOUT: usize = 3;

/// A multiplier used to calculate the extra number of blocks we allow in the
/// verifier, state, and block commit pipelines, on top of the configured
/// checkpoint verify concurrency limit.
///
/// This allows the verifier and state queues, and the block commit channel,
/// to hold a few extra tips responses worth of blocks, even when the sync
/// engine's own pipeline is full. Any unused capacity is shared between the
/// queues.
pub const VERIFICATION_PIPELINE_SCALING_MULTIPLIER: usize = 2;

/// A lower bound on the user-specified checkpoint verification concurrency limit.
///
/// Set to the maximum checkpoint interval, so the pipeline holds around a checkpoint's
/// worth of blocks.
///
/// ## Security
///
/// If a malicious node is chosen for an ObtainTips or ExtendTips request, it can
/// provide up to 500 malicious block hashes. These block hashes will be
/// distributed across all available peers. Assuming there are around 50 connected
/// peers, the malicious node will receive approximately 10 of those block requests.
///
/// Malicious deserialized blocks can take up a large amount of RAM, see
/// [`super::inbound::downloads::MAX_INBOUND_CONCURRENCY`] and #1880 for details.
/// So we want to keep the lookahead limit reasonably small.
///
/// Once these malicious blocks start failing validation, the syncer will cancel all
/// the pending download and verify tasks, drop all the blocks, and start a new
/// ObtainTips with a new set of peers.
pub const MIN_CHECKPOINT_CONCURRENCY_LIMIT: usize =
    zebra_chain::parameters::checkpoint::constants::MAX_CHECKPOINT_HEIGHT_GAP;

/// The default for the user-specified lookahead limit.
///
/// See [`MIN_CHECKPOINT_CONCURRENCY_LIMIT`] for details.
pub const DEFAULT_CHECKPOINT_CONCURRENCY_LIMIT: usize = MAX_TIPS_RESPONSE_HASH_COUNT * 2;

/// A lower bound on the user-specified concurrency limit.
///
/// If the concurrency limit is 0, Zebra can't download or verify any blocks.
pub const MIN_CONCURRENCY_LIMIT: usize = 1;

/// The expected maximum number of hashes in an ObtainTips or ExtendTips response.
///
/// This is used to allow block heights that are slightly beyond the lookahead limit,
/// but still limit the number of blocks in the pipeline between the downloader and
/// the state.
///
/// See [`MIN_CHECKPOINT_CONCURRENCY_LIMIT`] for details.
pub const MAX_TIPS_RESPONSE_HASH_COUNT: usize = 500;

/// Controls how long we wait for a tips response to return.
///
/// ## Correctness
///
/// If this timeout is removed (or set too high), the syncer will sometimes hang.
///
/// If this timeout is set too low, the syncer will sometimes get stuck in a
/// failure loop.
pub const TIPS_RESPONSE_TIMEOUT: Duration = Duration::from_secs(6);

/// Controls how long we wait between gossiping successive blocks or transactions.
///
/// ## Correctness
///
/// If this timeout is set too high, blocks and transactions won't propagate through
/// the network efficiently.
///
/// If this timeout is set too low, the peer set and remote peers can get overloaded.
pub const PEER_GOSSIP_DELAY: Duration = Duration::from_secs(7);

/// Controls how long we wait for a block download request to complete.
///
/// This timeout makes sure that the syncer doesn't hang when:
///   - the lookahead queue is full, and
///   - we are waiting for a request that is stuck.
///
/// See [`BLOCK_VERIFY_TIMEOUT`] for details.
///
/// ## Correctness
///
/// If this timeout is removed (or set too high), the syncer will sometimes hang.
///
/// If this timeout is set too low, the syncer will sometimes get stuck in a
/// failure loop.
///
/// We set the timeout so that it requires under 1 Mbps bandwidth for a full 2 MB block.
pub(super) const BLOCK_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(20);

/// Controls how long we wait for a block verify request to complete.
///
/// This timeout makes sure that the syncer doesn't hang when:
///  - the lookahead queue is full, and
///  - all pending verifications:
///    - are waiting on a missing download request,
///    - are waiting on a download or verify request that has failed, but we have
///      deliberately ignored the error,
///    - are for blocks a long way ahead of the current tip, or
///    - are for invalid blocks which will never verify, because they depend on
///      missing blocks or transactions.
///
/// These conditions can happen during normal operation - they are not bugs.
///
/// This timeout also mitigates or hides the following kinds of bugs:
///  - all pending verifications:
///    - are waiting on a download or verify request that has failed, but we have
///      accidentally dropped the error,
///    - are waiting on a download request that has hung inside Zebra,
///    - are on tokio threads that are waiting for blocked operations.
///
/// ## Correctness
///
/// If this timeout is removed (or set too high), the syncer will sometimes hang.
///
/// If this timeout is set too low, the syncer will sometimes get stuck in a
/// failure loop.
///
/// We've observed spurious 15 minute timeouts when a lot of blocks are being committed to
/// the state. But there are also some blocks that seem to hang entirely, and never return.
///
/// So we allow about half the spurious timeout, which might cause some re-downloads.
pub(super) const BLOCK_VERIFY_TIMEOUT: Duration = Duration::from_secs(8 * 60);

/// The interval between the sync cycle's engine progress checks.
///
/// Each check compares the state tip against the previous check. A cycle
/// whose tip has not moved for [`BLOCK_VERIFY_TIMEOUT`] is restarted, exactly
/// as the legacy syncer's per-batch verify timeout restarted it. This is the
/// backstop for a crawl that fed the engine hashes no peer serves (for
/// example, a peer advertising fabricated or reorged-away hashes): the engine
/// retries missing blocks forever, which is correct for pinned known-hash
/// lists, but must not wedge a tentative discovered range.
///
/// Using a prime number makes sure that progress checks don't synchronise
/// with other tasks.
const SYNC_PROGRESS_CHECK_INTERVAL: Duration = Duration::from_secs(59);

/// Controls how long we wait to restart syncing after finishing a sync run.
///
/// This delay should be long enough to:
///   - allow zcashd peers to process pending requests. If the node only has a
///     few peers, we want to clear as much peer state as possible. In
///     particular, zcashd sends "next block range" hints, based on zcashd's
///     internal model of our sync progress. But we want to discard these hints,
///     so they don't get confused with ObtainTips and ExtendTips responses, and
///   - allow in-progress downloads to time out.
///
/// This delay is particularly important on instances with slow or unreliable
/// networks, and on testnet, which has a small number of slow peers.
///
/// Using a prime number makes sure that syncer fanouts don't synchronise with other crawls.
///
/// ## Correctness
///
/// If this delay is removed (or set too low), the syncer will
/// sometimes get stuck in a failure loop, due to leftover downloads from
/// previous sync runs.
const SYNC_RESTART_DELAY: Duration = Duration::from_secs(67);

/// In regtest, use a much shorter restart delay so that downstream nodes pick up
/// newly-mined blocks quickly (e.g. after `generate(N)` in integration tests).
/// The default 67-second delay exceeds the typical `sync_all` timeout of 60 seconds.
const REGTEST_SYNC_RESTART_DELAY: Duration = Duration::from_secs(2);

/// Controls how long we wait to retry a failed attempt to download
/// and verify the genesis block.
///
/// This timeout gives the crawler time to find better peers.
///
/// ## Security
///
/// If this timeout is removed (or set too low), Zebra will immediately retry
/// to download and verify the genesis block from its peers. This can cause
/// a denial of service on those peers.
///
/// If this timeout is too short, old or buggy nodes will keep making useless
/// network requests. If there are a lot of them, it could overwhelm the network.
const GENESIS_TIMEOUT_RETRY: Duration = Duration::from_secs(10);

/// Sync configuration section.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// The number of parallel block download requests.
    ///
    /// Block downloads are driven by the sync engine's own byte-budget
    /// pipeline (see [`known_hash_lookahead_bytes`](Self::known_hash_lookahead_bytes)),
    /// so this limit only sizes Zebra's internal service buffers.
    #[serde(alias = "max_concurrent_block_requests")]
    pub download_concurrency_limit: usize,

    /// The number of checkpointed blocks Zebra's internal queues can hold.
    ///
    /// Blocks below the mandatory checkpoint are committed by the known-hash
    /// sync engine (bounded by its byte budget), so this limit only sizes
    /// Zebra's internal verifier and state queues.
    ///
    /// Zebra enforces a [`MIN_CHECKPOINT_CONCURRENCY_LIMIT`].
    /// Decreasing this value reduces RAM usage.
    #[serde(alias = "lookahead_limit")]
    pub checkpoint_verify_concurrency_limit: usize,

    /// The number of blocks submitted in parallel to the full verifier.
    ///
    /// This is set to a low value by default, to avoid verification timeouts on large blocks.
    /// Increasing this value may improve performance on machines with many cores.
    pub full_verify_concurrency_limit: usize,

    /// The number of threads used to verify signatures, proofs, and other CPU-intensive code.
    ///
    /// If the number of threads is not configured or zero, Zebra uses the number of logical cores.
    /// If the number of logical cores can't be detected, Zebra uses one thread.
    /// For details, see [the `rayon` documentation](https://docs.rs/rayon/latest/rayon/struct.ThreadPoolBuilder.html#method.num_threads).
    pub parallel_cpu_threads: usize,

    /// Enable the known-hash initial sync engine.
    ///
    /// When enabled on a network with a bundled known-hash list, initial sync
    /// downloads blocks directly by their pinned hashes instead of discovering
    /// hashes from peers, then hands off to tip-following sync (the same
    /// engine over hashes discovered from peers) at the end of the list.
    ///
    /// Enabled by default on networks with a bundled list (currently
    /// Mainnet); other networks decline to tip-following sync automatically.
    /// See `docs/design/known-hash-ibd.md` for the engine design.
    pub known_hash_sync: bool,

    /// The sync engine's lookahead limit, as a byte budget.
    ///
    /// Bounds the bytes of fetched and in-flight blocks the sync engine holds
    /// in memory ahead of the commit frontier — during known-hash initial
    /// sync and tip-following sync alike. The block-count lookahead is not
    /// configured: it auto-scales from the per-block size hints, so
    /// small-block eras look ahead further than large-block eras.
    ///
    /// Increasing this limit can improve sync throughput at the cost of RAM;
    /// decreasing it reduces RAM usage.
    pub known_hash_lookahead_bytes: usize,

    /// The number of seconds a block near the commit frontier may be in flight
    /// before the sync engine hedges it with a single-hash refetch from a
    /// different peer.
    ///
    /// Lower values fill frontier gaps faster but send more duplicate
    /// requests; higher values are more polite to slow peers.
    pub known_hash_gap_hedge_secs: u64,

    /// How many block heights ahead of the commit frontier the known-hash
    /// engine downloads note commitment trees, in snapshot-consume mode.
    ///
    /// Trees are fetched *ahead of* the block-commit frontier — a deeper
    /// lookahead than block fetch — so that by the time a block reaches the
    /// commit stage its sapling/orchard tree is already downloaded and verified,
    /// and the state takes the "tree supplied by download" path instead of
    /// folding note commitments. Only the ~7% of heights that update a tree are
    /// requested, so this many *heights* of margin schedules far fewer tree
    /// fetches in practice.
    ///
    /// Clamped to an internal ceiling so it cannot make the engine fetch
    /// unboundedly far ahead. Set to `0` to disable tree lookahead (the commit
    /// then always folds). Ignored outside snapshot-consume mode (the bundled
    /// `.bin` list folds notes the normal way).
    pub known_hash_tree_lookahead: u32,

    /// An override directory containing the known-hash list chunk files.
    ///
    /// When unset, the chunk files are resolved from the directories next to
    /// the `zebrad` binary, the platform data directory, or the development
    /// tree, in that order.
    pub known_hash_list_dir: Option<PathBuf>,

    /// The directory of **local snapshot-consume artifacts**: the directory the
    /// installer downloaded from the release assets, or one written locally by
    /// `emit-snapshot --emit-files`.
    ///
    /// **Experimental; defaults to `None`.**
    ///
    /// Required when [`snapshot_consume_sync`](Self::snapshot_consume_sync) is
    /// enabled. The engine reads each snapshot artifact (the known-hash chunks
    /// and the per-height note commitment trees; the unspent-output set, the
    /// address-balance set, and the chain value pools are loaded by the state)
    /// from this directory — verifying each against the pinned SHA-256
    /// constants, so a tampered or corrupt download is rejected. Blocks
    /// themselves still come over normal P2P.
    ///
    /// The directory layout is the one written by
    /// `emit-snapshot --emit-files --out-dir <dir>`; see
    /// [`crate::components::ibd::consume::local`] and
    /// `docs/design/snapshot-distribution.md`.
    pub known_hash_local_source_dir: Option<PathBuf>,

    /// Enable snapshot-consume (assumeUTXO-style) initial sync.
    ///
    /// **Experimental; defaults to `false`, leaving normal sync unchanged.**
    ///
    /// When enabled together with [`known_hash_sync`](Self::known_hash_sync),
    /// the engine reads the known-hash chunks and per-height note commitment
    /// trees from the artifact directory named by
    /// [`known_hash_local_source_dir`](Self::known_hash_local_source_dir)
    /// (which must also be set) and hands the snapshot to the state instead of
    /// deriving it from the blocks. The artifacts are content-addressed: each
    /// is verified against a pinned SHA-256 constant before it is trusted. See
    /// `docs/design/snapshot-distribution.md` and
    /// `docs/design/utxo-elision.md`.
    ///
    /// The state-side consume behaviours (direct tree writes, bulk balance
    /// loading, address-index elision) are configured separately under
    /// `[state] snapshot_consume`; this flag enables the engine-side read and
    /// verification of the snapshot artifacts.
    pub snapshot_consume_sync: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // 2/3 of the default outbound peer limit.
            download_concurrency_limit: 50,

            // A few max-length checkpoints.
            checkpoint_verify_concurrency_limit: DEFAULT_CHECKPOINT_CONCURRENCY_LIMIT,

            // This default is deliberately very low, so Zebra can verify a few large blocks in under 60 seconds,
            // even on machines with only a few cores.
            //
            // This lets users see the committed block height changing in every progress log,
            // and avoids hangs due to out-of-order verifications flooding the CPUs.
            //
            // TODO:
            // - limit full verification concurrency based on block transaction counts?
            // - move more disk work to blocking tokio threads,
            //   and CPU work to the rayon thread pool inside blocking tokio threads
            full_verify_concurrency_limit: 20,

            // Use one thread per CPU.
            //
            // If this causes tokio executor starvation, move CPU-intensive tasks to rayon threads,
            // or reserve a few cores for tokio threads, based on `num_cpus()`.
            parallel_cpu_threads: 0,

            known_hash_sync: true,

            // 256 MiB: enough lookahead to keep the network busy in every era
            // without a large RSS increase.
            known_hash_lookahead_bytes: 268_435_456,

            // A small multiple of a typical block round-trip.
            known_hash_gap_hedge_secs: 5,

            // Fetch trees deeper ahead of the frontier than blocks, so a
            // height's tree is already verified by the time its block commits.
            // At ~7% updating heights this schedules a few hundred tree fetches
            // at most. See `components::ibd::tree::TREE_LOOKAHEAD_DEFAULT`.
            known_hash_tree_lookahead: crate::components::ibd::tree::TREE_LOOKAHEAD_DEFAULT,

            // Use the layered asset search by default.
            known_hash_list_dir: None,

            // No artifact directory by default; snapshot-consume sync requires
            // one (downloaded by the installer or emitted locally).
            known_hash_local_source_dir: None,

            // Experimental snapshot-consume sync is off by default, so normal
            // sync is completely unaffected.
            snapshot_consume_sync: false,
        }
    }
}

/// Helps work around defects in the bitcoin protocol by checking whether
/// the returned hashes actually extend a chain tip.
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
struct CheckedTip {
    tip: block::Hash,
    expected_next: block::Hash,
}

pub struct ChainSync<ZN, ZS, ZV, ZSTip>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZN::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZS::Future: Send,
    ZV: Service<zebra_consensus::Request, Response = block::Hash, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZV::Future: Send,
    ZSTip: ChainTip + Clone + Send + 'static,
{
    // Configuration
    //
    /// The genesis hash for the configured network
    genesis_hash: block::Hash,

    /// The configured full verification concurrency limit, after applying the
    /// minimum limit.
    ///
    /// Caps each sync cycle engine's commit pipeline: the number of blocks
    /// concurrently inside the semantic verifier.
    full_verify_concurrency_limit: usize,

    /// Whether the node is running on regtest. Used to apply a shorter sync restart delay.
    is_regtest: bool,

    /// The engine's fetch lookahead limit, as a byte budget
    /// ([`Config::known_hash_lookahead_bytes`]).
    lookahead_bytes: usize,

    /// How long a block near the engine's commit frontier may be in flight
    /// before it is hedged ([`Config::known_hash_gap_hedge_secs`]).
    gap_hedge_after: Duration,

    /// The directory of the engine's disk overflow block cache: the same
    /// directory the known-hash initial sync engine uses (the two never run
    /// concurrently, and either one removes it when its range completes).
    cache_dir: PathBuf,

    // Services
    //
    /// A network service which is used to perform ObtainTips and ExtendTips
    /// requests.
    ///
    /// Has no retry logic, because failover is handled using fanout.
    tip_network: Timeout<ZN>,

    /// The unwrapped peer set: each sync cycle's engine builds its own
    /// batched block fetch over it (retrying, hedging, and peer weighting
    /// are the engine's own machinery).
    peer_set: ZN,

    /// The semantic block verifier, wrapped in a per-block timeout: the
    /// commit stage for each sync cycle's engine.
    verifier: Timeout<ZV>,

    /// The cached block chain state.
    state: ZS,

    /// Allows efficient access to the best tip of the blockchain.
    latest_chain_tip: ZSTip,

    /// Live peer set status, for sizing each engine's download pipeline.
    peer_set_status: watch::Receiver<zn::PeerSetStatus>,

    // Internal sync state
    //
    /// The tips that the syncer is currently following.
    prospective_tips: HashSet<CheckedTip>,

    /// The lengths of recent sync responses.
    recent_syncs: RecentSyncLengths,
}

/// Polls the network to determine whether further blocks are available and
/// downloads them.
///
/// This component is used for initial block sync, but the `Inbound` service is
/// responsible for participating in the gossip protocols used for block
/// diffusion.
impl<ZN, ZS, ZV, ZSTip> ChainSync<ZN, ZS, ZV, ZSTip>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZN::Future: Send,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZS::Future: Send,
    ZV: Service<zebra_consensus::Request, Response = block::Hash, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZV::Future: Send,
    ZSTip: ChainTip + Clone + Send + 'static,
{
    /// Returns a new syncer instance, using:
    ///  - config: the zebrad config, for the network and the engine limits
    ///  - peers: the zebra-network peers to contact for downloads
    ///  - verifier: the zebra-consensus verifier that checks the chain
    ///  - state: the zebra-state that stores the chain
    ///  - latest_chain_tip: the latest chain tip from `state`
    ///  - peer_set_status: the live peer set status watch from
    ///    [`zebra_network::init`], for sizing each engine's download pipeline
    ///  - cache_dir: the state cache directory; the engine's disk overflow
    ///    tier lives under `<cache_dir>/`[`ibd::cache::CACHE_DIR_NAME`]
    ///
    /// Also returns a [`SyncStatus`] to check if the syncer has likely reached the chain tip.
    pub fn new(
        config: &ZebradConfig,
        peers: ZN,
        verifier: ZV,
        state: ZS,
        latest_chain_tip: ZSTip,
        peer_set_status: watch::Receiver<zn::PeerSetStatus>,
        cache_dir: &Path,
    ) -> (Self, SyncStatus) {
        let mut full_verify_concurrency_limit = config.sync.full_verify_concurrency_limit;

        if full_verify_concurrency_limit < MIN_CONCURRENCY_LIMIT {
            warn!(
                "configured full verify concurrency limit {} too low, increasing to {}",
                config.sync.full_verify_concurrency_limit, MIN_CONCURRENCY_LIMIT,
            );

            full_verify_concurrency_limit = MIN_CONCURRENCY_LIMIT;
        }

        let tip_network = Timeout::new(peers.clone(), TIPS_RESPONSE_TIMEOUT);

        // We apply a timeout to the verifier to avoid hangs due to missing earlier blocks.
        let verifier = Timeout::new(verifier, BLOCK_VERIFY_TIMEOUT);

        let (sync_status, recent_syncs) = SyncStatus::new_for_network(&config.network.network);

        let new_syncer = Self {
            genesis_hash: config.network.network.genesis_hash(),
            full_verify_concurrency_limit,
            is_regtest: config.network.network.is_regtest(),
            lookahead_bytes: config.sync.known_hash_lookahead_bytes,
            gap_hedge_after: Duration::from_secs(config.sync.known_hash_gap_hedge_secs),
            cache_dir: cache_dir.join(ibd::cache::CACHE_DIR_NAME),
            tip_network,
            peer_set: peers,
            verifier,
            state,
            latest_chain_tip,
            peer_set_status,
            prospective_tips: HashSet::new(),
            recent_syncs,
        };

        (new_syncer, sync_status)
    }

    /// Runs the syncer to synchronize the chain and keep it synchronized.
    #[instrument(skip(self))]
    pub async fn sync(mut self) -> Result<(), Report> {
        // We can't download the genesis block using our normal algorithm,
        // due to protocol limitations
        self.request_genesis().await?;

        loop {
            if let Err(error) = self.sync_cycle().await {
                info!("sync cycle failed, will restart: {error:#}");
            }

            self.update_metrics();

            let restart_delay = if self.is_regtest {
                REGTEST_SYNC_RESTART_DELAY
            } else {
                SYNC_RESTART_DELAY
            };
            info!(
                timeout = ?restart_delay,
                state_tip = ?self.latest_chain_tip.best_tip_height(),
                "waiting to restart sync"
            );
            sleep(restart_delay).await;
        }
    }

    /// Runs one sync cycle: crawls the network for tentative tip hashes, and
    /// drives the generic IBD engine over them until the crawl and the engine
    /// both run out of work.
    ///
    /// The crawl feeds the engine's [`DiscoverySource`] through its
    /// [`DiscoveryFeed`](ibd::discovery::DiscoveryFeed), extending the tips
    /// just ahead of the engine's fetch frontier; the engine fetches,
    /// verifies (full semantic and contextual validation), and commits each
    /// discovered block in order.
    ///
    /// Returns `Ok` if it was able to synchronize as much of the chain as it
    /// could, and then ran out of prospective tips. This happens when
    /// synchronization finishes or if Zebra ended up following a fork. Either
    /// way, Zebra should attempt to obtain some more tips.
    ///
    /// Returns `Err` if there was an unrecoverable error and restarting the
    /// synchronization is necessary: the next cycle re-crawls from the state
    /// tip with fresh peers, converging exactly as the legacy syncer's
    /// restart loop did (committed blocks are never re-requested; tentative
    /// hashes are re-discovered).
    #[instrument(skip(self))]
    async fn sync_cycle(&mut self) -> Result<(), Report> {
        self.prospective_tips = HashSet::new();

        info!(
            state_tip = ?self.latest_chain_tip.best_tip_height(),
            "starting sync, obtaining new tips"
        );
        let hashes = timeout(SYNC_RESTART_DELAY, self.obtain_tips())
            .await
            .map_err(Into::into)
            // TODO: replace with flatten() when it stabilises (#70142)
            .and_then(convert::identity)
            .map_err(|e| {
                info!("temporary error obtaining tips: {:#}", e);
                e
            })?;
        self.update_metrics();

        if hashes.is_empty() {
            info!("exhausted prospective tip set");
            return Ok(());
        }

        // The engine assigns the discovered hashes sequential window heights
        // starting just above the state tip, anchored on the tip hash. When
        // the crawl actually starts on a side chain below the tip (a fork),
        // the assigned heights are offset from the true heights and the
        // anchor is not the first block's parent: that is fine, because the
        // assigned height is only the engine's window key — the fetch and
        // commit stages match blocks by hash alone, and the verifier derives
        // every consensus height and parent from the block itself.
        let (base, anchor) = match self.latest_chain_tip.best_tip_height_and_hash() {
            Some((tip_height, tip_hash)) => (
                tip_height
                    .next()
                    .expect("the state tip is far below the maximum block height"),
                tip_hash,
            ),
            // The genesis block was committed before the sync cycles started,
            // so an unpublished tip is at least the genesis block.
            None => (block::Height(1), self.genesis_hash),
        };

        let (mut feed, source) = DiscoverySource::new(base, anchor);
        feed.extend(hashes.into_iter().collect());

        let mut engine = Engine::new_semantic(
            self.peer_set.clone(),
            self.verifier.clone(),
            base,
            source,
            self.peer_set_status.clone(),
            BlockCache::new(&self.cache_dir),
            self.lookahead_bytes,
            self.full_verify_concurrency_limit,
            self.gap_hedge_after,
        );
        let mut engine_run = pin!(engine.run());

        let mut progress_tip = self.latest_chain_tip.best_tip_height();
        let mut last_progress = std::time::Instant::now();
        let mut progress_check = tokio::time::interval(SYNC_PROGRESS_CHECK_INTERVAL);

        // Drive the engine over the growing range, extending the tips just
        // ahead of its fetch frontier, until the crawl runs out of
        // prospective tips and the engine drains what it was fed.
        let mut crawl_finished = false;
        let outcome = loop {
            tokio::select! {
                outcome = &mut engine_run => break outcome,

                _ = feed.wants_more_hashes(), if !crawl_finished => {
                    if self.prospective_tips.is_empty() {
                        // No tips left to crawl this cycle: the fed range is
                        // final, so the engine completes once it drains. Keep
                        // looping (rather than awaiting the engine directly)
                        // so the no-progress restart below stays active
                        // through the drain.
                        crawl_finished = true;
                        feed.finish();
                        continue;
                    }

                    let hashes = timeout(SYNC_RESTART_DELAY, self.extend_tips())
                        .await
                        .map_err(Into::into)
                        // TODO: replace with flatten() when it stabilises (#70142)
                        .and_then(convert::identity)
                        .map_err(|e| {
                            info!("temporary error extending tips: {:#}", e);
                            e
                        })?;
                    self.update_metrics();

                    feed.extend(hashes.into_iter().collect());
                }

                _ = progress_check.tick() => {
                    let tip = self.latest_chain_tip.best_tip_height();
                    if tip != progress_tip {
                        progress_tip = tip;
                        last_progress = std::time::Instant::now();
                    } else if last_progress.elapsed() >= BLOCK_VERIFY_TIMEOUT {
                        // See [`SYNC_PROGRESS_CHECK_INTERVAL`]: a discovered
                        // range that cannot drain (fabricated or reorged-away
                        // hashes) must restart the cycle, not wedge it.
                        return Err(eyre!(
                            "sync cycle made no progress for {BLOCK_VERIFY_TIMEOUT:?}, restarting",
                        ));
                    }
                }
            }
        };

        let outcome = outcome.map_err(|error| eyre!(error))?;
        debug!(?outcome, "sync cycle engine completed");

        info!(
            state_tip = ?self.latest_chain_tip.best_tip_height(),
            "exhausted prospective tip set"
        );

        Ok(())
    }

    /// Given a block_locator list fan out request for subsequent hashes to
    /// multiple peers.
    ///
    /// Returns the deduplicated unknown hashes, in download order, for the
    /// engine's discovery feed, and records the new prospective tips for
    /// [`extend_tips`](Self::extend_tips) to follow.
    #[instrument(skip(self))]
    async fn obtain_tips(&mut self) -> Result<IndexSet<block::Hash>, Report> {
        let stage_start = std::time::Instant::now();

        let block_locator = self
            .state
            .ready()
            .await
            .map_err(|e| eyre!(e))?
            .call(zebra_state::Request::BlockLocator)
            .await
            .map(|response| match response {
                zebra_state::Response::BlockLocator(block_locator) => block_locator,
                _ => unreachable!(
                    "GetBlockLocator request can only result in Response::BlockLocator"
                ),
            })
            .map_err(|e| eyre!(e))?;

        debug!(
            tip = ?block_locator.first().expect("we have at least one block locator object"),
            ?block_locator,
            "got block locator and trying to obtain new chain tips"
        );

        let mut requests = FuturesUnordered::new();
        for attempt in 0..FANOUT {
            if attempt > 0 {
                // Let other tasks run, so we're more likely to choose a different peer.
                //
                // TODO: move fanouts into the PeerSet, so we always choose different peers (#2214)
                tokio::task::yield_now().await;
            }

            let ready_tip_network = self.tip_network.ready().await;
            requests.push(tokio::spawn(ready_tip_network.map_err(|e| eyre!(e))?.call(
                zn::Request::FindBlocks {
                    known_blocks: block_locator.clone(),
                    stop: None,
                },
            )));
        }

        let mut download_set = IndexSet::new();
        while let Some(res) = requests.next().await {
            match res
                .unwrap_or_else(|e @ JoinError { .. }| {
                    if e.is_panic() {
                        panic!("panic in obtain tips task: {e:?}");
                    } else {
                        info!(
                            "task error during obtain tips task: {e:?},\
                     is Zebra shutting down?"
                        );
                        Err(e.into())
                    }
                })
                .map_err::<Report, _>(|e| eyre!(e))
            {
                Ok(zn::Response::BlockHashes(hashes)) => {
                    trace!(?hashes);

                    // zcashd sometimes appends an unrelated hash at the start
                    // or end of its response.
                    //
                    // We can't discard the first hash, because it might be a
                    // block we want to download. So we just accept any
                    // out-of-order first hashes.

                    // We use the last hash for the tip, and we want to avoid bad
                    // tips from zcashd's quirk of appending an unrelated hash.
                    // So we discard the last hash on mainnet/testnet.
                    // (We don't need to worry about missed downloads, because we
                    // will pick them up again in ExtendTips.)
                    //
                    // In regtest we only connect to Zebra nodes, not zcashd,
                    // so we trust all hashes in the response and keep them all.
                    // This is necessary when there are only a small number of
                    // blocks to sync (e.g. 2 new blocks), where stripping the
                    // last hash leaves only 1 unknown hash and rchunks_exact(2)
                    // would discard the entire response.
                    let hashes = if self.is_regtest {
                        hashes.as_slice()
                    } else {
                        match hashes.as_slice() {
                            [] => continue,
                            [rest @ .., _last] => rest,
                        }
                    };
                    if hashes.is_empty() {
                        continue;
                    }

                    let mut first_unknown = None;
                    for (i, &hash) in hashes.iter().enumerate() {
                        if !self.state_contains(hash).await? {
                            first_unknown = Some(i);
                            break;
                        }
                    }

                    debug!(hashes.len = ?hashes.len(), ?first_unknown);

                    let unknown_hashes = if let Some(index) = first_unknown {
                        &hashes[index..]
                    } else {
                        continue;
                    };

                    trace!(?unknown_hashes);

                    let new_tip = if let Some(end) = unknown_hashes.rchunks_exact(2).next() {
                        CheckedTip {
                            tip: end[0],
                            expected_next: end[1],
                        }
                    } else {
                        debug!("discarding response that extends only one block");
                        continue;
                    };

                    // Make sure we get the same tips, regardless of the
                    // order of peer responses
                    if !download_set.contains(&new_tip.expected_next) {
                        debug!(?new_tip,
                                        "adding new prospective tip, and removing existing tips in the new block hash list");
                        self.prospective_tips
                            .retain(|t| !unknown_hashes.contains(&t.expected_next));
                        self.prospective_tips.insert(new_tip);
                    } else {
                        debug!(
                            ?new_tip,
                            "discarding prospective tip: already in download set"
                        );
                    }

                    // security: the first response determines our download order
                    //
                    // TODO: can we make the download order independent of response order?
                    let prev_download_len = download_set.len();
                    download_set.extend(unknown_hashes);
                    let new_download_len = download_set.len();
                    let new_hashes = new_download_len - prev_download_len;
                    debug!(new_hashes, "added hashes to download set");
                    metrics::histogram!("sync.obtain.response.hash.count")
                        .record(new_hashes as f64);
                }
                Ok(_) => unreachable!("network returned wrong response"),
                // We ignore this error because we made multiple fanout requests.
                Err(e) => debug!(?e),
            }
        }

        debug!(?self.prospective_tips);

        // Check that the new tips we got are actually unknown.
        for hash in &download_set {
            debug!(?hash, "checking if state contains hash");
            if self.state_contains(*hash).await? {
                return Err(eyre!("queued download of hash behind our chain tip"));
            }
        }

        let new_downloads = download_set.len();
        debug!(new_downloads, "feeding new hashes to the sync engine");
        metrics::gauge!("sync.obtain.queued.hash.count").set(new_downloads as f64);

        // security: use the actual number of new downloads from all peers,
        // so the last peer to respond can't toggle our mempool
        self.recent_syncs.push_obtain_tips_length(new_downloads);

        metrics::histogram!("sync.stage.duration_seconds", "stage" => "obtain_tips")
            .record(stage_start.elapsed().as_secs_f64());

        Ok(download_set)
    }

    /// Fans out a `FindBlocks` request for each prospective tip, following
    /// each tip forward.
    ///
    /// Returns the deduplicated unknown hashes, in download order, for the
    /// engine's discovery feed, and replaces the prospective tips with the
    /// extended ones.
    #[instrument(skip(self))]
    async fn extend_tips(&mut self) -> Result<IndexSet<block::Hash>, Report> {
        let stage_start = std::time::Instant::now();

        let tips = std::mem::take(&mut self.prospective_tips);

        let mut download_set = IndexSet::new();
        debug!(tips = ?tips.len(), "trying to extend chain tips");
        for tip in tips {
            debug!(?tip, "asking peers to extend chain tip");
            let mut responses = FuturesUnordered::new();
            for attempt in 0..FANOUT {
                if attempt > 0 {
                    // Let other tasks run, so we're more likely to choose a different peer.
                    //
                    // TODO: move fanouts into the PeerSet, so we always choose different peers (#2214)
                    tokio::task::yield_now().await;
                }

                let ready_tip_network = self.tip_network.ready().await;
                responses.push(tokio::spawn(ready_tip_network.map_err(|e| eyre!(e))?.call(
                    zn::Request::FindBlocks {
                        known_blocks: vec![tip.tip],
                        stop: None,
                    },
                )));
            }
            while let Some(res) = responses.next().await {
                match res
                    .expect("panic in spawned extend tips request")
                    .map_err::<Report, _>(|e| eyre!(e))
                {
                    Ok(zn::Response::BlockHashes(hashes)) => {
                        debug!(first = ?hashes.first(), len = ?hashes.len());
                        trace!(?hashes);

                        // zcashd sometimes appends an unrelated hash at the
                        // start or end of its response. Check the first hash
                        // against the previous response, and discard mismatches.
                        let unknown_hashes = match hashes.as_slice() {
                            [expected_hash, rest @ ..] if expected_hash == &tip.expected_next => {
                                rest
                            }
                            // If the first hash doesn't match, retry with the second.
                            [first_hash, expected_hash, rest @ ..]
                                if expected_hash == &tip.expected_next =>
                            {
                                debug!(?first_hash,
                                                ?tip.expected_next,
                                                ?tip.tip,
                                                "unexpected first hash, but the second matches: using the hashes after the match");
                                rest
                            }
                            // We ignore these responses
                            [] => continue,
                            [single_hash] => {
                                debug!(?single_hash,
                                                ?tip.expected_next,
                                                ?tip.tip,
                                                "discarding response containing a single unexpected hash");
                                continue;
                            }
                            [first_hash, second_hash, rest @ ..] => {
                                debug!(?first_hash,
                                                ?second_hash,
                                                rest_len = ?rest.len(),
                                                ?tip.expected_next,
                                                ?tip.tip,
                                                "discarding response that starts with two unexpected hashes");
                                continue;
                            }
                        };

                        // We use the last hash for the tip, and we want to avoid
                        // bad tips. So we discard the last hash. (We don't need
                        // to worry about missed downloads, because we will pick
                        // them up again in the next ExtendTips.)
                        let unknown_hashes = match unknown_hashes {
                            [] => continue,
                            [rest @ .., _last] => rest,
                        };

                        let new_tip = if let Some(end) = unknown_hashes.rchunks_exact(2).next() {
                            CheckedTip {
                                tip: end[0],
                                expected_next: end[1],
                            }
                        } else {
                            debug!("discarding response that extends only one block");
                            continue;
                        };

                        trace!(?unknown_hashes);

                        // Make sure we get the same tips, regardless of the
                        // order of peer responses
                        if !download_set.contains(&new_tip.expected_next) {
                            debug!(?new_tip,
                                            "adding new prospective tip, and removing any existing tips in the new block hash list");
                            self.prospective_tips
                                .retain(|t| !unknown_hashes.contains(&t.expected_next));
                            self.prospective_tips.insert(new_tip);
                        } else {
                            debug!(
                                ?new_tip,
                                "discarding prospective tip: already in download set"
                            );
                        }

                        // security: the first response determines our download order
                        //
                        // TODO: can we make the download order independent of response order?
                        let prev_download_len = download_set.len();
                        download_set.extend(unknown_hashes);
                        let new_download_len = download_set.len();
                        let new_hashes = new_download_len - prev_download_len;
                        debug!(new_hashes, "added hashes to download set");
                        metrics::histogram!("sync.extend.response.hash.count")
                            .record(new_hashes as f64);
                    }
                    Ok(_) => unreachable!("network returned wrong response"),
                    // We ignore this error because we made multiple fanout requests.
                    Err(e) => debug!(?e),
                }
            }
        }

        let new_downloads = download_set.len();
        debug!(new_downloads, "feeding new hashes to the sync engine");
        metrics::gauge!("sync.extend.queued.hash.count").set(new_downloads as f64);

        // security: use the actual number of new downloads from all peers,
        // so the last peer to respond can't toggle our mempool
        self.recent_syncs.push_extend_tips_length(new_downloads);

        metrics::histogram!("sync.stage.duration_seconds", "stage" => "extend_tips")
            .record(stage_start.elapsed().as_secs_f64());

        Ok(download_set)
    }

    /// Download the genesis block and commit it directly to the state, if it
    /// isn't currently known to our node.
    async fn request_genesis(&mut self) -> Result<(), Report> {
        // Due to Bitcoin protocol limitations, we can't request the genesis
        // block using our standard tip-following algorithm:
        //  - getblocks requires at least one hash
        //  - responses start with the block *after* the requested block, and
        //  - the genesis hash is used as a placeholder for "no matches".
        //
        // So we just download and commit the genesis block here.
        while !self.state_contains(self.genesis_hash).await? {
            info!("starting genesis block download");

            let response = timeout(SYNC_RESTART_DELAY, self.request_genesis_once()).await;

            match response {
                Ok(Ok(())) => {}
                // Handle timeouts and download/commit errors: startup races
                // (no ready peers yet, a peer without the block) all retry.
                Err(_elapsed) => {
                    info!("genesis block download timed out, retrying");
                    tokio::time::sleep(GENESIS_TIMEOUT_RETRY).await;
                }
                Ok(Err(error)) => {
                    info!(
                        ?error,
                        "could not download or commit genesis block, retrying"
                    );
                    tokio::time::sleep(GENESIS_TIMEOUT_RETRY).await;
                }
            }
        }

        Ok(())
    }

    /// Try once to download the genesis block from a peer, verify it against
    /// the network's pinned genesis hash, and commit it directly to the state.
    ///
    /// Blocks at or below the mandatory checkpoint floor never pass the
    /// semantic verifier's checkpoint gate, so genesis cannot go through the
    /// regular `downloads` path. Its hash is a network parameter — the same
    /// pin the removed checkpoint verifier applied — so the hash comparison is
    /// a complete verification, matching the startup path
    /// (`ibd::commit_genesis_if_missing`) for networks whose genesis block is
    /// not hard-coded (e.g. configured testnets with a custom genesis block).
    async fn request_genesis_once(&mut self) -> Result<(), Report> {
        let response = self
            .tip_network
            .ready()
            .await
            .map_err(|error| eyre!(error))?
            .call(zn::Request::BlocksByHash(
                iter::once(self.genesis_hash).collect(),
            ))
            .await
            .map_err(|error| eyre!(error))?;

        let zn::Response::Blocks(mut blocks) = response else {
            return Err(eyre!("wrong response to a genesis BlocksByHash request"));
        };

        let (block, _peer) = blocks
            .pop()
            .and_then(|inventory_response| inventory_response.available())
            .ok_or_else(|| eyre!("the peer did not have the genesis block"))?;

        let genesis = zs::CheckpointVerifiedBlock::from(block);
        if genesis.hash != self.genesis_hash {
            return Err(eyre!(
                "the peer served a block that is not the genesis block"
            ));
        }

        self.state
            .ready()
            .await
            .map_err(|error| eyre!(error))?
            .call(zebra_state::Request::CommitCheckpointVerifiedBlock(genesis))
            .await
            .map_err(|error| eyre!("committing the genesis block failed: {error}"))?;

        Ok(())
    }

    /// Returns `true` if the hash is present in the state, and `false`
    /// if the hash is not present in the state.
    pub(crate) async fn state_contains(&mut self, hash: block::Hash) -> Result<bool, Report> {
        match self
            .state
            .ready()
            .await
            .map_err(|e| eyre!(e))?
            .call(zebra_state::Request::KnownBlock(hash))
            .await
            .map_err(|e| eyre!(e))?
        {
            zs::Response::KnownBlock(loc) => Ok(loc.is_some()),
            _ => unreachable!("wrong response to known block request"),
        }
    }

    fn update_metrics(&self) {
        metrics::gauge!("sync.prospective_tips.len",).set(self.prospective_tips.len() as f64);
    }
}
