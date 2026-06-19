//! The syncer downloads and verifies large numbers of blocks from peers to Zebra.
//!
//! It is used when Zebra is a long way behind the current chain tip.

use std::{
    cmp::max,
    collections::{HashMap, HashSet},
    convert,
    future::Future,
    pin::Pin,
    task::Poll,
    time::{Duration, Instant},
};

use color_eyre::eyre::{eyre, Report};
use futures::{
    future::OptionFuture,
    stream::{FuturesUnordered, StreamExt},
};
use indexmap::IndexSet;
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{mpsc, watch},
    task::JoinError,
    time::{sleep, sleep_until, timeout},
};
use tower::{
    builder::ServiceBuilder, hedge::Hedge, limit::ConcurrencyLimit, retry::Retry, timeout::Timeout,
    Service, ServiceExt,
};

use zebra_chain::{
    block::{self, Height, HeightDiff},
    chain_tip::ChainTip,
};
use zebra_network::{self as zn, PeerSocketAddr};
use zebra_state as zs;

use crate::{
    components::sync::downloads::BlockDownloadVerifyError, config::ZebradConfig, BoxError,
};

mod downloads;
pub mod end_of_support;
mod gossip;
mod progress;
mod recent_sync_lengths;
mod status;

#[cfg(test)]
mod tests;

use downloads::{AlwaysHedge, Downloads, NotFoundKind};

pub use downloads::VERIFICATION_PIPELINE_SCALING_MULTIPLIER;
pub use gossip::{gossip_best_tip_block_hashes, BlockGossipError};
pub use progress::show_block_chain_progress;
pub use recent_sync_lengths::RecentSyncLengths;
pub use status::SyncStatus;

/// Controls the number of peers used for each ObtainTips and ExtendTips request.
const FANOUT: usize = 3;

/// Controls how many times we will retry each block download.
///
/// Failing block downloads is important because it defends against peers who
/// feed us bad hashes. But spurious failures of valid blocks cause the syncer to
/// restart from the previous checkpoint, potentially re-downloading blocks.
///
/// We also hedge requests, so we may retry up to twice this many times. Hedged
/// retries may be concurrent, inner retries are sequential.
const BLOCK_DOWNLOAD_RETRY_LIMIT: usize = 3;

/// Controls how many times the syncer requeues a required block hash after a peer responds
/// `notfound` (`NotFoundKind::Response`), before giving up on the round and obtaining fresh tips.
///
/// This retry happens at the sync queue level, so it can run after other peer requests finish and
/// newly ready peers become available. Each requeue routes to a different peer (the peer set marks
/// the responding peer as missing the hash), so the retries are peer-diverse and normally converge
/// to either a successful download or a `NotFoundKind::Registry` miss (no current peer has it).
///
/// The retries keep the in-flight download/verify pipeline alive — only an exhausted budget or a
/// registry-wide miss restarts the round. The budget is mainly a safety bound against a peer that
/// repeatedly advertises a hash via `FindBlocks` and then `notfound`s the download; peer
/// accountability in `zebra-network` disconnects such peers so this bound is rarely reached.
const MISSING_BLOCK_DOWNLOAD_RETRY_LIMIT: usize = 8;

/// Controls how many times the syncer retries a required block that the peer set reports as missing
/// from *all* current peers (`NotFoundKind::Registry`) before giving up on the round.
///
/// Unlike a single-peer `notfound`, a registry-wide miss can't be resolved by routing to another
/// currently-connected peer — it needs the peer crawler to find a peer that has the block, or the
/// inventory marks to expire. So we retry with a backoff (keeping the in-flight pipeline and its
/// already-downloaded blocks alive) for up to roughly [`REGISTRY_MISS_RETRY_BACKOFF`] ×
/// this many attempts, rather than discarding the whole round every time the head-of-line block is
/// temporarily unavailable. Only a genuinely stuck block (e.g. a bad tip) exhausts this and restarts.
const MISSING_BLOCK_REGISTRY_RETRY_LIMIT: usize = 60;

/// Backoff between retries of a block missing from all current peers (`NotFoundKind::Registry`).
///
/// Long enough to avoid hot-looping on the synchronous `NotFoundRegistry` routing error and to let
/// the peer crawler connect new peers / inventory marks expire, short enough that the pipeline
/// resumes promptly once a peer that has the block appears.
const REGISTRY_MISS_RETRY_BACKOFF: Duration = Duration::from_secs(2);

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
pub const MIN_CHECKPOINT_CONCURRENCY_LIMIT: usize = zebra_consensus::MAX_CHECKPOINT_HEIGHT_GAP;

/// The default for the user-specified lookahead limit.
///
/// Set to a few checkpoints' worth of blocks so the download pipeline can stay full while the
/// checkpoint verifier drains completed ranges, keeping the equihash-bound verifier continuously fed.
///
/// See [`MIN_CHECKPOINT_CONCURRENCY_LIMIT`] for details.
pub const DEFAULT_CHECKPOINT_CONCURRENCY_LIMIT: usize = MAX_TIPS_RESPONSE_HASH_COUNT * 2;

/// Default combined concurrency for Zakura block-body applies.
///
/// Zakura block sync can download checkpoint and full-verification bodies through
/// separate class limits. This cap bounds their combined apply queue so the
/// state/verifier backend controls the download window by finishing applies,
/// rather than accumulating a large backlog.
pub const DEFAULT_ZAKURA_BLOCK_APPLY_CONCURRENCY_LIMIT: usize = 32;

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

/// Start asking peers for more block hashes before we run out of hashes to download.
///
/// The syncer keeps a list of block hashes it has learned from `FindBlocks`
/// responses but has not yet requested as full blocks. When that list drops
/// below one typical `FindBlocks` response, start one background extension
/// request so downloads can continue without waiting for another peer round trip.
const MIN_UNREQUESTED_HASHES_BEFORE_EXTEND: usize = MAX_TIPS_RESPONSE_HASH_COUNT;

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

/// A shorter timeout used for the first few blocks after the final checkpoint.
///
/// This is a workaround for bug #5125, where the first fully validated blocks
/// after the final checkpoint fail with a timeout, due to a UTXO race condition.
const FINAL_CHECKPOINT_BLOCK_VERIFY_TIMEOUT: Duration = Duration::from_secs(2 * 60);

/// The number of blocks after the final checkpoint that get the shorter timeout.
///
/// We've only seen this error on the first few blocks after the final checkpoint.
const FINAL_CHECKPOINT_BLOCK_VERIFY_TIMEOUT_LIMIT: HeightDiff = 100;

/// Controls how long we wait for sync restart operations, including obtaining
/// tips and retrying the genesis block.
///
/// This should be long enough for peers to respond to tip requests on a thin or
/// flaky peer set. Shorter values can cause Zebra to loop on `obtain_tips`
/// timeouts without making progress.
const SYNC_RESTART_DELAY: Duration = Duration::from_secs(45);

/// Controls how long the syncer sleeps between sync runs before obtaining new
/// tips and restarting downloads.
///
/// This fork keeps the post-round idle short enough to recover quickly without
/// immediately hammering the same weak peer set after a failed sync round.
const SYNC_RESTART_SLEEP: Duration = Duration::from_secs(10);

/// In regtest, use a much shorter restart delay so that downstream nodes pick up
/// newly-mined blocks quickly (e.g. after `generate(N)` in integration tests).
/// The default restart sleep matches the typical fast-retry cadence for this fork.
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

/// How long the Zakura block-sync replacement waits for the verified tip to
/// advance before falling back to the legacy `ChainSync` body downloader.
///
/// When `v2_p2p` is enabled, the legacy syncer only bootstraps genesis and then
/// hands body sync to native Zakura sync (see [`ChainSync::bootstrap_genesis_then_pause`]).
/// If Zakura never makes progress — for example the node's only reachable peers
/// are legacy-only and do not advertise `NODE_P2P_V2`, or Zakura body sync has no
/// usable peers — parking the legacy syncer forever would leave the node
/// connected but stuck at genesis (an eclipse with non-upgrading peers can hold a
/// node there indefinitely). After this much time with no verified-tip progress,
/// the legacy syncer resumes as a fallback so legacy peers can drive body sync.
///
/// This is generous on purpose: a healthy node advancing its tip (mainnet
/// produces a block roughly every 75 seconds) resets the timer and never falls
/// back. Falling back when a node is genuinely caught up is harmless — the legacy
/// syncer simply finds no new blocks and idles.
const ZAKURA_BODY_SYNC_STALL_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How often [`ChainSync::bootstrap_genesis_then_pause`] polls the verified tip
/// while watching for Zakura body-sync progress.
const ZAKURA_BODY_SYNC_STALL_POLL: Duration = Duration::from_secs(10);

/// Gap (in blocks, best-header tip − verified tip) at or below which the node is
/// treated as caught up to the network frontier. At this distance, gossip keeping
/// the verified tip current is healthy, so the watchdog never falls back.
const ZAKURA_NEAR_TIP_GAP: HeightDiff = 2;

/// Minimum number of blocks the verified tip must close against the best-header
/// (network frontier) tip — measured since the last credited progress — to count
/// as real Zakura block-sync progress while the node is materially behind.
///
/// This is what distinguishes a working bulk downloader from a peer trickling
/// occasional next-height blocks over the gossip path. A healthy Zakura block sync
/// closes a real gap by hundreds to thousands of blocks per stall window, while a
/// gossip trickle slow enough to never catch up to the advancing network tip closes
/// only a handful (mainnet produces roughly one block per 75s, so over a single
/// [`ZAKURA_BODY_SYNC_STALL_TIMEOUT`] window the chain itself grows by only a few
/// blocks). Without this floor, one gossiped block per poll would reset the idle
/// timer forever and hold the fallback off while the node stayed arbitrarily far
/// behind the network.
const ZAKURA_BLOCK_SYNC_MIN_CLOSURE: HeightDiff = 64;

/// Cross-poll bookkeeping for [`ChainSync::bootstrap_genesis_then_pause`]'s Zakura
/// body-sync stall watchdog. See [`zakura_block_sync_stalled`].
#[derive(Clone, Copy, Debug)]
struct ZakuraStallTracker {
    /// Verified tip at the last poll. Only used by the degraded rule when no
    /// best-header frontier is known yet.
    last_verified_height: Option<Height>,

    /// Gap (best-header tip − verified tip) at the poll that last credited progress.
    /// Progress is credited again only once the gap closes a further
    /// [`ZAKURA_BLOCK_SYNC_MIN_CLOSURE`] below this anchor.
    progress_anchor_gap: Option<HeightDiff>,

    /// Consecutive idle polls without credited progress.
    idle_polls: u64,
}

impl ZakuraStallTracker {
    fn new(verified_height: Option<Height>) -> Self {
        Self {
            last_verified_height: verified_height,
            progress_anchor_gap: None,
            idle_polls: 0,
        }
    }
}

/// Decides whether Zakura block sync should be considered stalled — so the legacy
/// [`ChainSync::sync`] body downloader resumes as a fallback — from the latest
/// verified body tip and the best-header (network frontier) tip. Returns `true`
/// once `max_idle_polls` consecutive polls pass without credited progress.
///
/// Progress is deliberately **not** "the verified tip moved": inbound gossip blocks
/// bump the verified tip without Zakura block sync running, so a peer trickling
/// next-height blocks could otherwise hold the fallback off forever while the node
/// stays materially behind (the F-88602 finding). Instead, progress is one of:
///   * the node is within [`ZAKURA_NEAR_TIP_GAP`] of the frontier (caught up —
///     gossip keeping it current is fine), or
///   * the gap to the frontier has closed by at least
///     [`ZAKURA_BLOCK_SYNC_MIN_CLOSURE`] since the last credited progress (only a
///     working bulk downloader closes a real gap that fast).
///
/// When no frontier is known yet (`header_tip_height` is `None`), it degrades to the
/// original "verified tip advanced at all" rule so behavior does not regress before
/// header sync reports a frontier.
fn zakura_block_sync_stalled(
    tracker: &mut ZakuraStallTracker,
    verified_height: Option<Height>,
    header_tip_height: Option<Height>,
    max_idle_polls: u64,
) -> bool {
    // `Height - Height` yields a `HeightDiff` (i64); the best-header tip is never
    // below the verified tip in practice, but a negative gap is treated as caught up.
    let gap = match (header_tip_height, verified_height) {
        (Some(header), Some(verified)) => Some(header - verified),
        _ => None,
    };

    let made_progress = match gap {
        // No frontier known yet: fall back to the legacy "tip moved" rule.
        None => verified_height > tracker.last_verified_height,
        // Caught up to the frontier: healthy.
        Some(gap) if gap <= ZAKURA_NEAR_TIP_GAP => {
            tracker.progress_anchor_gap = Some(gap);
            true
        }
        // Materially behind: only a substantial closure since the anchor counts. The
        // anchor moves only when progress is credited (never on idle polls), so a
        // steady moderate sync still credits across polls, while a slow trickle that
        // never closes the floor keeps accumulating idle polls toward the fallback.
        Some(gap) => match tracker.progress_anchor_gap {
            None => {
                tracker.progress_anchor_gap = Some(gap);
                true
            }
            Some(anchor) if anchor - gap >= ZAKURA_BLOCK_SYNC_MIN_CLOSURE => {
                tracker.progress_anchor_gap = Some(gap);
                true
            }
            Some(_) => false,
        },
    };

    tracker.last_verified_height = verified_height;

    if made_progress {
        tracker.idle_polls = 0;
        false
    } else {
        tracker.idle_polls += 1;
        tracker.idle_polls >= max_idle_polls
    }
}

/// Queries the read-state service for the best-header (network frontier) tip height,
/// returning `None` if the service is unavailable or has no header tip yet.
async fn best_header_tip_height<RS>(read_state: &mut RS) -> Option<Height>
where
    RS: Service<zs::ReadRequest, Response = zs::ReadResponse, Error = BoxError>,
{
    let ready = read_state.ready().await.ok()?;
    match ready.call(zs::ReadRequest::BestHeaderTip).await {
        Ok(zs::ReadResponse::BestHeaderTip(tip)) => tip.map(|(height, _hash)| height),
        _ => None,
    }
}

/// Sync configuration section.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// The number of parallel block download requests.
    ///
    /// This is set to a low value by default, to avoid task and
    /// network contention. Increasing this value may improve
    /// performance on machines with a fast network connection.
    #[serde(alias = "max_concurrent_block_requests")]
    pub download_concurrency_limit: usize,

    /// The number of blocks submitted in parallel to the checkpoint verifier.
    ///
    /// Increasing this limit increases the buffer size, so it reduces
    /// the impact of an individual block request failing. However, it
    /// also increases memory and CPU usage if block validation stalls,
    /// or there are some large blocks in the pipeline.
    ///
    /// The block size limit is 2MB, so in theory, this could represent multiple
    /// gigabytes of data, if we downloaded arbitrary blocks. However,
    /// because we randomly load balance outbound requests, and separate
    /// block download from obtaining block hashes, an adversary would
    /// have to control a significant fraction of our peers to lead us
    /// astray.
    ///
    /// For reliable checkpoint syncing, Zebra enforces a
    /// [`MIN_CHECKPOINT_CONCURRENCY_LIMIT`].
    ///
    /// This is set to a high value by default, to avoid verification pipeline stalls.
    /// Decreasing this value reduces RAM usage.
    #[serde(alias = "lookahead_limit")]
    pub checkpoint_verify_concurrency_limit: usize,

    /// The number of blocks submitted in parallel to the full verifier.
    ///
    /// This is set to a low value by default, to avoid verification timeouts on large blocks.
    /// Increasing this value may improve performance on machines with many cores.
    pub full_verify_concurrency_limit: usize,

    /// The combined number of Zakura block-sync bodies submitted in parallel.
    ///
    /// This bounds the sum of checkpoint and full-verification block applies
    /// driven by Zakura block sync. The per-class limits still apply, but this
    /// cap keeps checkpoint sync from building a deep apply backlog and lets
    /// block-sync downloads self-throttle through the byte budget.
    pub zakura_block_apply_concurrency_limit: usize,

    /// The number of threads used to verify signatures, proofs, and other CPU-intensive code.
    ///
    /// If the number of threads is not configured or zero, Zebra uses the number of logical cores.
    /// If the number of logical cores can't be detected, Zebra uses one thread.
    /// For details, see [the `rayon` documentation](https://docs.rs/rayon/latest/rayon/struct.ThreadPoolBuilder.html#method.num_threads).
    pub parallel_cpu_threads: usize,

    /// Skip the Regtest direct genesis self-seed at startup, forcing this node to
    /// obtain the genesis block from peers like a Mainnet/Testnet node.
    ///
    /// On Regtest, zebrad normally commits the genesis block directly at startup
    /// (the genesis block is a known constant and there may be no peer to download
    /// it from). When this is `true`, that shortcut is skipped and genesis must be
    /// downloaded and verified from a peer instead. This exercises the production
    /// genesis-bootstrap path — in particular a Zakura-only node
    /// (`legacy_p2p = false`) that must fetch genesis over Zakura before native
    /// header/body sync can advance from an empty state.
    ///
    /// Defaults to `false`, so standalone Regtest nodes keep self-seeding genesis.
    ///
    /// Skipped when serializing so `zebrad generate` output (and the stored config
    /// compatibility snapshot) stays stable; it is only ever set by hand in
    /// test/bootstrap configs.
    #[serde(skip_serializing)]
    pub debug_skip_regtest_genesis_self_seed: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // 1/3 of the default outbound peer limit.
            download_concurrency_limit: 100,

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

            zakura_block_apply_concurrency_limit: DEFAULT_ZAKURA_BLOCK_APPLY_CONCURRENCY_LIMIT,

            // Use one thread per CPU.
            //
            // If this causes tokio executor starvation, move CPU-intensive tasks to rayon threads,
            // or reserve a few cores for tokio threads, based on `num_cpus()`.
            parallel_cpu_threads: 0,

            // Standalone Regtest nodes self-seed genesis; only opt-in test/bootstrap
            // setups download it from peers.
            debug_skip_regtest_genesis_self_seed: false,
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

    /// The largest block height for the checkpoint verifier, based on the current config.
    max_checkpoint_height: Height,

    /// The configured checkpoint verification concurrency limit, after applying the minimum limit.
    checkpoint_verify_concurrency_limit: usize,

    /// The configured full verification concurrency limit, after applying the minimum limit.
    full_verify_concurrency_limit: usize,

    /// Whether the node is running on regtest. Used to apply a shorter sync restart delay.
    is_regtest: bool,

    // Services
    //
    /// A network service which is used to perform ObtainTips and ExtendTips
    /// requests.
    ///
    /// Has no retry logic, because failover is handled using fanout.
    tip_network: Timeout<ZN>,

    /// A service which downloads and verifies blocks, using the provided
    /// network and verifier services.
    downloads: Pin<
        Box<
            Downloads<
                Hedge<ConcurrencyLimit<Retry<zn::RetryLimit, Timeout<ZN>>>, AlwaysHedge>,
                Timeout<ZV>,
                ZSTip,
            >,
        >,
    >,

    /// The cached block chain state.
    state: ZS,

    /// Allows efficient access to the best tip of the blockchain.
    latest_chain_tip: ZSTip,

    // Internal sync state
    //
    /// The tips that the syncer is currently following.
    prospective_tips: HashSet<CheckedTip>,

    /// The lengths of recent sync responses.
    recent_syncs: RecentSyncLengths,

    /// Queue-level retry counts for block hashes whose download failed with a single-peer
    /// `notfound` ([`NotFoundKind::Response`]). Bounded by [`MISSING_BLOCK_DOWNLOAD_RETRY_LIMIT`].
    missing_block_retry_counts: HashMap<block::Hash, usize>,

    /// Queue-level retry counts for block hashes missing from *all* current peers
    /// ([`NotFoundKind::Registry`]). Kept separate from [`Self::missing_block_retry_counts`] so the
    /// larger registry budget ([`MISSING_BLOCK_REGISTRY_RETRY_LIMIT`]) isn't defeated by an
    /// occasional single-peer `notfound` for the same hash sharing the smaller budget's count.
    registry_miss_retry_counts: HashMap<block::Hash, usize>,

    /// Required blocks that registry-missed and are waiting for their backoff to elapse before being
    /// retried, keyed by hash with the [`tokio::time::Instant`] each backoff expires.
    ///
    /// While this is non-empty, new speculative downloads are gated in [`Self::sync_round`] so the
    /// in-flight pipeline drains and frees ready-peer slots for the critical head-of-line block.
    /// The retries themselves are never gated: they fire from the `select!` timer arm so draining
    /// and tip extension continue concurrently during the backoff. A map so that a second block missing while the first is still
    /// backing off isn't dropped: every registry-missed required block stays scheduled.
    registry_miss_retry: HashMap<block::Hash, tokio::time::Instant>,

    /// Receiver that is `true` when the downloader is past the lookahead limit.
    /// This is based on the downloaded block height and the state tip height.
    past_lookahead_limit_receiver: zs::WatchReceiver<bool>,

    /// Sender for reporting peer addresses that advertised unexpectedly invalid transactions.
    misbehavior_sender: mpsc::Sender<(PeerSocketAddr, u32)>,
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
    ///  - chain: the zebra-chain `Network` to download (Mainnet or Testnet)
    ///  - peers: the zebra-network peers to contact for downloads
    ///  - verifier: the zebra-consensus verifier that checks the chain
    ///  - state: the zebra-state that stores the chain
    ///  - latest_chain_tip: the latest chain tip from `state`
    ///
    /// Also returns a [`SyncStatus`] to check if the syncer has likely reached the chain tip.
    pub fn new(
        config: &ZebradConfig,
        max_checkpoint_height: Height,
        peers: ZN,
        verifier: ZV,
        state: ZS,
        latest_chain_tip: ZSTip,
        misbehavior_sender: mpsc::Sender<(PeerSocketAddr, u32)>,
    ) -> (Self, SyncStatus) {
        let mut download_concurrency_limit = config.sync.download_concurrency_limit;
        let mut checkpoint_verify_concurrency_limit =
            config.sync.checkpoint_verify_concurrency_limit;
        let mut full_verify_concurrency_limit = config.sync.full_verify_concurrency_limit;

        if download_concurrency_limit < MIN_CONCURRENCY_LIMIT {
            warn!(
                "configured download concurrency limit {} too low, increasing to {}",
                config.sync.download_concurrency_limit, MIN_CONCURRENCY_LIMIT,
            );

            download_concurrency_limit = MIN_CONCURRENCY_LIMIT;
        }

        if checkpoint_verify_concurrency_limit < MIN_CHECKPOINT_CONCURRENCY_LIMIT {
            warn!(
                "configured checkpoint verify concurrency limit {} too low, increasing to {}",
                config.sync.checkpoint_verify_concurrency_limit, MIN_CHECKPOINT_CONCURRENCY_LIMIT,
            );

            checkpoint_verify_concurrency_limit = MIN_CHECKPOINT_CONCURRENCY_LIMIT;
        }

        if full_verify_concurrency_limit < MIN_CONCURRENCY_LIMIT {
            warn!(
                "configured full verify concurrency limit {} too low, increasing to {}",
                config.sync.full_verify_concurrency_limit, MIN_CONCURRENCY_LIMIT,
            );

            full_verify_concurrency_limit = MIN_CONCURRENCY_LIMIT;
        }

        let tip_network = Timeout::new(peers.clone(), TIPS_RESPONSE_TIMEOUT);

        // The Hedge middleware is the outermost layer, hedging requests
        // between two retry-wrapped networks.  The innermost timeout
        // layer is relatively unimportant, because slow requests will
        // probably be preemptively hedged.
        //
        // The Hedge goes outside the Retry, because the Retry layer
        // abstracts away spurious failures from individual peers
        // making a less-fallible network service, and the Hedge layer
        // tries to reduce latency of that less-fallible service.
        let block_network = Hedge::new(
            ServiceBuilder::new()
                .concurrency_limit(download_concurrency_limit)
                .retry(zn::RetryLimit::new(BLOCK_DOWNLOAD_RETRY_LIMIT))
                .timeout(BLOCK_DOWNLOAD_TIMEOUT)
                .service(peers),
            AlwaysHedge,
            20,
            0.95,
            2 * SYNC_RESTART_DELAY,
        );

        // We apply a timeout to the verifier to avoid hangs due to missing earlier blocks.
        let verifier = Timeout::new(verifier, BLOCK_VERIFY_TIMEOUT);

        let (sync_status, recent_syncs) = SyncStatus::new_for_network(&config.network.network);

        let (past_lookahead_limit_sender, past_lookahead_limit_receiver) = watch::channel(false);
        let past_lookahead_limit_receiver = zs::WatchReceiver::new(past_lookahead_limit_receiver);

        let downloads = Box::pin(Downloads::new(
            block_network,
            verifier,
            latest_chain_tip.clone(),
            past_lookahead_limit_sender,
            max(
                checkpoint_verify_concurrency_limit,
                full_verify_concurrency_limit,
            ),
            max_checkpoint_height,
        ));

        let new_syncer = Self {
            genesis_hash: config.network.network.genesis_hash(),
            max_checkpoint_height,
            checkpoint_verify_concurrency_limit,
            full_verify_concurrency_limit,
            is_regtest: config.network.network.is_regtest(),
            tip_network,
            downloads,
            state,
            latest_chain_tip,
            prospective_tips: HashSet::new(),
            recent_syncs,
            missing_block_retry_counts: HashMap::new(),
            registry_miss_retry_counts: HashMap::new(),
            registry_miss_retry: HashMap::new(),
            past_lookahead_limit_receiver,
            misbehavior_sender,
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
            if self.try_to_sync().await.is_err() {
                self.downloads.cancel_all();
            }

            self.update_metrics();

            let restart_delay = if self.is_regtest {
                REGTEST_SYNC_RESTART_DELAY
            } else {
                SYNC_RESTART_SLEEP
            };
            info!(
                timeout = ?restart_delay,
                state_tip = ?self.latest_chain_tip.best_tip_height(),
                "waiting to restart sync"
            );
            sleep(restart_delay).await;
        }
    }

    /// Downloads and verifies genesis, then hands body sync to native Zakura sync
    /// while watching for progress, falling back to the legacy syncer if Zakura
    /// makes none.
    ///
    /// Zakura block sync uses this bootstrap path because header range validation needs the
    /// committed genesis header before native Zakura header/body sync can advance from scratch.
    ///
    /// After genesis, native Zakura sync is expected to drive body downloads. But
    /// it cannot always: the default config enables both `v2_p2p` and `legacy_p2p`,
    /// and legacy-only peers (no `NODE_P2P_V2`) still connect, so a node whose
    /// reachable peers are legacy-only — or one eclipsed by non-upgrading peers —
    /// would have no usable Zakura body-sync peers. Parking forever there leaves
    /// the node connected but stuck at genesis. So instead of parking, watch the
    /// verified tip; if it does not advance for [`ZAKURA_BODY_SYNC_STALL_TIMEOUT`],
    /// resume the legacy [`ChainSync::sync`] loop as a fallback.
    ///
    /// `read_state` answers [`ReadRequest::BestHeaderTip`](zs::ReadRequest::BestHeaderTip)
    /// so the watchdog can tell genuine Zakura block-sync progress (the verified tip
    /// closing a real gap to the network frontier) from a peer trickling next-height
    /// blocks over gossip (which bumps the verified tip without body sync running).
    #[instrument(skip(self, read_state))]
    pub async fn bootstrap_genesis_then_pause<RS>(
        mut self,
        mut read_state: RS,
    ) -> Result<(), Report>
    where
        RS: Service<zs::ReadRequest, Response = zs::ReadResponse, Error = BoxError>
            + Send
            + 'static,
        RS::Future: Send,
    {
        self.request_genesis().await?;
        info!(
            "Zakura block sync replacement completed genesis bootstrap; \
             monitoring for Zakura body-sync progress"
        );

        // Number of consecutive idle polls (no credited progress) that trip the
        // fallback. `as_secs` is non-zero for both constants, so this is >= 1.
        let max_idle_polls = (ZAKURA_BODY_SYNC_STALL_TIMEOUT.as_secs()
            / ZAKURA_BODY_SYNC_STALL_POLL.as_secs())
        .max(1);

        let mut tracker = ZakuraStallTracker::new(self.latest_chain_tip.best_tip_height());
        loop {
            sleep(ZAKURA_BODY_SYNC_STALL_POLL).await;

            let verified_height = self.latest_chain_tip.best_tip_height();
            let header_tip_height = best_header_tip_height(&mut read_state).await;

            if zakura_block_sync_stalled(
                &mut tracker,
                verified_height,
                header_tip_height,
                max_idle_polls,
            ) {
                warn!(
                    verified_tip = ?verified_height,
                    header_tip = ?header_tip_height,
                    stall = ?ZAKURA_BODY_SYNC_STALL_TIMEOUT,
                    "Zakura body sync is not closing the gap to the network tip; falling back \
                     to legacy ChainSync so legacy peers can drive body sync"
                );
                return self.sync().await;
            }
        }
    }

    /// Tries to synchronize the chain as far as it can.
    ///
    /// Obtains some prospective tips and iteratively tries to extend them and download the missing
    /// blocks.
    ///
    /// Returns `Ok` if it was able to synchronize as much of the chain as it could, and then ran
    /// out of prospective tips. This happens when synchronization finishes or if Zebra ended up
    /// following a fork. Either way, Zebra should attempt to obtain some more tips.
    ///
    /// Returns `Err` if there was an unrecoverable error and restarting the synchronization is
    /// necessary. This includes outer timeouts, where an entire syncing step takes an extremely
    /// long time. (These usually indicate hangs.)
    #[instrument(skip(self))]
    async fn try_to_sync(&mut self) -> Result<(), Report> {
        self.prospective_tips = HashSet::new();
        self.missing_block_retry_counts.clear();
        self.registry_miss_retry_counts.clear();
        self.registry_miss_retry.clear();

        info!(
            state_tip = ?self.latest_chain_tip.best_tip_height(),
            "starting sync, obtaining new tips"
        );
        let extra_hashes = timeout(SYNC_RESTART_DELAY, self.obtain_tips())
            .await
            .map_err(Into::into)
            // TODO: replace with flatten() when it stabilises (#70142)
            .and_then(convert::identity)
            .map_err(|e| {
                info!("temporary error obtaining tips: {:#}", e);
                e
            })?;
        self.update_metrics();

        self.sync_round(extra_hashes).await?;

        info!("exhausted prospective tip set");

        Ok(())
    }

    /// Drives one sync round to completion: dispatches downloads, drains completed blocks, and
    /// continuously refills the hash reserve by extending tips *concurrently* with draining and
    /// dispatch.
    ///
    /// `reserve` is the set of discovered-but-not-yet-dispatched block hashes (initially the
    /// leftovers from `obtain_tips`).
    ///
    /// Returns `Ok(())` once the round is exhausted: nothing in flight, nothing queued, and no tips
    /// left to extend. Returns `Err` if an unrecoverable error means the sync should restart.
    #[instrument(skip(self, reserve))]
    async fn sync_round(&mut self, mut reserve: IndexSet<block::Hash>) -> Result<(), Report> {
        // The type of the in-flight tip-extension future.
        type ExtendOutput = Result<(IndexSet<block::Hash>, HashSet<CheckedTip>, usize), Report>;

        // The currently running request for more block hashes, if any.
        //
        // This future only asks peers for hashes. It does not request or verify
        // full blocks. We keep at most one extension request in flight so the
        // syncer cannot build up an unbounded backlog of undispatched hashes.
        let mut extend: Option<Pin<Box<dyn Future<Output = ExtendOutput> + Send>>> = None;

        // The last time this sync round made observable progress.
        //
        // Progress means a block finished verification, a background hash
        // extension finished, or more full-block downloads were queued. If none
        // of those happen for `BLOCK_VERIFY_TIMEOUT`, restart the round.
        let mut last_progress = Instant::now();

        loop {
            // Opportunistically handle any block tasks that are already finished, without blocking.
            while let Poll::Ready(Some(rsp)) = futures::poll!(self.downloads.next()) {
                // Handle completed block tasks. Missing blocks may be requeued; duplicate,
                // cancelled, behind-tip, above-lookahead, and no-height blocks are treated as
                // non-fatal. Other download or verification errors restart this sync round.
                self.handle_block_response_with_missing_retry(rsp).await?;
                last_progress = Instant::now();
            }
            metrics::gauge!("sync.reserve.depth").set(reserve.len() as f64);
            self.update_metrics();

            // Ask for more block hashes while we still have some left to download.
            //
            // Waiting until the undispatched hash list is empty would leave the
            // downloader idle while peers respond to `FindBlocks`. Starting the
            // next extension early lets downloads keep running during that round
            // trip.
            if extend.is_none()
                && reserve.len() < MIN_UNREQUESTED_HASHES_BEFORE_EXTEND
                && !self.prospective_tips.is_empty()
            {
                debug!(
                    tips.len = self.prospective_tips.len(),
                    in_flight = self.downloads.in_flight(),
                    reserve = reserve.len(),
                    "prefetching more tip hashes",
                );

                let tip_network = self.tip_network.clone();
                let tips = std::mem::take(&mut self.prospective_tips);
                extend = Some(Box::pin(Self::build_extend(tip_network, tips)));
            }

            // Dispatch from the reserve while we're below the lookahead limit.
            //
            // We pause new downloads once the syncer or downloader are past their lookahead limits.
            // To avoid a deadlock or long waits for blocks to expire, we ignore the lookahead limit
            // when there are only a small number of blocks waiting.
            let lookahead_limit = self.lookahead_limit(reserve.len());
            let past_lookahead = self.downloads.in_flight() >= lookahead_limit
                || (self.downloads.in_flight() >= lookahead_limit / 2
                    && self.past_lookahead_limit_receiver.cloned_watch_data());

            // Head-of-line priority: while a required block is missing from all current peers and
            // waiting on its registry-miss backoff, pause *new* speculative dispatch so in-flight
            // downloads drain and free up ready-peer slots. Otherwise lookahead work can keep every
            // peer busy and starve the critical retry. This is inert in healthy sync.
            let head_of_line_starved = !self.registry_miss_retry.is_empty();

            if !past_lookahead && !head_of_line_starved && !reserve.is_empty() {
                debug!(
                    tips.len = self.prospective_tips.len(),
                    in_flight = self.downloads.in_flight(),
                    reserve = reserve.len(),
                    lookahead_limit,
                    state_tip = ?self.latest_chain_tip.best_tip_height(),
                    "requesting more blocks",
                );

                let response = timeout(
                    BLOCK_VERIFY_TIMEOUT,
                    self.request_blocks(std::mem::take(&mut reserve)),
                )
                .await
                .map_err(Report::from)?;
                reserve = Self::handle_hash_response(response)?;
                last_progress = Instant::now();
                continue;
            }

            // The round is exhausted once there's nothing in flight, nothing queued, nothing left to
            // discover, and no critical block waiting to be retried after a registry-miss backoff.
            if self.downloads.in_flight() == 0
                && reserve.is_empty()
                && extend.is_none()
                && self.prospective_tips.is_empty()
                && self.registry_miss_retry.is_empty()
            {
                break;
            }

            // Wait for the next bit of progress: a completed block, a finished tip extension, or a
            // due registry-miss retry. At least one arm is enabled here: if nothing is in flight,
            // then either an extension is running or a registry-miss retry is pending. A stall
            // restarts the round.
            let has_inflight = self.downloads.in_flight() > 0;
            // Copy the earliest backoff deadline out so the timer future doesn't borrow `self`
            // across the `select!` while other arms borrow `self.downloads`.
            let registry_retry_at = self.registry_miss_retry.values().min().copied();
            let step = timeout(BLOCK_VERIFY_TIMEOUT, async {
                tokio::select! {
                    biased;

                    // Retry required blocks that registry-missed once their backoff elapses. This
                    // is not gated by speculative lookahead dispatch, so the head-of-line block gets
                    // another chance after the in-flight downloads have had time to drain.
                    _ = OptionFuture::from(registry_retry_at.map(sleep_until)),
                        if registry_retry_at.is_some() =>
                    {
                        let now = tokio::time::Instant::now();
                        let due: Vec<block::Hash> = self
                            .registry_miss_retry
                            .iter()
                            .filter(|(_, deadline)| **deadline <= now)
                            .map(|(hash, _)| *hash)
                            .collect();

                        for hash in due {
                            self.registry_miss_retry.remove(&hash);

                            match self.downloads.download_and_verify(hash).await {
                                Ok(())
                                | Err(BlockDownloadVerifyError::DuplicateBlockQueuedForDownload {
                                    ..
                                }) => {}
                                Err(error) => self.handle_block_response(Err(error))?,
                            }
                        }

                        last_progress = Instant::now();
                    }

                    rsp = self.downloads.next(), if has_inflight => {
                        let rsp = rsp.expect("downloads is nonempty");
                        self.handle_block_response_with_missing_retry(rsp).await?;
                        last_progress = Instant::now();
                        self.update_metrics();
                    }

                    extended = OptionFuture::from(extend.as_mut()), if extend.is_some() => {
                        let (download_set, new_tips, discovered) =
                            extended.expect("only polled while an extension is in flight")?;
                        self.prospective_tips = new_tips;
                        // security: use the actual number of new downloads from all peers, so the
                        // last peer to respond can't toggle our mempool.
                        self.recent_syncs.push_extend_tips_length(discovered);
                        reserve.extend(download_set);
                        extend = None;
                        last_progress = Instant::now();
                    }
                }

                Ok::<(), Report>(())
            })
            .await;

            match step {
                Ok(result) => result?,
                Err(_elapsed) => {
                    if last_progress.elapsed() >= BLOCK_VERIFY_TIMEOUT {
                        return Err(eyre!(
                            "sync round stalled: no block completed or tips extended within timeout"
                        ));
                    }
                }
            }
        }

        Ok(())
    }

    /// Given a block_locator list fan out request for subsequent hashes to
    /// multiple peers
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
        debug!(new_downloads, "queueing new downloads");
        metrics::gauge!("sync.obtain.queued.hash.count").set(new_downloads as f64);

        // security: use the actual number of new downloads from all peers,
        // so the last peer to respond can't toggle our mempool
        self.recent_syncs.push_obtain_tips_length(new_downloads);

        let response = self.request_blocks(download_set).await;

        metrics::histogram!("sync.stage.duration_seconds", "stage" => "obtain_tips")
            .record(stage_start.elapsed().as_secs_f64());

        Self::handle_hash_response(response).map_err(Into::into)
    }

    /// Asks peers to extend the given prospective `tips`, returning the newly discovered block
    /// hashes in download order (the `download_set`), the new prospective tip set, and the count of
    /// discovered hashes — *without* dispatching anything or touching `self`.
    ///
    /// Self-contained (owns a clone of the tip network and the taken tips) so the caller can poll it
    /// concurrently with draining completed downloads and dispatching new ones, overlapping the
    /// FindBlocks round-trip with the still-draining download buffer instead of stalling the
    /// pipeline once the reserve empties.
    #[instrument(skip_all)]
    async fn build_extend(
        mut tip_network: Timeout<ZN>,
        tips: HashSet<CheckedTip>,
    ) -> Result<(IndexSet<block::Hash>, HashSet<CheckedTip>, usize), Report> {
        let stage_start = std::time::Instant::now();

        let mut prospective_tips: HashSet<CheckedTip> = HashSet::new();
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

                let ready_tip_network = tip_network.ready().await;
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
                            prospective_tips.retain(|t| !unknown_hashes.contains(&t.expected_next));
                            prospective_tips.insert(new_tip);
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
        debug!(new_downloads, "discovered new hashes to download");
        metrics::gauge!("sync.extend.queued.hash.count").set(new_downloads as f64);

        metrics::histogram!("sync.stage.duration_seconds", "stage" => "extend_tips")
            .record(stage_start.elapsed().as_secs_f64());

        // The caller records `new_downloads` via `recent_syncs.push_extend_tips_length` on
        // write-back, preserving the "last peer can't toggle our mempool" security property.
        Ok((download_set, prospective_tips, new_downloads))
    }

    /// Download and verify the genesis block, if it isn't currently known to
    /// our node.
    async fn request_genesis(&mut self) -> Result<(), Report> {
        // Due to Bitcoin protocol limitations, we can't request the genesis
        // block using our standard tip-following algorithm:
        //  - getblocks requires at least one hash
        //  - responses start with the block *after* the requested block, and
        //  - the genesis hash is used as a placeholder for "no matches".
        //
        // So we just download and verify the genesis block here.
        while !self.state_contains(self.genesis_hash).await? {
            info!("starting genesis block download and verify");

            let response = timeout(SYNC_RESTART_DELAY, self.request_genesis_once())
                .await
                .map_err(Into::into);

            // 3 layers of results is not ideal, but we need the timeout on the outside.
            match response {
                Ok(Ok(Ok(response))) => self
                    .handle_block_response(Ok(response))
                    .expect("never returns Err for Ok"),
                // Handle fatal errors
                Ok(Err(fatal_error)) => Err(fatal_error)?,
                // Handle timeouts and block errors
                Err(error) | Ok(Ok(Err(error))) => {
                    if self.is_duplicate_finalized_genesis_error(&error) {
                        info!(
                            ?error,
                            "genesis block is already finalized, continuing sync"
                        );
                        return Ok(());
                    }

                    // TODO: exit syncer on permanent service errors (NetworkError, VerifierError)
                    if Self::should_restart_sync(&error) {
                        warn!(
                            ?error,
                            "could not download or verify genesis block, retrying"
                        );
                    } else {
                        info!(
                            ?error,
                            "temporary error downloading or verifying genesis block, retrying"
                        );
                    }

                    tokio::time::sleep(GENESIS_TIMEOUT_RETRY).await;
                }
            }
        }

        Ok(())
    }

    fn is_duplicate_finalized_genesis_error(&self, error: &BlockDownloadVerifyError) -> bool {
        match error {
            BlockDownloadVerifyError::Invalid {
                error,
                height,
                hash,
                ..
            } => {
                *height == Height(0)
                    && *hash == self.genesis_hash
                    && Self::is_duplicate_finalized_error(error)
            }
            _ => false,
        }
    }

    fn is_duplicate_finalized_error(error: &zebra_consensus::RouterError) -> bool {
        error.duplicate_location() == Some(&zs::KnownBlock::Finalized)
    }

    /// Try to download and verify the genesis block once.
    ///
    /// Fatal errors are returned in the outer result, temporary errors in the inner one.
    async fn request_genesis_once(
        &mut self,
    ) -> Result<Result<(Height, block::Hash), BlockDownloadVerifyError>, Report> {
        let response = self.downloads.download_and_verify(self.genesis_hash).await;
        Self::handle_response(response).map_err(|e| eyre!(e))?;

        let response = self.downloads.next().await.expect("downloads is nonempty");

        Ok(response)
    }

    /// Queue download and verify tasks for each block that isn't currently known to our node.
    ///
    /// TODO: turn obtain and extend tips into a separate task, which sends hashes via a channel?
    async fn request_blocks(
        &mut self,
        mut hashes: IndexSet<block::Hash>,
    ) -> Result<IndexSet<block::Hash>, BlockDownloadVerifyError> {
        let lookahead_limit = self.lookahead_limit(hashes.len());

        debug!(
            hashes.len = hashes.len(),
            ?lookahead_limit,
            "requesting blocks",
        );

        let extra_hashes = if hashes.len() > lookahead_limit {
            hashes.split_off(lookahead_limit)
        } else {
            IndexSet::new()
        };

        // Dispatch blocks with duplicate-tolerant error handling.
        // DuplicateBlockQueuedForDownload is caught and skipped instead of
        // propagating — this prevents dropping unprocessed hashes from the
        // batch, which would create frontier gaps and stalls (#5709).
        for hash in hashes.into_iter() {
            match self.downloads.download_and_verify(hash).await {
                Ok(()) => {}
                Err(BlockDownloadVerifyError::DuplicateBlockQueuedForDownload { .. }) => {
                    debug!("block request was already queued, continuing");
                }
                Err(error) => return Err(error),
            }
        }

        Ok(extra_hashes)
    }

    /// The configured lookahead limit, based on the currently verified height,
    /// and the number of hashes we haven't queued yet.
    fn lookahead_limit(&self, new_hashes: usize) -> usize {
        let max_checkpoint_height: usize = self
            .max_checkpoint_height
            .0
            .try_into()
            .expect("fits in usize");

        // When the state is empty, we want to verify using checkpoints
        let verified_height: usize = self
            .latest_chain_tip
            .best_tip_height()
            .unwrap_or(Height(0))
            .0
            .try_into()
            .expect("fits in usize");

        if verified_height >= max_checkpoint_height {
            self.full_verify_concurrency_limit
        } else if (verified_height + new_hashes) >= max_checkpoint_height {
            // If we're just about to start full verification, allow enough for the remaining checkpoint,
            // and also enough for a separate full verification lookahead.
            let checkpoint_hashes = verified_height + new_hashes - max_checkpoint_height;

            self.full_verify_concurrency_limit + checkpoint_hashes
        } else {
            self.checkpoint_verify_concurrency_limit
        }
    }

    /// Handles a response for a requested block.
    ///
    /// See [`Self::handle_response`] for more details.
    #[allow(unknown_lints)]
    fn handle_block_response(
        &mut self,
        response: Result<(Height, block::Hash), BlockDownloadVerifyError>,
    ) -> Result<(), BlockDownloadVerifyError> {
        match response {
            Ok((height, hash)) => {
                trace!(?height, ?hash, "verified and committed block to state");
                return Ok(());
            }

            Err(BlockDownloadVerifyError::Invalid {
                ref error,
                advertiser_addr: Some(advertiser_addr),
                ..
            }) if error.misbehavior_score() != 0 => {
                let _ = self
                    .misbehavior_sender
                    .try_send((advertiser_addr, error.misbehavior_score()));
            }

            Err(BlockDownloadVerifyError::AboveLookaheadHeightLimit {
                advertiser_addr: Some(advertiser_addr),
                ..
            }) => {
                let _ = self.misbehavior_sender.try_send((advertiser_addr, 100));
            }

            Err(BlockDownloadVerifyError::InvalidHeight {
                advertiser_addr: Some(advertiser_addr),
                ..
            }) => {
                let _ = self.misbehavior_sender.try_send((advertiser_addr, 100));
            }

            Err(_) => {}
        };

        Self::handle_response(response)
    }

    /// Handles a downloaded block response, requeueing required missing block hashes.
    ///
    /// The block download service already retries each `BlocksByHash` request and may hedge it to
    /// another peer. If a peer still responds `notfound` ([`NotFoundKind::Response`]), the syncer
    /// requeues the required hash — which routes to a different peer, since the peer set now marks
    /// the responding peer as missing it — and keeps the in-flight download/verify pipeline alive,
    /// rather than discarding the whole round. The requeues are bounded by
    /// [`MISSING_BLOCK_DOWNLOAD_RETRY_LIMIT`].
    ///
    /// A [`NotFoundKind::Registry`] miss means the peer set found that *every* ready peer is marked
    /// missing the block, so it can't be served right now. Rather than blocking the loop on an inline
    /// `sleep`, the hash is recorded in [`Self::registry_miss_retry`] with a backoff deadline; while
    /// any such retry is pending, [`Self::sync_round`] gives it head-of-line priority by pausing new
    /// speculative dispatch and re-dispatching the hash from its `select!` timer arm once the backoff
    /// elapses. Bounded by [`MISSING_BLOCK_REGISTRY_RETRY_LIMIT`]; only a block that stays missing
    /// for the whole budget (e.g. a bad tip) falls through to [`Self::handle_block_response`] and
    /// restarts the round to obtain fresh tips and peers.
    async fn handle_block_response_with_missing_retry(
        &mut self,
        response: Result<(Height, block::Hash), BlockDownloadVerifyError>,
    ) -> Result<(), Report> {
        if let Ok((_height, hash)) = response.as_ref() {
            self.missing_block_retry_counts.remove(hash);
            self.registry_miss_retry_counts.remove(hash);
            self.registry_miss_retry.remove(hash);
        }

        if let Some((hash, kind)) = response
            .as_ref()
            .err()
            .and_then(BlockDownloadVerifyError::not_found_download)
        {
            match kind {
                // A single peer didn't have the block, but others may. Requeue (routing to a
                // different peer) and keep the rest of the pipeline running. Only an exhausted
                // budget restarts the round.
                NotFoundKind::Response => {
                    let retry_count = self.missing_block_retry_counts.entry(hash).or_default();

                    if *retry_count < MISSING_BLOCK_DOWNLOAD_RETRY_LIMIT {
                        *retry_count += 1;

                        info!(
                            ?hash,
                            retry_attempt = *retry_count,
                            retry_limit = MISSING_BLOCK_DOWNLOAD_RETRY_LIMIT,
                            "missing sync block download failed, retrying required block"
                        );
                        metrics::counter!("sync.missing.block.requeued.count").increment(1);

                        match self.downloads.download_and_verify(hash).await {
                            Ok(()) => return Ok(()),
                            Err(BlockDownloadVerifyError::DuplicateBlockQueuedForDownload {
                                ..
                            }) => {
                                return Ok(());
                            }
                            Err(error) => self.handle_block_response(Err(error))?,
                        }

                        return Ok(());
                    }

                    self.missing_block_retry_counts.remove(&hash);

                    warn!(
                        ?hash,
                        retry_limit = MISSING_BLOCK_DOWNLOAD_RETRY_LIMIT,
                        "missing sync block retry budget exhausted, restarting sync"
                    );
                    metrics::counter!("sync.missing.block.retry.limit.count").increment(1);
                }

                // No currently-connected peer has the block. Routing to another connected peer
                // won't help, but discarding the whole round (and its already-downloaded blocks)
                // every time the head-of-line block is briefly unavailable is wasteful — it just
                // re-downloads the same range once the peer crawler finds a peer that has it.
                //
                // Instead, keep the in-flight pipeline alive and retry the block after a backoff,
                // giving the crawler time to connect new peers / inventory marks time to expire.
                // Only a block that stays missing for the whole budget (e.g. a bad tip) restarts.
                NotFoundKind::Registry => {
                    metrics::counter!("sync.missing.block.registry.miss.count").increment(1);

                    let retry_count = self.registry_miss_retry_counts.entry(hash).or_default();

                    if *retry_count < MISSING_BLOCK_REGISTRY_RETRY_LIMIT {
                        *retry_count += 1;

                        debug!(
                            ?hash,
                            retry_attempt = *retry_count,
                            retry_limit = MISSING_BLOCK_REGISTRY_RETRY_LIMIT,
                            "required sync block is missing from all current peers, retrying after backoff"
                        );
                        metrics::counter!("sync.missing.block.registry.retry.count").increment(1);

                        // Schedule the retry instead of blocking the loop on an inline `sleep`. While
                        // it's pending, `sync_round` gates new speculative dispatch (head-of-line
                        // priority) and keeps draining, so peers free up before the timer fires and
                        // the block can finally reach one that has it.
                        self.registry_miss_retry.insert(
                            hash,
                            tokio::time::Instant::now() + REGISTRY_MISS_RETRY_BACKOFF,
                        );

                        return Ok(());
                    }

                    self.registry_miss_retry_counts.remove(&hash);
                    self.registry_miss_retry.remove(&hash);

                    warn!(
                        ?hash,
                        retry_limit = MISSING_BLOCK_REGISTRY_RETRY_LIMIT,
                        "required sync block missing from all peers past retry budget, restarting sync"
                    );
                }
            }
        }

        self.handle_block_response(response)?;

        Ok(())
    }

    /// Handles a response to block hash submission, passing through any extra hashes.
    ///
    /// See [`Self::handle_response`] for more details.
    #[allow(unknown_lints)]
    fn handle_hash_response(
        response: Result<IndexSet<block::Hash>, BlockDownloadVerifyError>,
    ) -> Result<IndexSet<block::Hash>, BlockDownloadVerifyError> {
        match response {
            Ok(extra_hashes) => Ok(extra_hashes),
            Err(_) => Self::handle_response(response).map(|()| IndexSet::new()),
        }
    }

    /// Handles a response to a syncer request.
    ///
    /// Returns `Ok` if the request was successful, or if an expected error occurred,
    /// so that the synchronization can continue normally.
    ///
    /// Returns `Err` if an unexpected error occurred, to force the synchronizer to restart.
    #[allow(unknown_lints)]
    fn handle_response<T>(
        response: Result<T, BlockDownloadVerifyError>,
    ) -> Result<(), BlockDownloadVerifyError> {
        match response {
            Ok(_t) => Ok(()),
            Err(error) => {
                // TODO: exit syncer on permanent service errors (NetworkError, VerifierError)
                if Self::should_restart_sync(&error) {
                    Err(error)
                } else {
                    Ok(())
                }
            }
        }
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

    fn update_metrics(&mut self) {
        metrics::gauge!("sync.prospective_tips.len",).set(self.prospective_tips.len() as f64);
        metrics::gauge!("sync.downloads.in_flight",).set(self.downloads.in_flight() as f64);
    }

    /// Return if the sync should be restarted based on the given error
    /// from the block downloader and verifier stream.
    fn should_restart_sync(e: &BlockDownloadVerifyError) -> bool {
        match e {
            // Structural matches: downcasts
            BlockDownloadVerifyError::Invalid { error, .. } if error.is_duplicate_request() => {
                debug!(error = ?e, "block was already verified or committed, possibly from a previous sync run, continuing");
                false
            }
            BlockDownloadVerifyError::Invalid { error, .. }
                if Self::is_duplicate_finalized_error(error) =>
            {
                debug!(
                    error = ?e,
                    "block was already finalized, possibly from a previous sync run, continuing"
                );
                false
            }

            // Structural matches: direct
            BlockDownloadVerifyError::CancelledDuringDownload { .. }
            | BlockDownloadVerifyError::CancelledDuringVerification { .. } => {
                debug!(error = ?e, "block verification was cancelled, continuing");
                false
            }
            BlockDownloadVerifyError::BehindTipHeightLimit { .. } => {
                debug!(
                    error = ?e,
                    "block height is behind the current state tip, \
                     assuming the syncer will eventually catch up to the state, continuing"
                );
                false
            }
            BlockDownloadVerifyError::AboveLookaheadHeightLimit { .. } => {
                debug!(
                    error = ?e,
                    "block height is above the lookahead limit, \
                     dropping the block and continuing sync"
                );
                false
            }
            BlockDownloadVerifyError::InvalidHeight { .. } => {
                debug!(
                    error = ?e,
                    "block has no valid height, \
                     dropping the block and continuing sync"
                );
                false
            }
            BlockDownloadVerifyError::DuplicateBlockQueuedForDownload { .. } => {
                debug!(
                    error = ?e,
                    "queued duplicate block hash for download, \
                     assuming the syncer will eventually resolve duplicates, continuing"
                );
                false
            }

            BlockDownloadVerifyError::DownloadFailed { .. }
                if e.not_found_download_hash().is_some() =>
            {
                warn!(
                    error = ?e,
                    "required sync block was not found after retries, restarting sync"
                );
                true
            }

            _ => {
                // download_and_verify downcasts errors from the block verifier
                // into VerifyChainError, and puts the result inside one of the
                // BlockDownloadVerifyError enumerations. This downcast could
                // become incorrect e.g. after some refactoring, and it is difficult
                // to write a test to check it. The test below is a best-effort
                // attempt to catch if that happens and log it.
                //
                // TODO: add a proper test and remove this
                // https://github.com/ZcashFoundation/zebra/issues/2909
                let err_str = format!("{e:?}");
                if err_str.contains("NotFound") {
                    error!(?e,
                        "a BlockDownloadVerifyError that should have been filtered out was detected, \
                        which possibly indicates a programming error in the downcast inside \
                        zebrad::components::sync::downloads::Downloads::download_and_verify"
                    )
                }

                warn!(?e, "error downloading and verifying block");
                true
            }
        }
    }
}
