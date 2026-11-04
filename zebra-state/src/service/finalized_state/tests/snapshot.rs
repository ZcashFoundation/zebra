//! A finalized database imported from a state snapshot: history starts at the snapshot window.

use std::{collections::HashMap, sync::Arc};

use zebra_chain::{
    block::{self, Height},
    transaction,
};

use super::super::{DiskWriteBatch, FinalizedState, TransactionLocation, WriteDisk};
use crate::{
    service::{non_finalized_state::Chain, read},
    tests::{
        snapshot_db::{
            append, carve, commit_child, config, open, snapshot_state, spend_coinbase_of,
            write_marker, Coinbase, RETENTION,
        },
        FakeChainHelper,
    },
    BlockReadError, HistoryCapabilities, SnapshotBase, StateInitError,
};

/// The snapshot height `H` of the test chains.
const TIP: u32 = 60;

/// The window start `L` of the test chains: enough headers for the difficulty context.
const WINDOW_START: u32 = 21;

#[test]
fn snapshot_marker_sets_the_history_capabilities() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let (state, base) = snapshot_state(
        cache.path().to_path_buf(),
        TIP,
        WINDOW_START,
        Coinbase::Branched,
        0,
    );
    assert_eq!(state.db.snapshot_base(), Some(base));

    let history = state.db.history_capabilities().unwrap();
    assert_eq!(
        history,
        HistoryCapabilities {
            lowest_retained_height: Height(WINDOW_START),
            lowest_known_height: Height(WINDOW_START),
            snapshot_base: Some(base),
        }
    );
    assert!(history.is_pruned());
    assert!(history.knows_block(Height(0)));
    assert!(!history.knows_block(Height(WINDOW_START - 1)));
    assert!(history.knows_block(Height(WINDOW_START)));
    assert!(history.has_block_body(Height(WINDOW_START)));
    assert!(!history.has_block_body(Height(WINDOW_START - 1)));

    // Pruning bodies above the window start raises the floor, but the window start stays known.
    let mut batch = DiskWriteBatch::new();
    batch.zs_delete_range(
        &state.db.cf_handle("tx_by_loc").unwrap(),
        TransactionLocation::min_for_height(Height(WINDOW_START)),
        TransactionLocation::min_for_height(Height(WINDOW_START + 3)),
    );
    state.db.write_batch(batch).unwrap();
    let history = state.db.history_capabilities().unwrap();
    assert_eq!(history.lowest_retained_height, Height(WINDOW_START + 3));
    assert_eq!(history.lowest_known_height, Height(WINDOW_START));
    assert_eq!(history.snapshot_base, Some(base));
}

#[test]
fn snapshot_databases_are_isolated_from_30_0_readers() {
    use crate::{
        constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
        service::finalized_state::{
            CancelFormatChange, DbFormatChange, ZebraDb, STATE_COLUMN_FAMILIES_IN_CODE,
        },
        tests::snapshot_db::network,
    };

    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let (state, base) = snapshot_state(
        cache.path().to_path_buf(),
        TIP,
        WINDOW_START,
        Coinbase::Branched,
        0,
    );
    let snapshot_path = state.db.path().to_path_buf();
    let snapshot_tip = state.db.tip();
    let snapshot_version = state.db.format_version_on_disk().unwrap().unwrap();
    drop(state);

    let config = config(cache.path().to_path_buf(), Some(RETENTION));
    let mut old_version = semver::Version::new(30, 0, 0);
    old_version.build = snapshot_version.build.clone();
    let old_columns = || {
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .copied()
            .filter(|name| *name != "snapshot_base")
            .map(str::to_owned)
    };
    let old = ZebraDb::new(
        &config,
        STATE_DATABASE_KIND,
        &old_version,
        &network(),
        true,
        old_columns(),
        false,
    )
    .unwrap();
    let old_path = old.path().to_path_buf();
    let old_tip = old.tip();
    let disk_version = old.format_version_on_disk().unwrap();
    let change = DbFormatChange::open_database(&old_version, disk_version);
    let (cancel, cancelled) = crossbeam_channel::bounded(1);
    cancel.send(CancelFormatChange).unwrap();
    let _ = change.run_format_change_or_check(&old, old_tip.map(|tip| tip.0), &cancelled);
    drop(old);

    let current = ZebraDb::new(
        &config,
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        &network(),
        true,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    )
    .unwrap();
    assert_eq!(
        current.format_version_on_disk().unwrap(),
        Some(snapshot_version),
        "the old open must not downgrade the snapshot version"
    );
    assert_eq!(old_tip, None, "30.0 must not read the snapshot chain");
    assert_ne!(
        old_path, snapshot_path,
        "30.0 must not open the snapshot database directory"
    );
    assert_eq!(current.tip(), snapshot_tip);
    assert_eq!(current.snapshot_base(), Some(base));
}

#[test]
fn snapshot_30_1_database_moves_to_v32_without_record_changes() {
    use std::time::{Duration, Instant};

    use crate::{
        constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
        service::finalized_state::{ZebraDb, STATE_COLUMN_FAMILIES_IN_CODE},
        tests::snapshot_db::network,
        write_database_format_version_to_disk,
    };

    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let (state, base) = snapshot_state(
        cache.path().to_path_buf(),
        TIP,
        WINDOW_START,
        Coinbase::Branched,
        0,
    );
    let snapshot_tip = state.db.tip();
    let current_path = state.db.path().to_path_buf();
    drop(state);

    let network = network();
    let config = config(cache.path().to_path_buf(), Some(RETENTION));
    let legacy_path = config.db_path(STATE_DATABASE_KIND, 30, &network);
    std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
    std::fs::rename(current_path, &legacy_path).unwrap();
    let mut legacy_version = semver::Version::new(30, 1, 0);
    legacy_version.build = state_database_format_version_in_code().build;
    write_database_format_version_to_disk(
        &config,
        STATE_DATABASE_KIND,
        30,
        &legacy_version,
        &network,
    )
    .unwrap();

    let running_version = state_database_format_version_in_code();
    let mut db = ZebraDb::new(
        &config,
        STATE_DATABASE_KIND,
        &running_version,
        &network,
        false,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !db.ready_for_pruning() {
        db.check_for_panics();
        assert!(Instant::now() < deadline, "the v32 upgrade did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(db.path(), config.db_path(STATE_DATABASE_KIND, 32, &network));
    assert!(!legacy_path.exists());
    assert_eq!(db.tip(), snapshot_tip);
    assert_eq!(db.snapshot_base(), Some(base));
    assert_eq!(db.format_version_on_disk().unwrap(), Some(running_version));
}

#[test]
fn snapshot_marker_must_be_consistent_with_the_stored_chain() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let (state, base) = snapshot_state(
        cache.path().to_path_buf(),
        TIP,
        WINDOW_START,
        Coinbase::Branched,
        0,
    );
    for damaged in [
        SnapshotBase {
            block_hash: block::Hash([0xff; 32]),
            ..base
        },
        SnapshotBase {
            window_start: Height(TIP + 1),
            ..base
        },
        SnapshotBase {
            window_start: Height(0),
            ..base
        },
    ] {
        let mut batch = DiskWriteBatch::new();
        write_marker(&mut batch, &state.db, &damaged);
        state.db.write_batch(batch).unwrap();
        assert!(
            matches!(
                state.db.history_capabilities(),
                Err(StateInitError::HistoryRead { .. })
            ),
            "{damaged:?} must be rejected"
        );
    }
}

#[test]
fn reads_below_the_snapshot_window_are_unavailable() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let mut state = open(&config(cache.path().to_path_buf(), Some(RETENTION))).unwrap();
    append(&mut state, TIP - 1, Coinbase::Branched);
    let spent_height = Height(WINDOW_START - 2);
    let spent = state.db.block(spent_height.into()).unwrap().unwrap();
    let spend = spend_coinbase_of(&state.db, spent_height, Height(TIP));
    let spending = commit_child(&mut state, Coinbase::Branched, vec![spend]);
    carve(&state, Height(WINDOW_START), 0);

    // Blocks below the window are unknown, and their indexed transactions are unavailable.
    assert_eq!(state.db.block(spent_height.into()).unwrap(), None);
    assert_eq!(state.db.block(spent.hash().into()).unwrap(), None);
    assert_eq!(
        state
            .db
            .transaction(spent.transactions[0].hash())
            .unwrap_err(),
        BlockReadError::BlockBodyUnavailable {
            height: spent_height
        }
    );
    assert_eq!(
        state.db.transaction(transaction::Hash([0xff; 32])).unwrap(),
        None
    );
    assert_eq!(
        state
            .db
            .transaction(spending.transactions[0].hash())
            .unwrap()
            .unwrap()
            .1,
        Height(TIP)
    );

    // A window block spending below the window has no readable spent outputs, whether the
    // parent is still indexed because another of its outputs is unspent ...
    let no_chain = None::<Arc<Chain>>;
    assert_eq!(
        read::spent_outputs_for_block(no_chain.clone(), &state.db, spending.hash().into())
            .unwrap_err(),
        BlockReadError::BlockBodyUnavailable {
            height: spent_height
        }
    );
    // ... or dropped from the index because none of them is.
    let mut batch = DiskWriteBatch::new();
    batch.zs_delete(
        &state.db.cf_handle("tx_loc_by_hash").unwrap(),
        spent.transactions[0].hash(),
    );
    state.db.write_batch(batch).unwrap();
    assert_eq!(
        read::spent_outputs_for_block(no_chain.clone(), &state.db, spending.hash().into())
            .unwrap_err(),
        BlockReadError::BlockBodyUnavailable {
            height: Height(TIP)
        }
    );

    // Without a marker, an unknown parent is a reorg or a lagging view, and is skipped.
    let mut batch = DiskWriteBatch::new();
    batch.zs_delete(&state.db.cf_handle("snapshot_base").unwrap(), ());
    state.db.write_batch(batch).unwrap();
    assert_eq!(
        read::spent_outputs_for_block(no_chain, &state.db, spending.hash().into()).unwrap(),
        Some(HashMap::new())
    );
}

/// Builds a snapshot database with both deferred checks pending, and the valid block `H + 1`.
fn snapshot_with_deferred_checks(
    cache_dir: std::path::PathBuf,
) -> (FinalizedState, SnapshotBase, Arc<zebra_chain::block::Block>) {
    use crate::tests::snapshot_db::{child, tip_history_root};

    let (state, base) = snapshot_state(
        cache_dir,
        TIP,
        WINDOW_START,
        Coinbase::Branched,
        SnapshotBase::DEFERRED_BLOCK_HASH | SnapshotBase::DEFERRED_SUCCESSOR_COMMITMENTS,
    );
    let tip = state.db.tip_block().unwrap().unwrap();
    let successor = child(
        &tip,
        tip_history_root(&state.db),
        Coinbase::Branched,
        vec![],
    );
    (state, base, successor)
}

#[test]
fn checkpoint_commit_above_the_snapshot_discharges_both_deferred_checks() {
    use crate::{CheckpointVerifiedBlock, ValidateContextError};

    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let (mut state, base, successor) = snapshot_with_deferred_checks(cache.path().to_path_buf());

    // A successor that does not commit to the snapshot's history tree is rejected, and the
    // marker keeps both checks pending.
    let tampered = successor.clone().set_block_commitment([0xaa; 32]);
    let error = state
        .commit_finalized_direct(
            CheckpointVerifiedBlock::from(tampered).into(),
            None,
            "deferred snapshot checks test",
        )
        .unwrap_err();
    assert!(
        matches!(
            error.inner(),
            crate::CommitBlockError::ValidateContextError(inner)
                if matches!(**inner, ValidateContextError::InvalidBlockCommitment(_))
        ),
        "unexpected error: {error}"
    );
    assert_eq!(state.db.finalized_tip_height(), Some(base.height));
    assert_eq!(state.db.snapshot_base(), Some(base));

    // The valid successor commits, verifying the history tree and, because the checkpoint
    // verifier anchored it to `hash(H)`, the snapshot block hash too.
    state
        .commit_finalized_direct(
            CheckpointVerifiedBlock::from(successor.clone()).into(),
            None,
            "deferred snapshot checks test",
        )
        .unwrap();
    let discharged = SnapshotBase {
        deferred_checks: 0,
        ..base
    };
    assert_eq!(state.db.finalized_tip_height(), Some(Height(TIP + 1)));
    assert_eq!(state.db.snapshot_base(), Some(discharged));
    assert_eq!(
        state.db.history_capabilities().unwrap().snapshot_base,
        Some(discharged)
    );

    // Later commits leave the marker alone.
    commit_child(&mut state, Coinbase::Branched, vec![]);
    assert_eq!(state.db.snapshot_base(), Some(discharged));
}

#[test]
fn contextual_commit_above_the_snapshot_leaves_the_block_hash_check_pending() {
    use crate::{arbitrary::Prepare, service::non_finalized_state::NonFinalizedState};

    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let (mut state, base, successor) = snapshot_with_deferred_checks(cache.path().to_path_buf());

    let mut non_finalized = NonFinalizedState::new(&crate::tests::snapshot_db::network());
    non_finalized
        .commit_new_chain(successor.prepare(), &state.db)
        .unwrap();
    state
        .commit_finalized_direct(
            non_finalized.finalize(),
            None,
            "deferred snapshot checks test",
        )
        .unwrap();
    assert_eq!(state.db.finalized_tip_height(), Some(Height(TIP + 1)));
    assert_eq!(
        state.db.snapshot_base(),
        Some(SnapshotBase {
            deferred_checks: SnapshotBase::DEFERRED_BLOCK_HASH,
            ..base
        })
    );
}

/// The state service starts on a snapshot database whose window has no branch ID transaction:
/// the legacy-chain check reaches the snapshot base, and the startup format checks accept the
/// snapshot shape.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn state_service_starts_on_a_snapshot_database_with_a_branchless_window() {
    use std::time::{Duration, Instant};

    use zebra_chain::chain_tip::ChainTip;

    use crate::{service::StateService, tests::snapshot_db::network};

    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(90), async {
        let cache = tempfile::tempdir().unwrap();
        let config = config(cache.path().to_path_buf(), Some(RETENTION));
        let cache_dir = config.cache_dir.clone();
        tokio::task::spawn_blocking(move || {
            drop(snapshot_state(
                cache_dir,
                TIP,
                WINDOW_START,
                Coinbase::Legacy,
                0,
            ))
        })
        .await
        .unwrap();

        let (state, reader, tip, _) = StateService::new(config, &network(), Height(TIP), 1).await;
        assert_eq!(tip.best_tip_height(), Some(Height(TIP)));
        let mut db = reader.db.clone();
        tokio::task::spawn_blocking(move || {
            let deadline = Instant::now() + Duration::from_secs(60);
            while !db.ready_for_pruning() {
                db.check_for_panics();
                assert!(
                    Instant::now() < deadline,
                    "the startup format checks did not finish"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        })
        .await
        .unwrap();
        tokio::task::spawn_blocking(move || drop(state))
            .await
            .unwrap();
    })
    .await
    .unwrap();
}

#[test]
fn snapshot_database_requires_pruning() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let (state, base) = snapshot_state(
        cache.path().to_path_buf(),
        TIP,
        WINDOW_START,
        Coinbase::Branched,
        0,
    );
    drop(state);

    let archive = config(cache.path().to_path_buf(), None);
    let error = open(&archive).unwrap_err();
    assert!(
        matches!(
            error,
            StateInitError::SnapshotDatabase { height, window_start, .. }
                if height == base.height && window_start == base.window_start
        ),
        "unexpected error: {error}"
    );

    // Readers follow the primary's configuration, so they open without pruning.
    let reader = FinalizedState::new_with_debug(
        &archive,
        &crate::tests::snapshot_db::network(),
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        true,
    )
    .unwrap();
    assert_eq!(reader.db.snapshot_base(), Some(base));
    drop(reader);

    let pruned = open(&config(cache.path().to_path_buf(), Some(RETENTION))).unwrap();
    assert_eq!(pruned.db.snapshot_base(), Some(base));
}
