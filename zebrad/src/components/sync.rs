//! The syncer downloads and verifies large numbers of blocks from peers to Zebra.
//!
//! It is used when Zebra is a long way behind the current chain tip.

use std::{
    cmp::max,
    collections::{HashMap, HashSet},
    pin::Pin,
    sync::Arc,
    task::Poll,
    time::Duration,
};

use color_eyre::eyre::{eyre, Report};
use futures::{
    stream::{FuturesUnordered, StreamExt},
    FutureExt,
};
use indexmap::IndexSet;
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{mpsc, watch},
    task::JoinError,
    time::{sleep, sleep_until, timeout, Instant},
};
use tower::{
    builder::ServiceBuilder, limit::ConcurrencyLimit, retry::Retry, timeout::Timeout, Service,
    ServiceExt,
};

use zebra_chain::{
    block::{self, Height, HeightDiff},
    chain_tip::ChainTip,
    parameters::checkpoint::list::CheckpointList,
    state_snapshot::StateSnapshotCommitment,
};
use zebra_consensus::{error::TransactionError, RouterError, VerifyBlockError};
use zebra_network as zn;
use zebra_node_services::constants::DEFAULT_CHECKPOINT_CONCURRENCY_LIMIT;
use zebra_state as zs;

use crate::{
    components::{
        checkpoint_sync::{known_hashes::KnownHashPins, EngineConfig, LegacyOwnership, SyncOwner},
        sync::downloads::BlockDownloadVerifyError,
    },
    config::ZebradConfig,
    BoxError,
};

mod budget;
mod diagnostics;
mod downloads;
pub mod end_of_support;
mod gossip;
mod hedge;
pub(crate) mod progress;
mod recent_sync_lengths;
mod status;

#[cfg(test)]
mod tests;

use budget::PipelineAdmission;
use downloads::Downloads;
use hedge::SuccessHedge;

pub use budget::{PipelineBudget, SyncMemoryLimits, MAX_CHECKPOINT_CHARGE_RATIO};
pub(crate) use diagnostics::{RestartReason, SyncDiagnostics};
pub use downloads::VERIFICATION_PIPELINE_SCALING_MULTIPLIER;
pub use gossip::{gossip_best_tip_block_hashes, gossip_early_blocks, BlockGossipError};
pub use progress::{record_tip_block_delay, show_block_chain_progress};
pub use recent_sync_lengths::RecentSyncLengths;
pub use status::SyncStatus;

/// Match the missing-block errors used by both hedging and sync recovery.
fn is_not_found(error: &dyn std::error::Error) -> bool {
    // TODO: improve this by checking the type (#2908).
    format!("{error:?}").contains("NotFound")
}

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

/// Controls how many times the syncer will re-request a block whose download
/// failed because no peer delivered it (a `NotFound`), before giving up and
/// letting the normal tip re-walk handle it.
///
/// Without this re-request, a single missing block at the checkpoint frontier
/// is dropped and never re-fetched, wedging the whole verify pipeline until the
/// 8-minute `BLOCK_VERIFY_TIMEOUT` fires (#5709). Each attempt already goes
/// through the tower-level `BLOCK_DOWNLOAD_RETRY_LIMIT` (and hedging), so this
/// is a coarse, hash-scoped retry on top of an exhausted per-request retry.
const MAX_BLOCK_REOBTAIN_RETRIES: u8 = 3;

/// The fewest `TransparentInputNotFound` drops without a verified block before the sync
/// restarts, so a low `full_verify_concurrency_limit` can't disable the #11168 exemption.
const MIN_UTXO_RACE_DROPS_BEFORE_RESTART: usize = 4;

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

/// A lower bound on the user-specified concurrency limit.
///
/// If the concurrency limit is 0, Zebra can't download or verify any blocks.
pub const MIN_CONCURRENCY_LIMIT: usize = 1;

/// A lower bound on `sync.v2_target_peers`.
///
/// With no target peers, the checkpoint sync engine would claim block download
/// and never download a block.
pub const MIN_V2_TARGET_PEERS: usize = 1;

/// A lower bound on `sync.v2_lookahead_bytes`: one maximum-size block.
///
/// With a smaller lookahead, the checkpoint sync engine could not request a
/// maximum-size block.
pub const MIN_V2_LOOKAHEAD_BYTES: u64 = block::MAX_BLOCK_BYTES;

/// Controls how long we wait for a tips response to return.
///
/// ## Correctness
///
/// If this timeout is removed (or set too high), the syncer will sometimes hang.
///
/// If this timeout is set too low, the syncer will sometimes get stuck in a
/// failure loop.
pub const TIPS_RESPONSE_TIMEOUT: Duration = Duration::from_secs(6);

/// The per-peer wire-response budget used to derive complete download deadlines.
///
/// Connections start this timeout at dispatch, not while waiting for capacity. The complete sync
/// operation is separately bounded by [`BLOCK_DOWNLOAD_ROUND_TIMEOUT`].
pub(super) const BLOCK_DOWNLOAD_TIMEOUT: Duration = zn::constants::REQUEST_TIMEOUT;

/// Bounds one complete block download round, including readiness, capacity waits and both hedges.
///
/// Reuse the existing four-attempt response budget rather than charging capacity waits as attempts.
/// The retry limit is the constant 3, so conversion to u32 is lossless.
const BLOCK_DOWNLOAD_ROUND_TIMEOUT: Duration =
    BLOCK_DOWNLOAD_TIMEOUT.saturating_mul(BLOCK_DOWNLOAD_RETRY_LIMIT as u32 + 1);

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

/// How long the syncer waits after genesis, below the final checkpoint, for the
/// checkpoint sync engine to claim block download before it starts syncing.
///
/// The engine needs a version 2 handshake with two peers before it can claim.
/// Without this wait, a syncer with a fast link to a peer could download the
/// checkpointed blocks first, or start a round the claim then cancels.
pub const ENGINE_CLAIM_GRACE: Duration = Duration::from_secs(5);

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

    /// Whether to download the checkpointed part of the chain from version 2
    /// peers with `get-block-range` requests, using the checkpoint sync engine.
    ///
    /// The engine runs beside the legacy syncer. It takes over block download
    /// when at least two connected version 2 peers can serve block ranges and
    /// the state tip is far below the final checkpoint, and it hands block
    /// download back to the legacy syncer at a checkpoint height. The legacy
    /// syncer always downloads the genesis block and the blocks above the
    /// final checkpoint, and it takes over again if the engine's peers go
    /// away.
    ///
    /// Off by default. It needs `network.v2_listen_addr` and version 2 peers.
    pub v2_checkpoint_sync: bool,

    /// The most block data the checkpoint sync engine requests ahead of the
    /// committed state tip, in bytes.
    ///
    /// Larger values can keep more peers busy, and use more memory. Values
    /// below one maximum-size block (2,000,000 bytes) are raised to it.
    pub v2_lookahead_bytes: u64,

    /// The slowest download rate, in bytes per second, at which the checkpoint
    /// sync engine keeps downloading from a peer. A slower peer's request is
    /// cancelled and sent to another peer.
    pub v2_min_peer_rate: u64,

    /// Whether to build the UTXO set below the final checkpoint from a
    /// spentness-hint artifact: `"off"` (the default), `"auto"` or `"require"`.
    ///
    /// The artifact marks which transparent outputs are still unspent at the
    /// final checkpoint, so the state does not write and then delete outputs
    /// that are spent before it. Every other part of the state stays exact.
    /// The artifact is fetched from version 2 peers, and is trusted only
    /// through the hash that this Zebra release pins for the network.
    ///
    /// Hints apply only to a state that starts empty. With `"auto"`, Zebra
    /// syncs without hints if it cannot acquire a verified artifact. With
    /// `"require"`, Zebra exits instead of syncing an empty state without
    /// hints. After a restart part way through a hinted sync, Zebra resumes it
    /// with the artifact saved in the state's cache directory, or fetches the
    /// artifact again.
    ///
    /// Hints need the checkpoint sync engine (`v2_checkpoint_sync`): without
    /// it, `"auto"` syncs without hints, and `"require"` stops Zebra at
    /// startup.
    pub v2_spentness_hints: zebra_chain::parameters::spentness_hints::Mode,

    /// The number of version 2 peers the checkpoint sync engine downloads from
    /// at the same time. A value of 0 is raised to 1.
    pub v2_target_peers: usize,

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

    /// The number of threads used to verify signatures, proofs, and other CPU-intensive code.
    ///
    /// If the number of threads is not configured or zero, Zebra uses the number of logical cores.
    /// If the number of logical cores can't be detected, Zebra uses one thread.
    /// For details, see [the `rayon` documentation](https://docs.rs/rayon/latest/rayon/struct.ThreadPoolBuilder.html#method.num_threads).
    pub parallel_cpu_threads: usize,

    /// The known-hash list pins for Regtest or a custom testnet, which let
    /// version 2 checkpoint sync anchor its downloads at every height of the
    /// list instead of only at checkpoints.
    ///
    /// Each pin is the SHA-256 of one 50,000-height chunk of `get-hashes`
    /// entries, as 64 hex digits. Unset by default. Mainnet and the default
    /// Testnet use the pins compiled into Zebra, and refuse this field: Zebra
    /// does not start if it is set for them.
    pub v2_known_hash_pins: Option<KnownHashPins>,

    /// Whether an empty pruned node downloads a state snapshot over the version 2 protocol and
    /// syncs only the blocks above it, instead of syncing from genesis.
    ///
    /// Off by default. The snapshot is downloaded only when `state.prune_blocks_older_than` is
    /// set, the state is not ephemeral, no state database exists yet, a snapshot commitment is
    /// pinned for the network or configured in `state_snapshot`, `consensus.checkpoint_sync` is
    /// on, zcashd-compat mode is off, and `state_snapshot_peers` or `network.initial_v2_peers`
    /// names a peer. Otherwise Zebra logs why and syncs normally.
    ///
    /// The UTXO set, the nullifiers, the Sprout tree, and the value pools of a snapshot are
    /// trusted on its commitment, like the checkpoints are trusted, and the history below the
    /// snapshot is never downloaded.
    pub state_snapshot_sync: bool,

    /// The state snapshot to trust: its `height`, the `hash` of the block at that height, and
    /// the `manifest` hash, as hex strings.
    ///
    /// `None` by default, which uses the commitment this build pins for the network, if any. On
    /// Mainnet and the default Testnet, a configured commitment is accepted only if it equals the
    /// pinned one, or if `unsafe_trust_state_snapshot` is on. On Regtest and custom testnets any
    /// commitment is accepted.
    pub state_snapshot: Option<StateSnapshotCommitment>,

    /// Whether to trust a configured `state_snapshot` on Mainnet or the default Testnet that
    /// differs from the commitment this build pins.
    ///
    /// Off by default. Turning it on makes the node trust whoever produced the configured
    /// snapshot with its view of the chain state, and logs a warning at every start.
    pub unsafe_trust_state_snapshot: bool,

    /// The version 2 peers the snapshot is downloaded from, as `host:port` strings.
    ///
    /// Empty by default, which uses `network.initial_v2_peers`.
    pub state_snapshot_peers: Vec<String>,

    /// What to do when the snapshot cannot be downloaded or is rejected.
    ///
    /// `"error"` by default: exit with an actionable message. An aborted download leaves no state
    /// directory and is retried on the next start; a rejected snapshot stays refused until its
    /// commitment changes or its rejection marker is removed after investigation. `"genesis"`
    /// continues with a normal sync from genesis.
    pub state_snapshot_fallback: StateSnapshotFallback,

    /// Whether to serve the state snapshots this node holds to version 2 peers.
    ///
    /// Every node with `network.v2_listen_addr` set advertises the `NODE_SYNC_ARTIFACTS` service,
    /// with or without this setting, and answers the objects it does not hold as not found.
    ///
    /// Off by default. A node that downloaded a snapshot keeps its pieces only when this is on;
    /// the `zebrad export-state-snapshot` command adds snapshots to serve.
    pub serve_state_snapshots: bool,
}

/// What `zebrad start` does when a state snapshot cannot be downloaded or is rejected.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StateSnapshotFallback {
    /// Exit with an actionable error. An aborted download is retried on the next start; a rejected
    /// snapshot stays refused until its commitment changes or its marker is removed.
    #[default]
    Error,

    /// Continue with a normal sync from genesis.
    Genesis,
}

impl Default for Config {
    fn default() -> Self {
        let engine = EngineConfig::default();

        Self {
            // 2/3 of the default outbound peer limit.
            download_concurrency_limit: 50,

            // The checkpoint sync engine is opt-in.
            v2_checkpoint_sync: false,
            v2_lookahead_bytes: engine.lookahead_bytes,
            v2_min_peer_rate: engine.min_peer_rate,
            // Spentness hints are opt-in.
            v2_spentness_hints: Default::default(),
            v2_target_peers: engine.target_peers,

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

            // Mainnet and the default Testnet use their compiled-in lists.
            v2_known_hash_pins: None,
            state_snapshot_sync: false,
            state_snapshot: None,
            unsafe_trust_state_snapshot: false,
            state_snapshot_peers: Vec::new(),
            state_snapshot_fallback: StateSnapshotFallback::Error,
            serve_state_snapshots: false,
        }
    }
}

impl Config {
    /// The checkpoint sync engine's parameters, from the `v2_*` fields.
    ///
    /// Raises `v2_target_peers` to [`MIN_V2_TARGET_PEERS`] and
    /// `v2_lookahead_bytes` to [`MIN_V2_LOOKAHEAD_BYTES`], with a warning.
    pub fn v2_engine_config(&self) -> EngineConfig {
        let mut target_peers = self.v2_target_peers;
        let mut lookahead_bytes = self.v2_lookahead_bytes;

        if target_peers < MIN_V2_TARGET_PEERS {
            warn!(
                "configured v2 target peers {} too low, increasing to {}",
                self.v2_target_peers, MIN_V2_TARGET_PEERS,
            );

            target_peers = MIN_V2_TARGET_PEERS;
        }

        if lookahead_bytes < MIN_V2_LOOKAHEAD_BYTES {
            warn!(
                "configured v2 lookahead bytes {} too low, increasing to {}",
                self.v2_lookahead_bytes, MIN_V2_LOOKAHEAD_BYTES,
            );

            lookahead_bytes = MIN_V2_LOOKAHEAD_BYTES;
        }

        EngineConfig {
            lookahead_bytes,
            min_peer_rate: self.v2_min_peer_rate,
            target_peers,
        }
    }
}

/// Waits until the checkpoint sync engine owns block download, or forever if
/// its supervisor is gone (or was never started).
async fn wait_for_engine_claim(owner: &mut watch::Receiver<SyncOwner>) {
    if owner.wait_for(|owner| !owner.is_legacy()).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Holds the syncer back after genesis until the spentness-hints decision is
/// made ([`ChainSync::with_hints_gate`]).
///
/// The state's writer adopts hints only before block 1 commits, and the
/// checkpoint sync engine claims block download only after genesis. Without
/// this gate, a syncer that queued blocks above genesis before the claim could
/// commit block 1 first, and the state would sync without hints for good.
#[derive(Debug)]
struct HintsGate {
    /// `true` once the hints task finished, with or without an artifact. A
    /// dropped sender counts as `true`.
    decided: watch::Receiver<bool>,

    /// When the syncer stops waiting and syncs without hints, or `None` to
    /// wait as long as acquisition takes.
    deadline: Option<Instant>,
}

/// Ownership channels whose owner is always this syncer, for a node that does
/// not run the checkpoint sync engine.
fn inert_ownership() -> LegacyOwnership {
    let (_owner_sender, owner) = watch::channel(SyncOwner::Legacy);
    let (ack, _ack_receiver) = watch::channel(0);
    let (_unit_lengths_sender, unit_lengths) = mpsc::channel(1);

    LegacyOwnership {
        owner,
        ack,
        unit_lengths,
    }
}

/// Helps work around defects in the bitcoin protocol by checking whether
/// the returned hashes actually extend a chain tip.
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
struct CheckedTip {
    tip: block::Hash,
    expected_next: block::Hash,
}

pub struct ChainSync<ZN, ZS, ZSR, ZV, ZSTip>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZN::Future: Send + 'static,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZS::Future: Send,
    ZSR: Service<zs::ReadRequest, Response = zs::ReadResponse, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZSR::Future: Send,
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
                ConcurrencyLimit<SuccessHedge<ConcurrencyLimit<Retry<zn::RetryLimit, ZN>>>>,
                Timeout<ZV>,
                ZSR,
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

    /// Receiver that is `true` when the downloader is past the lookahead limit.
    /// This is based on the downloaded block height and the state tip height.
    past_lookahead_limit_receiver: zs::WatchReceiver<bool>,

    /// Sender for reporting peer addresses that advertised unexpectedly invalid transactions.
    misbehavior_sender: mpsc::Sender<zn::MisbehaviorReport>,

    /// Blocks whose download failed with `NotFound` and should be re-requested on
    /// the next sync round, instead of being silently dropped (#5709).
    reobtain_hashes: IndexSet<block::Hash>,

    /// Per-hash count of how many times a `NotFound` block has been re-requested,
    /// bounded by [`MAX_BLOCK_REOBTAIN_RETRIES`].
    block_reobtain_retries: HashMap<block::Hash, u8>,

    /// `TransparentInputNotFound` drops since the last verified block, bounded by the
    /// full-verify concurrency limit so a poisoned hash batch can't suppress restarts.
    utxo_race_drops: usize,

    /// Publishes the syncer state that the progress task logs when sync stalls.
    diagnostics: watch::Sender<SyncDiagnostics>,

    /// Which component owns block download: this syncer, or the checkpoint
    /// sync engine. Inert, so this syncer always owns it, unless
    /// [`ChainSync::with_ownership`] replaces it.
    ownership: LegacyOwnership,

    /// Holds this syncer back after genesis while the state may still adopt
    /// spentness hints, or `None` once there is nothing to wait for.
    hints: Option<HintsGate>,
}

/// A sync round that ended early, and why.
#[derive(Debug)]
struct RestartError {
    reason: RestartReason,
    /// The block error kind, for [`RestartReason::BlockError`].
    kind: Option<&'static str>,
    error: Report,
}

impl RestartError {
    fn new(reason: RestartReason, error: impl Into<Report>) -> Self {
        Self {
            reason,
            kind: None,
            error: error.into(),
        }
    }
}

/// Polls the network to determine whether further blocks are available and
/// downloads them.
///
/// This component is used for initial block sync, but the `Inbound` service is
/// responsible for participating in the gossip protocols used for block
/// diffusion.
impl<ZN, ZS, ZSR, ZV, ZSTip> ChainSync<ZN, ZS, ZSR, ZV, ZSTip>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZN::Future: Send + 'static,
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZS::Future: Send,
    ZSR: Service<zs::ReadRequest, Response = zs::ReadResponse, Error = BoxError>
        + Send
        + Sync
        + Clone
        + 'static,
    ZSR::Future: Send,
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
    ///  - read_state: a read-only handle to the zebra-state, used to check downloaded block heights
    ///  - latest_chain_tip: the latest chain tip from `state`
    ///
    /// Also returns a [`SyncStatus`] to check if the syncer has likely reached the chain tip.
    ///
    /// The pipeline has no byte bound: use [`ChainSync::new_with_limits`] for one.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: &ZebradConfig,
        max_checkpoint_height: Height,
        peers: ZN,
        verifier: ZV,
        state: ZS,
        read_state: ZSR,
        latest_chain_tip: ZSTip,
        misbehavior_sender: mpsc::Sender<zn::MisbehaviorReport>,
    ) -> (Self, SyncStatus) {
        Self::new_inner(
            config,
            max_checkpoint_height,
            peers,
            verifier,
            state,
            read_state,
            latest_chain_tip,
            misbehavior_sender,
            None,
        )
    }

    /// Returns a new syncer instance like [`ChainSync::new`], whose download pipeline is bounded
    /// by `limits` (design §4.2).
    ///
    /// `checkpoint_list` is the list the checkpoint verifier uses, as returned by
    /// `zebra_consensus::router::init_checkpoint_list`. With a byte bound, the syncer always
    /// admits the downloads of the oldest checkpoint range above the state tip, so the checkpoint
    /// verifier can complete that range and the pipeline can drain.
    ///
    /// With [`SyncMemoryLimits::default()`], this is [`ChainSync::new`].
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_limits(
        config: &ZebradConfig,
        max_checkpoint_height: Height,
        peers: ZN,
        verifier: ZV,
        state: ZS,
        read_state: ZSR,
        latest_chain_tip: ZSTip,
        misbehavior_sender: mpsc::Sender<zn::MisbehaviorReport>,
        checkpoint_list: Arc<CheckpointList>,
        limits: SyncMemoryLimits,
    ) -> (Self, SyncStatus) {
        Self::new_inner(
            config,
            max_checkpoint_height,
            peers,
            verifier,
            state,
            read_state,
            latest_chain_tip,
            misbehavior_sender,
            Some((checkpoint_list, limits)),
        )
    }

    /// Returns a new syncer instance, bounding its pipeline by `pipeline_limits` if given.
    #[allow(clippy::too_many_arguments)]
    fn new_inner(
        config: &ZebradConfig,
        max_checkpoint_height: Height,
        peers: ZN,
        verifier: ZV,
        state: ZS,
        read_state: ZSR,
        latest_chain_tip: ZSTip,
        misbehavior_sender: mpsc::Sender<zn::MisbehaviorReport>,
        pipeline_limits: Option<(Arc<CheckpointList>, SyncMemoryLimits)>,
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

        // Capacity waiting stays inside the network request and does not consume a retry.
        // Connections start response deadlines at wire dispatch; Downloads bounds readiness
        // and the complete hedged retry operation with one absolute deadline.
        // Hedge entire retry sequences so individual peer failures are retried before hedging.
        // The inner limit bounds active sequences across both arms; exhausted arms release it
        // immediately so their delayed siblings can start even at a limit of one.
        // The outer limit bounds logical downloads, holding admission while any arm can succeed.
        // Success, final failure, or cancellation releases it before block verification starts.
        let block_network = SuccessHedge::new(
            ServiceBuilder::new()
                .concurrency_limit(download_concurrency_limit)
                .retry(zn::RetryLimit::new(BLOCK_DOWNLOAD_RETRY_LIMIT))
                .service(peers),
        );
        let block_network = ConcurrencyLimit::new(block_network, download_concurrency_limit);

        // We apply a timeout to the verifier to avoid hangs due to missing earlier blocks.
        let verifier = Timeout::new(verifier, BLOCK_VERIFY_TIMEOUT);

        let (sync_status, recent_syncs) = SyncStatus::new_for_network(&config.network.network);
        let (past_lookahead_limit_sender, past_lookahead_limit_receiver) = watch::channel(false);
        let past_lookahead_limit_receiver = zs::WatchReceiver::new(past_lookahead_limit_receiver);

        let mut downloads = Downloads::new(
            block_network,
            verifier,
            read_state,
            latest_chain_tip.clone(),
            past_lookahead_limit_sender,
            max(
                checkpoint_verify_concurrency_limit,
                full_verify_concurrency_limit,
            ),
            max_checkpoint_height,
        );
        if let Some(admission) = pipeline_limits.and_then(|(checkpoint_list, limits)| {
            PipelineAdmission::new(
                PipelineBudget::new(limits),
                checkpoint_list,
                max_checkpoint_height,
            )
        }) {
            downloads = downloads.with_pipeline_admission(admission);
        }
        let downloads = Box::pin(downloads);

        let (diagnostics, diagnostics_receiver) = watch::channel(SyncDiagnostics {
            in_flight_blocks: downloads.in_flight_blocks(),
            ..SyncDiagnostics::default()
        });
        let sync_status = sync_status.with_diagnostics(diagnostics_receiver);

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
            past_lookahead_limit_receiver,
            misbehavior_sender,
            reobtain_hashes: IndexSet::new(),
            block_reobtain_retries: HashMap::new(),
            utxo_race_drops: 0,
            diagnostics,
            ownership: inert_ownership(),
            hints: None,
        };

        (new_syncer, sync_status)
    }

    /// Shares block download with the checkpoint sync engine through
    /// `ownership`.
    ///
    /// The syncer then starts a sync round only while it owns block download,
    /// ends its round and cancels its downloads when the engine claims it, and
    /// acknowledges the claim. While the engine owns block download, the
    /// syncer counts the engine's committed units as sync lengths, so the
    /// mempool and block gossip stay off.
    pub fn with_ownership(mut self, ownership: LegacyOwnership) -> Self {
        self.ownership = ownership;
        self
    }

    /// Holds this syncer back after genesis until `decided` is `true` or its
    /// sender is dropped: the spentness-hints task has finished, with or
    /// without an artifact, so the state has decided whether it syncs with
    /// hints.
    ///
    /// The checkpoint sync engine claims block download right after genesis
    /// when hints may apply, and this syncer acknowledges that claim while it
    /// waits. Without `timeout`, it waits as long as acquisition takes; with
    /// it, it syncs without hints once `timeout` has passed since this call.
    pub fn with_hints_gate(
        mut self,
        decided: watch::Receiver<bool>,
        timeout: Option<Duration>,
    ) -> Self {
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        self.hints = Some(HintsGate { decided, deadline });
        self
    }

    /// Returns the download pipeline's byte bound, which the inbound block downloader shares.
    ///
    /// It has no bound unless the syncer was built with one.
    pub fn pipeline_budget(&self) -> PipelineBudget {
        self.downloads.pipeline_budget()
    }

    /// Runs the syncer to synchronize the chain and keep it synchronized.
    #[instrument(skip(self))]
    pub async fn sync(mut self) -> Result<(), Report> {
        // We can't download the genesis block using our normal algorithm,
        // due to protocol limitations
        self.request_genesis().await?;
        self.wait_for_engine_claim_grace().await;

        loop {
            self.wait_for_download_ownership().await;
            if !self.wait_for_hints_decision().await {
                // The engine claimed block download: acknowledge the claim first.
                continue;
            }

            let start_tip = self.latest_chain_tip.best_tip_height();
            let result = self.try_to_sync().await;
            if result.is_err() {
                self.count_finished_block_errors();
                self.downloads.cancel_all();
            }

            if !self.owns_download() {
                // The engine claimed block download during this round. Cancel the
                // round's downloads like the error path does; the next
                // `wait_for_download_ownership` then acknowledges the claim, and
                // waits for the engine to hand block download back.
                if result.is_ok() {
                    self.downloads.cancel_all();
                }
                self.update_metrics();
                continue;
            }

            self.update_metrics();

            let restart_delay = if self.is_regtest {
                REGTEST_SYNC_RESTART_DELAY
            } else {
                SYNC_RESTART_DELAY
            };
            let state_tip = self.latest_chain_tip.best_tip_height();
            let made_progress = state_tip > start_tip;
            let (reason, kind, error) = match result {
                Ok(()) => (RestartReason::ExhaustedTips, None, None),
                Err(restart) => (
                    restart.reason,
                    restart.kind,
                    Some(format!("{:#}", restart.error)),
                ),
            };
            self.record_restart(reason, made_progress);

            if error.is_some() && !made_progress {
                warn!(
                    reason = reason.as_str(),
                    kind,
                    error,
                    timeout = ?restart_delay,
                    ?state_tip,
                    "sync failed without advancing the state tip, waiting to restart sync"
                );
            } else {
                info!(
                    reason = reason.as_str(),
                    kind,
                    made_progress,
                    error,
                    timeout = ?restart_delay,
                    ?state_tip,
                    "waiting to restart sync"
                );
            }
            // An engine claim ends the delay, so the engine does not wait for it.
            tokio::select! {
                () = sleep(restart_delay) => {}
                () = wait_for_engine_claim(&mut self.ownership.owner) => {}
            }
        }
    }

    /// Counts a finished sync round.
    fn record_restart(&self, reason: RestartReason, made_progress: bool) {
        metrics::counter!(
            "sync.restart.count",
            "reason" => reason.as_str(),
            "made_progress" => if made_progress { "true" } else { "false" },
        )
        .increment(1);

        self.diagnostics.send_modify(|diagnostics| {
            diagnostics.last_restart = Some(reason);
            diagnostics.restarts_without_progress = if made_progress {
                0
            } else {
                diagnostics.restarts_without_progress.saturating_add(1)
            };
        });
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
    async fn try_to_sync(&mut self) -> Result<(), RestartError> {
        self.prospective_tips = HashSet::new();

        self.reobtain_hashes.clear();
        self.block_reobtain_retries.clear();
        self.utxo_race_drops = 0;
        self.downloads.take_cancelled_network_wait();

        info!(
            state_tip = ?self.latest_chain_tip.best_tip_height(),
            "starting sync, obtaining new tips"
        );
        // Queueing the blocks it finds can wait for network capacity for longer than this timeout.
        let mut extra_hashes = match timeout(SYNC_RESTART_DELAY, self.obtain_tips()).await {
            Ok(tips) => {
                tips.map_err(|e| Self::tips_restart_error(RestartReason::ObtainTipsError, e))?
            }
            Err(elapsed) => {
                return Err(self.stage_timeout_error(RestartReason::ObtainTipsTimeout, elapsed));
            }
        };
        self.update_metrics();

        while (!self.prospective_tips.is_empty() || !extra_hashes.is_empty())
            && self.owns_download()
        {
            // Avoid hangs due to service readiness or other internal operations
            extra_hashes = match timeout(BLOCK_VERIFY_TIMEOUT, self.try_to_sync_once(extra_hashes))
                .await
            {
                Ok(extra_hashes) => extra_hashes?,
                Err(elapsed) => {
                    return Err(self.stage_timeout_error(RestartReason::SyncStepTimeout, elapsed));
                }
            };
        }

        if !self.owns_download() {
            info!(
                owner = ?*self.ownership.owner.borrow(),
                "the checkpoint sync engine claimed block download, ending this sync round"
            );
            return Ok(());
        }

        info!("exhausted prospective tip set");

        Ok(())
    }

    /// Whether this syncer owns block download.
    ///
    /// It also does if the checkpoint sync engine's supervisor stopped
    /// without handing block download back, so block download never stops.
    fn owns_download(&self) -> bool {
        self.ownership.owner.borrow().is_legacy() || self.ownership.owner.has_changed().is_err()
    }

    /// Waits up to [`ENGINE_CLAIM_GRACE`] for the checkpoint sync engine to
    /// claim block download, if it runs and the state tip is below the final
    /// checkpoint.
    async fn wait_for_engine_claim_grace(&mut self) {
        // The inert ownership has no sender. The state has genesis, even if the
        // latest tip has not caught up yet.
        let tip = self.latest_chain_tip.best_tip_height().unwrap_or(Height(0));
        if self.ownership.owner.has_changed().is_err() || tip >= self.max_checkpoint_height {
            return;
        }

        info!(
            grace = ?ENGINE_CLAIM_GRACE,
            "waiting for the checkpoint sync engine to claim block download"
        );
        tokio::select! {
            () = sleep(ENGINE_CLAIM_GRACE) => {}
            () = wait_for_engine_claim(&mut self.ownership.owner) => {}
        }
    }

    /// Waits until this syncer owns block download.
    ///
    /// While the checkpoint sync engine owns it, this cancels this syncer's
    /// downloads and then acknowledges each claim, and pushes the block counts
    /// of the engine's committed units into the recent sync lengths. A round
    /// that ran out of tips leaves its downloads in flight, so a claim during
    /// the restart delay reaches this with downloads still running.
    async fn wait_for_download_ownership(&mut self) {
        let mut waited = false;

        loop {
            while let Ok(unit_length) = self.ownership.unit_lengths.try_recv() {
                self.recent_syncs.push_extend_tips_length(unit_length);
            }

            let owner = *self.ownership.owner.borrow_and_update();
            let SyncOwner::Engine { target, generation } = owner else {
                if waited {
                    info!(
                        state_tip = ?self.latest_chain_tip.best_tip_height(),
                        "the checkpoint sync engine handed block download back to the syncer"
                    );
                }
                return;
            };

            let acknowledged = *self.ownership.ack.borrow() == generation;
            if !acknowledged {
                // The engine starts downloading once it sees the ack, so this
                // syncer must not download or submit blocks after it.
                self.downloads.cancel_all();
            }
            self.ownership.ack.send_replace(generation);
            if !acknowledged {
                info!(
                    ?target,
                    generation,
                    state_tip = ?self.latest_chain_tip.best_tip_height(),
                    "the checkpoint sync engine owns block download, waiting for it to finish"
                );
            }
            waited = true;

            tokio::select! {
                changed = self.ownership.owner.changed() => {
                    if changed.is_err() {
                        warn!(
                            state_tip = ?self.latest_chain_tip.best_tip_height(),
                            "the checkpoint sync engine stopped without handing block download \
                             back, resuming sync"
                        );
                        return;
                    }
                }
                Some(unit_length) = self.ownership.unit_lengths.recv() => {
                    self.recent_syncs.push_extend_tips_length(unit_length);
                }
            }
        }
    }

    /// Waits until the spentness-hints decision is made, so that this syncer
    /// commits no block above genesis that the state could still write with
    /// hints ([`HintsGate`]).
    ///
    /// Returns `true` once the decision is made, or the gate's deadline has
    /// passed, and from then on without waiting. Returns `false` if the
    /// checkpoint sync engine claimed block download first: the caller then
    /// acknowledges the claim, and calls this again once it owns block
    /// download.
    async fn wait_for_hints_decision(&mut self) -> bool {
        let Some(gate) = self.hints.as_mut() else {
            return true;
        };

        let deadline = gate.deadline;
        let timed_out = async move {
            match deadline {
                Some(deadline) => sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            // An error is a dropped sender, which counts as decided.
            _ = gate.decided.wait_for(|decided| *decided) => {
                debug!("the spentness-hints decision is made, starting sync");
            }
            () = timed_out => {
                warn!(
                    state_tip = ?self.latest_chain_tip.best_tip_height(),
                    "the spentness-hints decision took too long, syncing without hints"
                );
            }
            () = wait_for_engine_claim(&mut self.ownership.owner) => return false,
        }

        self.hints = None;
        true
    }

    /// Tries to synchronize the chain once, using the existing `extra_hashes`.
    ///
    /// Tries to extend the existing tips and download the missing blocks.
    ///
    /// Returns `Ok(extra_hashes)` if it was able to extend once and synchronize sone of the chain.
    /// Returns `Err` if there was an unrecoverable error and restarting the synchronization is
    /// necessary.
    #[instrument(skip(self, extra_hashes))]
    async fn try_to_sync_once(
        &mut self,
        mut extra_hashes: IndexSet<block::Hash>,
    ) -> Result<IndexSet<block::Hash>, RestartError> {
        // Check whether any block tasks are currently ready.
        while let Poll::Ready(Some(rsp)) = futures::poll!(self.downloads.next()) {
            // Some temporary errors are ignored, and syncing continues with other blocks.
            // If it turns out they were actually important, syncing will run out of blocks, and
            // the syncer will reset itself.
            self.handle_block_response(rsp)
                .map_err(Self::block_restart_error)?;
        }
        // Re-request any blocks that just failed with `NotFound`, before pausing
        // on the lookahead limit (#5709).
        self.reobtain_missing_blocks().await;
        self.update_metrics();

        // Pause new downloads while the syncer or downloader are past their lookahead limits.
        //
        // To avoid a deadlock or long waits for blocks to expire, we ignore the download
        // lookahead limit when there are only a small number of blocks waiting.
        while self.downloads.in_flight() >= self.lookahead_limit(extra_hashes.len())
            || (self.downloads.in_flight() >= self.lookahead_limit(extra_hashes.len()) / 2
                && self.past_lookahead_limit_receiver.cloned_watch_data())
        {
            trace!(
                tips.len = self.prospective_tips.len(),
                in_flight = self.downloads.in_flight(),
                extra_hashes = extra_hashes.len(),
                lookahead_limit = self.lookahead_limit(extra_hashes.len()),
                state_tip = ?self.latest_chain_tip.best_tip_height(),
                "waiting for pending blocks",
            );

            let response = self.downloads.next().await.expect("downloads is nonempty");

            self.handle_block_response(response)
                .map_err(Self::block_restart_error)?;
            // A block that just failed with `NotFound` is what unblocks the
            // verifier, so re-request it now rather than waiting for the pause
            // loop to clear — which it cannot until this block arrives (#5709).
            self.reobtain_missing_blocks().await;
            self.update_metrics();
        }

        // Once we're below the lookahead limit, we can request more blocks or hashes.
        if !extra_hashes.is_empty() {
            debug!(
                tips.len = self.prospective_tips.len(),
                in_flight = self.downloads.in_flight(),
                extra_hashes = extra_hashes.len(),
                lookahead_limit = self.lookahead_limit(extra_hashes.len()),
                state_tip = ?self.latest_chain_tip.best_tip_height(),
                "requesting more blocks",
            );

            let response = self.request_blocks(extra_hashes).await;
            extra_hashes =
                Self::handle_hash_response(response).map_err(Self::block_restart_error)?;
        } else {
            info!(
                tips.len = self.prospective_tips.len(),
                in_flight = self.downloads.in_flight(),
                extra_hashes = extra_hashes.len(),
                lookahead_limit = self.lookahead_limit(extra_hashes.len()),
                state_tip = ?self.latest_chain_tip.best_tip_height(),
                "extending tips",
            );

            extra_hashes = self
                .extend_tips()
                .await
                .map_err(|e| Self::tips_restart_error(RestartReason::ExtendTipsError, e))?;
        }
        self.update_metrics();

        Ok(extra_hashes)
    }

    /// Re-issues downloads for blocks that failed with `NotFound` (#5709).
    ///
    /// These are re-requested even while the download pipeline is past its
    /// lookahead limit or its byte bound, because a missing low block is exactly what stops the
    /// checkpoint verifier from advancing. Waiting for the lookahead pause or the byte bound to
    /// clear would deadlock — neither can clear until this block arrives.
    /// The per-hash retry count is bounded by [`MAX_BLOCK_REOBTAIN_RETRIES`].
    /// Blocks that can't get network capacity before the download deadline stay queued for the
    /// next sync round.
    async fn reobtain_missing_blocks(&mut self) {
        // The checkpoint sync engine downloads these blocks while it owns block download.
        if self.reobtain_hashes.is_empty() || !self.owns_download() {
            return;
        }

        let mut hashes = std::mem::take(&mut self.reobtain_hashes).into_iter();
        for hash in hashes.by_ref() {
            // The block was removed from the in-flight set when its download
            // failed, so this re-queues it. A residual duplicate/queue error is
            // benign — it means the block is already being handled.
            if let Err(error) = self.queue_block_exempt(hash).await {
                trace!(?hash, ?error, "re-download of missing block not queued");
                // Any other error means the network failed or wasn't ready before the download
                // deadline. Keep this block and the rest for the next sync round, rather than
                // dropping blocks the verifier is waiting for, or waiting out each deadline.
                if !matches!(
                    error,
                    BlockDownloadVerifyError::DuplicateBlockQueuedForDownload { .. }
                ) {
                    self.reobtain_hashes.insert(hash);
                    break;
                }
            }
        }
        self.reobtain_hashes.extend(hashes);
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

                    let hashes = hashes.as_slice();
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

                    if let Some(end) = unknown_hashes.rchunks_exact(2).next() {
                        let new_tip = CheckedTip {
                            tip: end[0],
                            expected_next: end[1],
                        };

                        // Make sure we get the same tips, regardless of the
                        // order of peer responses
                        if !download_set.contains(&new_tip.expected_next) {
                            debug!(
                                ?new_tip,
                                "adding new prospective tip, and removing existing tips in the \
                                new block hash list",
                            );
                            self.prospective_tips
                                .retain(|t| !unknown_hashes.contains(&t.expected_next));
                            self.prospective_tips.insert(new_tip);
                        } else {
                            debug!(
                                ?new_tip,
                                "discarding prospective tip: already in download set"
                            );
                        }
                    } else {
                        debug!("downloading response that extends only one block");
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

        // State can advance during fanout. Compact unknown hashes in place, preserving download
        // order.
        let mut retained = 0;
        for index in 0..download_set.len() {
            let hash = download_set[index];
            debug!(?hash, "checking if state contains hash");
            if !self.state_contains(hash).await? {
                if retained != index {
                    download_set.swap_indices(retained, index);
                }
                retained += 1;
            }
        }
        download_set.truncate(retained);

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

                        // Legacy zcashd nodes could prepend an unrelated hash
                        // to their response. Check the first hash against the
                        // previous response, and discard mismatches.
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

                        if unknown_hashes.is_empty() {
                            debug!(?tip.tip, "response contained no new hashes after the expected overlap");
                            continue;
                        }

                        trace!(?unknown_hashes);

                        if let Some(end) = unknown_hashes.rchunks_exact(2).next() {
                            let new_tip = CheckedTip {
                                tip: end[0],
                                expected_next: end[1],
                            };

                            // Make sure we get the same tips, regardless of the
                            // order of peer responses
                            if !download_set.contains(&new_tip.expected_next) {
                                debug!(
                                    ?new_tip,
                                    "adding new prospective tip, and removing any existing tips \
                                    in the new block hash list",
                                );
                                self.prospective_tips
                                    .retain(|t| !unknown_hashes.contains(&t.expected_next));
                                self.prospective_tips.insert(new_tip);
                            } else {
                                debug!(
                                    ?new_tip,
                                    "discarding prospective tip: already in download set"
                                );
                            }
                        } else {
                            debug!("downloading response that extends only one block");
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
        debug!(new_downloads, "queueing new downloads");
        metrics::gauge!("sync.extend.queued.hash.count").set(new_downloads as f64);

        // security: use the actual number of new downloads from all peers,
        // so the last peer to respond can't toggle our mempool
        self.recent_syncs.push_extend_tips_length(new_downloads);

        let response = self.request_blocks(download_set).await;

        metrics::histogram!("sync.stage.duration_seconds", "stage" => "extend_tips")
            .record(stage_start.elapsed().as_secs_f64());

        Self::handle_hash_response(response).map_err(Into::into)
    }

    /// Download and verify the genesis block, if it isn't currently known to
    /// our node.
    ///
    /// A state that already has a tip is not checked: a snapshot-imported state
    /// ships genesis with its window, and any other state committed
    /// genesis before its tip, so asking would only download a block below the
    /// tip.
    async fn request_genesis(&mut self) -> Result<(), Report> {
        if let Some(tip_height) = self.latest_chain_tip.best_tip_height() {
            debug!(
                ?tip_height,
                "the state has a tip, so the genesis block is not requested"
            );
            return Ok(());
        }

        // Due to Bitcoin protocol limitations, we can't request the genesis
        // block using our standard tip-following algorithm:
        //  - getblocks requires at least one hash
        //  - responses start with the block *after* the requested block, and
        //  - the genesis hash is used as a placeholder for "no matches".
        //
        // So we just download and verify the genesis block here.
        while !self.state_contains(self.genesis_hash).await? {
            info!("starting genesis block download and verify");

            let response = match timeout(SYNC_RESTART_DELAY, self.request_genesis_once()).await {
                Ok(response) => Ok(response),
                Err(_) => Err(self.genesis_timeout()),
            };

            // 3 layers of results is not ideal, but we need the timeout on the outside.
            match response {
                Ok(Ok(Ok(response))) => self
                    .handle_block_response(Ok(response))
                    .expect("never returns Err for Ok"),
                // Handle fatal errors
                Ok(Err(fatal_error)) => Err(fatal_error)?,
                // Handle timeouts and block errors
                Err(error) | Ok(Ok(Err(error))) => {
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

    /// Try to download and verify the genesis block once.
    ///
    /// Fatal errors are returned in the outer result, temporary errors in the inner one.
    async fn request_genesis_once(
        &mut self,
    ) -> Result<Result<(Height, block::Hash), BlockDownloadVerifyError>, Report> {
        // Every later block waits for the genesis block, so it never waits for the byte bound.
        let response = self.queue_block_exempt(self.genesis_hash).await;
        Self::handle_response(response).map_err(|e| eyre!(e))?;
        self.update_metrics();

        let response = self
            .downloads
            .next()
            .await
            .expect("downloads is nonempty")
            .inspect_err(|error| self.count_block_error(error));
        self.update_metrics();

        Ok(response)
    }

    /// Queue download and verify tasks for each block that isn't currently known to our node.
    ///
    /// TODO: turn obtain and extend tips into a separate task, which sends hashes via a channel?
    async fn request_blocks(
        &mut self,
        mut hashes: IndexSet<block::Hash>,
    ) -> Result<IndexSet<block::Hash>, BlockDownloadVerifyError> {
        // The checkpoint sync engine may have claimed block download while this step
        // waited on the network. The round then ends, so these hashes are not needed.
        if !self.owns_download() {
            debug!(
                hashes.len = hashes.len(),
                "the checkpoint sync engine owns block download, not queueing blocks",
            );
            return Ok(IndexSet::new());
        }

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
        let mut hashes = hashes.into_iter();
        while let Some(hash) = hashes.next() {
            match self.queue_block(hash).await {
                Ok(()) => {}
                Err(BlockDownloadVerifyError::DuplicateBlockQueuedForDownload { .. }) => {
                    debug!("block request was already queued, continuing");
                }
                Err(BlockDownloadVerifyError::PipelineFull { .. }) => {
                    // Keep this hash and the rest of the batch, in order, ahead of the extra
                    // hashes, so the syncer observes finished blocks before it asks again
                    // (design §4.2 step 2).
                    debug!(
                        deferred = hashes.len() + 1 + extra_hashes.len(),
                        "sync pipeline is above its memory limit, deferring block requests",
                    );
                    let mut deferred: IndexSet<block::Hash> =
                        std::iter::once(hash).chain(hashes).collect();
                    deferred.extend(extra_hashes);

                    return Ok(deferred);
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
        if let Err(error) = &response {
            self.count_block_error(error);
        }

        match response {
            Ok((height, hash)) => {
                trace!(?height, ?hash, "verified and committed block to state");

                // The block arrived, so forget any re-request bookkeeping for it.
                self.reobtain_hashes.shift_remove(&hash);
                self.block_reobtain_retries.remove(&hash);
                self.utxo_race_drops = 0;

                return Ok(());
            }

            Err(
                ref verify_error @ BlockDownloadVerifyError::Invalid {
                    ref error,
                    advertiser_addr: Some(advertiser_addr),
                    ..
                },
            ) if error.misbehavior_score() != 0 => {
                let _ = self.misbehavior_sender.try_send(zn::MisbehaviorReport::new(
                    advertiser_addr,
                    error.misbehavior_score(),
                    verify_error.to_string(),
                ));
            }

            Err(
                ref verify_error @ BlockDownloadVerifyError::InvalidHeight {
                    advertiser_addr: Some(advertiser_addr),
                    ..
                },
            ) => {
                let _ = self.misbehavior_sender.try_send(zn::MisbehaviorReport::new(
                    advertiser_addr,
                    100,
                    verify_error.to_string(),
                ));
            }

            // The downloader only sets `advertiser_addr` here when Zebra already holds the parent
            // header and it proves the body's claimed height wrong. A peer can answer with a
            // canonical header and a rewritten coinbase height because the initial hash check does
            // not recompute the header's commitment to the body's authorizing data
            // (GHSA-g95h-hw6g-pvgv). Peers that serve genuinely old blocks arrive unattributed and
            // fall through unscored. Score only a parent-proven rewrite.
            Err(
                ref verify_error @ BlockDownloadVerifyError::BehindTipHeightLimit {
                    advertiser_addr: Some(advertiser_addr),
                    ..
                },
            ) => {
                let _ = self.misbehavior_sender.try_send(zn::MisbehaviorReport::new(
                    advertiser_addr,
                    100,
                    format!("parent header contradicts coinbase height: {verify_error}"),
                ));
            }

            // `AboveLookaheadHeightLimit` deliberately falls through unscored, and must
            // stay that way (GHSA-qhr3-cvch-5fh2): `FindBlocks` responses carry no
            // address, so the follow-up request goes to an independently chosen, honest
            // peer that served the block but did not choose its height. Scoring it let a
            // malicious `FindBlocks` responder evict honest peers throughout IBD. Unlike
            // the behind-tip sibling advisory, the block here is genuine, so there is no
            // local proof of forgery to re-attribute with. Do not add scoring back.
            Err(_) => {}
        };

        // A hash the syncer still needs, whose download did not produce a usable block, is
        // otherwise dropped here and only rediscovered by a later sync round. Re-queue it for the
        // next sync round, bounded by `MAX_BLOCK_REOBTAIN_RETRIES`:
        // - `DownloadFailed`/`NotFound`: no peer delivered the block, which wedges the checkpoint
        //   frontier until the verify timeout (#5709).
        // - `DownloadTimedOut`: no peer delivered the block within the download round. One hedged
        //   request can already have failed with `NotFound` while its sibling was still waiting.
        // - `BehindTipHeightLimit`: the body was not a usable block for this hash, so the hash is
        //   still missing (GHSA-g95h-hw6g-pvgv). This re-request runs whether or not the peer could
        //   be attributed, and covers the window before a score reaches the address book, which
        //   only applies misbehavior reports in batches.
        // - UTXO races (#11168): the block was never rejected, and the state parks its
        //   children until it arrives, so re-request it instead of waiting for a tip walk.
        // - `Invalid` with a mismatched authorizing data commitment: the block itself is fine, only
        //   the served body was forged, so the hash is still valid and still wanted. Misbehavior
        //   reports are flushed to the address book on `MISBEHAVIOR_FLUSH_INTERVAL`, so the first
        //   re-request can briefly land on the same peer again; `MAX_BLOCK_REOBTAIN_RETRIES` bounds
        //   that.
        // - `Invalid` because an ancestor had a mismatched authorizing data commitment: the state
        //   rejects queued descendants along with the forged ancestor, but their hashes are still
        //   wanted, and the peers that served them are not scored.
        // Other consensus failures (`Invalid`/`ValidationRequestError`) are deliberately
        // excluded — re-downloading a block the network already rejected is pointless.
        let reobtain_hash = match &response {
            Err(BlockDownloadVerifyError::DownloadFailed { error, hash })
                if is_not_found(error.as_ref()) =>
            {
                Some(*hash)
            }
            Err(BlockDownloadVerifyError::DownloadTimedOut { hash }) => Some(*hash),
            Err(BlockDownloadVerifyError::BehindTipHeightLimit { hash, .. }) => Some(*hash),
            Err(e @ BlockDownloadVerifyError::Invalid { hash, .. })
                if Self::is_utxo_lookup_timeout(e) =>
            {
                Some(*hash)
            }
            Err(BlockDownloadVerifyError::Invalid { error, hash, .. })
                if error.is_auth_commitment_mismatch()
                    || error.is_descendant_of_auth_commitment_mismatch() =>
            {
                Some(*hash)
            }
            Err(e @ BlockDownloadVerifyError::ValidationRequestError { hash, .. })
                if Self::is_post_checkpoint_verify_timeout(e) =>
            {
                Some(*hash)
            }
            _ => None,
        };

        if let Some(hash) = reobtain_hash {
            let attempts = self.block_reobtain_retries.entry(hash).or_insert(0);
            if *attempts < MAX_BLOCK_REOBTAIN_RETRIES {
                *attempts += 1;
                self.reobtain_hashes.insert(hash);
                debug!(
                    ?hash,
                    attempts = *attempts,
                    "re-queueing missing block for re-download"
                );
            } else {
                debug!(
                    ?hash,
                    "missing block exceeded re-download retries, dropping"
                );
                self.block_reobtain_retries.remove(&hash);
            }
        }

        // A UTXO race resolves as soon as the parent commits. A whole lookahead wave of
        // timeouts with no commit isn't the race, so restart instead of draining the batch.
        if let Err(error) = &response {
            if Self::is_utxo_lookup_timeout(error) {
                self.utxo_race_drops += 1;
                if self.utxo_race_drops
                    >= self
                        .full_verify_concurrency_limit
                        .max(MIN_UTXO_RACE_DROPS_BEFORE_RESTART)
                {
                    warn!(
                        drops = self.utxo_race_drops,
                        "no block verified across a full wave of UTXO lookup timeouts, restarting sync"
                    );
                    return response.map(|_| ());
                }
            }
        }

        Self::handle_response(response)
    }

    /// Returns `true` for the `AwaitUtxo` timeout that `should_restart_sync` exempts (#11168).
    fn is_utxo_lookup_timeout(e: &BlockDownloadVerifyError) -> bool {
        matches!(
            e,
            BlockDownloadVerifyError::Invalid {
                error: RouterError::Block { source },
                ..
            } if matches!(
                **source,
                VerifyBlockError::Transaction(TransactionError::TransparentInputNotFound)
            )
        )
    }

    /// Returns `true` for the short post-final-checkpoint verify timeout (#5125) that
    /// `should_restart_sync` exempts; the 8-minute tower timeout is a different type.
    fn is_post_checkpoint_verify_timeout(e: &BlockDownloadVerifyError) -> bool {
        matches!(
            e,
            BlockDownloadVerifyError::ValidationRequestError { error, .. }
                if error.is::<tokio::time::error::Elapsed>()
        )
    }

    /// Returns a bounded label for a block download or verify error, for metrics and logs.
    fn block_error_kind(e: &BlockDownloadVerifyError) -> &'static str {
        use BlockDownloadVerifyError::*;

        match e {
            Invalid { error, .. } if error.is_duplicate_request() => "already_verified",
            Invalid { error, .. }
                if error.is_auth_commitment_mismatch()
                    || error.is_descendant_of_auth_commitment_mismatch() =>
            {
                "auth_commitment_mismatch"
            }
            e if Self::is_utxo_lookup_timeout(e) => "utxo_lookup_timeout",
            e if Self::is_post_checkpoint_verify_timeout(e) => "final_checkpoint_verify_timeout",
            Invalid { .. } => "invalid",
            ValidationRequestError { error, .. }
                if error.is::<tower::timeout::error::Elapsed>() =>
            {
                "verify_timeout"
            }
            ValidationRequestError { .. } => "validation_request",
            DownloadFailed { error, .. } if is_not_found(error.as_ref()) => "not_found",
            DownloadFailed { .. } => "download_failed",
            DownloadTimedOut { .. } => "download_timed_out",
            AboveLookaheadHeightLimit { .. } => "above_lookahead_limit",
            BehindTipHeightLimit { .. } => "behind_tip",
            InvalidHeight { .. } => "invalid_height",
            PipelineFull { .. } => "pipeline_full",
            DuplicateBlockQueuedForDownload { .. } => "duplicate_queued",
            CancelledDuringDownload { .. }
            | CancelledAwaitingVerifierReadiness { .. }
            | CancelledDuringVerification { .. } => "cancelled",
            NetworkServiceError { .. } => "network_service",
            VerifierServiceError { .. } => "verifier_service",
            Timeout => "network_ready_timeout",
            GenesisTimeout => "genesis_timeout",
        }
    }

    /// Queues `hash` for download and verification, which can wait for network capacity.
    ///
    /// Publishes the downloads before that wait. A block that is already queued isn't counted as
    /// an error, because the syncer ignores that error: it happens when a timed-out genesis
    /// attempt or an earlier round left the block queued.
    async fn queue_block(&mut self, hash: block::Hash) -> Result<(), BlockDownloadVerifyError> {
        self.update_metrics();

        let result = self.downloads.download_and_verify(hash).await;

        match &result {
            Ok(()) | Err(BlockDownloadVerifyError::DuplicateBlockQueuedForDownload { .. }) => {}
            Err(error) => self.count_block_error(error),
        }

        result
    }

    /// Queues a required block without waiting for byte capacity, with the same diagnostics.
    async fn queue_block_exempt(
        &mut self,
        hash: block::Hash,
    ) -> Result<(), BlockDownloadVerifyError> {
        self.update_metrics();

        let result = self.downloads.download_and_verify_exempt(hash).await;

        match &result {
            Ok(()) | Err(BlockDownloadVerifyError::DuplicateBlockQueuedForDownload { .. }) => {}
            Err(error) => self.count_block_error(error),
        }

        result
    }

    /// Counts a block whose wait for network capacity an enclosing timeout cancelled, as the
    /// network readiness timeout it would have reached, and returns whether there was one.
    fn count_cancelled_network_wait(&mut self) -> bool {
        let cancelled = self.downloads.take_cancelled_network_wait();
        if cancelled {
            self.count_block_error(&BlockDownloadVerifyError::Timeout);
        }

        cancelled
    }

    /// Returns the error for a sync stage that its timeout cancelled, as `reason`, or as a block
    /// error if the stage was waiting for network capacity, which also counts that block.
    fn stage_timeout_error(
        &mut self,
        reason: RestartReason,
        elapsed: tokio::time::error::Elapsed,
    ) -> RestartError {
        if self.count_cancelled_network_wait() {
            Self::block_restart_error(BlockDownloadVerifyError::Timeout)
        } else {
            RestartError::new(reason, elapsed)
        }
    }

    /// Returns the error for a genesis attempt that the genesis timeout cancelled, and counts it
    /// if the block was still waiting for network capacity.
    fn genesis_timeout(&mut self) -> BlockDownloadVerifyError {
        if self.count_cancelled_network_wait() {
            BlockDownloadVerifyError::Timeout
        } else {
            BlockDownloadVerifyError::GenesisTimeout
        }
    }

    /// Counts the errors of downloads that finished without the syncer reading their results,
    /// before a restart drops them.
    fn count_finished_block_errors(&mut self) {
        while let Some(Some(response)) = self.downloads.next().now_or_never() {
            if let Err(error) = response {
                self.count_block_error(&error);
            }
        }
    }

    /// Counts a block that failed to queue, download, or verify.
    fn count_block_error(&self, error: &BlockDownloadVerifyError) {
        metrics::counter!("sync.block.error.count", "kind" => Self::block_error_kind(error))
            .increment(1);
    }

    /// Wraps an error from obtaining or extending tips, which also queue the blocks they find, so
    /// a block that failed to queue is reported as a block error.
    fn tips_restart_error(reason: RestartReason, error: Report) -> RestartError {
        match error.downcast::<BlockDownloadVerifyError>() {
            Ok(error) => Self::block_restart_error(error),
            Err(error) => RestartError::new(reason, error),
        }
    }

    /// Wraps a block error that restarts the sync.
    fn block_restart_error(error: BlockDownloadVerifyError) -> RestartError {
        RestartError {
            reason: RestartReason::BlockError,
            kind: Some(Self::block_error_kind(&error)),
            error: error.into(),
        }
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
    ///
    /// Blocks that are only queued in the state, waiting for their parent, are treated as not
    /// present. A sync restart cancels in-flight downloads, so the parent of a queued block may
    /// never arrive unless the syncer downloads it again along with its queued children. If the
    /// syncer skipped queued blocks, `obtain_tips` would reject every download set that contains
    /// them, and the sync would stall until Zebra restarts. Re-submitting a queued block is
    /// harmless: the block verifier rejects it as a duplicate, which doesn't restart the sync.
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
            zs::Response::KnownBlock(loc) => {
                Ok(loc.is_some_and(|loc| loc != zs::KnownBlock::Queue))
            }
            _ => unreachable!("wrong response to known block request"),
        }
    }

    fn update_metrics(&mut self) {
        let in_flight = self.downloads.in_flight();

        metrics::gauge!("sync.prospective_tips.len",).set(self.prospective_tips.len() as f64);
        metrics::gauge!("sync.downloads.in_flight",).set(in_flight as f64);

        self.diagnostics.send_modify(|diagnostics| {
            diagnostics.in_flight = in_flight;
        });
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
                if error.is_auth_commitment_mismatch() =>
            {
                debug!(
                    error = ?e,
                    "served block body did not match the authorizing data commitment in its \
                     header: the block hash is still valid and is re-requested, and the serving \
                     peer is scored, continuing"
                );
                false
            }
            BlockDownloadVerifyError::Invalid { error, .. }
                if error.is_descendant_of_auth_commitment_mismatch() =>
            {
                debug!(
                    error = ?e,
                    "an ancestor's served body did not match the authorizing data commitment in \
                     its header: this block hash is still valid and is re-requested, continuing"
                );
                false
            }
            // An `AwaitUtxo` timeout: the spent output is usually in a recent block whose
            // commit a restart would cancel, looping near the tip (#11168, #11132).
            e if Self::is_utxo_lookup_timeout(e) => {
                debug!(error = ?e, "block spends an output that is not in our state yet, re-requesting, continuing");
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
                    "block height is behind the current state tip: re-requesting the hash, \
                     and scoring the peer if the body contradicts the parent we hold, continuing"
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
            BlockDownloadVerifyError::PipelineFull { .. } => {
                debug!(
                    error = ?e,
                    "sync pipeline is above its memory limit, \
                     requesting the block again later, continuing"
                );
                false
            }

            BlockDownloadVerifyError::DownloadFailed { ref error, .. }
                if is_not_found(error.as_ref()) =>
            {
                // Covers these errors:
                // - NotFoundResponse
                // - NotFoundRegistry
                //
                // TODO: improve this by checking the type (#2908)
                //       restart after a certain number of NotFound errors?
                debug!(error = ?e, "block was not found, possibly from a peer that doesn't have the block yet, continuing");
                false
            }
            BlockDownloadVerifyError::DownloadTimedOut { .. } => {
                debug!(error = ?e, "no peer delivered the block within the download round, re-requesting, continuing");
                false
            }

            // The short post-final-checkpoint verify timeout is a UTXO race (#5125), not an
            // invalid block; the 8-minute tower timeout stays in the catch-all (#5709).
            e if Self::is_post_checkpoint_verify_timeout(e) => {
                debug!(error = ?e, "initial fully verified block timed out waiting for its parent's outputs, re-requesting, continuing");
                false
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

                warn!(
                    ?e,
                    kind = Self::block_error_kind(e),
                    "error downloading and verifying block"
                );
                true
            }
        }
    }
}
