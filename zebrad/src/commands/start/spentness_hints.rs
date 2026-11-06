//! Startup wiring for spentness hints (`[sync] v2_spentness_hints`).
//!
//! Hints need the checkpoint sync engine: only its gate holds back block 1
//! until acquisition finishes, so [`effective_mode`] turns them off, or stops
//! Zebra with `require`, when `sync.v2_checkpoint_sync` is off.
//!
//! When hints are on, `start`:
//!
//! 1. initializes the state with [`zebra_state::init_with_spentness`];
//! 2. if the state starts empty, or a restart interrupted a hinted sync, gives
//!    the checkpoint sync engine and the legacy syncer a gate, so no block above
//!    genesis commits before the hints task finishes; otherwise it drops the
//!    state's sender, and the state syncs without hints (S:386);
//! 3. loads the verified artifact that an earlier run saved in the state's cache
//!    directory, if there is one;
//! 4. otherwise sums the known-hash `txouts` through the pinned terminal height
//!    after the network starts, then acquires and verifies the artifact from
//!    peers;
//! 5. sends the verified artifact to the state, and opens the gate;
//! 6. registers the artifact for `get-object` serving.
//!
//! The state's writer decides whether it uses the artifact: it starts hinted
//! construction on an empty state, resumes an unfinished construction after a
//! restart, and ignores the artifact otherwise (zebra-state's
//! `SpentnessWriter::new`).
//!
//! With [`Mode::Auto`], a missing pin, a failed acquisition, or no known-hash
//! output count within [`KNOWN_HASH_SUM_TIMEOUT`] opens the gate without hints,
//! and drops the state's sender, so a resumed writer fails at once instead of
//! waiting for an artifact that never comes. With [`Mode::Require`], the same
//! failures stop Zebra.
//!
//! Serving is separate from acquisition: with any mode, a node that has version 2
//! enabled serves the saved artifact for the current pin ([`spawn_serving`]).

use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use color_eyre::eyre::{eyre, Report};
use tokio::{sync::watch, task::JoinHandle};
use tracing_futures::Instrument;

use zebra_chain::{
    block::Height,
    chain_tip::ChainTip,
    parameters::{
        known_hashes::KnownHashListSpec,
        spentness_hints::{MaxCheckpoint, Mode, ParsedArtifact, VerifiedArtifact},
        Network,
    },
};
use zebra_network::sync_peer::{ObjectIndex, ObjectSource};

use crate::components::checkpoint_sync::{
    hints::{self, acquire_spentness_hints, serve_spentness_hints},
    known_hashes::KnownHashes,
};

/// How long `auto` waits for the known-hash output count, the sum of `txouts`
/// through the pinned terminal height, before it syncs without hints.
///
/// This bounds only the known-hash fetch that runs right after the network starts.
/// The artifact download that follows has its own retry and timeout policy
/// (`hints::AUTO_TIMEOUT`, longer for a large artifact up to
/// [`hints::MAX_AUTO_TIMEOUT`], and `hints::AUTO_ROUNDS`), in [`acquire_spentness_hints`].
pub(super) const KNOWN_HASH_SUM_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How long the legacy syncer waits after genesis for the hints task with `auto`,
/// before it syncs without hints.
///
/// The hints task bounds `auto` itself, with [`KNOWN_HASH_SUM_TIMEOUT`] and the
/// download timeout, which is at most [`hints::MAX_AUTO_TIMEOUT`], so this fires only
/// if that task is stuck: about 45 minutes. With `require`, the legacy syncer waits
/// as long as acquisition takes.
pub(super) const LEGACY_SYNC_HINTS_TIMEOUT: Duration = Duration::from_secs(
    KNOWN_HASH_SUM_TIMEOUT.as_secs() + hints::MAX_AUTO_TIMEOUT.as_secs() + 5 * 60,
);

/// The directory, under the state's cache directory, that holds the verified artifact.
const ARTIFACT_DIR: &str = "spentness-hints";

/// The hints mode `start` uses: `mode`, unless the checkpoint sync engine is off.
///
/// Without the engine nothing holds back block 1 until hints are acquired, so the
/// state could never adopt them: `auto` syncs without hints, and `require` fails.
pub(super) fn effective_mode(mode: Mode, checkpoint_sync: bool) -> Result<Mode, Report> {
    match (mode, checkpoint_sync) {
        (Mode::Off, _) | (_, true) => Ok(mode),
        (Mode::Auto, false) => {
            warn!(
                "sync.v2_spentness_hints is \"auto\", but sync.v2_checkpoint_sync is off, \
                 so Zebra syncs without spentness hints"
            );
            Ok(Mode::Off)
        }
        (Mode::Require, false) => Err(eyre!(
            "sync.v2_spentness_hints is \"require\", which needs sync.v2_checkpoint_sync"
        )),
    }
}

/// The state side of spentness hints, created before the state.
pub(super) struct StateHints {
    mode: Mode,
    /// Delivers the verified artifact to the state's finalized writer.
    artifact: watch::Sender<Option<Arc<VerifiedArtifact>>>,
}

impl StateHints {
    /// Returns the sender half and the state's receiver when `mode` uses hints.
    pub(super) fn new(mode: Mode) -> Option<(StateHints, zebra_state::SpentnessHintsReceiver)> {
        if mode == Mode::Off {
            return None;
        }

        let (artifact, receiver) = watch::channel(None);
        Some((StateHints { mode, artifact }, receiver))
    }

    /// Returns the node side of hints for the state that `latest_chain_tip` tracks,
    /// or `None` if the state cannot use them.
    ///
    /// A state uses hints when it is empty (it has no block, or only genesis), or
    /// when `incomplete`: a restart interrupted its hinted sync
    /// (`ReadStateService::has_incomplete_spentness_hints`). Any other state
    /// syncs without hints, whatever the mode (S:386), so its sender is dropped.
    pub(super) fn for_state(
        self,
        latest_chain_tip: &impl ChainTip,
        incomplete: bool,
    ) -> Option<NodeHints> {
        let tip = latest_chain_tip.best_tip_height();
        let empty_state = tip.is_none_or(|tip| tip <= Height(0));
        if !empty_state && !incomplete {
            info!(
                ?tip,
                "the state is not empty, so Zebra syncs it without spentness hints",
            );
            return None;
        }

        let (gate, gate_receiver) = watch::channel(false);
        Some(NodeHints {
            mode: self.mode,
            artifact: self.artifact,
            gate,
            gate_receiver,
        })
    }
}

/// Spentness hints for a node whose state can use them.
pub(super) struct NodeHints {
    mode: Mode,
    artifact: watch::Sender<Option<Arc<VerifiedArtifact>>>,
    /// Opened when the hints task finishes, with or without an artifact.
    gate: watch::Sender<bool>,
    gate_receiver: watch::Receiver<bool>,
}

impl NodeHints {
    /// The gate for `CheckpointSync::new`: the engine commits nothing above genesis
    /// until it is `true`.
    pub(super) fn gate(&self) -> watch::Receiver<bool> {
        self.gate_receiver.clone()
    }

    /// The gate for `ChainSync::with_hints_gate`, and how long the legacy syncer
    /// waits for it: [`LEGACY_SYNC_HINTS_TIMEOUT`] with `auto`, and as long as
    /// acquisition takes with `require`.
    ///
    /// The engine claims block download only after genesis, so without this gate
    /// the legacy syncer could commit block 1 before the claim, and the state
    /// would sync without hints.
    pub(super) fn legacy_gate(&self) -> (watch::Receiver<bool>, Option<Duration>) {
        let timeout = (self.mode == Mode::Auto).then_some(LEGACY_SYNC_HINTS_TIMEOUT);
        (self.gate_receiver.clone(), timeout)
    }

    /// Spawns the hints task, which ends after the artifact is sent to the state and
    /// registered for serving through `objects`, or after Zebra falls back to syncing
    /// without hints.
    ///
    /// The task's error is fatal: it only fails with `require`.
    pub(super) fn spawn(
        self,
        network: Network,
        peers: zebra_network::SyncPeers,
        state_cache_dir: PathBuf,
        objects: Option<watch::Sender<Arc<ObjectIndex>>>,
    ) -> JoinHandle<Result<(), Report>> {
        tokio::spawn(
            self.run(
                network,
                MaxCheckpoint::for_network,
                peers,
                state_cache_dir.join(ARTIFACT_DIR),
                objects,
            )
            .in_current_span(),
        )
    }

    /// Runs the hints task, with `pin_for` giving the pin of the saved artifact.
    async fn run(
        self,
        network: Network,
        pin_for: fn(&Network) -> Option<MaxCheckpoint>,
        peers: zebra_network::SyncPeers,
        artifact_dir: PathBuf,
        objects: Option<watch::Sender<Arc<ObjectIndex>>>,
    ) -> Result<(), Report> {
        let mode = self.mode;

        let saved = load_saved(&network, pin_for, &artifact_dir)
            .await
            .unwrap_or_else(|error| {
                warn!(?error, "not using the saved spentness-hint artifact");
                None
            });

        let artifact = match saved {
            Some(artifact) => Some(artifact),
            None => match (
                acquire(&network, mode, peers, artifact_dir.clone()).await,
                mode,
            ) {
                (Ok(artifact), _) => artifact,
                (Err(error), Mode::Require) => {
                    return Err(error.wrap_err(
                        "spentness hints are required, but no verified artifact was acquired",
                    ));
                }
                (Err(error), _) => {
                    warn!(?error, "syncing without spentness hints");
                    None
                }
            },
        };

        if let Some(artifact) = &artifact {
            info!(
                height = ?artifact.header().terminal_height,
                outputs = artifact.header().output_count,
                "offering spentness hints below the final checkpoint to the state"
            );
            // The state drops its receiver only on shutdown.
            let _ = self.artifact.send(Some(Arc::new(artifact.clone())));
        }

        // Send the artifact before opening the gate, so the state's writer has it
        // before the engine submits the first block above the state's tip. The
        // engine treats a dropped gate as open. The state's sender is dropped
        // here: the state keeps an artifact that was sent, and without one a
        // resumed writer fails at once instead of waiting for it.
        let NodeHints {
            artifact: sender,
            gate,
            ..
        } = self;
        drop(sender);
        let _ = gate.send(true);

        if let (Some(artifact), Some(objects)) = (artifact, objects) {
            serve(artifact, artifact_dir, objects).await;
        }

        Ok(())
    }
}

/// Creates the served-object index for `zebra_network::Config::object_source`, when
/// version 2 is enabled. A version 2 node with an object source advertises
/// `NODE_SYNC_ARTIFACTS`, so its peers ask it for the artifact.
pub(super) fn object_source(
    network_config: &zebra_network::Config,
) -> Option<(watch::Sender<Arc<ObjectIndex>>, ObjectSource)> {
    network_config
        .v2_listen_addr
        .map(|_| ObjectSource::channel(Arc::new(ObjectIndex::new())))
}

/// Spawns a task that serves the saved artifact for the current pin through
/// `objects`, if there is one, for a node without a hints task.
///
/// Together with the hints task, this serves the artifact with any mode and any
/// state height.
pub(super) fn spawn_serving(
    network: Network,
    state_cache_dir: PathBuf,
    objects: watch::Sender<Arc<ObjectIndex>>,
) -> JoinHandle<()> {
    let artifact_dir = state_cache_dir.join(ARTIFACT_DIR);
    tokio::spawn(
        async move {
            match load_saved(&network, MaxCheckpoint::for_network, &artifact_dir).await {
                Ok(Some(artifact)) => serve(artifact, artifact_dir, objects).await,
                Ok(None) => {}
                Err(error) => warn!(?error, "not serving the saved spentness-hint artifact"),
            }
        }
        .in_current_span(),
    )
}

/// Registers `artifact` for `get-object` serving.
async fn serve(
    artifact: VerifiedArtifact,
    artifact_dir: PathBuf,
    objects: watch::Sender<Arc<ObjectIndex>>,
) {
    // Serving writes the file if it is missing, which is blocking I/O.
    let served = tokio::task::spawn_blocking(move || {
        serve_spentness_hints(&artifact, artifact_dir, &objects)
    })
    .await
    .expect("artifact serving does not panic");

    if let Err(error) = served {
        warn!(?error, "not serving the spentness-hint artifact");
    }
}

/// Loads and verifies the artifact for the current pin from `artifact_dir`, if an
/// earlier run saved it there, with [`hints::load_saved_artifact`], which also
/// deletes the files of earlier pins and a file that does not hash to the pin.
///
/// Returns `None` without looking up the pin when the directory is missing or
/// empty, and `None` when the network has no pin or the pin's file is missing.
async fn load_saved(
    network: &Network,
    pin_for: fn(&Network) -> Option<MaxCheckpoint>,
    artifact_dir: &Path,
) -> Result<Option<VerifiedArtifact>, Report> {
    let network = network.clone();
    let artifact_dir = artifact_dir.to_owned();

    // Reading and hashing an artifact of up to 512 MiB is blocking work.
    tokio::task::spawn_blocking(move || {
        let has_files = match std::fs::read_dir(&artifact_dir) {
            Ok(mut entries) => entries.next().is_some(),
            Err(error) if error.kind() == ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        if !has_files {
            return Ok(None);
        }

        let Some(pin) = pin_for(&network) else {
            return Ok(None);
        };

        // Acquisition knows the artifact's length from the known-hash lists, but
        // the pinned hash alone identifies the file.
        let Some(bytes) = hints::load_saved_artifact(&artifact_dir, pin.object_hash(), None)?
        else {
            return Ok(None);
        };

        // The pinned hash covers the whole artifact, so it also covers the bit
        // count that acquisition checked against the known-hash lists.
        let artifact = ParsedArtifact::parse(bytes)?.verify(&network, &pin)?;
        info!(?artifact_dir, "loaded the saved spentness-hint artifact");

        Ok(Some(artifact))
    })
    .await
    .expect("loading the artifact does not panic")
}

/// Acquires and verifies the network's pinned artifact.
async fn acquire(
    network: &Network,
    mode: Mode,
    peers: zebra_network::SyncPeers,
    artifact_dir: PathBuf,
) -> Result<Option<VerifiedArtifact>, Report> {
    let pin = MaxCheckpoint::for_network(network)
        .ok_or_else(|| eyre!("this release pins no spentness hints for {network}"))?;
    let spec = KnownHashListSpec::for_network(network)
        .ok_or_else(|| eyre!("this release pins no known-hash list for {network}"))?;

    let mut known_hashes = KnownHashes::new(spec, network.clone(), peers.clone());
    let output_count = known_hashes.cumulative_txouts(pin.height);
    let expected_output_count = match mode {
        Mode::Auto => tokio::time::timeout(KNOWN_HASH_SUM_TIMEOUT, output_count)
            .await
            .map_err(|_| eyre!("no known-hash output count within {KNOWN_HASH_SUM_TIMEOUT:?}"))?,
        Mode::Require | Mode::Off => output_count.await,
    }
    .map_err(|error| eyre!(error).wrap_err("summing known-hash output counts"))?;

    Ok(acquire_spentness_hints(
        network.clone(),
        mode,
        peers,
        expected_output_count,
        artifact_dir,
    )
    .await?)
}

#[cfg(test)]
mod tests;
