//! The decision rule (design, §6): whether `zebrad start` bootstraps from a
//! state snapshot before it initializes the state.
//!
//! Every condition must hold, and the first that fails is logged once at
//! info, so an operator who enabled snapshot sync can see why it did not run.

use std::fmt;

use zebra_chain::{
    parameters::{Network, NetworkUpgrade},
    state_snapshot::StateSnapshotCommitment,
};
use zebra_state::state_snapshot::store::ArtifactStore;

use crate::config::ZebradConfig;

#[cfg(test)]
mod tests;

/// Whether this start bootstraps from a state snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Decision {
    /// Bootstrap from `commitment`, downloading from `peers`: the configured
    /// `[sync] state_snapshot_peers`, or `[network] initial_v2_peers` when
    /// they are empty.
    Bootstrap {
        /// The commitment the snapshot is verified against.
        commitment: StateSnapshotCommitment,
        /// The peers to download from, as `host:port` strings.
        peers: Vec<String>,
    },

    /// Start normally, because this condition of the decision rule failed.
    Skip(SkipReason),
}

/// A condition of the decision rule that did not hold.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkipReason {
    /// `[sync] state_snapshot_sync` is off.
    Disabled,

    /// `[state] prune_blocks_older_than` is not set: an archive node syncs
    /// every block.
    NotPruned,

    /// `[state] ephemeral` is set.
    Ephemeral,

    /// A current-major database directory exists, or a previous-major one can
    /// be restored.
    ExistingState,

    /// No commitment is pinned for the network or configured.
    NoCommitment,

    /// The configured commitment is not allowed on this network (design, §2):
    /// it differs from the pinned one, and `[sync] unsafe_trust_state_snapshot`
    /// is off.
    CommitmentNotAllowed,

    /// A precondition of the trust model (design, §2) does not hold.
    PreconditionFailed(Precondition),

    /// Neither `[sync] state_snapshot_peers` nor `[network] initial_v2_peers`
    /// names a peer.
    NoPeers,

    /// zcashd-compat mode is on, which needs an archive node.
    ZcashdCompat,

    /// The commitment's manifest is marked as rejected in the artifact store.
    Rejected,
}

/// The preconditions of the trust model (design, §2), which the decision rule
/// and the importer both check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Precondition {
    /// `[consensus] checkpoint_sync` is on.
    CheckpointSync,

    /// NU5 is active at the snapshot height.
    Nu5Active,

    /// The block after the snapshot height is at or below the last checkpoint.
    BelowLastCheckpoint,

    /// The history tree at the snapshot height is not empty.
    HistoryTreeNonEmpty,
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SkipReason::Disabled => "sync.state_snapshot_sync is off",
            SkipReason::NotPruned => "state.prune_blocks_older_than is not set",
            SkipReason::Ephemeral => "state.ephemeral is set",
            SkipReason::ExistingState => "a state database already exists",
            SkipReason::NoCommitment => "no state snapshot commitment is pinned or configured",
            SkipReason::CommitmentNotAllowed => {
                "the configured state snapshot is not the pinned one, and \
                 sync.unsafe_trust_state_snapshot is off"
            }
            SkipReason::PreconditionFailed(Precondition::CheckpointSync) => {
                "consensus.checkpoint_sync is off"
            }
            SkipReason::PreconditionFailed(Precondition::Nu5Active) => {
                "NU5 is not active at the snapshot height"
            }
            SkipReason::PreconditionFailed(Precondition::BelowLastCheckpoint) => {
                "the block after the snapshot height is above the last checkpoint"
            }
            SkipReason::PreconditionFailed(Precondition::HistoryTreeNonEmpty) => {
                "the history tree is empty at the snapshot height"
            }
            SkipReason::NoPeers => {
                "neither sync.state_snapshot_peers nor network.initial_v2_peers names a peer"
            }
            SkipReason::ZcashdCompat => "zcashd-compat mode is on",
            SkipReason::Rejected => "the snapshot was rejected by an earlier bootstrap",
        })
    }
}

/// Applies the decision rule to `config`, consulting `store` for rejected
/// manifests.
///
/// The first condition that fails is logged at info, except for snapshot
/// sync being off, which is logged at debug because it is the default. The
/// `[sync] unsafe_trust_state_snapshot` warning fires at every start the flag
/// is on, whatever the decision (design, §2).
pub fn should_bootstrap(config: &ZebradConfig, store: &ArtifactStore) -> Decision {
    if config.sync.unsafe_trust_state_snapshot {
        warn!(
            "sync.unsafe_trust_state_snapshot is on: a configured state snapshot is trusted on \
             Mainnet and the default Testnet even when it differs from the commitment this \
             build pins, so its producer is trusted with this node's view of the chain state"
        );
    }

    let decision = decide(config, store);

    match &decision {
        Decision::Skip(reason @ SkipReason::Disabled) => {
            debug!(%reason, "not bootstrapping from a state snapshot");
        }
        Decision::Skip(reason) => {
            info!(%reason, "not bootstrapping from a state snapshot");
        }
        Decision::Bootstrap { .. } => {}
    }

    decision
}

/// Evaluates the conditions of the decision rule in the design's order and
/// returns the first that fails.
fn decide(config: &ZebradConfig, store: &ArtifactStore) -> Decision {
    let network = &config.network.network;

    if !config.sync.state_snapshot_sync {
        return Decision::Skip(SkipReason::Disabled);
    }

    if config.state.prune_blocks_older_than.is_none() {
        return Decision::Skip(SkipReason::NotPruned);
    }

    if config.state.ephemeral {
        return Decision::Skip(SkipReason::Ephemeral);
    }

    if let Some(existing) = existing_state_database(&config.state, network) {
        info!(
            path = %existing.display(),
            "a state database exists, so the state snapshot bootstrap does not run; \
             delete it to bootstrap from a snapshot"
        );
        return Decision::Skip(SkipReason::ExistingState);
    }

    let commitment = match select_commitment(&config.sync, network) {
        Ok(Some(commitment)) => commitment,
        Ok(None) => return Decision::Skip(SkipReason::NoCommitment),
        Err(reason) => return Decision::Skip(reason),
    };

    if let Err(precondition) = check_preconditions(config, network, &commitment) {
        return Decision::Skip(SkipReason::PreconditionFailed(precondition));
    }

    let peers = bootstrap_peers(config);
    if peers.is_empty() {
        return Decision::Skip(SkipReason::NoPeers);
    }

    if config.zcashd_compat.enabled {
        return Decision::Skip(SkipReason::ZcashdCompat);
    }

    if store.is_rejected(commitment.manifest) {
        return Decision::Skip(SkipReason::Rejected);
    }

    Decision::Bootstrap { commitment, peers }
}

/// Returns the path of a state database the bootstrap must not overwrite: the
/// running major version's directory, or the previous major version's when
/// the running version restores it (design, §6).
///
/// Existence is probed by path, never by opening the database, because opening
/// a missing database creates it. A directory that exists but holds no
/// database also counts: the import renames a new database into that path,
/// and an operator who left one behind deletes it to opt in.
fn existing_state_database(
    config: &zebra_state::Config,
    network: &Network,
) -> Option<std::path::PathBuf> {
    let running_major = zebra_state::state_database_format_version_in_code().major;

    let mut majors = vec![running_major];
    if zebra_state::restorable_db_versions().contains(&running_major) {
        majors.push(running_major.saturating_sub(1));
    }

    majors.into_iter().find_map(|major| {
        let on_disk = zebra_state::database_format_version_on_disk(
            config,
            zebra_state::constants::STATE_DATABASE_KIND,
            major,
            network,
        );
        match on_disk {
            // A readable version file or a database directory.
            Ok(Some(_)) => {}
            // No database at this major version.
            Ok(None) => return None,
            // An unreadable version file: something is there, so leave it alone.
            Err(error) => {
                warn!(?error, major, "could not read the state database version");
            }
        }
        Some(config.db_path(zebra_state::constants::STATE_DATABASE_KIND, major, network))
    })
}

/// Returns the commitment to trust (design, §2): the configured one where it
/// is allowed, otherwise the pinned one.
///
/// A configured commitment is allowed on Regtest and custom testnets, and on
/// Mainnet and the default Testnet when it equals the pinned one or
/// `unsafe_trust_state_snapshot` is on. Otherwise the pinned commitment wins,
/// and if there is none, the configured one is not allowed.
fn select_commitment(
    sync: &crate::components::sync::Config,
    network: &Network,
) -> Result<Option<StateSnapshotCommitment>, SkipReason> {
    let pinned = network.state_snapshot_commitment();

    let Some(configured) = sync.state_snapshot else {
        return Ok(pinned);
    };

    let has_pinned_trust = matches!(network, Network::Mainnet) || network.is_default_testnet();

    if !has_pinned_trust || pinned == Some(configured) {
        return Ok(Some(configured));
    }

    if sync.unsafe_trust_state_snapshot {
        warn!(
            height = configured.height.0,
            hash = %configured.block_hash,
            manifest = %configured.manifest,
            "trusting the configured state snapshot, which is not the one this build pins, \
             because sync.unsafe_trust_state_snapshot is on"
        );
        return Ok(Some(configured));
    }

    match pinned {
        Some(pinned) => {
            warn!(
                configured_height = configured.height.0,
                configured_manifest = %configured.manifest,
                pinned_height = pinned.height.0,
                pinned_manifest = %pinned.manifest,
                "ignoring the configured state snapshot on a network with a pinned commitment: \
                 set sync.unsafe_trust_state_snapshot to use it"
            );
            Ok(Some(pinned))
        }
        None => Err(SkipReason::CommitmentNotAllowed),
    }
}

/// Checks the preconditions of the trust model (design, §2) that hold or fail
/// on the configuration alone.
///
/// The history tree at `H` is non-empty exactly when Heartwood is active at
/// `H`, which NU5 activation implies; the importer checks the shipped tree
/// itself.
fn check_preconditions(
    config: &ZebradConfig,
    network: &Network,
    commitment: &StateSnapshotCommitment,
) -> Result<(), Precondition> {
    if !config.consensus.checkpoint_sync {
        return Err(Precondition::CheckpointSync);
    }

    let height = commitment.height;

    let active_at = |upgrade: NetworkUpgrade| {
        upgrade
            .activation_height(network)
            .is_some_and(|activation| activation <= height)
    };

    if !active_at(NetworkUpgrade::Nu5) {
        return Err(Precondition::Nu5Active);
    }

    let (_, max_checkpoint_height) =
        zebra_consensus::router::init_checkpoint_list(config.consensus.clone(), network);
    let successor = (height + 1).ok_or(Precondition::BelowLastCheckpoint)?;
    if successor > max_checkpoint_height {
        return Err(Precondition::BelowLastCheckpoint);
    }

    if !active_at(NetworkUpgrade::Heartwood) {
        return Err(Precondition::HistoryTreeNonEmpty);
    }

    Ok(())
}

/// Returns the bootstrap peers: `[sync] state_snapshot_peers`, or
/// `[network] initial_v2_peers` when they are empty.
fn bootstrap_peers(config: &ZebradConfig) -> Vec<String> {
    if !config.sync.state_snapshot_peers.is_empty() {
        config.sync.state_snapshot_peers.clone()
    } else {
        config.network.initial_v2_peers.iter().cloned().collect()
    }
}
