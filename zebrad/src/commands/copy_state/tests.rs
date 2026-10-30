//! Copy preflight must leave an unavailable target range untouched.

use std::{sync::Arc, time::Duration};

use zebra_chain::{block::Block, serialization::ZcashDeserializeInto, transaction::Transaction};
use zebra_state::{
    CheckpointVerifiedBlock, FinalizedState, TransactionLocation, TypedColumnFamily,
};

use super::*;

fn populate(config: &new_zs::Config, tip: u32, prune: bool) {
    let mut state = FinalizedState::new(
        config,
        &Network::Mainnet,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .unwrap();
    while !state.db.ready_for_pruning() {
        state.db.check_for_panics();
        std::thread::sleep(Duration::from_millis(10));
    }
    for (_, bytes) in zebra_test::vectors::MAINNET_BLOCKS.range(0..=tip) {
        state
            .commit_finalized_direct(
                CheckpointVerifiedBlock::from(
                    bytes.zcash_deserialize_into::<Arc<Block>>().unwrap(),
                )
                .into(),
                None,
                "copy preflight test",
            )
            .unwrap();
    }
    if prune {
        // Validate the archive before dropping its high-level owner. Only the raw fixture handle
        // remains when we deliberately remove history, so shutdown cannot validate damaged data.
        state.db.shutdown(true);
        let db = state.db.db().clone();
        drop(state);
        TypedColumnFamily::<TransactionLocation, Transaction>::new(&db, "tx_by_loc")
            .unwrap()
            .new_batch_for_writing()
            .zs_delete_range(
                &TransactionLocation::min_for_height(Height(1)),
                &TransactionLocation::min_for_height(Height(2)),
            )
            .write_batch()
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unavailable_copy_range_does_not_create_or_modify_target() {
    let _guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(60), async {
        let source_dir = tempfile::tempdir().unwrap();
        let source = new_zs::Config {
            cache_dir: source_dir.path().to_path_buf(),
            ..Default::default()
        };
        let populate_source = source.clone();
        tokio::task::spawn_blocking(move || populate(&populate_source, 3, true))
            .await
            .unwrap();
        for existing in [false, true] {
            let target_dir = tempfile::tempdir().unwrap();
            let target = new_zs::Config {
                cache_dir: target_dir.path().join("target"),
                ..Default::default()
            };
            if existing {
                let populate_target = target.clone();
                tokio::task::spawn_blocking(move || populate(&populate_target, 0, false))
                    .await
                    .unwrap();
            }
            let command = CopyStateCmd {
                max_source_height: None,
                target_config_path: None,
                filters: vec![],
            };
            let error = command
                .copy(&Network::Mainnet, source.clone(), target.clone())
                .await
                .unwrap_err();
            assert!(error
                .to_string()
                .contains("requires unavailable block history"));
            if existing {
                let (_, db, _) = new_zs::spawn_init_read_only(target, &Network::Mainnet)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(db.finalized_tip_height(), Some(Height(0)));
            } else {
                assert!(!target.cache_dir.exists());
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn reusable_archive_copy_target_uses_retained_source_suffix() {
    let _guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(60), async {
        let network = Network::Mainnet;
        let source_dir = tempfile::tempdir().unwrap();
        let source = new_zs::Config {
            cache_dir: source_dir.path().to_path_buf(),
            ..Default::default()
        };
        let populate_source = source.clone();
        tokio::task::spawn_blocking(move || populate(&populate_source, 3, true))
            .await
            .unwrap();

        let current_version = new_zs::state_database_format_version_in_code();
        let mut previous_version = current_version.clone();
        previous_version.major -= 1;
        for target_tip in [0, 1] {
            let target_dir = tempfile::tempdir().unwrap();
            let target = new_zs::Config {
                cache_dir: target_dir.path().to_path_buf(),
                ..Default::default()
            };
            let populate_target = target.clone();
            tokio::task::spawn_blocking(move || populate(&populate_target, target_tip, false))
                .await
                .unwrap();

            // These adjacent major formats have compatible records; only the directory differs.
            let current_path = target.db_path("state", current_version.major, &network);
            let previous_path = target.db_path("state", previous_version.major, &network);
            std::fs::rename(
                current_path.parent().unwrap(),
                previous_path.parent().unwrap(),
            )
            .unwrap();
            new_zs::write_database_format_version_to_disk(
                &target,
                "state",
                previous_version.major,
                &previous_version,
                &network,
            )
            .unwrap();
            let version_path = target.version_file_path("state", previous_version.major, &network);
            let version_bytes = std::fs::read(&version_path).unwrap();

            // Preflight must read the same archive that the writer would reuse, without moving it.
            let (_, db, _) = new_zs::spawn_init_read_only(target.clone(), &network)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(db.path(), previous_path);
            assert_eq!(db.finalized_tip_height(), Some(Height(target_tip)));
            assert!(!db.history_capabilities().unwrap().is_pruned());
            drop(db);
            assert!(!current_path.parent().unwrap().exists());
            assert_eq!(std::fs::read(&version_path).unwrap(), version_bytes);

            let command = CopyStateCmd {
                max_source_height: None,
                target_config_path: None,
                filters: vec![],
            };
            let result = command.copy(&network, source.clone(), target.clone()).await;
            if target_tip == 0 {
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("requires unavailable block history"));
                assert!(previous_path.exists());
                assert!(!current_path.parent().unwrap().exists());
                assert_eq!(std::fs::read(&version_path).unwrap(), version_bytes);
                continue;
            }

            result.unwrap();
            assert!(!previous_path.exists());
            let (_, db, _) = new_zs::spawn_init_read_only(target, &network)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(db.path(), current_path);
            assert_eq!(db.finalized_tip_height(), Some(Height(3)));
            for height in 0..=3 {
                assert!(db.block(Height(height).into()).unwrap().is_some());
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupted_upgrade_copy_target_is_rejected_without_changes() {
    let _guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(60), async {
        let network = Network::Mainnet;
        let source_dir = tempfile::tempdir().unwrap();
        let source = new_zs::Config {
            cache_dir: source_dir.path().to_path_buf(),
            ..Default::default()
        };
        let populate_source = source.clone();
        tokio::task::spawn_blocking(move || populate(&populate_source, 3, false))
            .await
            .unwrap();

        let target_dir = tempfile::tempdir().unwrap();
        let target = new_zs::Config {
            cache_dir: target_dir.path().to_path_buf(),
            ..Default::default()
        };
        let populate_target = target.clone();
        tokio::task::spawn_blocking(move || populate(&populate_target, 0, false))
            .await
            .unwrap();
        let current_major = new_zs::state_database_format_version_in_code().major;
        let previous_major = current_major - 1;
        let current_path = target.db_path("state", current_major, &network);
        let previous_path = target.db_path("state", previous_major, &network);
        std::fs::rename(
            current_path.parent().unwrap(),
            previous_path.parent().unwrap(),
        )
        .unwrap();
        // Record an unfinished transition into the previous directory, not an empty target.
        let mut interrupted_version = new_zs::state_database_format_version_in_code();
        interrupted_version.major = previous_major - 1;
        new_zs::write_database_format_version_to_disk(
            &target,
            "state",
            previous_major,
            &interrupted_version,
            &network,
        )
        .unwrap();
        let files = || {
            std::fs::read_dir(&previous_path)
                .unwrap()
                .map(|entry| {
                    let path = entry.unwrap().path();
                    let bytes = std::fs::read(&path).unwrap();
                    (path, bytes)
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let before = files();

        let command = CopyStateCmd {
            max_source_height: None,
            target_config_path: None,
            filters: vec![],
        };
        let error = command.copy(&network, source, target).await.unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<new_zs::StateInitError>(),
                Some(new_zs::StateInitError::ReadOnlyDatabaseNeedsUpgrade { path })
                    if path == &previous_path
            ),
            "copy preflight must report the target's unfinished transition: {error}"
        );
        assert_eq!(
            files(),
            before,
            "the target files, including its version, must not change"
        );
        assert!(!current_path.parent().unwrap().exists());
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn retained_copy_range_can_resume_or_copy_only_genesis() {
    let _guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(60), async {
        let source_dir = tempfile::tempdir().unwrap();
        let source = new_zs::Config {
            cache_dir: source_dir.path().to_path_buf(),
            ..Default::default()
        };
        let populate_source = source.clone();
        tokio::task::spawn_blocking(move || populate(&populate_source, 3, true))
            .await
            .unwrap();
        for max_height in [0, 2, 3] {
            let target_dir = tempfile::tempdir().unwrap();
            let target = new_zs::Config {
                cache_dir: target_dir.path().to_path_buf(),
                ..Default::default()
            };
            if max_height > 0 {
                let populate_target = target.clone();
                tokio::task::spawn_blocking(move || populate(&populate_target, 1, false))
                    .await
                    .unwrap();
            }
            let command = CopyStateCmd {
                max_source_height: Some(max_height),
                target_config_path: None,
                filters: vec![],
            };
            command
                .copy(&Network::Mainnet, source.clone(), target.clone())
                .await
                .unwrap();
            let (_, db, _) = new_zs::spawn_init_read_only(target, &Network::Mainnet)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(db.finalized_tip_height(), Some(Height(max_height)));
        }
    })
    .await
    .unwrap();
}
