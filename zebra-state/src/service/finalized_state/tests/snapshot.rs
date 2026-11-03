//! A finalized database imported from a state snapshot: history starts at the snapshot window.

use zebra_chain::block::{self, Height};

use super::super::{DiskWriteBatch, FinalizedState, TransactionLocation, WriteDisk};
use crate::{
    tests::snapshot_db::{config, open, snapshot_state, write_marker, Coinbase, RETENTION},
    HistoryCapabilities, SnapshotBase, StateInitError,
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
