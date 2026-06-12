//! Tests for the finalized bulk-write phase guard.

use std::sync::{Arc, Mutex};

use zebra_chain::{
    block::{Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
};

use crate::{service::finalized_state::FinalizedState, Config};

use super::{
    FinalizedWriteDb, FinalizedWritePhase, L0_CHECK_INTERVAL_BLOCKS, L0_DRAINED_FILE_LIMIT,
    L0_DRAIN_FILE_LIMIT,
};

/// A database operation recorded by [`MockDb`].
#[derive(Clone, Debug, PartialEq, Eq)]
enum DbCall {
    SetAutoCompaction(bool),
    SetSkipWal(bool),
    Level0FileCount,
    FlushAllColumnFamilies,
}

/// A mock database that records the operations called on it.
#[derive(Clone, Default)]
struct MockDb {
    state: Arc<Mutex<MockDbState>>,
}

#[derive(Default)]
struct MockDbState {
    calls: Vec<DbCall>,
    level0_file_count: u64,
}

impl MockDb {
    fn calls(&self) -> Vec<DbCall> {
        self.state
            .lock()
            .expect("mock state mutex is never poisoned: recording methods don't panic")
            .calls
            .clone()
    }

    fn set_level0_file_count(&self, level0_file_count: u64) {
        self.state
            .lock()
            .expect("mock state mutex is never poisoned: recording methods don't panic")
            .level0_file_count = level0_file_count;
    }

    fn record(&self, call: DbCall) {
        self.state
            .lock()
            .expect("mock state mutex is never poisoned: recording methods don't panic")
            .calls
            .push(call);
    }
}

impl FinalizedWriteDb for MockDb {
    fn set_auto_compaction(&self, enabled: bool) {
        self.record(DbCall::SetAutoCompaction(enabled));
    }

    fn set_skip_wal(&self, skip_wal: bool) {
        self.record(DbCall::SetSkipWal(skip_wal));
    }

    fn level0_file_count(&self) -> u64 {
        self.record(DbCall::Level0FileCount);
        self.state
            .lock()
            .expect("mock state mutex is never poisoned: recording methods don't panic")
            .level0_file_count
    }

    fn flush_all_column_families(&self) {
        self.record(DbCall::FlushAllColumnFamilies);
    }
}

/// The default config must not skip the WAL: it is an explicit opt-in.
#[test]
fn wal_skip_config_defaults_to_off() {
    let _init_guard = zebra_test::init();

    assert!(
        !Config::default().disable_wal_during_ibd,
        "skipping the WAL loses crash durability, so it must be an explicit opt-in",
    );
    assert!(
        !Config::ephemeral().disable_wal_during_ibd,
        "ephemeral test configs must use the production WAL default",
    );
}

/// Creating the guard pauses auto-compaction, and dropping it resumes
/// auto-compaction. Without the config opt-in, the WAL flag is never touched.
#[test]
fn pauses_compaction_on_create_and_resumes_on_drop() {
    let _init_guard = zebra_test::init();

    let db = MockDb::default();

    let phase = FinalizedWritePhase::new(db.clone(), false);
    assert_eq!(db.calls(), vec![DbCall::SetAutoCompaction(false)]);

    drop(phase);
    assert_eq!(
        db.calls(),
        vec![
            DbCall::SetAutoCompaction(false),
            DbCall::SetAutoCompaction(true),
        ],
        "without the WAL opt-in, the guard must only toggle auto-compaction: \
         it must not touch the WAL flag or flush",
    );
}

/// With the config opt-in, the guard skips the WAL for the whole phase, then
/// re-enables the WAL and flushes the database before resuming compaction.
#[test]
fn skips_wal_when_opted_in_and_flushes_on_drop() {
    let _init_guard = zebra_test::init();

    let db = MockDb::default();

    let phase = FinalizedWritePhase::new(db.clone(), true);
    assert_eq!(
        db.calls(),
        vec![DbCall::SetAutoCompaction(false), DbCall::SetSkipWal(true)],
    );

    drop(phase);
    assert_eq!(
        db.calls(),
        vec![
            DbCall::SetAutoCompaction(false),
            DbCall::SetSkipWal(true),
            DbCall::SetSkipWal(false),
            DbCall::FlushAllColumnFamilies,
            DbCall::SetAutoCompaction(true),
        ],
        "the WAL must be re-enabled before the flush, so no write can slip \
         in between the flush and the WAL being re-enabled",
    );
}

/// While the WAL is skipped, the guard flushes the database once the
/// periodic crash-loss interval elapses, and not before.
#[test]
fn wal_skip_flushes_periodically() {
    let _init_guard = zebra_test::init();

    let db = MockDb::default();
    let mut phase = FinalizedWritePhase::new(db.clone(), true);

    // Within the interval: no flush.
    phase.block_committed();
    assert_eq!(
        db.calls(),
        vec![DbCall::SetAutoCompaction(false), DbCall::SetSkipWal(true)],
        "no flush before the crash-loss interval elapses",
    );

    // Pretend the interval elapsed: the next committed block flushes.
    phase.last_wal_skip_flush = std::time::Instant::now() - super::WAL_SKIP_FLUSH_INTERVAL * 2;
    phase.block_committed();
    assert_eq!(
        db.calls(),
        vec![
            DbCall::SetAutoCompaction(false),
            DbCall::SetSkipWal(true),
            DbCall::FlushAllColumnFamilies,
        ],
        "a committed block after the interval triggers a crash-loss flush",
    );

    // The flush timestamp was reset: the next block doesn't flush again.
    phase.block_committed();
    assert_eq!(
        db.calls().len(),
        3,
        "the interval restarts after each periodic flush",
    );
}

/// Without the WAL-skip opt-in, the guard never issues periodic flushes.
#[test]
fn no_periodic_flush_without_wal_skip() {
    let _init_guard = zebra_test::init();

    let db = MockDb::default();
    let mut phase = FinalizedWritePhase::new(db.clone(), false);

    phase.last_wal_skip_flush = std::time::Instant::now() - super::WAL_SKIP_FLUSH_INTERVAL * 2;
    phase.block_committed();

    assert_eq!(
        db.calls(),
        vec![DbCall::SetAutoCompaction(false)],
        "with the WAL on, every write is already durable, so no periodic flush",
    );
}

/// The guard re-enables auto-compaction when the write task panics.
#[test]
fn resumes_compaction_on_panic_unwind() {
    let _init_guard = zebra_test::init();

    let db = MockDb::default();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _phase = FinalizedWritePhase::new(db.clone(), true);
        panic!("simulated write task panic");
    }));
    assert!(
        result.is_err(),
        "the panic must propagate through the guard"
    );

    assert_eq!(
        db.calls().last(),
        Some(&DbCall::SetAutoCompaction(true)),
        "auto-compaction must be re-enabled during the panic unwind",
    );
    assert!(
        db.calls().contains(&DbCall::SetSkipWal(false)),
        "the WAL must be re-enabled during the panic unwind",
    );
}

/// While paused, the guard checks level 0 pressure every
/// [`L0_CHECK_INTERVAL_BLOCKS`] blocks, re-enables auto-compaction over the
/// drain limit, and pauses it again once the backlog drains.
#[test]
fn level0_guard_drains_backlog_then_repauses() {
    let _init_guard = zebra_test::init();

    let db = MockDb::default();
    db.set_level0_file_count(L0_DRAIN_FILE_LIMIT + 1);

    let mut phase = FinalizedWritePhase::new(db.clone(), false);

    // No level 0 checks happen before the block interval is reached.
    for _ in 0..L0_CHECK_INTERVAL_BLOCKS - 1 {
        phase.block_committed();
    }
    assert!(
        !db.calls().contains(&DbCall::Level0FileCount),
        "the level 0 file count must only be checked at the block interval",
    );

    // At the interval, the backlog is over the limit: compaction drains it.
    phase.block_committed();
    assert_eq!(
        db.calls().last(),
        Some(&DbCall::SetAutoCompaction(true)),
        "auto-compaction must be re-enabled to drain the level 0 backlog",
    );

    // While the backlog is above the drained limit, nothing toggles.
    db.set_level0_file_count(L0_DRAINED_FILE_LIMIT + 1);
    for _ in 0..L0_CHECK_INTERVAL_BLOCKS {
        phase.block_committed();
    }
    assert_eq!(
        db.calls().last(),
        Some(&DbCall::Level0FileCount),
        "auto-compaction must stay enabled until the backlog drains below \
         the drained limit, to avoid rapid toggling",
    );

    // Once the backlog drains, compaction is paused again.
    db.set_level0_file_count(L0_DRAINED_FILE_LIMIT);
    for _ in 0..L0_CHECK_INTERVAL_BLOCKS {
        phase.block_committed();
    }
    assert_eq!(
        db.calls().last(),
        Some(&DbCall::SetAutoCompaction(false)),
        "auto-compaction must be paused again once the backlog drains",
    );

    // Dropping the guard still ends with auto-compaction enabled.
    drop(phase);
    assert_eq!(db.calls().last(), Some(&DbCall::SetAutoCompaction(true)));
}

/// A backlog at or below the drain limit doesn't re-enable auto-compaction.
#[test]
fn level0_guard_ignores_backlog_at_or_below_limit() {
    let _init_guard = zebra_test::init();

    let db = MockDb::default();
    db.set_level0_file_count(L0_DRAIN_FILE_LIMIT);

    let mut phase = FinalizedWritePhase::new(db.clone(), false);
    for _ in 0..L0_CHECK_INTERVAL_BLOCKS {
        phase.block_committed();
    }

    assert_eq!(
        db.calls()
            .iter()
            .filter(|call| matches!(call, DbCall::SetAutoCompaction(_)))
            .count(),
        1,
        "a backlog at the drain limit must not re-enable auto-compaction: \
         only the initial pause may change the setting",
    );

    drop(phase);
}

/// Commits real blocks to a real database with the guard active, and checks
/// that auto-compaction is re-enabled after the phase ends.
///
/// RocksDB has no property that reads `disable_auto_compactions` back, so
/// this test probes the in-process flag that mirrors the last successful
/// `set_options_cf` round (see `DiskDb::auto_compaction_disabled`).
#[test]
fn finalized_write_phase_pauses_compaction_for_real_db_commits() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let mut state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );
    let db = state.db.clone();

    assert!(!db.auto_compaction_disabled());
    assert!(!db.skip_wal());

    let mut phase = FinalizedWritePhase::new(db.clone(), db.config().disable_wal_during_ibd);
    assert!(
        db.auto_compaction_disabled(),
        "creating the guard must pause auto-compaction",
    );
    assert!(
        !db.skip_wal(),
        "the WAL flag must only be set when the config opts in",
    );

    let blocks = network.blockchain_map();
    for height in 0..=2 {
        let block: Arc<Block> = blocks
            .get(&height)
            .expect("test vectors include the first blocks of every network")
            .zcash_deserialize_into()
            .expect("test vectors deserialize");

        state
            .commit_finalized_direct(block.into(), None, "finalized write phase test")
            .expect("test vectors are valid blocks");
        phase.block_committed();
    }

    assert_eq!(state.db.finalized_tip_height(), Some(Height(2)));

    drop(phase);
    assert!(
        !db.auto_compaction_disabled(),
        "dropping the guard must resume auto-compaction",
    );
}

/// With the config opt-in, the guard skips the WAL while committing real
/// blocks, and the flushed blocks stay readable after the phase ends.
#[test]
fn finalized_write_phase_skips_wal_with_config_opt_in() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let config = Config {
        disable_wal_during_ibd: true,
        ..Config::ephemeral()
    };
    let mut state = FinalizedState::new(
        &config,
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    );
    let db = state.db.clone();

    let mut phase = FinalizedWritePhase::new(db.clone(), db.config().disable_wal_during_ibd);
    assert!(
        db.skip_wal(),
        "the guard must activate WAL skipping when the config opts in",
    );
    assert!(db.auto_compaction_disabled());

    let blocks = network.blockchain_map();
    for height in 0..=2 {
        let block: Arc<Block> = blocks
            .get(&height)
            .expect("test vectors include the first blocks of every network")
            .zcash_deserialize_into()
            .expect("test vectors deserialize");

        state
            .commit_finalized_direct(block.into(), None, "finalized write phase WAL test")
            .expect("test vectors are valid blocks");
        phase.block_committed();
    }

    drop(phase);
    assert!(!db.skip_wal(), "dropping the guard must re-enable the WAL",);
    assert!(!db.auto_compaction_disabled());

    // The bulk writes were flushed by the guard, so they stay readable.
    assert_eq!(state.db.finalized_tip_height(), Some(Height(2)));
}
