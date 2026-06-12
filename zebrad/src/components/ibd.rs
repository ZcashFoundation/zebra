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
    path::{Path, PathBuf},
    time::Duration,
};

use tokio::sync::watch;
use tower::Service;

use zebra_chain::{
    block,
    chain_tip::ChainTip,
    parameters::{
        known_hashes::{KnownHashError, KnownHashList, KnownHashListSpec},
        Network,
    },
};
use zebra_network as zn;
use zebra_state as zs;

use crate::{components::sync::Config, BoxError};

pub mod cache;
pub mod convert;
pub mod engine;
pub mod fetch;

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

            // Re-opened on every restart: the engine consumes the list, and
            // restarts are rare enough that re-verifying the assets is
            // cheaper than holding a second copy across the whole run.
            let list = match KnownHashList::open(
                &self.network,
                self.config.known_hash_list_dir.as_deref(),
            ) {
                Ok(Some(list)) => list,
                // Unreachable: `for_network()` returned a spec just above, and
                // `open()` only returns `None` when there is no spec.
                Ok(None) => return Ok(IbdOutcome::Declined(DeclineReason::NoList)),
                Err(error @ KnownHashError::AssetsNotFound { .. }) => {
                    // With the checkpoint verifier removed, the engine is the
                    // only path that can commit blocks at or below the
                    // mandatory floor: a missing asset set is a hard,
                    // actionable error, never a silent fallback (design doc
                    // §6.3).
                    return Err(error.into());
                }
                // Corrupt, tampered, or unreadable assets: surface the error
                // instead of silently syncing without the engine.
                Err(error) => return Err(error.into()),
            };

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
                list_max_height = ?list.max_height(),
                restarts,
                "starting the known-hash IBD engine",
            );

            let mut engine = engine::Engine::new(
                self.network.clone(),
                self.peer_set.clone(),
                self.state.clone(),
                next_commit,
                list,
                self.peer_set_status.clone(),
                // The cache index is rebuilt from disk by the engine's
                // restore scan on every (re)start.
                cache::BlockCache::new(&self.cache_dir),
                self.config.known_hash_lookahead_bytes,
                Duration::from_secs(self.config.known_hash_gap_hedge_secs),
            );

            match engine.run().await {
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
}
