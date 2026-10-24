//! Copy preflight preserves unavailable targets, and replay never prunes block bodies.

use std::{sync::Arc, time::Duration};

use zebra_chain::{
    block::{self, Block, ChainHistoryBlockTxAuthCommitmentHash},
    parameters::{testnet::ConfiguredActivationHeights, NetworkUpgrade},
    serialization::ZcashDeserializeInto,
    transaction::{LockTime, Transaction},
    transparent::Input,
};
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

#[tokio::test(flavor = "multi_thread")]
async fn pruned_copy_target_is_rejected_before_writable_init() {
    let _guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(60), async {
        let source_dir = tempfile::tempdir().unwrap();
        let source = new_zs::Config {
            cache_dir: source_dir.path().to_path_buf(),
            ..Default::default()
        };
        let populate_source = source.clone();
        tokio::task::spawn_blocking(move || populate(&populate_source, 3, false))
            .await
            .unwrap();
        for retention in [None, Some(10_000)] {
            let target_dir = tempfile::tempdir().unwrap();
            let target = new_zs::Config {
                cache_dir: target_dir.path().to_path_buf(),
                prune_blocks_older_than: retention,
                ..Default::default()
            };
            let populate_target = target.clone();
            tokio::task::spawn_blocking(move || populate(&populate_target, 2, true))
                .await
                .unwrap();
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
                .contains("copy target has unavailable block history"));
            assert!(error.to_string().contains("separate cache directory"));
            let (_, db, _) = new_zs::spawn_init_read_only(target, &Network::Mainnet)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(db.finalized_tip_height(), Some(Height(2)));
            assert_eq!(
                db.history_capabilities().unwrap().lowest_retained_height,
                Height(2)
            );
            assert!(db.block(Height(2).into()).unwrap().is_some());
        }
    })
    .await
    .unwrap();
}

fn populate_regtest(config: &new_zs::Config, network: &Network, tip: u32) {
    let mut state = FinalizedState::new(
        config,
        network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .unwrap();
    while !state.db.ready_for_pruning() {
        state.db.check_for_panics();
        std::thread::sleep(Duration::from_millis(10));
    }
    state
        .commit_finalized_direct(
            CheckpointVerifiedBlock::from(block::genesis::regtest_genesis_block()).into(),
            None,
            "copy pruning test",
        )
        .unwrap();
    for height in 1..=tip {
        let parent = state.db.tip_block().unwrap().unwrap();
        let mut block = Block {
            header: parent.header.clone(),
            transactions: vec![Arc::new(Transaction::test_v5(
                NetworkUpgrade::Nu5,
                vec![Input::Coinbase {
                    height: Height(height),
                    data: vec![0],
                    sequence: u32::MAX,
                }],
                vec![],
                LockTime::unlocked(),
                Height(height),
            ))],
        };
        let header = Arc::make_mut(&mut block.header);
        header.previous_block_hash = parent.hash();
        header.time += chrono::Duration::seconds(75);
        header.merkle_root = block.transactions.iter().map(|tx| tx.hash()).collect();
        let root = state
            .db
            .history_tree()
            .hash()
            .unwrap_or_else(|| block::CHAIN_HISTORY_ACTIVATION_RESERVED.into());
        Arc::make_mut(&mut block.header).commitment_bytes = <[u8; 32]>::from(
            ChainHistoryBlockTxAuthCommitmentHash::from_commitments(&root, &block.auth_data_root()),
        )
        .into();
        state
            .commit_finalized_direct(
                CheckpointVerifiedBlock::from(Arc::new(block)).into(),
                None,
                "copy pruning test",
            )
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pruning_configured_copy_preserves_new_and_existing_bodies() {
    let _guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(60), async {
        let network = Network::new_regtest(zebra_chain::parameters::testnet::RegtestParameters {
            activation_heights: ConfiguredActivationHeights {
                canopy: Some(1),
                nu5: Some(1),
                ..Default::default()
            },
            funding_streams: Some(Vec::new()),
            ..Default::default()
        });
        let retention = new_zs::MAX_BLOCK_REORG_HEIGHT + 1;
        let tip = retention + 249;
        let source_dir = tempfile::tempdir().unwrap();
        let source = new_zs::Config {
            cache_dir: source_dir.path().to_path_buf(),
            ..Default::default()
        };
        let populate_source = source.clone();
        let populate_network = network.clone();
        tokio::task::spawn_blocking(move || {
            populate_regtest(&populate_source, &populate_network, tip)
        })
        .await
        .unwrap();
        for existing in [false, true] {
            let target_dir = tempfile::tempdir().unwrap();
            let target = new_zs::Config {
                cache_dir: target_dir.path().to_path_buf(),
                prune_blocks_older_than: Some(retention),
                ..Default::default()
            };
            if existing {
                let populate_target = target.clone();
                let populate_network = network.clone();
                tokio::task::spawn_blocking(move || {
                    populate_regtest(&populate_target, &populate_network, retention + 1)
                })
                .await
                .unwrap();
            }
            let command = CopyStateCmd {
                max_source_height: None,
                target_config_path: None,
                filters: vec![],
            };
            command
                .copy(&network, source.clone(), target.clone())
                .await
                .unwrap();
            assert_eq!(target.prune_blocks_older_than, Some(retention));
            let (_, db, _) = new_zs::spawn_init_read_only(target, &network)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(db.finalized_tip_height(), Some(Height(tip)));
            assert!(!db.history_capabilities().unwrap().is_pruned());
            for height in 0..=tip {
                assert!(db.block(Height(height).into()).unwrap().is_some());
            }
        }
    })
    .await
    .unwrap();
}
