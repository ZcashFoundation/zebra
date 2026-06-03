//! Tests for the offline finalized-state rollback utility.
//!
//! The core test is a round-trip equivalence check: sync a finalized state to height `N`, roll it
//! back to `M`, and assert it is *semantically* equivalent to a state freshly synced to `M`. We
//! compare via queries (tip, value pool, note-commitment/history roots, transparent balances and
//! UTXOs, nullifier presence) rather than raw bytes. Reopening each database also runs the
//! de-duplicate-tree format check, which guards against rollback leaving a duplicate sapling or
//! orchard tree entry (it would brick the node on the next startup).

use std::{env, path::Path};

use tempfile::TempDir;

use zebra_chain::{block::Height, parameters::Network, transparent};
use zebra_test::prelude::*;

use crate::{
    config::Config,
    preview_rollback_finalized_state, rollback_finalized_state,
    service::{
        arbitrary::PreparedChain,
        finalized_state::{CheckpointVerifiedBlock, FinalizedState},
    },
    RollbackFinalizedStateError, RollbackFinalizedStateOptions, SemanticallyVerifiedBlock,
};

/// Number of proptest cases. Each case syncs and rolls back several on-disk databases, so the
/// default is low; raise it with `PROPTEST_CASES` for deeper coverage.
const DEFAULT_ROLLBACK_PROPTEST_CASES: u32 = 1;

/// Cap on the synced height, to keep the genesis-to-target treestate rebuild fast.
const MAX_SYNCED_BLOCKS: usize = 12;

fn proptest_cases() -> u32 {
    env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_ROLLBACK_PROPTEST_CASES)
}

/// A persistent (non-ephemeral) state config rooted at `dir`, so the rollback utility can reopen
/// the same database by path after the syncing handle is dropped.
fn config_at(dir: &Path) -> Config {
    Config {
        cache_dir: dir.to_path_buf(),
        ephemeral: false,
        ..Config::default()
    }
}

fn height(n: usize) -> Height {
    Height(u32::try_from(n).expect("test heights fit in u32"))
}

/// Syncs a fresh finalized state at `config` by committing `blocks` in order, then drops it so the
/// database lock is released for the rollback utility to reopen.
fn sync_to(config: &Config, network: &Network, blocks: &[SemanticallyVerifiedBlock]) {
    let mut state = FinalizedState::new(
        config,
        network,
        #[cfg(feature = "elasticsearch")]
        false,
    );

    for block in blocks {
        let checkpoint_verified = CheckpointVerifiedBlock::from(block.block.clone());
        state
            .commit_finalized_direct(checkpoint_verified.into(), None, "rollback test")
            .expect("committing a generated block to a fresh state succeeds");
    }
}

/// Reopens the finalized state at `config` for read queries.
fn reopen(config: &Config, network: &Network) -> FinalizedState {
    FinalizedState::new(
        config,
        network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
}

/// Asserts that `rolled` (a state rolled back to the tip of `retained`) is semantically equivalent
/// to `fresh` (a state synced only up to that tip), and that all data from the `removed` blocks is
/// gone from `rolled`.
fn assert_equivalent(
    rolled: &FinalizedState,
    fresh: &FinalizedState,
    network: &Network,
    retained: &[SemanticallyVerifiedBlock],
    removed: &[SemanticallyVerifiedBlock],
) {
    let (rolled, fresh) = (&rolled.db, &fresh.db);

    // Aggregates that fold in every committed block and note.
    assert_eq!(rolled.tip(), fresh.tip(), "tip");
    assert_eq!(
        rolled.finalized_value_pool(),
        fresh.finalized_value_pool(),
        "value pool"
    );
    assert_eq!(
        rolled.sprout_tree_for_tip().root(),
        fresh.sprout_tree_for_tip().root(),
        "sprout root"
    );
    assert_eq!(
        rolled.sapling_tree_for_tip().root(),
        fresh.sapling_tree_for_tip().root(),
        "sapling root"
    );
    assert_eq!(
        rolled.orchard_tree_for_tip().root(),
        fresh.orchard_tree_for_tip().root(),
        "orchard root"
    );
    assert_eq!(
        rolled.history_tree().hash(),
        fresh.history_tree().hash(),
        "history root"
    );

    // Transparent indexes and shielded nullifiers in the retained range must match a fresh sync.
    for block in retained {
        for transaction in &block.block.transactions {
            let tx_hash = transaction.hash();

            for (index, output) in transaction.outputs().iter().enumerate() {
                let outpoint = transparent::OutPoint::from_usize(tx_hash, index);
                assert_eq!(
                    rolled.utxo(&outpoint).is_some(),
                    fresh.utxo(&outpoint).is_some(),
                    "utxo presence for {outpoint:?}"
                );

                if let Some(address) = output.address(network) {
                    assert_eq!(
                        rolled.address_balance(&address),
                        fresh.address_balance(&address),
                        "balance for {address:?}"
                    );
                }
            }

            for nullifier in transaction.sprout_nullifiers() {
                assert!(
                    rolled.contains_sprout_nullifier(nullifier),
                    "retained sprout nullifier"
                );
            }
            for nullifier in transaction.sapling_nullifiers() {
                assert!(
                    rolled.contains_sapling_nullifier(nullifier),
                    "retained sapling nullifier"
                );
            }
            for nullifier in transaction.orchard_nullifiers() {
                assert!(
                    rolled.contains_orchard_nullifier(nullifier),
                    "retained orchard nullifier"
                );
            }
        }
    }

    // Everything from the removed blocks must be gone.
    for block in removed {
        assert!(
            rolled.height(block.hash).is_none(),
            "removed block {:?} is still present",
            block.hash
        );

        for transaction in &block.block.transactions {
            for nullifier in transaction.sprout_nullifiers() {
                assert!(
                    !rolled.contains_sprout_nullifier(nullifier),
                    "removed sprout nullifier"
                );
            }
            for nullifier in transaction.sapling_nullifiers() {
                assert!(
                    !rolled.contains_sapling_nullifier(nullifier),
                    "removed sapling nullifier"
                );
            }
            for nullifier in transaction.orchard_nullifiers() {
                assert!(
                    !rolled.contains_orchard_nullifier(nullifier),
                    "removed orchard nullifier"
                );
            }
        }
    }
}

#[test]
fn rollback_matches_fresh_sync() -> Result<()> {
    let _init_guard = zebra_test::init();

    proptest!(
        ProptestConfig::with_cases(proptest_cases()),
        // no_shrink: a failure points at the rollback logic, not the chain shape, and re-syncing
        // databases to shrink would be slow with little diagnostic value.
        |((chain, count, network, _history_tree) in PreparedChain::default().no_shrink())| {
            let synced: Vec<SemanticallyVerifiedBlock> =
                chain.iter().take(count.min(MAX_SYNCED_BLOCKS)).cloned().collect();
            prop_assume!(synced.len() >= 3);
            let tip = synced.len() - 1;

            // Roll back to genesis, the middle, and one below the tip. Each target uses its own
            // freshly-synced database because the rollback mutates the source state in place.
            for target in [0, tip / 2, tip - 1] {
                let synced_dir = TempDir::new().expect("temp dir");
                let fresh_dir = TempDir::new().expect("temp dir");
                let synced_config = config_at(synced_dir.path());
                let fresh_config = config_at(fresh_dir.path());

                sync_to(&synced_config, &network, &synced);
                sync_to(&fresh_config, &network, &synced[..=target]);

                let summary = rollback_finalized_state(
                    synced_config.clone(),
                    &network,
                    RollbackFinalizedStateOptions {
                        target_height: height(target),
                        keep_rolled_back_blocks: false,
                        max_checkpoint_height: None,
                    },
                )
                .expect("rollback to a valid lower height succeeds");

                prop_assert_eq!(summary.new_tip.0, height(target));
                prop_assert_eq!(summary.rolled_back_count, height(tip).0 - height(target).0);

                assert_equivalent(
                    &reopen(&synced_config, &network),
                    &reopen(&fresh_config, &network),
                    &network,
                    &synced[..=target],
                    &synced[target + 1..],
                );
            }
        }
    );

    Ok(())
}

#[test]
fn rollback_rejects_invalid_requests() -> Result<()> {
    let _init_guard = zebra_test::init();

    proptest!(
        ProptestConfig::with_cases(proptest_cases()),
        |((chain, count, network, _history_tree) in PreparedChain::default().no_shrink())| {
            let synced: Vec<SemanticallyVerifiedBlock> =
                chain.iter().take(count.min(MAX_SYNCED_BLOCKS)).cloned().collect();
            prop_assume!(synced.len() >= 2);
            let tip = synced.len() - 1;

            let synced_dir = TempDir::new().expect("temp dir");
            let synced_config = config_at(synced_dir.path());
            sync_to(&synced_config, &network, &synced);

            // The validation errors surface through the read-only preview path, so none of these
            // mutate the database.
            let preview = |target: usize, keep: bool, max: Option<Height>| {
                preview_rollback_finalized_state(
                    synced_config.clone(),
                    &network,
                    RollbackFinalizedStateOptions {
                        target_height: height(target),
                        keep_rolled_back_blocks: keep,
                        max_checkpoint_height: max,
                    },
                )
            };

            prop_assert!(
                matches!(
                    preview(tip, false, None),
                    Err(RollbackFinalizedStateError::TargetIsTip { .. })
                ),
                "rolling back to the tip is rejected"
            );
            prop_assert!(
                matches!(
                    preview(tip + 1, false, None),
                    Err(RollbackFinalizedStateError::TargetAboveTip { .. })
                ),
                "rolling back above the tip is rejected"
            );
            // Bug #2: keeping blocks below the max checkpoint height would silently drop them.
            prop_assert!(
                matches!(
                    preview(tip - 1, true, Some(height(tip))),
                    Err(RollbackFinalizedStateError::KeepBelowMaxCheckpoint { .. })
                ),
                "keeping blocks below the max checkpoint is rejected"
            );
            // The same target is allowed once it is at/above the max checkpoint height.
            prop_assert!(
                preview(tip - 1, true, Some(height(tip - 1))).is_ok(),
                "keeping blocks at the max checkpoint is allowed"
            );

            // An empty database has no tip to roll back from.
            let empty_dir = TempDir::new().expect("temp dir");
            let empty_config = config_at(empty_dir.path());
            sync_to(&empty_config, &network, &[]);
            prop_assert!(
                matches!(
                    preview_rollback_finalized_state(
                        empty_config,
                        &network,
                        RollbackFinalizedStateOptions {
                            target_height: Height(0),
                            keep_rolled_back_blocks: false,
                            max_checkpoint_height: None,
                        },
                    ),
                    Err(RollbackFinalizedStateError::EmptyState)
                ),
                "rolling back an empty state is rejected"
            );
        }
    );

    Ok(())
}

#[test]
fn rollback_keeps_blocks_for_restore() -> Result<()> {
    let _init_guard = zebra_test::init();

    proptest!(
        ProptestConfig::with_cases(proptest_cases()),
        |((chain, count, network, _history_tree) in PreparedChain::default().no_shrink())| {
            let synced: Vec<SemanticallyVerifiedBlock> =
                chain.iter().take(count.min(MAX_SYNCED_BLOCKS)).cloned().collect();
            prop_assume!(synced.len() >= 3);
            let tip = synced.len() - 1;
            let target = tip - 2;

            let synced_dir = TempDir::new().expect("temp dir");
            let synced_config = config_at(synced_dir.path());
            sync_to(&synced_config, &network, &synced);

            let summary = rollback_finalized_state(
                synced_config,
                &network,
                RollbackFinalizedStateOptions {
                    target_height: height(target),
                    keep_rolled_back_blocks: true,
                    // A zero max checkpoint height keeps the request above the checkpoint guard.
                    max_checkpoint_height: Some(Height(0)),
                },
            )
            .expect("keeping rolled-back blocks succeeds above the max checkpoint");

            let backup = summary
                .backup
                .expect("keep_rolled_back_blocks records a backup summary");
            prop_assert_eq!(backup.block_count, tip - target);
            prop_assert!(backup.path.exists(), "backup directory was written");
        }
    );

    Ok(())
}
