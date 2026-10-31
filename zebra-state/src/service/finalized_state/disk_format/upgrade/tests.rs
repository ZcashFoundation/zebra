//! Preserve spending-index layout across interrupted format changes.

use super::*;
use crate::{service::finalized_state::FinalizedState, Config};
use zebra_chain::parameters::Network;

fn state() -> (tempfile::TempDir, FinalizedState) {
    // Version-file checks need a stable path; ephemeral configs generate a path on each call.
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let state = FinalizedState::new_with_debug(
        &config,
        &Network::Mainnet,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )
    .unwrap();
    (cache, state)
}

#[cfg(feature = "indexer")]
#[test]
fn interrupted_upgrade_preserves_spending_index_layout() {
    use std::sync::Arc;

    use crate::{CheckpointVerifiedBlock, DiskWriteBatch, TransactionLocation, WriteDisk};
    use zebra_chain::{block::Block, serialization::ZcashDeserializeInto};

    let _guard = zebra_test::init();
    let (_cache, mut state) = state();
    for (_, bytes) in zebra_test::vectors::MAINNET_BLOCKS.range(0..=3) {
        state
            .commit_finalized_direct(
                CheckpointVerifiedBlock::from(
                    bytes.zcash_deserialize_into::<Arc<Block>>().unwrap(),
                )
                .into(),
                None,
                "format interruption test",
            )
            .unwrap();
    }
    let disk_version = Version::parse("28.0.0+indexer").unwrap();
    state
        .db
        .update_format_version_on_disk(&disk_version)
        .unwrap();
    let running_version = state.db.format_version_in_code();
    let change = DbFormatChange::open_database(&running_version, Some(disk_version.clone()));
    let mut batch = DiskWriteBatch::new();
    batch.zs_delete_range(
        &state.db.cf_handle("tx_by_loc").unwrap(),
        TransactionLocation::min_for_height(Height(1)),
        TransactionLocation::min_for_height(Height(2)),
    );
    state.db.write_batch(batch).unwrap();
    let (sender, receiver) = bounded(1);

    // Stop between the format upgrade and spending-index conversion, as a crash would.
    change
        .apply_format_upgrade(&state.db, Some(Height(3)), &receiver)
        .unwrap();
    let interrupted_version = state.db.format_version_on_disk().unwrap().unwrap();
    assert_eq!(interrupted_version.major, running_version.major);
    assert_eq!(interrupted_version.build, disk_version.build);

    // A fresh startup decision must pass the history/layout preflight. Cancel the subsequent
    // detailed checks: this intentionally pruned fixture lacks their recent address window.
    let restarted = DbFormatChange::open_database(&running_version, Some(interrupted_version));
    sender.send(CancelFormatChange).unwrap();
    assert_eq!(
        restarted.run_format_change_or_check(&state.db, Some(Height(3)), &receiver),
        Err(CancelFormatChange)
    );
}

#[test]
fn interrupted_downgrade_preserves_spending_index_layout() {
    let _guard = zebra_test::init();
    let (_cache, state) = state();
    let running_version = state.db.format_version_in_code();
    let mut disk_version = running_version.clone();
    disk_version.major += 1;
    disk_version.build = if cfg!(feature = "indexer") {
        semver::BuildMetadata::EMPTY
    } else {
        semver::BuildMetadata::new("indexer").unwrap()
    };
    state
        .db
        .update_format_version_on_disk(&disk_version)
        .unwrap();

    DbFormatChange::mark_as_downgraded(&state.db);
    let interrupted_version = state.db.format_version_on_disk().unwrap().unwrap();
    assert_eq!(interrupted_version.major, running_version.major);
    assert_eq!(interrupted_version.build, disk_version.build);
}

#[test]
fn empty_upgrade_records_the_running_spending_index_layout() {
    let _guard = zebra_test::init();
    let (_cache, state) = state();
    let running_version = state.db.format_version_in_code();
    let mut disk_version = Version::new(28, 0, 0);
    if !cfg!(feature = "indexer") {
        disk_version.build = semver::BuildMetadata::new("indexer").unwrap();
    }
    state
        .db
        .update_format_version_on_disk(&disk_version)
        .unwrap();
    let change = DbFormatChange::open_database(&running_version, Some(disk_version));
    let (_sender, receiver) = bounded(1);
    change
        .run_format_change_or_check(&state.db, None, &receiver)
        .unwrap();
    assert_eq!(
        state.db.format_version_on_disk().unwrap(),
        Some(running_version)
    );
}
