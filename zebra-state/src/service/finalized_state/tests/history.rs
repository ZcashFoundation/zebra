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
    FinalizedState::new_with_debug(
        config,
        &Network::Mainnet,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        read_only,
    )
}

fn populated(config: &Config) -> FinalizedState {
    let mut state = open(config, false).unwrap();
    state
        .db
        .update_format_version_on_disk(&state_database_format_version_in_code())
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
fn pruning_without_a_restart_witness_is_refused() {
    let _init_guard = zebra_test::init();
    let state = populated(&Config::ephemeral());
    state.db.mark_ready_for_pruning();
    let mut batch = DiskWriteBatch::new();
    assert!(batch.prepare_history_pruning(&state.db, Height(2)).is_err());
    assert_eq!(batch, DiskWriteBatch::new());
    assert!(state.db.block(Height(1).into()).unwrap().is_some());
}

#[test]
fn history_floor_and_deleted_bodies_share_a_snapshot_and_survive_reopen() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let state = populated(&config);
    let before = state.db.snapshot();
    let tx_cf = state.db.cf_handle("tx_by_loc").unwrap();
    let first_deleted = TransactionLocation::min_for_height(Height(1)).as_bytes();
    let mut batch = DiskWriteBatch::new();
    delete_body_prefix(&mut batch, &state.db, Height(2));
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

    for invalid_floor in [Height(0), Height(1), Height(2), Height(4), Height::MAX] {
        let mut invalid = DiskWriteBatch::new();
        assert!(invalid
            .prepare_history_pruning(&state.db, invalid_floor)
            .is_err());
        assert_eq!(invalid, DiskWriteBatch::new());
    }
    let mut batch = DiskWriteBatch::new();
    delete_body_prefix(&mut batch, &state.db, Height(3));
    state.db.write(batch).unwrap();
    drop(state);

    let state = open(&config, true).unwrap();
    let history = state.db.history_capabilities().unwrap();
    assert!(history.is_pruned());
    assert_eq!(history.lowest_retained_height, Height(3));
    let tx_cf = state.db.cf_handle("tx_by_loc").unwrap();
    for height in 0..=3 {
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
        FinalizedState::new(
            &config,
            &Network::Mainnet,
            #[cfg(feature = "elasticsearch")]
            false
        ),
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

#[tokio::test]
async fn read_only_capability_requests_do_not_mutate_history() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let cache = tempfile::tempdir().unwrap();
        let config = Config {
            cache_dir: cache.path().to_path_buf(),
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

/// Existing secondaries must survive archive writer restarts without a new column family.
#[test]
fn legacy_secondary_survives_archive_writer_restarts() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let network = Network::Mainnet;
    let version = semver::Version::new(29, 0, 0);
    let old_columns = || {
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .copied()
            .filter(|name| *name != "history_capabilities")
            .map(str::to_owned)
    };
    let mut state = FinalizedState {
        db: ZebraDb::new(
            &config,
            STATE_DATABASE_KIND,
            &version,
            &network,
            true,
            old_columns(),
            false,
        )
        .unwrap(),
        debug_stop_at_height: None,
        #[cfg(feature = "elasticsearch")]
        elastic_db: None,
        #[cfg(feature = "elasticsearch")]
        elastic_blocks: vec![],
    };
    state.db.update_format_version_on_disk(&version).unwrap();
    let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap();
    state
        .commit_finalized_direct(
            CheckpointVerifiedBlock::from(genesis).into(),
            None,
            "legacy archive restart test",
        )
        .unwrap();
    let expected_tip = state.db.tip();
    let expected_body = state.db.block(Height::MIN.into());
    let expected_path = state.db.path().to_path_buf();
    drop(state);

    let secondary = ZebraDb::new(
        &config,
        STATE_DATABASE_KIND,
        &version,
        &network,
        true,
        old_columns(),
        true,
    )
    .unwrap();
    for _ in 0..2 {
        let db = ZebraDb::new(
            &config,
            STATE_DATABASE_KIND,
            &version,
            &network,
            true,
            STATE_COLUMN_FAMILIES_IN_CODE
                .iter()
                .map(ToString::to_string),
            false,
        )
        .unwrap();
        assert_eq!(db.format_version_on_disk().unwrap(), Some(version.clone()));
        assert_eq!(db.path(), expected_path);
        assert_eq!(db.tip(), expected_tip);
        assert_eq!(db.block(Height::MIN.into()), expected_body);
        drop(db);
        secondary.try_catch_up_with_primary().unwrap();
        assert!(secondary.cached_size() > 0);
        secondary.print_db_metrics();
        secondary.export_metrics();
        assert_eq!(secondary.tip(), expected_tip);
        assert_eq!(secondary.block(Height::MIN.into()), expected_body);
        assert_eq!(
            secondary.history_capabilities().unwrap(),
            HistoryCapabilities::default()
        );
    }
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

#[test]
fn pruning_retains_a_restart_witness_fifteen_thousand_blocks_back() {
    use zebra_chain::{
        parameters::{testnet::ConfiguredActivationHeights, NetworkUpgrade},
        transaction::{LockTime, Transaction},
        transparent::Input,
    };

    let _init_guard = zebra_test::init();
    let network = Network::new_regtest(zebra_chain::parameters::testnet::RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            nu5: Some(1),
            ..Default::default()
        },
        ..Default::default()
    });
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let state = FinalizedState::new_with_debug(
        &config,
        &network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )
    .unwrap();
    state
        .db
        .update_format_version_on_disk(&state_database_format_version_in_code())
        .unwrap();
    let witness_height = Height(3);
    let tip_height = Height(witness_height.0 + 15_000);
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

    // The fixture writes block indexes directly; readiness is tested separately above.
    let mut batch = DiskWriteBatch::new();
    assert!(batch
        .prepare_history_pruning(&state.db, Height(5_004))
        .is_err());
    assert_eq!(batch, DiskWriteBatch::new());
    state.db.mark_ready_for_pruning();
    batch
        .prepare_history_pruning(&state.db, Height(5_004))
        .unwrap();
    state.db.write(batch).unwrap();
    assert_eq!(
        state
            .db
            .history_capabilities()
            .unwrap()
            .lowest_retained_height,
        witness_height
    );
    drop(state);

    let reopened = FinalizedState::new_with_debug(
        &config,
        &network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        true,
    )
    .unwrap();
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
