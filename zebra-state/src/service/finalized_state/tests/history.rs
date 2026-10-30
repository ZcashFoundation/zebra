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
    batch.prepare_history_pruning(&state.db, Height(2)).unwrap();
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
    batch.prepare_history_pruning(&state.db, Height(3)).unwrap();
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
        batch.prepare_history_pruning(&state.db, Height(2)).unwrap();
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
        batch
            .prepare_history_pruning(&secondary, Height(3))
            .unwrap();
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
