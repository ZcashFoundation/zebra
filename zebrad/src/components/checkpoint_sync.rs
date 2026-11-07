//! Checkpoint-anchored initial block download over the version 2 protocol.
//!
//! The engine runs *beside* the legacy [`ChainSync`](super::ChainSync); they
//! never download the same heights. Ownership of the chain is a
//! [`SyncOwner`] in a `watch` channel: the engine owns the heights from the
//! state tip up to its target, the legacy syncer everything else, and the
//! legacy syncer is also the fallback. Hand-offs happen at checkpoint
//! heights unless a commit failed mid-interval, and fast-forward the
//! `CheckpointVerifier` to the durable state tip through the `checkpoint_tip`
//! channel (`zebra_consensus::router::init_with_checkpoint_tip`).
//!
//! Module tree:
//! - this file: ownership types and the supervisor state machine;
//! - [`engine`]: the engine task, window and commit path;
//! - [`scheduler`]: unit planning, pull budgets, stalls, end-game;
//! - [`convert`]: raw bytes → `CheckpointVerifiedBlock` on rayon;
//! - [`anchors`]: checkpoint-list anchors;
//! - [`known_hashes`]: the known-hash list, fetched from peers and verified
//!   against its pins, as a [`HashSource`](scheduler::HashSource) that
//!   anchors units at every listed height;
//! - [`hints`]: spentness-hint acquisition, verification and serving.
//!
//! The supervisor's state machine:
//!
//! ```text
//! Idle(Legacy) ──(≥ 2 range-serving peers && target − tip ≥ 20 intervals)──► Claiming
//! Claiming: publish Engine{target, generation}; wait for the syncer's ack;
//!           the engine starts at tip + 1
//! Running ──(target reached | too few peers | no progress | commit timeouts | defect)──► Draining
//! Draining: stop issuing, cancel streams, await every submitted commit,
//!           publish the durable tip on `checkpoint_tip`, publish Legacy ──► Idle
//! ```
//!
//! Re-claiming is allowed after a cooldown, when peers reappear and the gap
//! is large again, so fallback works both ways.

use std::{collections::BTreeSet, net::IpAddr, sync::Arc, time::Duration};

use color_eyre::eyre::eyre;
use tokio::{
    sync::{mpsc, watch},
    time,
};

use zebra_chain::{
    block,
    chain_tip::ChainTip,
    parameters::{known_hashes::KnownHashListSpec, Network},
};
use zebra_consensus::CommittedTip;
use zebra_network::{types::PeerServices, SyncPeer};

use crate::BoxError;

use anchors::Anchors;
use engine::{Charged, EngineExit, EngineRun, RangePeer};
use known_hashes::{ChunkPins, HashPeer, KnownHashes, PinnedList};
use scheduler::HashSource;

pub mod anchors;
pub mod convert;
pub mod engine;
pub mod hints;
pub mod known_hashes;
pub mod scheduler;

pub use engine::EngineConfig;

/// Which component owns block download.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SyncOwner {
    /// The legacy syncer owns every height. The initial value.
    Legacy,

    /// The engine owns the heights from the state tip up to `target`, a
    /// checkpoint height. The legacy syncer must not download or submit
    /// blocks while this is published.
    Engine {
        /// The engine's target: a checkpoint height.
        target: block::Height,
        /// Increases by one with every claim. The legacy syncer acknowledges
        /// a claim by publishing its generation on the ack channel once its
        /// restart path has cancelled its downloads.
        generation: u64,
    },
}

impl SyncOwner {
    /// Whether the legacy syncer owns download.
    pub fn is_legacy(&self) -> bool {
        matches!(self, SyncOwner::Legacy)
    }
}

/// The legacy syncer's side of the ownership channels (wired into
/// `ChainSync`).
#[derive(Debug)]
pub struct LegacyOwnership {
    /// The current owner. `ChainSync::sync` waits for
    /// `owner.wait_for(SyncOwner::is_legacy)` before each `try_to_sync`, and
    /// `try_to_sync`'s tip loop also requires `owner.borrow().is_legacy()`.
    pub owner: watch::Receiver<SyncOwner>,

    /// Acknowledges an engine claim: the syncer sends the claim's
    /// `generation` after it has cancelled its downloads.
    pub ack: watch::Sender<u64>,

    /// The block counts of committed engine units, which the syncer pushes
    /// into its `RecentSyncLengths` so `SyncStatus::is_close_to_tip` stays
    /// false while the engine runs.
    pub unit_lengths: mpsc::Receiver<usize>,
}

/// The supervisor's side of the ownership channels.
#[derive(Debug)]
pub struct EngineOwnership {
    /// Publishes the owner. Only the supervisor sends.
    pub owner: watch::Sender<SyncOwner>,

    /// The legacy syncer's acknowledged claim generation.
    pub ack: watch::Receiver<u64>,

    /// Sends committed unit block counts to the legacy syncer.
    pub unit_lengths: mpsc::Sender<usize>,

    /// Publishes the durable state tip after an engine run, for the
    /// checkpoint verifier's fast-forward. It is a checkpoint unless a commit
    /// failed mid-interval. Later, it also publishes commits that landed
    /// after the hand-back.
    pub checkpoint_tip: watch::Sender<Option<CommittedTip>>,
}

/// The fewest checkpoint intervals between the tip and the target for which
/// the engine claims ownership (about 5,000 heights).
pub const MIN_ENGINE_SPAN_INTERVALS: usize = 20;

/// How often an idle supervisor re-evaluates the claim conditions.
pub const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// How long the supervisor waits after a hand-back before it may claim
/// again, so the engine does not flap.
pub const RECLAIM_COOLDOWN: Duration = Duration::from_secs(60);

/// The capacity of the unit-length channel to the legacy syncer.
const UNIT_LENGTHS_CAPACITY: usize = 64;

/// Creates the ownership channels. Called in `start` before
/// `zebra_consensus::router::init_with_checkpoint_tip`, which takes the
/// returned `checkpoint_tip` receiver.
pub fn ownership_channels() -> (
    EngineOwnership,
    LegacyOwnership,
    watch::Receiver<Option<CommittedTip>>,
) {
    let (owner_tx, owner_rx) = watch::channel(SyncOwner::Legacy);
    let (ack_tx, ack_rx) = watch::channel(0);
    let (lengths_tx, lengths_rx) = mpsc::channel(UNIT_LENGTHS_CAPACITY);
    let (tip_tx, tip_rx) = watch::channel(None);

    (
        EngineOwnership {
            owner: owner_tx,
            ack: ack_rx,
            unit_lengths: lengths_tx,
            checkpoint_tip: tip_tx,
        },
        LegacyOwnership {
            owner: owner_rx,
            ack: ack_tx,
            unit_lengths: lengths_rx,
        },
        tip_rx,
    )
}

/// The supervisor: claims ownership when enough range-serving peers exist
/// and the gap is large, runs the engine, drains it, hands back to the
/// legacy syncer, and re-claims later.
///
/// Generic over the state services like `ChainSync`.
pub struct CheckpointSync<ZS, ZSTip> {
    supervisor: Supervisor<SyncPeer, ZS, ZSTip>,
}

impl<ZS, ZSTip> CheckpointSync<ZS, ZSTip>
where
    ZS: tower::Service<
            zebra_state::Request,
            Response = zebra_state::Response,
            Error = zebra_chain::BoxError,
        > + Send
        + Clone
        + 'static,
    ZS::Future: Send,
    ZSTip: zebra_chain::chain_tip::ChainTip + Clone + Send + 'static,
{
    /// Creates the supervisor.
    ///
    /// - `peers`: the registry's receiver from
    ///   `zebra_network::SyncPeersSender::spawn`;
    /// - `state`: the buffered state service (commits go through
    ///   `zs::Request::CommitCheckpointVerifiedBlock`);
    /// - `latest_chain_tip`: the engine starts only once
    ///   `best_tip_height().is_some()`, after the legacy syncer's genesis;
    /// - `known_hashes`: the network's known-hash list, from
    ///   [`known_hashes::known_hash_list`], if it has one. Each engine run
    ///   fetches its chunks from `peers` and anchors units at its verified
    ///   entries, and at checkpoints where they are not resident yet;
    /// - `hints_gate`: `Some` when spentness hints may apply (a later
    ///   change passes it for a non-`Off` mode on an empty state; `start`
    ///   passes `None`). The supervisor then
    ///   claims ownership as soon as genesis is committed, whatever the peer
    ///   count and gap, and the engine submits no commit until the gate's
    ///   value is `true` (acquisition finished, with or without an artifact).
    ///   If the gate's sender is dropped, it is treated as `true`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: EngineConfig,
        network: Network,
        peers: zebra_network::SyncPeers,
        state: ZS,
        latest_chain_tip: ZSTip,
        ownership: EngineOwnership,
        hints_gate: Option<watch::Receiver<bool>>,
        known_hashes: Option<KnownHashListSpec>,
    ) -> Self {
        let known_hashes = known_hashes.map(|spec| {
            Arc::new(PinnedList {
                spec,
                network: network.clone(),
            }) as Arc<dyn ChunkPins>
        });

        CheckpointSync {
            supervisor: Supervisor::new(
                config,
                network,
                peers,
                state,
                latest_chain_tip,
                ownership,
                hints_gate,
                known_hashes,
            ),
        }
    }

    /// Plans the engine's downloads only up to the highest checkpoint at or
    /// below `header_tip`, the header chain tip that headers-first sync
    /// publishes, so that the engine downloads no body before its header is
    /// verified. The engine still claims block download as before, and waits
    /// for the headers.
    pub fn with_headers_first(
        mut self,
        header_tip: watch::Receiver<Option<block::Height>>,
    ) -> Self {
        self.supervisor.header_tip = Some(header_tip);
        self
    }

    /// Runs until shutdown. Errors only on a fatal defect (a wrong
    /// checkpoint or pin), after handing ownership back to the legacy
    /// syncer.
    pub async fn run(self) -> Result<(), color_eyre::Report> {
        self.supervisor.run().await
    }
}

/// The supervisor, generic over the peer handle so the tests can simulate
/// peers.
struct Supervisor<P, ZS, ZSTip> {
    config: EngineConfig,
    network: Network,
    anchors: Anchors,
    peers: watch::Receiver<Arc<[P]>>,
    state: ZS,
    latest_chain_tip: ZSTip,
    ownership: EngineOwnership,
    hints_gate: Option<watch::Receiver<bool>>,
    /// The known-hash list's pins, if the network has a list. Each run
    /// fetches the chunks it needs again: at most 4 of about 2 MB each.
    known_hashes: Option<Arc<dyn ChunkPins>>,
    charged: Charged,
    generation: u64,
    /// The IP addresses of the peers the last run blamed for its lack of
    /// progress: they do not count toward the next claim, and the next run
    /// does not use them, so that a few peers that keep failing the frontier
    /// cannot make the engine claim block download, and the legacy syncer
    /// cancel its downloads, over and over. Keyed by IP address rather than
    /// connection, so that reconnecting does not clear the blame.
    blamed: BTreeSet<IpAddr>,
    /// With headers-first sync, the header chain tip, see
    /// [`CheckpointSync::with_headers_first`].
    header_tip: Option<watch::Receiver<Option<block::Height>>>,
}

impl<P, ZS, ZSTip> Supervisor<P, ZS, ZSTip>
where
    P: RangePeer + HashPeer,
    ZS: tower::Service<zebra_state::Request, Response = zebra_state::Response, Error = BoxError>
        + Send
        + Clone
        + 'static,
    ZS::Future: Send,
    ZSTip: ChainTip + Clone + Send + 'static,
{
    #[allow(clippy::too_many_arguments)]
    fn new(
        config: EngineConfig,
        network: Network,
        peers: watch::Receiver<Arc<[P]>>,
        state: ZS,
        latest_chain_tip: ZSTip,
        ownership: EngineOwnership,
        hints_gate: Option<watch::Receiver<bool>>,
        known_hashes: Option<Arc<dyn ChunkPins>>,
    ) -> Self {
        Supervisor {
            config,
            anchors: Anchors::new(&network.checkpoint_list()),
            network,
            peers,
            state,
            latest_chain_tip,
            ownership,
            hints_gate,
            known_hashes,
            charged: Charged::default(),
            generation: 0,
            blamed: BTreeSet::new(),
            header_tip: None,
        }
    }

    async fn run(mut self) -> Result<(), color_eyre::Report> {
        loop {
            // The legacy syncer's `request_genesis` always runs first.
            let tip = loop {
                if let Some(tip) = self.latest_chain_tip.best_tip_height() {
                    break tip;
                }
                // Wake as soon as genesis commits: on a fast link the legacy
                // syncer can download thousands of blocks in one poll interval.
                tokio::select! {
                    changed = self.latest_chain_tip.best_tip_changed() => {
                        if changed.is_err() {
                            // The state is shutting down.
                            time::sleep(IDLE_POLL_INTERVAL).await;
                        }
                    }
                    _ = time::sleep(IDLE_POLL_INTERVAL) => {}
                }
            };

            self.catch_up_checkpoint_tip();

            let Some(target) = self.claim_target(tip) else {
                self.wait_idle().await;
                continue;
            };

            // Start fetching the known-hash chunks above the tip now, so that
            // they arrive while the syncer cancels its downloads, and the
            // run's first units can already be anchored at known hashes.
            let mut hashes = self.known_hashes.clone().map(|pins| {
                Box::new(KnownHashes::with_pins(pins, self.peers.clone())) as Box<dyn HashSource>
            });
            if let Some(hashes) = hashes.as_mut() {
                hashes.request(
                    tip.next().expect("tips are below the maximum height"),
                    target,
                );
            }

            // Claiming: publish the claim and wait for the syncer to cancel
            // its downloads.
            self.generation += 1;
            let generation = self.generation;
            self.ownership
                .owner
                .send_replace(SyncOwner::Engine { target, generation });
            info!(
                ?target,
                generation, "checkpoint sync engine claiming block download"
            );
            if self
                .ownership
                .ack
                .wait_for(|acked| *acked >= generation)
                .await
                .is_err()
            {
                // The legacy syncer is gone: Zebra is shutting down.
                self.ownership.owner.send_replace(SyncOwner::Legacy);
                return Ok(());
            }

            // Running: start at the tip the syncer left.
            let tip = self.latest_chain_tip.best_tip_height().unwrap_or(tip);
            let start = tip.next().expect("tips are below the maximum height");
            let outcome = if tip < target {
                EngineRun {
                    config: self.config.clone(),
                    network: self.network.clone(),
                    anchors: self.anchors.clone(),
                    start,
                    target,
                    peers: self.peers.clone(),
                    state: self.state.clone(),
                    // Every run is gated until the gate is seen open: a
                    // run that hands back before then must not let the
                    // legacy syncer or a later run commit.
                    hints_gate: self.hints_gate.clone(),
                    unit_lengths: self.ownership.unit_lengths.clone(),
                    charged: self.charged.clone(),
                    blamed: self.blamed.clone(),
                    hashes,
                    header_tip: self.header_tip.clone(),
                }
                .run()
                .await
            } else {
                engine::EngineOutcome {
                    exit: EngineExit::TargetReached,
                    base: start,
                    blamed: BTreeSet::new(),
                    known_hash_units: 0,
                }
            };

            if self
                .hints_gate
                .as_ref()
                .is_some_and(|gate| *gate.borrow() || gate.has_changed().is_err())
            {
                // Acquisition finished: later runs and claims are ungated.
                self.hints_gate = None;
            }

            // Draining is done: publish the durable tip, then hand back.
            //
            // The tip is read from the state, not from the engine's frontier:
            // a commit that timed out can still land after the drain. It is a
            // checkpoint unless a commit failed mid-interval. The legacy
            // syncer downloads from the state tip, so the verifier must
            // resume exactly there, not at the checkpoint below, whose
            // interval it could never complete. The legacy syncer submits
            // nothing until it sees the hand-back, so the verifier resets to
            // this tip even below its progress; a commit that lands later is
            // republished by `catch_up_checkpoint_tip`.
            let durable = self.latest_chain_tip.best_tip_height_and_hash();
            if let Some((height, hash)) = durable.filter(|(height, _)| *height > tip) {
                self.ownership
                    .checkpoint_tip
                    .send_replace(Some(CommittedTip {
                        height,
                        hash,
                        hand_back: Some(generation),
                    }));
            }
            self.ownership.owner.send_replace(SyncOwner::Legacy);
            info!(
                exit = ?outcome.exit,
                engine_base = ?outcome.base,
                durable_tip = ?durable.map(|(height, _)| height),
                known_hash_units = outcome.known_hash_units,
                "checkpoint sync engine handed block download back to the legacy syncer"
            );
            // Only the last run's blame counts: a peer blamed earlier may
            // have recovered, and the cooldown already limits how often a
            // run can fail. Blamed peers, including those that refused
            // ranges, do not count toward the next claim.
            self.blamed = outcome.blamed;

            match outcome.exit {
                EngineExit::Defect(defect) => {
                    return Err(eyre!("checkpoint sync engine stopped: {defect}"));
                }
                EngineExit::Shutdown => return Ok(()),
                EngineExit::TargetReached
                | EngineExit::TooFewPeers
                | EngineExit::NoProgress
                | EngineExit::CommitTimeout => self.cool_down().await,
            }
        }
    }

    /// Waits [`RECLAIM_COOLDOWN`] after a hand-back, keeping the checkpoint
    /// verifier caught up with the state.
    async fn cool_down(&mut self) {
        let until = time::Instant::now() + RECLAIM_COOLDOWN;
        loop {
            let now = time::Instant::now();
            if now >= until {
                break;
            }
            time::sleep((until - now).min(IDLE_POLL_INTERVAL)).await;
            self.catch_up_checkpoint_tip();
        }
    }

    /// Republishes the state tip to the checkpoint verifier if a commit
    /// landed after the last published tip, while the legacy syncer owns
    /// block download.
    ///
    /// A commit that an engine run timed out on was already queued in the
    /// state, so it can land after the hand-back. The legacy syncer then
    /// downloads from the higher state tip, and the verifier must move
    /// there too, or it waits forever for heights already in the state. The
    /// verifier applies these tips only above its progress, so the legacy
    /// syncer's own commits are never rewound.
    fn catch_up_checkpoint_tip(&self) {
        if self.generation == 0 {
            // No run yet: only the legacy syncer has committed.
            return;
        }
        let Some((height, hash)) = self.latest_chain_tip.best_tip_height_and_hash() else {
            return;
        };
        self.ownership.checkpoint_tip.send_if_modified(|published| {
            let behind = published.is_none_or(|published| height > published.height);
            if behind {
                *published = Some(CommittedTip {
                    height,
                    hash,
                    // A hand-back the verifier has not seen yet stays a
                    // hand-back: the legacy syncer has submitted nothing
                    // since it.
                    hand_back: published.and_then(|published| published.hand_back),
                });
            }
            behind
        });
    }

    /// The target to claim for a state at `tip`, if the claim conditions
    /// hold.
    fn claim_target(&self, tip: block::Height) -> Option<block::Height> {
        let peers = self.peers.borrow().clone();
        let mut tips: Vec<block::Height> = peers
            .iter()
            .filter(|peer| peer.services().contains(PeerServices::NODE_NETWORK))
            .filter(|peer| !peer.is_closed())
            .filter(|peer| !self.blamed.contains(&peer.ip()))
            .map(|peer| peer.remote_tip_height())
            .collect();
        tips.sort();

        if self.hints_gate.is_some() {
            // Claim right after genesis, whatever the peers and the gap, so
            // nothing commits before hint acquisition finishes.
            let peer_tip = tips
                .get(tips.len() / 2)
                .copied()
                .unwrap_or(block::Height::MAX);
            return self.anchors.target(tip, peer_tip);
        }

        // Only peers that reach the first interval count, as in the engine's
        // peer selection: a claim the engine would hand back for too few
        // peers would cancel the legacy syncer's downloads for nothing.
        let first_interval_end = tip
            .next()
            .ok()
            .and_then(|start| self.anchors.at_or_above(start))
            .map(|(checkpoint, _)| checkpoint)?;
        tips.retain(|peer_tip| *peer_tip >= first_interval_end);
        if tips.len() < scheduler::MIN_ENGINE_PEERS {
            return None;
        }
        // The median of those peers: the advertised tips are untrusted and
        // only lower the target.
        let target = self.anchors.target(tip, tips[tips.len() / 2])?;
        (self.anchors.intervals_between(tip, target) >= MIN_ENGINE_SPAN_INTERVALS).then_some(target)
    }

    async fn wait_idle(&mut self) {
        tokio::select! {
            changed = self.peers.changed() => {
                if changed.is_err() {
                    // The registry is gone: no peer will appear.
                    time::sleep(IDLE_POLL_INTERVAL).await;
                }
            }
            _ = time::sleep(IDLE_POLL_INTERVAL) => {}
        }
    }
}

#[cfg(test)]
mod tests;
