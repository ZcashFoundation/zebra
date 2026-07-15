//! The known-hash initial block download (IBD) engine.
//!
//! During initial sync on networks with a bundled known-hash list, the engine
//! downloads blocks directly by their pinned hashes instead of discovering
//! hashes from peers, verifies them against the list, and commits them
//! straight to the finalized state. When the engine finishes (or declines to
//! run), the legacy syncer takes over from the real tip.
//!
//! This module holds the supervisor: it decides whether the engine can run,
//! owns the engine task, and reports the [`IbdOutcome`] used by startup
//! wiring to hand off to the legacy syncer.
//!
//! The engine is gated behind the `sync.known_hash_sync` config flag, which
//! defaults to on for Mainnet (enabled in Phase E1). See
//! `docs/design/known-hash-ibd.md` for the full design; the supervisor is
//! specified in §4.7.

use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    time::Duration,
};

use color_eyre::eyre::{eyre, Report};
use tokio::{sync::watch, task::JoinHandle};
use tower::{Service, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::{
    block,
    chain_tip::ChainTip,
    parameters::{
        known_hashes::{KnownHashError, KnownHashList, KnownHashListSpec, HASHES_PER_CHUNK},
        Network,
    },
};
use zebra_network as zn;
use zebra_state as zs;

use crate::{components::sync::Config, BoxError};

use self::{consume::CfHashSource, engine::HashSource};

pub mod cache;
pub mod consume;
pub mod convert;
pub mod engine;
pub mod fetch;
pub mod semantic;
pub mod tree;

#[cfg(test)]
mod tests;

/// The delay between supervisor restarts of the engine after a
/// fatal-but-retryable error.
///
/// Long enough to let transient network or state conditions clear, short
/// enough that a restart barely dents sync throughput.
pub const IBD_RESTART_DELAY: Duration = Duration::from_secs(15);

/// The number of consecutive engine restarts with zero frontier progress
/// after which the supervisor may degrade to the legacy syncer — only above
/// the mandatory checkpoint height (design doc §4.1, §4.7).
///
/// Below the mandatory checkpoint the engine restarts forever with alarms:
/// semantic sync below Canopy is not a sound fallback.
pub const IBD_MAX_RESTARTS_WITHOUT_PROGRESS: u32 = 5;

/// How often the engine repeats its warning while the commit frontier is
/// stalled.
///
/// Frequent enough that operators notice a stuck sync, infrequent enough to
/// avoid flooding the logs while the stall escalation ladder works.
pub const IBD_STALL_WARN_INTERVAL: Duration = Duration::from_secs(60);

/// The result of running the known-hash IBD engine.
///
/// Whatever the outcome, the legacy syncer starts afterwards from a block
/// locator at the real tip; the outcome only determines logging and metrics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IbdOutcome {
    /// The engine committed every block through the list's
    /// [`max_height`](KnownHashList::max_height).
    Completed {
        /// The last height committed by the engine.
        final_height: block::Height,
    },

    /// The engine did not run; the legacy syncer starts immediately.
    Declined(DeclineReason),

    /// The engine gave up above the mandatory checkpoint floor and handed
    /// its unfinished range to the legacy syncer:
    /// [`IBD_MAX_RESTARTS_WITHOUT_PROGRESS`] consecutive engine restarts made
    /// zero frontier progress, and the legacy syncer is correct, just slower
    /// (design doc §4.1, §4.7; degradation is only permitted above the
    /// mandatory checkpoint height — below it the engine restarts forever
    /// with alarms).
    Degraded,
}

/// Reasons the known-hash engine declines to run.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DeclineReason {
    /// No known-hash list is bundled for the configured network.
    NoList,

    /// The chain tip is already at or past the end of the list, so there is
    /// nothing for the engine to download.
    AlreadyPast,

    /// The `sync.known_hash_sync` config flag is off.
    DisabledByConfig,
}

/// The known-hash IBD engine supervisor.
///
/// Checks the preconditions for running the engine (config flag, bundled
/// list, verified assets, tip below the end of the list), then runs the
/// engine task and returns its [`IbdOutcome`].
pub struct IbdEngine<ZN, ZS, ZSTip>
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
    ZSTip: ChainTip + Clone + Send + 'static,
{
    /// The sync configuration section, including the known-hash engine
    /// settings.
    config: Config,

    /// The configured network.
    network: Network,

    /// A network service which is used to download blocks by hash.
    peer_set: ZN,

    /// A buffered state service which is used to commit verified blocks.
    state: ZS,

    /// Allows efficient access to the best tip of the blockchain.
    latest_chain_tip: ZSTip,

    /// Live peer set status, for sizing the engine's download pipeline.
    peer_set_status: watch::Receiver<zn::PeerSetStatus>,

    /// The disk overflow tier's directory
    /// (`<state cache_dir>/`[`cache::CACHE_DIR_NAME`]).
    cache_dir: PathBuf,
}

impl<ZN, ZS, ZSTip> IbdEngine<ZN, ZS, ZSTip>
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
    ZSTip: ChainTip + Clone + Send + 'static,
{
    /// Returns a new known-hash IBD engine supervisor, using:
    /// - `config`: the `[sync]` config section,
    /// - `network`: the configured network,
    /// - `peer_set`: the buffered zebra-network peer set,
    /// - `state`: the buffered zebra-state service,
    /// - `latest_chain_tip`: the latest chain tip from `state`,
    /// - `peer_set_status`: the live peer set status watch from
    ///   [`zebra_network::init`],
    /// - `cache_dir`: the state cache directory; the disk overflow tier
    ///   lives under `<cache_dir>/`[`cache::CACHE_DIR_NAME`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Config,
        network: Network,
        peer_set: ZN,
        state: ZS,
        latest_chain_tip: ZSTip,
        peer_set_status: watch::Receiver<zn::PeerSetStatus>,
        cache_dir: &Path,
    ) -> Self {
        Self {
            config,
            network,
            peer_set,
            state,
            latest_chain_tip,
            peer_set_status,
            cache_dir: cache_dir.join(cache::CACHE_DIR_NAME),
        }
    }

    /// Checks the engine preconditions, then runs the engine — restarting it
    /// after [`IBD_RESTART_DELAY`] on fatal-but-retryable errors — and
    /// returns its outcome (design doc §4.7).
    ///
    /// Every (re)start re-derives the first uncommitted height from the
    /// state tip and rebuilds the engine window; the consecutive-failure
    /// counter resets whenever a restart makes frontier progress. Above the
    /// mandatory checkpoint height,
    /// [`IBD_MAX_RESTARTS_WITHOUT_PROGRESS`] zero-progress restarts degrade
    /// to the legacy syncer; below it the engine restarts forever with
    /// alarms.
    ///
    /// Returns `Ok(IbdOutcome::Declined(_))` when the engine cannot or should
    /// not run; the caller starts the legacy syncer either way. Returns an
    /// error on broken or tampered list assets and on non-retryable engine
    /// failures (list diagnostics, state shutdown), which need operator
    /// attention rather than a silent fallback.
    pub async fn run(self) -> Result<IbdOutcome, BoxError> {
        if !self.config.known_hash_sync {
            debug!("known-hash sync is disabled by config; using the legacy syncer");
            return Ok(IbdOutcome::Declined(DeclineReason::DisabledByConfig));
        }

        let Some(spec) = KnownHashListSpec::for_network(&self.network) else {
            info!(
                network = %self.network,
                "no known-hash list is bundled for this network; using the legacy syncer",
            );
            return Ok(IbdOutcome::Declined(DeclineReason::NoList));
        };

        let mandatory_floor = self.network.mandatory_checkpoint_height();

        // Snapshot-consume sync reads every artifact from the local artifact
        // directory, so the pairing is validated up front — before peers
        // connect or any asset set is opened and hashed.
        let local_source_dir = if self.config.snapshot_consume_sync {
            let Some(dir) = self.config.known_hash_local_source_dir.clone() else {
                // Without the artifact directory there is nothing to consume
                // from: a misconfiguration, not a retryable engine failure.
                return Err(eyre!(
                    "sync.snapshot_consume_sync is enabled, but \
                     sync.known_hash_local_source_dir is not set; point it at the \
                     snapshot artifact directory downloaded by the installer \
                     (or emitted by `zebrad emit-snapshot --emit-files`), \
                     or disable snapshot_consume_sync",
                )
                .into());
            };
            Some(dir)
        } else {
            None
        };

        let mut restarts: u32 = 0;
        let mut failures_without_progress: u32 = 0;
        let mut last_start_height: Option<block::Height> = None;

        loop {
            // Check the tip against the spec constant before opening the
            // list: a synced node must not re-read and re-hash the full
            // asset set (~103 MB on Mainnet) on every restart.
            let tip_height = self.latest_chain_tip.best_tip_height();

            if tip_height >= Some(spec.max_height) {
                return Ok(if restarts == 0 {
                    info!(
                        ?tip_height,
                        list_max_height = ?spec.max_height,
                        "chain tip is already past the known-hash list; \
                         using the legacy syncer",
                    );
                    IbdOutcome::Declined(DeclineReason::AlreadyPast)
                } else {
                    // A previous run committed through the end of the list
                    // before failing.
                    IbdOutcome::Completed {
                        final_height: spec.max_height,
                    }
                });
            }

            // Every (re)start re-derives the first uncommitted height from
            // the state tip (design doc §4.7).
            let next_commit = tip_height.map_or(block::Height(0), |tip| {
                tip.next()
                    .expect("a tip below the list max height is far below Height::MAX")
            });

            // A restart that advanced the frontier resets the failure count.
            if last_start_height.is_some_and(|previous| next_commit > previous) {
                failures_without_progress = 0;
            }
            last_start_height = Some(next_commit);

            info!(
                ?next_commit,
                list_max_height = ?spec.max_height,
                restarts,
                snapshot_consume = self.config.snapshot_consume_sync,
                "starting the known-hash IBD engine",
            );

            // In snapshot-consume mode the engine reads hashes from the
            // `known_hash_chunk` column family (reading missing chunks from the
            // artifact directory and verifying them against the pinned hashes),
            // and the bundled v1 `.bin` list is never opened — it carries no
            // per-height tree roots and could silently disagree with the v2
            // chunks. Otherwise it reads the bundled `.bin` list directly. Both
            // paths run the same generic engine over a `HashSource`.
            let run_result = if let Some(dir) = &local_source_dir {
                info!(
                    local_source = %dir.display(),
                    "snapshot-consume sync is reading artifacts from the local \
                     artifact directory",
                );
                let snapshot_source = consume::LocalSnapshotSource::new(dir.clone());
                let source = CfHashSource::new(spec, snapshot_source, self.state.clone());
                Self::bootstrap_and_run_engine(
                    self.network.clone(),
                    self.peer_set.clone(),
                    self.state.clone(),
                    self.peer_set_status.clone(),
                    self.cache_dir.clone(),
                    self.config.clone(),
                    next_commit,
                    source,
                )
                .await
            } else {
                // Re-opened on every restart: the engine consumes the list, and
                // restarts are rare enough that re-verifying the assets is
                // cheaper than holding a second copy across the whole run.
                //
                // Opening reads and hashes the full asset set (~103 MB on
                // Mainnet), so it runs on a blocking thread instead of stalling
                // the async runtime.
                let open_result = tokio::task::spawn_blocking({
                    let network = self.network.clone();
                    let list_dir = self.config.known_hash_list_dir.clone();
                    move || KnownHashList::open(&network, list_dir.as_deref())
                })
                .await?;

                let list = match open_result {
                    Ok(Some(list)) => list,
                    // Unreachable: `for_network()` returned a spec just above,
                    // and `open()` only returns `None` when there is no spec.
                    Ok(None) => return Ok(IbdOutcome::Declined(DeclineReason::NoList)),
                    Err(error @ KnownHashError::AssetsNotFound { .. }) => {
                        // With the checkpoint verifier removed, the engine is
                        // the only path that can commit blocks at or below the
                        // mandatory floor: a missing asset set is a hard,
                        // actionable error, never a silent fallback (design doc
                        // §6.3).
                        return Err(error.into());
                    }
                    // Corrupt, tampered, or unreadable assets: surface the
                    // error instead of silently syncing without the engine.
                    Err(error) => return Err(error.into()),
                };

                Self::build_and_run_engine(
                    self.network.clone(),
                    self.peer_set.clone(),
                    self.state.clone(),
                    self.peer_set_status.clone(),
                    self.cache_dir.clone(),
                    self.config.clone(),
                    next_commit,
                    list,
                )
                .await
            };

            match run_result {
                Ok(outcome) => return Ok(outcome),

                // Fatal diagnostics and shutdowns propagate: restarting
                // cannot fix a broken list or a closed state (§4.6).
                Err(error) if !error.is_retryable() => return Err(error.into()),

                Err(error) => {
                    restarts += 1;
                    failures_without_progress += 1;

                    warn!(
                        %error,
                        restarts,
                        failures_without_progress,
                        "known-hash IBD engine failed; restarting",
                    );

                    if next_commit > mandatory_floor
                        && failures_without_progress >= IBD_MAX_RESTARTS_WITHOUT_PROGRESS
                    {
                        warn!(
                            ?next_commit,
                            failures_without_progress,
                            "known-hash IBD engine made no progress over repeated restarts \
                             above the mandatory checkpoint; degrading to the legacy syncer",
                        );
                        return Ok(IbdOutcome::Degraded);
                    }

                    tokio::time::sleep(IBD_RESTART_DELAY).await;
                }
            }
        }
    }

    /// Builds the known-hash engine over `source` starting at `next_commit` and
    /// runs it to completion.
    ///
    /// Generic over the [`HashSource`] so the same construction serves both the
    /// bundled `.bin` list ([`KnownHashList`]) and the CF-backed
    /// [`CfHashSource`] used in snapshot-consume mode. Takes owned service
    /// clones rather than borrowing `self`, so the returned future stays `Send`
    /// without bounding the chain-tip type as `Sync`.
    #[allow(clippy::too_many_arguments)]
    async fn build_and_run_engine<L>(
        network: Network,
        peer_set: ZN,
        state: ZS,
        peer_set_status: watch::Receiver<zn::PeerSetStatus>,
        cache_dir: PathBuf,
        config: Config,
        next_commit: block::Height,
        source: L,
    ) -> Result<IbdOutcome, engine::EngineError>
    where
        L: HashSource,
    {
        // In snapshot-consume mode the engine reads note commitment trees from
        // the artifact directory too (the single tree-fetch dispatch point).
        // The local source only applies when snapshot-consume sync is enabled;
        // outside it no tree fetch is ever scheduled.
        let local_snapshot_source = config
            .snapshot_consume_sync
            .then(|| config.known_hash_local_source_dir.clone())
            .flatten()
            .map(consume::LocalSnapshotSource::new);

        let mut engine = engine::Engine::new(
            network,
            peer_set,
            state,
            next_commit,
            source,
            peer_set_status,
            // The cache index is rebuilt from disk by the engine's restore scan
            // on every (re)start.
            cache::BlockCache::new(&cache_dir),
            config.known_hash_lookahead_bytes,
            Duration::from_secs(config.known_hash_gap_hedge_secs),
            config.known_hash_tree_lookahead,
            local_snapshot_source,
        );

        engine.run().await
    }

    /// Primes the snapshot-consume source and runs the engine.
    ///
    /// Before syncing, reads and verifies the known-hash chunk(s) covering the
    /// first window (so the engine has hashes, size hints, and tree roots for the
    /// heights it is about to fetch). Each chunk is content-addressed against its
    /// pinned SHA-256 and persisted to the `known_hash_chunk` column family
    /// (design doc `snapshot-distribution.md`). A deterministic artifact failure
    /// (a missing, corrupt, or hash-mismatched file) surfaces as a fatal
    /// diagnostic — re-reading the same artifact directory can never cure it —
    /// while a transient state-service failure stays retryable.
    ///
    /// As the commit frontier advances, the engine re-primes the covering chunks
    /// itself through [`HashSource::ensure_covers`](engine::HashSource::ensure_covers)
    /// on each refill pass, so the lookups keep working across chunk boundaries;
    /// this bootstrap step only primes the very first window so a chunk-unavailable
    /// failure is surfaced before the engine starts.
    ///
    /// Takes owned service clones (like [`build_and_run_engine`]) so the future
    /// is `Send`.
    ///
    /// [`build_and_run_engine`]: Self::build_and_run_engine
    #[allow(clippy::too_many_arguments)]
    async fn bootstrap_and_run_engine(
        network: Network,
        peer_set: ZN,
        state: ZS,
        peer_set_status: watch::Receiver<zn::PeerSetStatus>,
        cache_dir: PathBuf,
        config: Config,
        next_commit: block::Height,
        mut source: CfHashSource<ZS>,
    ) -> Result<IbdOutcome, engine::EngineError> {
        // Prime the chunk(s) covering the first uncommitted height, so the
        // engine's first window has hashes before any block fetch is issued. The
        // window spans at most two chunks (design doc §6.4); `ensure_covers`
        // covers from `next_commit` through the next chunk's first height.
        let cover_end = block::Height(next_commit.0.saturating_add(HASHES_PER_CHUNK));

        if let Err(error) = source.ensure_covers(next_commit, cover_end).await {
            // A missing, corrupt, or hash-mismatched artifact file fails the
            // same way on every read: surface it as a fatal diagnostic the
            // operator must act on, instead of restart-looping. Only a
            // transient state-service failure is worth a restart.
            if error.is_deterministic() {
                warn!(
                    %error,
                    ?next_commit,
                    "the snapshot artifact directory cannot supply a verified \
                     known-hash chunk; fix or re-download the artifacts",
                );
                return Err(engine::EngineError::ArtifactDiagnostic(Box::new(error)));
            }

            warn!(
                %error,
                ?next_commit,
                "failed to prime a known-hash chunk for snapshot-consume sync; \
                 the supervisor will restart the bootstrap",
            );
            return Err(engine::EngineError::List(Box::new(error)));
        }

        Self::build_and_run_engine(
            network,
            peer_set,
            state,
            peer_set_status,
            cache_dir,
            config,
            next_commit,
            source,
        )
        .await
    }
}

/// Commits the Regtest genesis block directly to the state, if the state
/// doesn't already contain it.
///
/// In Regtest, the legacy syncer's genesis download requires a connected
/// peer, which a standalone Regtest node doesn't have. Genesis is below the
/// checkpoint gate (`zebra_consensus::checkpoints`), so it is committed as a
/// checkpoint-verified block: its hash is checked against the network
/// genesis hash, which is the same pin the deleted checkpoint verifier
/// applied.
pub async fn commit_regtest_genesis_if_missing<ZS>(
    network: &Network,
    state: ZS,
) -> Result<(), Report>
where
    ZS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Send + Clone + 'static,
    ZS::Future: Send,
{
    let genesis_block = zebra_chain::block::genesis::regtest_genesis_block();
    let genesis = zs::CheckpointVerifiedBlock::from(genesis_block);

    assert_eq!(
        genesis.hash,
        network.genesis_hash(),
        "the Regtest genesis block hash should match the network genesis hash"
    );

    let known = state
        .clone()
        .oneshot(zs::Request::KnownBlock(genesis.hash))
        .await
        .map_err(|error| eyre!(error))?;
    if matches!(known, zs::Response::KnownBlock(Some(_))) {
        return Ok(());
    }

    state
        .oneshot(zs::Request::CommitCheckpointVerifiedBlock(genesis))
        .await
        .map_err(|error| eyre!("committing the Regtest genesis block failed: {error}"))?;

    Ok(())
}

/// Spawns the known-hash IBD engine and the legacy syncer as one combined,
/// supervised task: the engine runs to completion first, then `sync_fut`
/// (the legacy syncer) takes over from the real tip (design doc §4.7).
///
/// `ChainSync` is constructed at startup — its status handles feed the
/// mempool, gossip, and progress tasks — but futures are lazy: `sync_fut`
/// does not run until awaited, after the engine returns. The returned handle
/// goes into the startup supervision `select!`, so ctrl-c and sibling-task
/// failures abort the engine cleanly.
///
/// Engine errors need operator attention (broken assets, state shutdown), so
/// the combined task exits with the error instead of silently falling back
/// to the legacy syncer.
pub fn spawn_engine_then_legacy_sync<ZN, ZS, ZSTip, F>(
    engine: IbdEngine<ZN, ZS, ZSTip>,
    sync_fut: F,
) -> JoinHandle<Result<(), Report>>
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
    ZSTip: ChainTip + Clone + Send + 'static,
    F: Future<Output = Result<(), Report>> + Send + 'static,
{
    // Boxing the engine future erases the higher-ranked lifetimes its
    // generic services would otherwise leak into the spawned task type.
    let engine_fut: Pin<Box<dyn Future<Output = Result<IbdOutcome, BoxError>> + Send>> =
        Box::pin(engine.run());

    // Boxing erases the generic syncer future type, which otherwise trips
    // rustc's implementation-of-`From`-is-not-general-enough false positive
    // when captured by the combined task's async block.
    let sync_fut: Pin<Box<dyn Future<Output = Result<(), Report>> + Send>> = Box::pin(sync_fut);

    // The engine runs as its own supervised task; the combined task awaits
    // its outcome, so the syncer only drives after the engine returns.
    let engine_task = tokio::spawn(engine_fut.in_current_span());

    tokio::spawn(
        async move {
            match engine_task.await {
                Ok(Ok(outcome)) => {
                    info!(
                        ?outcome,
                        "known-hash IBD engine finished; starting the legacy syncer",
                    );
                }
                Ok(Err(error)) => {
                    return Err(eyre!("known-hash IBD engine failed: {error}"));
                }
                Err(join_error) => {
                    return Err(eyre!("known-hash IBD engine panicked: {join_error}"));
                }
            }

            sync_fut.await
        }
        .in_current_span(),
    )
}
