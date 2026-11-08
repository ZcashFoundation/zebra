//! Fixed test vectors for the decision rule: every precondition is broken one
//! at a time from a configuration that bootstraps.

use std::path::Path;

use tempfile::TempDir;

use zebra_chain::{
    block::{Hash, Height},
    parameters::{
        testnet::{ConfiguredActivationHeights, ConfiguredCheckpoints, RegtestParameters},
        Network,
    },
    state_snapshot::{ObjectHash, StateSnapshotCommitment},
};
use zebra_state::state_snapshot::store::ArtifactStore;

use crate::{
    components::state_snapshot::decision::{should_bootstrap, Decision, Precondition, SkipReason},
    config::ZebradConfig,
};

/// The snapshot height of the test commitment.
const HEIGHT: u32 = 10;

/// The last checkpoint of the test network, above `HEIGHT + 1`.
const LAST_CHECKPOINT: u32 = 20;

/// The commitment every test configures.
fn commitment() -> StateSnapshotCommitment {
    StateSnapshotCommitment {
        height: Height(HEIGHT),
        block_hash: Hash([0xAA; 32]),
        manifest: ObjectHash([0xBB; 32]),
    }
}

/// A Regtest network with NU5 active from height 1 and a checkpoint at
/// `last_checkpoint`, so the preconditions hold for `HEIGHT`.
fn regtest(nu5: Option<u32>, last_checkpoint: Option<u32>) -> Network {
    let genesis_hash = Network::new_regtest(Default::default()).genesis_hash();
    let mut checkpoints = vec![(Height(0), genesis_hash)];
    if let Some(last_checkpoint) = last_checkpoint {
        checkpoints.push((Height(last_checkpoint), Hash([0x11; 32])));
    }

    Network::new_regtest(RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            nu5,
            ..Default::default()
        },
        checkpoints: Some(ConfiguredCheckpoints::HeightsAndHashes(checkpoints)),
        ..Default::default()
    })
}

/// A configuration that passes every condition on `network`, with its state
/// under `cache_dir`.
fn bootstrapping_config(network: Network, cache_dir: &Path) -> ZebradConfig {
    let mut config = ZebradConfig::default();
    config.network.network = network;
    config.state.cache_dir = cache_dir.to_path_buf();
    config.state.ephemeral = false;
    config.state.prune_blocks_older_than = Some(zebra_state::MIN_PRUNING_RETENTION);
    config.consensus.checkpoint_sync = true;
    config.zcashd_compat.enabled = false;
    config.sync.state_snapshot_sync = true;
    config.sync.state_snapshot = Some(commitment());
    config.sync.state_snapshot_peers = vec!["127.0.0.1:18233".to_string()];
    config
}

/// The store and the decision for `config`, with no rejection marks.
fn decide(config: &ZebradConfig, cache_dir: &Path) -> Decision {
    let store = ArtifactStore::at(cache_dir.join("store"));
    should_bootstrap(config, &store)
}

fn skip(reason: SkipReason) -> Decision {
    Decision::Skip(reason)
}

/// A configuration that satisfies every condition bootstraps from the
/// configured commitment and peers.
#[test]
fn bootstraps_when_every_condition_holds() {
    let _init_guard = zebra_test::init();
    let dir = TempDir::new().unwrap();

    let config = bootstrapping_config(regtest(Some(1), Some(LAST_CHECKPOINT)), dir.path());

    assert_eq!(
        decide(&config, dir.path()),
        Decision::Bootstrap {
            commitment: commitment(),
            peers: vec!["127.0.0.1:18233".to_string()],
        }
    );
}

/// Each condition, broken on its own, skips the bootstrap with its reason.
#[test]
fn each_failing_condition_skips_with_its_reason() {
    let _init_guard = zebra_test::init();
    let dir = TempDir::new().unwrap();
    let network = regtest(Some(1), Some(LAST_CHECKPOINT));

    let cases: Vec<(&str, Box<dyn Fn(&mut ZebradConfig)>, SkipReason)> = vec![
        (
            "disabled",
            Box::new(|config| config.sync.state_snapshot_sync = false),
            SkipReason::Disabled,
        ),
        (
            "not pruned",
            Box::new(|config| config.state.prune_blocks_older_than = None),
            SkipReason::NotPruned,
        ),
        (
            "ephemeral",
            Box::new(|config| config.state.ephemeral = true),
            SkipReason::Ephemeral,
        ),
        (
            "no commitment",
            Box::new(|config| config.sync.state_snapshot = None),
            SkipReason::NoCommitment,
        ),
        (
            "checkpoint sync off",
            Box::new(|config| config.consensus.checkpoint_sync = false),
            SkipReason::PreconditionFailed(Precondition::CheckpointSync),
        ),
        (
            "no peers",
            Box::new(|config| {
                config.sync.state_snapshot_peers.clear();
                config.network.initial_v2_peers.clear();
            }),
            SkipReason::NoPeers,
        ),
        (
            "zcashd-compat",
            Box::new(|config| config.zcashd_compat.enabled = true),
            SkipReason::ZcashdCompat,
        ),
    ];

    for (name, break_condition, reason) in cases {
        let mut config = bootstrapping_config(network.clone(), dir.path());
        break_condition(&mut config);
        assert_eq!(decide(&config, dir.path()), skip(reason), "{name}");
    }
}

/// A current-major database directory, even an empty one, skips the bootstrap.
#[test]
fn existing_state_database_skips() {
    let _init_guard = zebra_test::init();
    let dir = TempDir::new().unwrap();
    let network = regtest(Some(1), Some(LAST_CHECKPOINT));
    let config = bootstrapping_config(network.clone(), dir.path());

    let running_major = zebra_state::state_database_format_version_in_code().major;
    let current = config.state.db_path("state", running_major, &network);
    std::fs::create_dir_all(&current).unwrap();
    assert_eq!(decide(&config, dir.path()), skip(SkipReason::ExistingState));
    std::fs::remove_dir_all(&current).unwrap();
    assert!(matches!(
        decide(&config, dir.path()),
        Decision::Bootstrap { .. }
    ));
}

/// Every registered source directory is protected, preferring v32, and
/// including sources older than the major immediately before it.
#[test]
fn restorable_state_databases_skip() {
    let _init_guard = zebra_test::init();
    let dir = TempDir::new().unwrap();
    let network = regtest(Some(1), Some(LAST_CHECKPOINT));
    let config = bootstrapping_config(network.clone(), dir.path());

    let running_major = zebra_state::state_database_format_version_in_code().major;
    assert_eq!(running_major, 33);
    let sources = zebra_state::restorable_db_source_versions(running_major);
    assert_eq!(sources.first(), Some(&32));
    assert!(sources.contains(&31));
    assert!(sources.contains(&30));
    assert!(sources.contains(&29));

    for source_major in [32, 31, 30, 29] {
        let source = config.state.db_path("state", source_major, &network);
        std::fs::create_dir_all(&source).unwrap();
        assert_eq!(
            decide(&config, dir.path()),
            skip(SkipReason::ExistingState),
            "v{source_major} must be found when v{running_major} is current"
        );
        std::fs::remove_dir_all(&source).unwrap();
        assert!(matches!(
            decide(&config, dir.path()),
            Decision::Bootstrap { .. }
        ));
    }
}

/// On Mainnet, a pruned node with no configured commitment bootstraps from the
/// pinned one.
#[test]
fn mainnet_bootstraps_from_the_pinned_commitment() {
    let _init_guard = zebra_test::init();
    let dir = TempDir::new().unwrap();

    let pinned = Network::Mainnet
        .state_snapshot_commitment()
        .expect("Mainnet pins a state snapshot commitment");

    let mut config = bootstrapping_config(Network::Mainnet, dir.path());
    config.sync.state_snapshot = None;
    assert_eq!(
        decide(&config, dir.path()),
        Decision::Bootstrap {
            commitment: pinned,
            peers: vec!["127.0.0.1:18233".to_string()],
        }
    );

    // Configuring the pinned commitment changes nothing.
    config.sync.state_snapshot = Some(pinned);
    assert_eq!(
        decide(&config, dir.path()),
        Decision::Bootstrap {
            commitment: pinned,
            peers: vec!["127.0.0.1:18233".to_string()],
        }
    );
}

/// On Mainnet, a configured commitment that is not the pinned one is ignored
/// for the pinned one, unless `unsafe_trust_state_snapshot` is on; with it, the
/// configured one is used.
#[test]
fn configured_commitment_on_mainnet_needs_unsafe_trust() {
    let _init_guard = zebra_test::init();
    let dir = TempDir::new().unwrap();

    let pinned = Network::Mainnet
        .state_snapshot_commitment()
        .expect("Mainnet pins a state snapshot commitment");

    // NU5 is active at this height, and the block after it is below the last checkpoint.
    let mainnet_commitment = StateSnapshotCommitment {
        height: Height(2_000_000),
        ..commitment()
    };
    assert_ne!(mainnet_commitment, pinned);

    let mut config = bootstrapping_config(Network::Mainnet, dir.path());
    config.sync.state_snapshot = Some(mainnet_commitment);
    assert_eq!(
        decide(&config, dir.path()),
        Decision::Bootstrap {
            commitment: pinned,
            peers: vec!["127.0.0.1:18233".to_string()],
        }
    );

    config.sync.unsafe_trust_state_snapshot = true;
    assert_eq!(
        decide(&config, dir.path()),
        Decision::Bootstrap {
            commitment: mainnet_commitment,
            peers: vec!["127.0.0.1:18233".to_string()],
        }
    );
}

/// The chain preconditions: NU5 must be active at the snapshot height, and
/// the block after it must be at or below the last checkpoint.
#[test]
fn chain_preconditions_skip() {
    let _init_guard = zebra_test::init();
    let dir = TempDir::new().unwrap();

    // Regtest without a configured NU5 height never activates NU5.
    let config = bootstrapping_config(regtest(None, Some(LAST_CHECKPOINT)), dir.path());
    assert_eq!(
        decide(&config, dir.path()),
        skip(SkipReason::PreconditionFailed(Precondition::Nu5Active))
    );

    // Only the genesis checkpoint: `HEIGHT + 1` is above it.
    let config = bootstrapping_config(regtest(Some(1), None), dir.path());
    assert_eq!(
        decide(&config, dir.path()),
        skip(SkipReason::PreconditionFailed(
            Precondition::BelowLastCheckpoint
        ))
    );

    // A checkpoint exactly at `HEIGHT + 1` is enough.
    let config = bootstrapping_config(regtest(Some(1), Some(HEIGHT + 1)), dir.path());
    assert!(matches!(
        decide(&config, dir.path()),
        Decision::Bootstrap { .. }
    ));

    // A checkpoint at `HEIGHT` is not: the successor is above it.
    let config = bootstrapping_config(regtest(Some(1), Some(HEIGHT)), dir.path());
    assert_eq!(
        decide(&config, dir.path()),
        skip(SkipReason::PreconditionFailed(
            Precondition::BelowLastCheckpoint
        ))
    );

    // The maximum height has no successor.
    let mut config = bootstrapping_config(regtest(Some(1), Some(LAST_CHECKPOINT)), dir.path());
    config.sync.state_snapshot = Some(StateSnapshotCommitment {
        height: Height::MAX,
        ..commitment()
    });
    assert_eq!(
        decide(&config, dir.path()),
        skip(SkipReason::PreconditionFailed(
            Precondition::BelowLastCheckpoint
        ))
    );
}

/// The version 2 peers aren't dialled for the bootstrap: the network connects to them, and the
/// bootstrap uses those connections.
#[test]
fn initial_v2_peers_are_not_dialled() {
    let _init_guard = zebra_test::init();
    let dir = TempDir::new().unwrap();

    let mut config = bootstrapping_config(regtest(Some(1), Some(LAST_CHECKPOINT)), dir.path());
    config.sync.state_snapshot_peers.clear();
    config.network.initial_v2_peers = ["seed.example:18233".to_string()].into_iter().collect();
    assert_eq!(
        decide(&config, dir.path()),
        skip(SkipReason::NoPeers),
        "without version 2, the connections can't serve the snapshot"
    );

    config.network.v2_listen_addr = Some("127.0.0.1:0".parse().unwrap());
    assert_eq!(
        decide(&config, dir.path()),
        Decision::Bootstrap {
            commitment: commitment(),
            peers: Vec::new(),
        }
    );
}

/// Without configured peers, a node with version 2 on bootstraps from the peers it discovers.
#[test]
fn discovers_peers_without_configured_ones() {
    let _init_guard = zebra_test::init();
    let dir = TempDir::new().unwrap();

    let mut config = bootstrapping_config(regtest(Some(1), Some(LAST_CHECKPOINT)), dir.path());
    config.sync.state_snapshot_peers.clear();
    config.network.initial_v2_peers.clear();
    config.network.v2_listen_addr = Some("127.0.0.1:0".parse().unwrap());

    assert_eq!(
        decide(&config, dir.path()),
        Decision::Bootstrap {
            commitment: commitment(),
            peers: Vec::new(),
        }
    );
}

/// A state that holds only the headers an earlier start synced for the bootstrap is bootstrapped
/// over, but not once the marker is gone.
#[test]
fn header_only_state_bootstraps_again() {
    use crate::components::state_snapshot::decision::header_sync_marker;

    let _init_guard = zebra_test::init();
    let dir = TempDir::new().unwrap();
    let network = regtest(Some(1), Some(LAST_CHECKPOINT));
    let config = bootstrapping_config(network.clone(), dir.path());

    let running_major = zebra_state::state_database_format_version_in_code().major;
    std::fs::create_dir_all(config.state.db_path("state", running_major, &network)).unwrap();
    let marker = header_sync_marker(&config.state, &network);
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, []).unwrap();
    assert!(matches!(
        decide(&config, dir.path()),
        Decision::Bootstrap { .. }
    ));

    std::fs::remove_file(&marker).unwrap();
    assert_eq!(decide(&config, dir.path()), skip(SkipReason::ExistingState));
}

/// A manifest marked as rejected in the store is not downloaded again.
#[test]
fn rejected_manifest_skips() {
    let _init_guard = zebra_test::init();
    let dir = TempDir::new().unwrap();

    let config = bootstrapping_config(regtest(Some(1), Some(LAST_CHECKPOINT)), dir.path());
    let store = ArtifactStore::at(dir.path().join("store"));

    let mark = store.rejected_path(commitment().manifest);
    std::fs::create_dir_all(mark.parent().unwrap()).unwrap();
    std::fs::write(&mark, b"").unwrap();

    assert_eq!(
        should_bootstrap(&config, &store),
        skip(SkipReason::Rejected)
    );
}

/// Every skip reason has a one-sentence explanation.
#[test]
fn skip_reasons_display_one_sentence() {
    let _init_guard = zebra_test::init();

    let reasons = [
        SkipReason::Disabled,
        SkipReason::NotPruned,
        SkipReason::Ephemeral,
        SkipReason::ExistingState,
        SkipReason::NoCommitment,
        SkipReason::CommitmentNotAllowed,
        SkipReason::PreconditionFailed(Precondition::CheckpointSync),
        SkipReason::PreconditionFailed(Precondition::Nu5Active),
        SkipReason::PreconditionFailed(Precondition::BelowLastCheckpoint),
        SkipReason::PreconditionFailed(Precondition::HistoryTreeNonEmpty),
        SkipReason::NoPeers,
        SkipReason::ZcashdCompat,
        SkipReason::Rejected,
    ];
    let texts: std::collections::HashSet<String> =
        reasons.iter().map(ToString::to_string).collect();
    assert_eq!(texts.len(), reasons.len());
    for text in texts {
        assert!(!text.is_empty() && !text.contains('\n'), "{text:?}");
    }
}
