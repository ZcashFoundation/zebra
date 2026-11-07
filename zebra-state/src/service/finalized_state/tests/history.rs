//! Historical body availability, atomicity, and initialization safety.

use std::{sync::Arc, time::Duration};

use tower::ServiceExt;
use zebra_chain::{
    block::{Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
};

use super::super::{
    DiskDb, DiskWriteBatch, FinalizedState, IntoDisk, TransactionLocation, WriteDisk, ZebraDb,
    STATE_COLUMN_FAMILIES_IN_CODE,
};
use crate::{
    constants::STATE_DATABASE_KIND, init_read_only, state_database_format_version_in_code,
    CheckpointVerifiedBlock, Config, HistoryCapabilities, ReadRequest, ReadResponse,
    StateInitError,
};

fn open(config: &Config, read_only: bool) -> Result<FinalizedState, StateInitError> {
    FinalizedState::new_with_debug(config, &Network::Mainnet, true, read_only)
}

fn populated(config: &Config) -> FinalizedState {
    let mut state = open(config, false).unwrap();
    state
        .db
        .update_format_version_on_disk(&state.db.format_version_in_code())
        .unwrap();
    for (_, bytes) in zebra_test::vectors::MAINNET_BLOCKS.range(0..=3) {
        let block = bytes.zcash_deserialize_into::<Arc<Block>>().unwrap();
        state
            .commit_finalized_direct(
                CheckpointVerifiedBlock::from(block).into(),
                None,
                "history capability test",
            )
            .unwrap();
    }
    assert_eq!(state.db.finalized_tip_height(), Some(Height(3)));
    state
}

// Deliberately bypass pruning safety when constructing damaged or externally pruned databases.
fn delete_body_prefix(batch: &mut DiskWriteBatch, db: &ZebraDb, end: Height) {
    batch.zs_delete_range(
        &db.cf_handle("tx_by_loc").unwrap(),
        TransactionLocation::min_for_height(Height(1)),
        TransactionLocation::min_for_height(end),
    );
}

#[test]
fn known_transaction_bodies_are_pruned_only_below_the_actual_floor() {
    use crate::BlockReadError;

    let _init_guard = zebra_test::init();
    for height in [Height(1), Height(2)] {
        let state = populated(&Config::ephemeral());
        let block = state.db.block(height.into()).unwrap().unwrap();
        let tx = &block.transactions[0];
        let retained = state.db.block(Height(3).into()).unwrap().unwrap();
        let mut batch = DiskWriteBatch::new();
        batch.zs_delete_range(
            &state.db.cf_handle("tx_by_loc").unwrap(),
            TransactionLocation::min_for_height(height),
            TransactionLocation::min_for_height(height.next().unwrap()),
        );
        state.db.write_batch(batch).unwrap();

        let expected = if height == Height(1) {
            BlockReadError::BlockBodyUnavailable { height }
        } else {
            BlockReadError::CorruptBlock { height }
        };
        assert_eq!(state.db.transaction(tx.hash()).unwrap_err(), expected);
        for query in [height.into(), block.hash().into()] {
            assert_eq!(state.db.block(query).unwrap_err(), expected);
            assert_eq!(state.db.block_and_size(query).unwrap_err(), expected);
            assert!(state.db.block_header(query).is_some());
            assert_eq!(
                state.db.transaction_hashes_for_block(query).unwrap()[0],
                tx.hash()
            );
        }
        assert!(state
            .db
            .transaction(zebra_chain::transaction::Hash([0xff; 32]))
            .unwrap()
            .is_none());
        assert_eq!(
            state
                .db
                .transaction(retained.transactions[0].hash())
                .unwrap()
                .unwrap()
                .0,
            retained.transactions[0]
        );
    }
}

#[test]
fn secondary_transactions_distinguish_torn_views_from_pruned_history() {
    use crate::BlockReadError;

    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let primary = populated(&config);
    let blocks: Vec<_> = (1..=3)
        .map(|height| primary.db.block(Height(height).into()).unwrap().unwrap())
        .collect();
    primary.db.flush().unwrap();
    let secondary = open(&config, true).unwrap();

    let mut batch = DiskWriteBatch::new();
    batch.zs_delete(
        &primary.db.cf_handle("tx_by_loc").unwrap(),
        TransactionLocation::min_for_height(Height(2)),
    );
    primary.db.write_batch(batch).unwrap();
    primary.db.flush().unwrap();
    secondary.db.try_catch_up_with_primary().unwrap();
    assert!(secondary
        .db
        .transaction(blocks[1].transactions[0].hash())
        .unwrap()
        .is_none());
    assert!(secondary.db.block(Height(2).into()).unwrap().is_none());
    assert_eq!(
        secondary.db.history_capabilities().unwrap(),
        HistoryCapabilities::default()
    );

    let mut batch = DiskWriteBatch::new();
    delete_body_prefix(&mut batch, &primary.db, Height(3));
    primary.db.write_batch(batch).unwrap();
    primary.db.flush().unwrap();
    secondary.db.try_catch_up_with_primary().unwrap();
    assert_eq!(
        secondary
            .db
            .history_capabilities()
            .unwrap()
            .lowest_retained_height,
        Height(3)
    );
    for block in &blocks[..2] {
        let height = block.coinbase_height().unwrap();
        let expected = BlockReadError::BlockBodyUnavailable { height };
        assert_eq!(
            secondary
                .db
                .transaction(block.transactions[0].hash())
                .unwrap_err(),
            expected
        );
        assert_eq!(secondary.db.block(height.into()).unwrap_err(), expected);
        assert_eq!(
            secondary.db.block_and_size(height.into()).unwrap_err(),
            expected
        );
    }
    assert!(secondary
        .db
        .transaction(blocks[2].transactions[0].hash())
        .unwrap()
        .is_some());
    assert!(secondary
        .db
        .transaction(zebra_chain::transaction::Hash([0xff; 32]))
        .unwrap()
        .is_none());

    // An invalid suffix is not evidence of pruning, including during secondary catch-up.
    let mut batch = DiskWriteBatch::new();
    delete_body_prefix(&mut batch, &primary.db, Height(4));
    primary.db.write_batch(batch).unwrap();
    primary.db.flush().unwrap();
    secondary.db.try_catch_up_with_primary().unwrap();
    assert!(secondary.db.history_capabilities().is_err());
    assert!(secondary
        .db
        .transaction(blocks[2].transactions[0].hash())
        .unwrap()
        .is_none());
    assert_eq!(
        primary
            .db
            .transaction(blocks[2].transactions[0].hash())
            .unwrap_err(),
        BlockReadError::CorruptBlock { height: Height(3) }
    );
}

#[test]
fn pruning_without_a_restart_witness_waits_without_mutation() {
    let _init_guard = zebra_test::init();
    let state = populated(&Config {
        prune_blocks_older_than: Some(10_000),
        ..Config::ephemeral()
    });
    state.db.mark_ready_for_pruning();
    let mut batch = DiskWriteBatch::new();
    assert_eq!(
        batch
            .prepare_history_pruning(
                &state.db,
                Height(2),
                state.db.history_capabilities().unwrap(),
                state.db.ready_for_pruning(),
                state.db.finalized_tip_height(),
                &mut Default::default()
            )
            .unwrap(),
        None
    );
    assert_eq!(batch, DiskWriteBatch::new());
    assert!(state.db.block(Height(1).into()).unwrap().is_some());
}

#[test]
fn invalid_pruning_retention_does_not_create_a_cache_directory() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().join("invalid-pruning"),
        prune_blocks_older_than: Some(9_999),
        ..Config::default()
    };
    for read_only in [false, true] {
        let low_level = DiskDb::new(
            &config,
            STATE_DATABASE_KIND,
            &state_database_format_version_in_code(),
            &Network::Mainnet,
            STATE_COLUMN_FAMILIES_IN_CODE
                .iter()
                .map(ToString::to_string),
            read_only,
        );
        assert!(matches!(
            low_level,
            Err(StateInitError::InvalidPruningRetention { .. })
        ));
        assert!(!config.cache_dir.exists());
        assert!(matches!(
            open(&config, read_only),
            Err(StateInitError::InvalidPruningRetention { .. })
        ));
        assert!(!config.cache_dir.exists());
    }
}

#[test]
fn pruning_stages_at_most_one_hundred_contiguous_heights() {
    let _init_guard = zebra_test::init();
    let state = populated_with_witness(&Config::ephemeral(), Height(350), Height(351));
    state.db.mark_ready_for_pruning();
    let mut previous = Height(1);
    for expected in [Height(101), Height(201), Height(301), Height(350)] {
        let mut batch = DiskWriteBatch::new();
        assert_eq!(
            batch
                .prepare_history_pruning(
                    &state.db,
                    Height(351),
                    state.db.history_capabilities().unwrap(),
                    state.db.ready_for_pruning(),
                    state.db.finalized_tip_height(),
                    &mut Default::default()
                )
                .unwrap(),
            Some(expected)
        );
        assert!(state.db.block(previous.into()).unwrap().is_some());
        state.db.write(batch).unwrap();
        assert_eq!(
            state
                .db
                .history_capabilities()
                .unwrap()
                .lowest_retained_height,
            expected
        );
        for height in previous.0..expected.0 {
            let height = Height(height);
            assert_eq!(
                state.db.block(height.into()).unwrap_err(),
                crate::BlockReadError::BlockBodyUnavailable { height }
            );
            assert!(state.db.hash(height).is_some());
            assert!(!state
                .db
                .transaction_hashes_for_block(height.into())
                .unwrap()
                .is_empty());
        }
        for retained in [Height(0), expected, Height(351)] {
            assert!(state.db.block(retained.into()).unwrap().is_some());
        }
        previous = expected;
    }
    let mut waiting = DiskWriteBatch::new();
    assert_eq!(
        waiting
            .prepare_history_pruning(
                &state.db,
                Height(351),
                state.db.history_capabilities().unwrap(),
                state.db.ready_for_pruning(),
                state.db.finalized_tip_height(),
                &mut Default::default()
            )
            .unwrap(),
        None
    );
    assert_eq!(waiting, DiskWriteBatch::new());
}

#[test]
fn pruning_restart_check_errors_leave_the_batch_unchanged() {
    let _init_guard = zebra_test::init();
    let state = populated_with_witness(&Config::ephemeral(), Height(3), Height(4));
    state.db.mark_ready_for_pruning();
    let mut damage = DiskWriteBatch::new();
    damage.zs_delete(
        &state.db.cf_handle("tx_by_loc").unwrap(),
        TransactionLocation::min_for_height(Height(4)),
    );
    state.db.write(damage).unwrap();

    let mut batch = DiskWriteBatch::new();
    assert!(batch
        .prepare_history_pruning(
            &state.db,
            Height(2),
            state.db.history_capabilities().unwrap(),
            state.db.ready_for_pruning(),
            state.db.finalized_tip_height(),
            &mut Default::default()
        )
        .is_err());
    assert_eq!(batch, DiskWriteBatch::new());
    assert_eq!(
        state.db.history_capabilities().unwrap(),
        HistoryCapabilities::default()
    );
    assert!(state.db.block(Height(1).into()).unwrap().is_some());
}

#[test]
fn history_floor_and_deleted_bodies_share_a_snapshot_and_survive_reopen() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let mut config = Config {
        cache_dir: cache.path().to_path_buf(),
        prune_blocks_older_than: Some(10_000),
        ..Config::default()
    };
    let state = populated_with_witness(&config, Height(3), Height(4));
    let network = state.db.network();
    state.db.mark_ready_for_pruning();
    let before = state.db.snapshot();
    let tx_cf = state.db.cf_handle("tx_by_loc").unwrap();
    let first_deleted = TransactionLocation::min_for_height(Height(1)).as_bytes();
    let mut batch = DiskWriteBatch::new();
    assert_eq!(
        batch
            .prepare_history_pruning(
                &state.db,
                Height(2),
                state.db.history_capabilities().unwrap(),
                state.db.ready_for_pruning(),
                state.db.finalized_tip_height(),
                &mut Default::default()
            )
            .unwrap(),
        Some(Height(2))
    );
    assert_eq!(
        state.db.history_capabilities().unwrap(),
        HistoryCapabilities::default()
    );
    assert!(state
        .db
        .get_pinned_cf(&tx_cf, first_deleted)
        .unwrap()
        .is_some());
    state.db.write(batch).unwrap();

    assert!(before
        .get_pinned_cf(&tx_cf, first_deleted)
        .unwrap()
        .is_some());
    let after = state.db.snapshot();
    assert!(after
        .get_pinned_cf(&tx_cf, first_deleted)
        .unwrap()
        .is_none());
    let mut retained = after.raw_iterator_cf(&tx_cf);
    retained.seek(first_deleted);
    assert_eq!(
        retained.key(),
        Some(
            TransactionLocation::min_for_height(Height(2))
                .as_bytes()
                .as_slice()
        )
    );
    assert_eq!(
        state
            .db
            .history_capabilities()
            .unwrap()
            .lowest_retained_height,
        Height(2)
    );
    drop(retained);
    drop(after);
    drop(before);

    for invalid_floor in [Height(0), Height(1), Height(2), Height(5), Height::MAX] {
        let mut invalid = DiskWriteBatch::new();
        assert!(invalid
            .prepare_history_pruning(
                &state.db,
                invalid_floor,
                state.db.history_capabilities().unwrap(),
                state.db.ready_for_pruning(),
                state.db.finalized_tip_height(),
                &mut Default::default()
            )
            .is_err());
        assert_eq!(invalid, DiskWriteBatch::new());
    }
    drop(state);

    let reopen = |config: &Config, read_only| {
        FinalizedState::new_with_debug(config, &network, true, read_only).unwrap()
    };
    config.prune_blocks_older_than = Some(10_000);
    let state = reopen(&config, false);
    assert_eq!(
        state
            .db
            .history_capabilities()
            .unwrap()
            .lowest_retained_height,
        Height(2)
    );
    state.db.mark_ready_for_pruning();
    let mut batch = DiskWriteBatch::new();
    assert_eq!(
        batch
            .prepare_history_pruning(
                &state.db,
                Height(3),
                state.db.history_capabilities().unwrap(),
                state.db.ready_for_pruning(),
                state.db.finalized_tip_height(),
                &mut Default::default()
            )
            .unwrap(),
        Some(Height(3))
    );
    state.db.write(batch).unwrap();
    drop(state);

    config.prune_blocks_older_than = Some(20_000);
    let state = reopen(&config, false);
    let history = state.db.history_capabilities().unwrap();
    assert!(history.is_pruned());
    assert_eq!(history.lowest_retained_height, Height(3));
    let tx_cf = state.db.cf_handle("tx_by_loc").unwrap();
    for height in 0..=4 {
        let height = Height(height);
        let retained = state
            .db
            .get_pinned_cf(
                &tx_cf,
                TransactionLocation::min_for_height(height).as_bytes(),
            )
            .unwrap()
            .is_some();
        assert_eq!(retained, history.has_block_body(height));
        assert!(
            state.db.hash(height).is_some(),
            "pruning must retain block identity"
        );
        assert!(
            !state
                .db
                .transaction_hashes_for_block(height.into())
                .unwrap()
                .is_empty(),
            "pruning must retain transaction identifiers"
        );
    }
}

#[test]
fn missing_body_prefix_is_rejected_by_every_writable_database_entry_point() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let state = populated(&config);
    let mut batch = DiskWriteBatch::new();
    batch.zs_delete_range(
        &state.db.cf_handle("tx_by_loc").unwrap(),
        TransactionLocation::min_for_height(Height(1)),
        TransactionLocation::min_for_height(Height(2)),
    );
    state.db.write(batch).unwrap();
    drop(state);
    let version = state_database_format_version_in_code();
    let columns = || {
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string)
    };
    assert!(matches!(
        DiskDb::new(
            &config,
            STATE_DATABASE_KIND,
            &version,
            &Network::Mainnet,
            columns(),
            false
        ),
        Err(StateInitError::PrunedDatabase { .. })
    ));
    assert!(matches!(
        ZebraDb::new(
            &config,
            STATE_DATABASE_KIND,
            &version,
            &Network::Mainnet,
            true,
            columns(),
            false
        ),
        Err(StateInitError::PrunedDatabase { .. })
    ));
    assert!(matches!(
        open(&config, false),
        Err(StateInitError::PrunedDatabase { .. })
    ));
    assert!(matches!(
        FinalizedState::new(&config, &Network::Mainnet),
        Err(StateInitError::PrunedDatabase { .. })
    ));
    assert_eq!(
        open(&config, true)
            .unwrap()
            .db
            .history_capabilities()
            .unwrap()
            .lowest_retained_height,
        Height(2)
    );
}

#[test]
fn pruned_v30_archive_startup_never_falls_back_to_v29() {
    let _guard = zebra_test::init();
    let network = Network::Mainnet;
    for both_directories in [false, true] {
        let cache = tempfile::tempdir().unwrap();
        let mut config = Config {
            cache_dir: cache.path().to_path_buf(),
            prune_blocks_older_than: Some(10_000),
            ..Config::default()
        };
        let state = populated(&config);
        let tip = state.db.tip();
        let mut batch = DiskWriteBatch::new();
        delete_body_prefix(&mut batch, &state.db, Height(2));
        state.db.write(batch).unwrap();
        drop(state);
        let archive_path = config.db_path(STATE_DATABASE_KIND, 29, &network);
        let pruned_path = config.db_path(STATE_DATABASE_KIND, 30, &network);

        // A stale archive must neither win selection nor be overwritten by the newer state.
        if both_directories {
            let stale_cache = tempfile::tempdir().unwrap();
            let stale_config = Config {
                cache_dir: stale_cache.path().to_path_buf(),
                ..Config::default()
            };
            let stale = populated(&stale_config);
            drop(stale);
            std::fs::create_dir_all(archive_path.parent().unwrap()).unwrap();
            std::fs::rename(
                stale_config.db_path(STATE_DATABASE_KIND, 29, &network),
                &archive_path,
            )
            .unwrap();
        }
        config.prune_blocks_older_than = None;
        assert_eq!(config.state_database_format_version(&network).major, 30);
        assert_eq!(
            crate::state_database_format_version_on_disk(&config, &network)
                .unwrap()
                .unwrap()
                .major,
            30
        );
        for skip_checks in [false, true] {
            let result = FinalizedState::new_with_debug(&config, &network, skip_checks, false);
            assert!(matches!(
                result,
                Err(StateInitError::PrunedDatabase { path, .. }) if path == pruned_path
            ));
            assert_eq!(archive_path.exists(), both_directories);
        }
        let reader = open(&config, true).unwrap();
        assert_eq!(reader.db.path(), pruned_path);
        assert_eq!(reader.db.tip(), tip);
        assert_eq!(
            reader
                .db
                .history_capabilities()
                .unwrap()
                .lowest_retained_height,
            Height(2)
        );
        drop(reader);
        if both_directories {
            let archive = ZebraDb::new(
                &config,
                STATE_DATABASE_KIND,
                &state_database_format_version_in_code(),
                &network,
                true,
                STATE_COLUMN_FAMILIES_IN_CODE
                    .iter()
                    .map(ToString::to_string),
                true,
            )
            .unwrap();
            assert_eq!(archive.path(), archive_path);
            assert!(!archive.history_capabilities().unwrap().is_pruned());
            assert!(archive.block(Height(1).into()).unwrap().is_some());
        }
    }
}

#[tokio::test]
async fn pruned_database_startup_reports_archive_recovery_message() {
    let _guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let cache = tempfile::tempdir().unwrap();
        let mut config = Config {
            cache_dir: cache.path().to_path_buf(),
            prune_blocks_older_than: Some(10_000),
            ..Config::default()
        };
        let state = populated(&config);
        let mut batch = DiskWriteBatch::new();
        delete_body_prefix(&mut batch, &state.db, Height(2));
        state.db.write(batch).unwrap();
        drop(state);
        config.prune_blocks_older_than = None;
        let expected = StateInitError::PrunedDatabase {
            path: config.db_path(STATE_DATABASE_KIND, 30, &Network::Mainnet),
            lowest_retained_height: Height(2),
        }
        .to_string();

        let error = tokio::spawn(async move {
            crate::service::StateService::new(config, &Network::Mainnet, Height(0), 1).await;
        })
        .await
        .expect_err("archive startup must refuse pruned state");
        let panic = error.into_panic();
        assert_eq!(panic.downcast_ref::<String>().unwrap(), &expected);
    })
    .await
    .unwrap();
}

#[test]
fn never_pruned_v30_remains_selected_without_pruning_config() {
    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let mut config = Config {
        cache_dir: cache.path().to_path_buf(),
        prune_blocks_older_than: Some(10_000),
        ..Config::default()
    };
    let state = populated(&config);
    let tip = state.db.tip();
    drop(state);
    config.prune_blocks_older_than = None;
    for read_only in [false, true] {
        let state = open(&config, read_only).unwrap();
        assert_eq!(state.db.major_version(), 30);
        assert_eq!(state.db.tip(), tip);
        assert!(!state.db.history_capabilities().unwrap().is_pruned());
        assert!(!config
            .db_path(STATE_DATABASE_KIND, 29, &Network::Mainnet)
            .exists());
    }
}

#[tokio::test]
async fn read_only_capability_requests_do_not_mutate_history() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let cache = tempfile::tempdir().unwrap();
        let config = Config {
            cache_dir: cache.path().to_path_buf(),
            prune_blocks_older_than: Some(10_000),
            ..Config::default()
        };
        let state = populated(&config);
        let (reader, secondary, _sender) =
            init_read_only(config.clone(), &Network::Mainnet).unwrap();
        assert_eq!(
            reader
                .oneshot(ReadRequest::HistoryCapabilities)
                .await
                .unwrap(),
            ReadResponse::HistoryCapabilities(HistoryCapabilities::default())
        );
        drop(secondary);
        let mut batch = DiskWriteBatch::new();
        delete_body_prefix(&mut batch, &state.db, Height(2));
        state.db.write(batch).unwrap();
        drop(state);
        let (reader, secondary, _sender) =
            init_read_only(config.clone(), &Network::Mainnet).unwrap();
        let expected = HistoryCapabilities {
            lowest_retained_height: Height(2),
        };
        assert_eq!(
            reader
                .oneshot(ReadRequest::HistoryCapabilities)
                .await
                .unwrap(),
            ReadResponse::HistoryCapabilities(expected)
        );
        let mut batch = DiskWriteBatch::new();
        assert!(!secondary.ready_for_pruning());
        assert!(batch
            .prepare_history_pruning(
                &secondary,
                Height(3),
                secondary.history_capabilities().unwrap(),
                secondary.ready_for_pruning(),
                secondary.finalized_tip_height(),
                &mut Default::default()
            )
            .is_err());
        assert_eq!(batch, DiskWriteBatch::new());
        delete_body_prefix(&mut batch, &secondary, Height(3));
        assert!(
            secondary.write(batch).is_err(),
            "secondary instances cannot change policy or delete bodies"
        );
        drop(secondary);
        assert_eq!(
            open(&config, true)
                .unwrap()
                .db
                .history_capabilities()
                .unwrap(),
            expected
        );
    })
    .await
    .unwrap();
}

#[test]
fn empty_and_genesis_only_states_have_archive_capabilities() {
    let _init_guard = zebra_test::init();
    let mut state = open(&Config::ephemeral(), false).unwrap();
    assert_eq!(
        state.db.history_capabilities().unwrap(),
        HistoryCapabilities::default()
    );
    let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap();
    state
        .commit_finalized_direct(
            CheckpointVerifiedBlock::from(genesis).into(),
            None,
            "genesis body availability test",
        )
        .unwrap();
    assert_eq!(
        state.db.history_capabilities().unwrap(),
        HistoryCapabilities::default()
    );
}

#[test]
fn invalid_retained_suffix_is_not_reported_as_archive_or_pruned() {
    let _init_guard = zebra_test::init();
    for missing_suffix in [true, false] {
        let cache = tempfile::tempdir().unwrap();
        let config = Config {
            cache_dir: cache.path().to_path_buf(),
            ..Config::default()
        };
        let state = populated(&config);
        let transactions = state.db.cf_handle("tx_by_loc").unwrap();
        let mut batch = DiskWriteBatch::new();
        if missing_suffix {
            batch.zs_delete_range(
                &transactions,
                TransactionLocation::min_for_height(Height(1)),
                TransactionLocation::min_for_height(Height(4)),
            );
        } else {
            batch.zs_delete(
                &transactions,
                TransactionLocation::min_for_height(Height(1)),
            );
            batch.zs_insert(
                &transactions,
                TransactionLocation::from_index(Height(1), 1),
                (),
            );
        }
        state.db.write(batch).unwrap();
        assert!(matches!(
            state.db.history_capabilities(),
            Err(StateInitError::HistoryRead { .. })
        ));
        drop(state);
        for read_only in [false, true] {
            assert!(matches!(
                open(&config, read_only),
                Err(StateInitError::HistoryRead { .. })
            ));
        }
    }
}

#[test]
fn pruned_index_mode_is_checked_before_returning_a_writable_database() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        prune_blocks_older_than: Some(10_000),
        ..Config::default()
    };
    let state = populated(&config);
    let mut version = state.db.format_version_in_code();
    version.build = if cfg!(feature = "indexer") {
        semver::BuildMetadata::EMPTY
    } else {
        semver::BuildMetadata::new("indexer").unwrap()
    };
    state.db.update_format_version_on_disk(&version).unwrap();
    let mut batch = DiskWriteBatch::new();
    delete_body_prefix(&mut batch, &state.db, Height(2));
    state.db.write(batch).unwrap();
    let tip = state.db.tip();
    let block_info = state.db.block_info(Height(1).into());
    drop(state);

    let error = DiskDb::new(
        &config,
        STATE_DATABASE_KIND,
        &config.state_database_format_version(&Network::Mainnet),
        &Network::Mainnet,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    )
    .expect_err("a pruned database cannot change its spending-index mode");
    assert!(error.to_string().contains("spending-index mode"));
    let error = open(&config, false)
        .expect_err("disabling background format checks must not bypass the open-time preflight");
    assert!(error.to_string().contains("spending-index mode"));

    let reader = open(&config, true).unwrap();
    assert_eq!(reader.db.format_version_on_disk().unwrap(), Some(version));
    assert_eq!(reader.db.tip(), tip);
    assert_eq!(reader.db.block_info(Height(1).into()), block_info);
    assert_eq!(
        reader
            .db
            .history_capabilities()
            .unwrap()
            .lowest_retained_height,
        Height(2)
    );
}

#[test]
fn history_migrations_and_index_mode_changes_fail_before_mutation() {
    use super::super::disk_format::upgrade::DbFormatChange;

    let _init_guard = zebra_test::init();
    for change_index_mode in [false, true] {
        let cache = tempfile::tempdir().unwrap();
        let state = populated(&Config {
            cache_dir: cache.path().to_path_buf(),
            ..Config::default()
        });
        let mut version = state_database_format_version_in_code();
        if change_index_mode {
            version.build = if cfg!(feature = "indexer") {
                semver::BuildMetadata::EMPTY
            } else {
                semver::BuildMetadata::new("indexer").unwrap()
            };
        } else {
            version.major = 26;
        }
        state.db.update_format_version_on_disk(&version).unwrap();
        let mut batch = DiskWriteBatch::new();
        delete_body_prefix(&mut batch, &state.db, Height(2));
        state.db.write(batch).unwrap();
        let before = state.db.block_info(Height(1).into());
        let change = DbFormatChange::open_database(
            &state_database_format_version_in_code(),
            Some(version.clone()),
        );
        let (_sender, receiver) = crossbeam_channel::bounded(1);
        let error = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            change.run_format_change_or_check(&state.db, Some(Height(3)), &receiver)
        }))
        .unwrap_err();
        let message = error
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| error.downcast_ref::<&str>().copied())
            .unwrap();
        assert!(
            message.contains("requires unavailable block history"),
            "{message}"
        );
        assert_eq!(state.db.format_version_on_disk().unwrap(), Some(version));
        assert_eq!(state.db.block_info(Height(1).into()), before);
        assert!(!state.db.ready_for_pruning());
    }
}

#[test]
fn shutdown_checks_cannot_enable_pruning_after_cancelled_index_conversion() {
    use super::super::disk_format::upgrade::{CancelFormatChange, DbFormatChange};

    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let state = populated(&Config {
        cache_dir: cache.path().to_path_buf(),
        prune_blocks_older_than: Some(10_000),
        ..Config::default()
    });
    let running_version = state.db.format_version_in_code();
    let mut disk_version = running_version.clone();
    disk_version.build = if cfg!(feature = "indexer") {
        semver::BuildMetadata::EMPTY
    } else {
        semver::BuildMetadata::new("indexer").unwrap()
    };
    state
        .db
        .update_format_version_on_disk(&disk_version)
        .unwrap();
    let startup = DbFormatChange::open_database(&running_version, Some(disk_version.clone()));
    let (cancel, cancelled) = crossbeam_channel::bounded(1);
    cancel.send(CancelFormatChange).unwrap();
    assert_eq!(
        startup.run_format_change_or_check(&state.db, Some(Height(3)), &cancelled),
        Err(CancelFormatChange)
    );
    assert!(!state.db.ready_for_pruning());
    assert_eq!(
        state.db.format_version_on_disk().unwrap(),
        Some(disk_version.clone())
    );

    // Shutdown checks the new blocks without completing startup's spending-index conversion.
    let (_keep_open, never_cancelled) = crossbeam_channel::bounded(1);
    DbFormatChange::check_new_blocks(&state.db)
        .run_format_change_or_check(&state.db, None, &never_cancelled)
        .unwrap();
    assert_eq!(
        state.db.format_version_on_disk().unwrap(),
        Some(disk_version)
    );
    assert!(!state.db.ready_for_pruning());
    let mut batch = DiskWriteBatch::new();
    assert!(batch
        .prepare_history_pruning(
            &state.db,
            Height(2),
            state.db.history_capabilities().unwrap(),
            state.db.ready_for_pruning(),
            state.db.finalized_tip_height(),
            &mut Default::default()
        )
        .is_err());
    assert_eq!(batch, DiskWriteBatch::new());
    assert!(state.db.block(Height(1).into()).unwrap().is_some());

    startup
        .run_format_change_or_check(&state.db, Some(Height(3)), &never_cancelled)
        .unwrap();
    assert_eq!(
        state.db.format_version_on_disk().unwrap(),
        Some(running_version)
    );
    assert!(state.db.ready_for_pruning());
}

#[test]
fn pruning_readiness_waits_for_migrations_and_uncancelled_checks() {
    use super::super::disk_format::upgrade::{CancelFormatChange, DbFormatChange};

    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let state = populated(&Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    });
    state.db.mark_finished_format_upgrades();
    assert!(!state.db.ready_for_pruning());

    let old_version = semver::Version::new(26, 0, 0);
    state
        .db
        .update_format_version_on_disk(&old_version)
        .unwrap();
    let change =
        DbFormatChange::open_database(&state_database_format_version_in_code(), Some(old_version));
    let (sender, receiver) = crossbeam_channel::bounded(1);
    sender.send(CancelFormatChange).unwrap();
    assert_eq!(
        change.run_format_change_or_check(&state.db, Some(Height(3)), &receiver),
        Err(CancelFormatChange)
    );
    assert!(!state.db.ready_for_pruning());
    change
        .run_format_change_or_check(&state.db, Some(Height(3)), &receiver)
        .unwrap();
    assert!(state.db.ready_for_pruning());
}

fn populated_with_witness(
    config: &Config,
    witness_height: Height,
    tip_height: Height,
) -> FinalizedState {
    use zebra_chain::{
        parameters::{testnet::ConfiguredActivationHeights, NetworkUpgrade},
        transaction::{LockTime, Transaction},
        transparent::Input,
    };

    let network = Network::new_regtest(zebra_chain::parameters::testnet::RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            nu5: Some(1),
            ..Default::default()
        },
        ..Default::default()
    });
    let config = Config {
        prune_blocks_older_than: Some(10_000),
        ..config.clone()
    };
    let state = FinalizedState::new_with_debug(&config, &network, true, false).unwrap();
    state
        .db
        .update_format_version_on_disk(&state.db.format_version_in_code())
        .unwrap();
    let mut header: zebra_chain::block::Header = zebra_test::vectors::DUMMY_HEADER
        .as_slice()
        .zcash_deserialize_into()
        .unwrap();
    let mut batch = DiskWriteBatch::new();
    let cf = |name| state.db.cf_handle(name).unwrap();
    let genesis: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .unwrap();
    for height in 0..=tip_height.0 {
        let height = Height(height);
        let inputs = vec![Input::Coinbase {
            height,
            data: if height.is_min() {
                genesis.transactions[0].inputs()[0]
                    .miner_data()
                    .unwrap()
                    .clone()
            } else {
                vec![0]
            },
            sequence: u32::MAX,
        }];
        let tx = Arc::new(if height == witness_height {
            Transaction::test_v5(
                NetworkUpgrade::current(&network, height),
                inputs,
                vec![],
                LockTime::unlocked(),
                height,
            )
        } else {
            Transaction::test_v4(inputs, vec![], LockTime::unlocked(), height)
        });
        header.merkle_root = std::iter::once(tx.hash()).collect();
        let hash = header.hash();
        let location = TransactionLocation::from_usize(height, 0);
        batch.zs_insert(&cf("block_header_by_height"), height, header);
        batch.zs_insert(&cf("hash_by_height"), height, hash);
        batch.zs_insert(&cf("height_by_hash"), hash, height);
        batch.zs_insert(&cf("tx_by_loc"), location, &tx);
        batch.zs_insert(&cf("hash_by_tx_loc"), location, tx.hash());
        batch.zs_insert(&cf("tx_loc_by_hash"), tx.hash(), location);
        header.previous_block_hash = hash;
    }
    state.db.write(batch).unwrap();
    state
}

#[test]
fn pruning_waits_after_a_long_branchless_suffix() {
    let _init_guard = zebra_test::init();
    let witness_height = Height(3);
    let suffix_length = u32::try_from(crate::constants::MAX_LEGACY_CHAIN_BLOCKS).unwrap() + 1;
    let state = populated_with_witness(
        &Config::ephemeral(),
        witness_height,
        Height(witness_height.0 + suffix_length),
    );
    state.db.mark_ready_for_pruning();
    let mut witness = Default::default();
    for _ in 0..2 {
        let mut batch = DiskWriteBatch::new();
        assert_eq!(
            batch
                .prepare_history_pruning(
                    &state.db,
                    Height(101),
                    state.db.history_capabilities().unwrap(),
                    state.db.ready_for_pruning(),
                    state.db.finalized_tip_height(),
                    &mut witness
                )
                .unwrap(),
            None
        );
        assert_eq!(batch, DiskWriteBatch::new());
    }
    assert_eq!(
        state.db.history_capabilities().unwrap(),
        HistoryCapabilities::default()
    );
    assert!(state.db.block(witness_height.into()).unwrap().is_some());
}

#[test]
fn pruning_retains_a_restart_witness_fifteen_thousand_blocks_back() {
    use zebra_chain::parameters::NetworkUpgrade;

    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let witness_height = Height(3);
    let tip_height = Height(witness_height.0 + 15_000);
    let state = populated_with_witness(&config, witness_height, tip_height);
    let network = state.db.network();

    // The fixture writes block indexes directly; readiness is tested separately above.
    let mut batch = DiskWriteBatch::new();
    assert!(batch
        .prepare_history_pruning(
            &state.db,
            Height(5_004),
            state.db.history_capabilities().unwrap(),
            state.db.ready_for_pruning(),
            state.db.finalized_tip_height(),
            &mut Default::default()
        )
        .is_err());
    assert_eq!(batch, DiskWriteBatch::new());
    state.db.mark_ready_for_pruning();
    assert_eq!(
        batch
            .prepare_history_pruning(
                &state.db,
                Height(5_004),
                state.db.history_capabilities().unwrap(),
                state.db.ready_for_pruning(),
                state.db.finalized_tip_height(),
                &mut Default::default()
            )
            .unwrap(),
        Some(witness_height)
    );
    state.db.write(batch).unwrap();
    assert_eq!(
        state
            .db
            .history_capabilities()
            .unwrap()
            .lowest_retained_height,
        witness_height
    );
    let mut waiting = DiskWriteBatch::new();
    assert_eq!(
        waiting
            .prepare_history_pruning(
                &state.db,
                Height(5_004),
                state.db.history_capabilities().unwrap(),
                state.db.ready_for_pruning(),
                state.db.finalized_tip_height(),
                &mut Default::default()
            )
            .unwrap(),
        None
    );
    assert_eq!(waiting, DiskWriteBatch::new());
    assert!(state.db.block(witness_height.into()).unwrap().is_some());
    drop(state);

    let reopened = FinalizedState::new_with_debug(&config, &network, true, true).unwrap();
    let ancestors = crate::service::block_iter::known_chain_ancestor_iter::<Block>(
        None,
        &reopened.db,
        tip_height.into(),
    );
    assert_eq!(
        crate::service::check::legacy_chain(
            NetworkUpgrade::Nu5.activation_height(&network).unwrap(),
            ancestors,
            &network,
            crate::constants::MAX_LEGACY_CHAIN_BLOCKS,
        )
        .unwrap(),
        Some(witness_height)
    );
}

#[test]
fn pruned_subtree_checks_still_reject_missing_or_incorrect_roots() {
    use super::super::disk_format::upgrade::add_subtrees::subtree_format_calculation_pre_checks;
    use hex_literal::hex;
    use zebra_chain::subtree::NoteCommitmentSubtree;

    let _init_guard = zebra_test::init();
    let state = populated(&Config::ephemeral());
    let tx = state
        .db
        .block(Height(3).into())
        .unwrap()
        .unwrap()
        .transactions[0]
        .clone();
    let mut batch = DiskWriteBatch::new();
    let floor = Height(2_000_000);
    delete_body_prefix(&mut batch, &state.db, floor);
    batch.zs_insert(
        &state.db.cf_handle("tx_by_loc").unwrap(),
        TransactionLocation::min_for_height(floor),
        tx,
    );
    state.db.write(batch).unwrap();
    assert!(subtree_format_calculation_pre_checks(&state.db).is_err());

    let mut sapling = NoteCommitmentSubtree {
        index: 0.into(),
        end_height: Height(558_822),
        root: sapling_crypto::Node::from_bytes(hex!(
            "754bb593ea42d231a7ddf367640f09bbf59dc00f2c1d2003cc340e0c016b5b13"
        ))
        .unwrap(),
    };
    let mut orchard = NoteCommitmentSubtree {
        index: 0.into(),
        end_height: Height(1_707_429),
        root: hex!("d4e323b3ae0cabfb6be4087fec8c66d9a9bbfc354bf1d9588b6620448182063b")
            .as_slice()
            .try_into()
            .unwrap(),
    };
    let mut batch = DiskWriteBatch::new();
    batch.insert_sapling_subtree(&state.db, &sapling);
    batch.insert_orchard_subtree(&state.db, &orchard);
    state.db.write(batch).unwrap();
    assert!(subtree_format_calculation_pre_checks(&state.db).is_ok());

    sapling.root = sapling_crypto::Node::from_bytes([0; 32]).unwrap();
    let mut batch = DiskWriteBatch::new();
    batch.insert_sapling_subtree(&state.db, &sapling);
    state.db.write(batch).unwrap();
    assert!(subtree_format_calculation_pre_checks(&state.db)
        .unwrap_err()
        .contains("sapling: Err"));

    orchard.root = [0; 32].as_slice().try_into().unwrap();
    let mut batch = DiskWriteBatch::new();
    batch.insert_orchard_subtree(&state.db, &orchard);
    state.db.write(batch).unwrap();
    assert!(subtree_format_calculation_pre_checks(&state.db)
        .unwrap_err()
        .contains("orchard: Err"));
}

/// Reset never removes a completion committed after its captured migration tip.
#[test]
fn subtree_reset_preserves_concurrent_completions() {
    use super::super::disk_format::upgrade::add_subtrees::reset;
    use zebra_chain::{
        orchard, sapling,
        sapling::tree::legacy::{
            LegacyFrontier, LegacyLeaf, LegacyNonEmptyFrontier, LegacyPosition,
        },
        subtree::{NoteCommitmentSubtree, TRACKED_SUBTREE_HEIGHT},
    };

    let _init_guard = zebra_test::init();
    let bytes = |position, leaf, ommers| {
        let frontier = LegacyFrontier::<[u8; 32], 32> {
            frontier: Some(LegacyNonEmptyFrontier {
                position: LegacyPosition(position),
                leaf,
                ommers,
            }),
        };
        bincode::serialize(&(frontier, None::<[u8; 32]>)).unwrap()
    };
    for completed in [0u16, 1] {
        let state = populated(&Config::ephemeral());
        let captured = Height(3);
        let mut batch = DiskWriteBatch::new();
        let sapling = NoteCommitmentSubtree {
            index: completed.into(),
            end_height: Height(4),
            root: sapling_crypto::Node::from_bytes([0; 32]).unwrap(),
        };
        let orchard: NoteCommitmentSubtree<orchard::tree::Node> = NoteCommitmentSubtree {
            index: completed.into(),
            end_height: Height(4),
            root: [0; 32].as_slice().try_into().unwrap(),
        };
        if completed == 1 {
            // Exactly one completed subtree must be reset, including its final index.
            let encoded = bytes(
                (1 << TRACKED_SUBTREE_HEIGHT) - 1,
                LegacyLeaf::Right([0; 32], [0; 32]),
                vec![[0; 32]; usize::from(TRACKED_SUBTREE_HEIGHT) - 1],
            );
            let sapling_tree: sapling::tree::NoteCommitmentTree =
                bincode::deserialize(&encoded).unwrap();
            let orchard_tree: orchard::tree::NoteCommitmentTree =
                bincode::deserialize(&encoded).unwrap();
            assert!(sapling_tree.is_complete_subtree());
            assert!(orchard_tree.is_complete_subtree());
            batch.create_sapling_tree(&state.db, &captured, &sapling_tree);
            batch.create_orchard_tree(&state.db, &captured, &orchard_tree);
            batch.insert_sapling_subtree(
                &state.db,
                &NoteCommitmentSubtree {
                    index: 0.into(),
                    end_height: captured,
                    ..sapling
                },
            );
            batch.insert_orchard_subtree(
                &state.db,
                &NoteCommitmentSubtree {
                    index: 0.into(),
                    end_height: captured,
                    ..orchard
                },
            );
        }
        let encoded = bytes(
            (usize::from(completed) + 1) << TRACKED_SUBTREE_HEIGHT,
            LegacyLeaf::Left([0; 32]),
            vec![[0; 32]],
        );
        let sapling_tree: sapling::tree::NoteCommitmentTree =
            bincode::deserialize(&encoded).unwrap();
        let orchard_tree: orchard::tree::NoteCommitmentTree =
            bincode::deserialize(&encoded).unwrap();
        batch.create_sapling_tree(&state.db, &Height(4), &sapling_tree);
        batch.create_orchard_tree(&state.db, &Height(4), &orchard_tree);
        batch.insert_sapling_subtree(&state.db, &sapling);
        batch.insert_orchard_subtree(&state.db, &orchard);
        state.db.write_batch(batch).unwrap();

        let (_cancel, receiver) = crossbeam_channel::bounded(1);
        reset(captured, &state.db, &receiver).unwrap();
        assert_eq!(state.db.sapling_subtree_by_index(completed), Some(sapling));
        assert_eq!(state.db.orchard_subtree_by_index(completed), Some(orchard));
        if completed == 1 {
            assert!(state.db.sapling_subtree_by_index(0).is_none());
            assert!(state.db.orchard_subtree_by_index(0).is_none());
        }
    }
}

/// Once pruning may run, it can delete the completing block's body between the availability check
/// and the body read, so the subtree checks must use the stored subtree even when bodies are retained.
#[test]
fn subtree_checks_use_stored_subtrees_once_pruning_may_run() {
    use super::super::disk_format::upgrade::add_subtrees::subtree_format_calculation_pre_checks;
    use hex_literal::hex;
    use zebra_chain::subtree::NoteCommitmentSubtree;

    let _init_guard = zebra_test::init();
    let config = Config {
        prune_blocks_older_than: Some(10_000),
        ..Config::ephemeral()
    };
    let state = populated(&config);
    assert!(state
        .db
        .history_capabilities()
        .unwrap()
        .has_block_body(Height(1_707_429)));

    let mut sapling = NoteCommitmentSubtree {
        index: 0.into(),
        end_height: Height(558_822),
        root: sapling_crypto::Node::from_bytes([0; 32]).unwrap(),
    };
    let mut orchard = NoteCommitmentSubtree {
        index: 0.into(),
        end_height: Height(1_707_429),
        root: [0; 32].as_slice().try_into().unwrap(),
    };
    let mut batch = DiskWriteBatch::new();
    batch.insert_sapling_subtree(&state.db, &sapling);
    batch.insert_orchard_subtree(&state.db, &orchard);
    state.db.write(batch).unwrap();

    // Before startup checks finish, pruning cannot run, so the early-chain calculation is used.
    assert!(subtree_format_calculation_pre_checks(&state.db).is_ok());

    state.db.mark_ready_for_pruning();
    let error = subtree_format_calculation_pre_checks(&state.db).unwrap_err();
    assert!(error.contains("sapling: Err"));
    assert!(error.contains("orchard: Err"));

    sapling.root = sapling_crypto::Node::from_bytes(hex!(
        "754bb593ea42d231a7ddf367640f09bbf59dc00f2c1d2003cc340e0c016b5b13"
    ))
    .unwrap();
    orchard.root = hex!("d4e323b3ae0cabfb6be4087fec8c66d9a9bbfc354bf1d9588b6620448182063b")
        .as_slice()
        .try_into()
        .unwrap();
    let mut batch = DiskWriteBatch::new();
    batch.insert_sapling_subtree(&state.db, &sapling);
    batch.insert_orchard_subtree(&state.db, &orchard);
    state.db.write(batch).unwrap();
    assert!(subtree_format_calculation_pre_checks(&state.db).is_ok());

    // Before the first subtrees complete, nothing is stored and the check still passes.
    let config = Config {
        prune_blocks_older_than: Some(10_000),
        ..Config::ephemeral()
    };
    let state = populated(&config);
    state.db.mark_ready_for_pruning();
    assert!(subtree_format_calculation_pre_checks(&state.db).is_ok());
}

/// Finalization can complete the first subtree between its initial lookup and the tip-tree read.
#[test]
fn subtree_checks_allow_completion_after_missing_subtree_lookup() {
    use super::super::disk_format::upgrade::add_subtrees::{
        subtree_format_calculation_pre_checks, AFTER_MISSING_ORCHARD_SUBTREE,
        AFTER_MISSING_SAPLING_SUBTREE,
    };
    use hex_literal::hex;
    use zebra_chain::{
        orchard, sapling,
        sapling::tree::legacy::{
            LegacyFrontier, LegacyLeaf, LegacyNonEmptyFrontier, LegacyPosition,
        },
        subtree::{NoteCommitmentSubtree, TRACKED_SUBTREE_HEIGHT},
    };

    let _init_guard = zebra_test::init();
    let config = Config {
        prune_blocks_older_than: Some(10_000),
        ..Config::ephemeral()
    };
    let state = populated(&config);
    state.db.mark_ready_for_pruning();
    assert!(state.db.sapling_subtree_by_index(0).is_none());
    assert!(state.db.orchard_subtree_by_index(0).is_none());
    assert!(state.db.sapling_tree_for_tip().subtree_index().is_none());
    assert!(state.db.orchard_tree_for_tip().subtree_index().is_none());

    let sapling = NoteCommitmentSubtree {
        index: 0.into(),
        end_height: Height(558_822),
        root: sapling_crypto::Node::from_bytes(hex!(
            "754bb593ea42d231a7ddf367640f09bbf59dc00f2c1d2003cc340e0c016b5b13"
        ))
        .unwrap(),
    };
    let orchard: NoteCommitmentSubtree<orchard::tree::Node> = NoteCommitmentSubtree {
        index: 0.into(),
        end_height: Height(1_707_429),
        root: hex!("d4e323b3ae0cabfb6be4087fec8c66d9a9bbfc354bf1d9588b6620448182063b")
            .as_slice()
            .try_into()
            .unwrap(),
    };
    // Both tree types use this legacy encoding. One completed subtree followed by a note
    // needs only the known subtree root as its ommer, not all of the preceding commitments.
    let tree_bytes = |root| {
        let frontier = LegacyFrontier::<[u8; 32], 32> {
            frontier: Some(LegacyNonEmptyFrontier {
                position: LegacyPosition(1 << TRACKED_SUBTREE_HEIGHT),
                leaf: LegacyLeaf::Left([0; 32]),
                ommers: vec![root],
            }),
        };
        bincode::serialize(&(frontier, None::<[u8; 32]>)).unwrap()
    };
    let sapling_tree: sapling::tree::NoteCommitmentTree =
        bincode::deserialize(&tree_bytes(sapling.root.to_bytes())).unwrap();
    let orchard_tree: orchard::tree::NoteCommitmentTree =
        bincode::deserialize(&tree_bytes(orchard.root.to_repr())).unwrap();
    assert_eq!(sapling_tree.subtree_index(), Some(1.into()));
    assert_eq!(orchard_tree.subtree_index(), Some(1.into()));

    let writer = state.db.clone();
    AFTER_MISSING_SAPLING_SUBTREE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let mut batch = DiskWriteBatch::new();
            batch.create_sapling_tree(&writer, &sapling.end_height, &sapling_tree);
            batch.insert_sapling_subtree(&writer, &sapling);
            batch.zs_insert(
                &writer.cf_handle("hash_by_height").unwrap(),
                sapling.end_height,
                writer.hash(Height(3)).unwrap(),
            );
            writer.write_batch(batch).unwrap();
        }));
    });
    let writer = state.db.clone();
    AFTER_MISSING_ORCHARD_SUBTREE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let mut batch = DiskWriteBatch::new();
            batch.create_orchard_tree(&writer, &orchard.end_height, &orchard_tree);
            batch.insert_orchard_subtree(&writer, &orchard);
            batch.zs_insert(
                &writer.cf_handle("hash_by_height").unwrap(),
                orchard.end_height,
                writer.hash(Height(3)).unwrap(),
            );
            writer.write_batch(batch).unwrap();
        }));
    });

    let result = subtree_format_calculation_pre_checks(&state.db);
    assert!(AFTER_MISSING_SAPLING_SUBTREE.with(|hook| hook.borrow_mut().take().is_none()));
    assert!(AFTER_MISSING_ORCHARD_SUBTREE.with(|hook| hook.borrow_mut().take().is_none()));
    assert_eq!(result, Ok(()));
}

#[test]
fn recent_address_checks_reject_an_unavailable_required_window() {
    use super::super::disk_format::upgrade::{
        block_info_and_address_received::Upgrade, DiskFormatUpgrade,
    };

    let _init_guard = zebra_test::init();
    let state = populated(&Config::ephemeral());
    let mut batch = DiskWriteBatch::new();
    delete_body_prefix(&mut batch, &state.db, Height(2));
    state.db.write(batch).unwrap();
    let (_sender, receiver) = crossbeam_channel::bounded(1);
    let error = Upgrade.validate(&state.db, &receiver).unwrap().unwrap_err();
    assert!(error.contains("require unavailable block history"));
}
