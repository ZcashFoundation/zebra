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
//! The engine is gated behind the `sync.known_hash_sync` config flag
//! (default off). See `docs/design/known-hash-ibd.md` for the full design;
//! the supervisor is specified in §4.7.

use std::time::Duration;

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

pub mod engine;
pub mod fetch;
pub mod peer_stats;

#[cfg(test)]
mod tests;

/// The delay between supervisor restarts of the engine after a
/// fatal-but-retryable error.
///
/// Long enough to let transient network or state conditions clear, short
/// enough that a restart barely dents sync throughput.
pub const IBD_RESTART_DELAY: Duration = Duration::from_secs(15);

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
    /// its unfinished range to the legacy syncer.
    Degraded(DegradeReason),
}

/// Reasons the known-hash engine declines to run.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DeclineReason {
    /// No known-hash list is bundled for the configured network.
    NoList,

    /// The list chunk files were not found in any searched directory.
    AssetMissing,

    /// The chain tip is already at or past the end of the list, so there is
    /// nothing for the engine to download.
    AlreadyPast,

    /// The `sync.known_hash_sync` config flag is off.
    DisabledByConfig,

    /// The engine core is not implemented yet, so a startable engine still
    /// hands straight off to the legacy syncer.
    ///
    /// Removed when D2–D6 land (see `docs/design/known-hash-ibd.md` §9
    /// Phase D); until then this variant keeps the skeleton honest: it never
    /// claims `Completed` for work it did not do.
    NotYetImplemented,
}

/// Reasons the engine may hand an unfinished range back to the legacy syncer.
///
/// Degradation is only permitted above the mandatory checkpoint height
/// (`docs/design/known-hash-ibd.md` §4.1); below it the engine loops forever
/// with alarms instead.
///
/// Currently uninhabited: the supervisor restart policy that produces
/// degradations lands with the rest of Phase D.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DegradeReason {}

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
    /// - `latest_chain_tip`: the latest chain tip from `state`.
    pub fn new(
        config: Config,
        network: Network,
        peer_set: ZN,
        state: ZS,
        latest_chain_tip: ZSTip,
    ) -> Self {
        Self {
            config,
            network,
            peer_set,
            state,
            latest_chain_tip,
        }
    }

    /// Checks the engine preconditions, then runs the engine and returns its
    /// outcome.
    ///
    /// Returns `Ok(IbdOutcome::Declined(_))` when the engine cannot or should
    /// not run; the caller starts the legacy syncer either way. Returns an
    /// error only on broken or tampered list assets, which need operator
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

        // Check the tip against the spec constant before opening the list:
        // a synced node must not re-read and re-hash the full asset set
        // (~103 MB on Mainnet) on every restart.
        let tip_height = self.latest_chain_tip.best_tip_height();

        if tip_height >= Some(spec.max_height) {
            info!(
                ?tip_height,
                list_max_height = ?spec.max_height,
                "chain tip is already past the known-hash list; using the legacy syncer",
            );
            return Ok(IbdOutcome::Declined(DeclineReason::AlreadyPast));
        }

        let list =
            match KnownHashList::open(&self.network, self.config.known_hash_list_dir.as_deref()) {
                Ok(Some(list)) => list,
                // Unreachable: `for_network()` returned a spec just above, and
                // `open()` only returns `None` when there is no spec.
                Ok(None) => return Ok(IbdOutcome::Declined(DeclineReason::NoList)),
                Err(error @ KnownHashError::AssetsNotFound { .. }) => {
                    warn!(
                        %error,
                        "known-hash list assets not found; using the legacy syncer",
                    );
                    return Ok(IbdOutcome::Declined(DeclineReason::AssetMissing));
                }
                // Corrupt, tampered, or unreadable assets: surface the error
                // instead of silently syncing without the engine.
                Err(error) => return Err(error.into()),
            };

        // Every (re)start re-derives the first uncommitted height from the
        // state tip (design doc §4.7).
        let next_commit = tip_height.map_or(block::Height(0), |tip| {
            tip.next()
                .expect("a tip below the list max height is far below Height::MAX")
        });

        info!(
            ?next_commit,
            list_max_height = ?list.max_height(),
            "starting the known-hash IBD engine",
        );

        // TODO(known-hash-ibd D6): plumb the real peer set status watch
        // (`PeerSet::status_receiver()`) through `start.rs`; until then the
        // engine sees a default (empty) status and sizes its batch
        // concurrency at the minimum.
        let (_status_sender, peer_status) =
            tokio::sync::watch::channel(zn::PeerSetStatus::default());

        let engine = engine::Engine::new(
            self.peer_set,
            self.state,
            next_commit,
            list,
            peer_status,
            self.config.known_hash_lookahead_bytes,
            Duration::from_secs(self.config.known_hash_gap_hedge_secs),
        );

        // TODO(known-hash-ibd D4/D6): supervisor restart loop with
        // `IBD_RESTART_DELAY`, progress tracking, and degradation above the
        // mandatory checkpoint floor (design doc §4.7).
        engine.run().await
    }
}
