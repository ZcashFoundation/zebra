//! An RAII guard that pauses RocksDB auto-compaction, and optionally skips
//! the write-ahead log, while the finalized block write loop is running.
//!
//! During the initial sync, the write task commits a long, strictly-ordered
//! run of checkpoint-verified blocks. Auto-compaction competes with those
//! writes for disk bandwidth, so it is paused for the whole phase and
//! re-enabled when the phase ends. The guard re-enables it on *every* exit
//! path: loop completion, early shutdown returns, and panic unwinds.
//!
//! If Zebra is killed before the guard drops (e.g. `kill -9`), there is no
//! persistent effect: see
//! [`DiskDb::set_auto_compaction`](crate::service::finalized_state::DiskDb::set_auto_compaction).
//!
//! While compaction is paused, level 0 SST files accumulate without bound,
//! which degrades the write thread's own reads (every level 0 file can
//! overlap every key, so point lookups check all of them). The guard
//! periodically checks the level 0 file count, and temporarily re-enables
//! auto-compaction until the backlog drains.

use crate::service::finalized_state::ZebraDb;

/// Check the level 0 file count every time this many blocks have been
/// committed.
///
/// Checking is cheap (a few RocksDB property reads), but it doesn't need to
/// be frequent: level 0 pressure builds over thousands of blocks.
const L0_CHECK_INTERVAL_BLOCKS: u64 = 10_000;

/// Temporarily re-enable auto-compaction when any column family has more
/// than this many level 0 files.
///
/// Every level 0 file can overlap every key range, so reads can check all of
/// them: above ~64 files, the write thread's own UTXO and tree lookups slow
/// down more than paused compaction speeds them up.
const L0_DRAIN_FILE_LIMIT: u64 = 64;

/// Once draining, pause auto-compaction again when every column family is
/// back down to this many level 0 files or fewer.
///
/// This is half of [`L0_DRAIN_FILE_LIMIT`] so the guard doesn't rapidly
/// toggle compaction when the file count hovers around the limit.
const L0_DRAINED_FILE_LIMIT: u64 = L0_DRAIN_FILE_LIMIT / 2;

/// The database operations needed by [`FinalizedWritePhase`].
///
/// This is a trait so the guard's pause/resume sequencing can be unit-tested
/// against a mock without opening a RocksDB instance: RocksDB has no property
/// that reads the `disable_auto_compactions` option back.
///
/// All methods are infallible: implementations must handle (log) database
/// errors, because the guard calls these methods from [`Drop`], where errors
/// can't be propagated.
pub(crate) trait FinalizedWriteDb {
    /// Enables or disables auto-compaction on every column family.
    fn set_auto_compaction(&self, enabled: bool);

    /// Enables or disables write-ahead log skipping for future writes.
    fn set_skip_wal(&self, skip_wal: bool);

    /// Returns the largest number of level 0 SST files in any column family.
    fn level0_file_count(&self) -> u64;

    /// Flushes every column family's memtables to SST files on disk, making
    /// writes that skipped the write-ahead log durable.
    fn flush_all_column_families(&self);
}

impl FinalizedWriteDb for ZebraDb {
    fn set_auto_compaction(&self, enabled: bool) {
        if let Err(error) = ZebraDb::set_auto_compaction(self, enabled) {
            // A failed pause just leaves compaction running, which is safe.
            // A failed resume leaves compaction off until the next restart,
            // which re-enables it (see `DiskDb::set_auto_compaction`).
            warn!(?error, enabled, "failed to update database auto-compaction");
        }
    }

    fn set_skip_wal(&self, skip_wal: bool) {
        ZebraDb::set_skip_wal(self, skip_wal);
    }

    fn level0_file_count(&self) -> u64 {
        ZebraDb::level0_file_count(self)
    }

    fn flush_all_column_families(&self) {
        if let Err(error) = ZebraDb::flush_all_column_families(self) {
            // If the flush fails, writes that skipped the WAL stay buffered in
            // the memtables. They are still readable, and RocksDB flushes them
            // in the background or at shutdown; a crash before then loses
            // them, and Zebra re-downloads the lost blocks at startup.
            warn!(?error, "failed to flush database after bulk-write phase");
        }
    }
}

/// An RAII guard for the finalized (checkpoint) bulk-write phase.
///
/// Creating the guard pauses auto-compaction, and skips the write-ahead log
/// if `skip_wal` is set. Dropping the guard restores both, and flushes the
/// database if the write-ahead log was skipped, so that bulk-written blocks
/// become durable.
///
/// Call [`FinalizedWritePhase::block_committed`] after each committed block
/// so the guard can bound level 0 file growth (see the module docs).
pub(crate) struct FinalizedWritePhase<D: FinalizedWriteDb> {
    /// The database controlled by this guard.
    db: D,

    /// Whether this guard activated write-ahead log skipping.
    skip_wal: bool,

    /// Whether auto-compaction is temporarily re-enabled to drain a level 0
    /// file backlog.
    draining_level0: bool,

    /// The number of blocks committed since the last level 0 check.
    blocks_since_level0_check: u64,
}

impl<D: FinalizedWriteDb> FinalizedWritePhase<D> {
    /// Starts the bulk-write phase: pauses auto-compaction, and skips the
    /// write-ahead log if `skip_wal` is set.
    ///
    /// # Correctness
    ///
    /// `skip_wal` must only be set if the user opted in via
    /// [`Config::disable_wal_during_ibd`](crate::Config::disable_wal_during_ibd).
    pub(crate) fn new(db: D, skip_wal: bool) -> Self {
        info!(
            skip_wal,
            "pausing database auto-compaction for the finalized bulk-write phase"
        );

        db.set_auto_compaction(false);

        if skip_wal {
            db.set_skip_wal(true);
        }

        Self {
            db,
            skip_wal,
            draining_level0: false,
            blocks_since_level0_check: 0,
        }
    }

    /// Records a committed block, and bounds level 0 file growth.
    ///
    /// Every [`L0_CHECK_INTERVAL_BLOCKS`] blocks, checks the level 0 file
    /// count, and temporarily re-enables auto-compaction above
    /// [`L0_DRAIN_FILE_LIMIT`] files, until the backlog drains back down to
    /// [`L0_DRAINED_FILE_LIMIT`] files.
    pub(crate) fn block_committed(&mut self) {
        self.blocks_since_level0_check += 1;
        if self.blocks_since_level0_check < L0_CHECK_INTERVAL_BLOCKS {
            return;
        }
        self.blocks_since_level0_check = 0;

        let level0_file_count = self.db.level0_file_count();

        if !self.draining_level0 && level0_file_count > L0_DRAIN_FILE_LIMIT {
            info!(
                level0_file_count,
                limit = L0_DRAIN_FILE_LIMIT,
                "level 0 file backlog is degrading reads, \
                 temporarily re-enabling auto-compaction until it drains"
            );

            self.db.set_auto_compaction(true);
            self.draining_level0 = true;
        } else if self.draining_level0 && level0_file_count <= L0_DRAINED_FILE_LIMIT {
            info!(
                level0_file_count,
                "level 0 file backlog has drained, pausing auto-compaction again"
            );

            self.db.set_auto_compaction(false);
            self.draining_level0 = false;
        }
    }
}

impl<D: FinalizedWriteDb> Drop for FinalizedWritePhase<D> {
    fn drop(&mut self) {
        info!(
            skip_wal = self.skip_wal,
            "finalized bulk-write phase ended, resuming database auto-compaction"
        );

        if self.skip_wal {
            // Make future writes durable again before flushing, so no write
            // can slip in between the flush and the WAL being re-enabled.
            self.db.set_skip_wal(false);

            // Make the WAL-less bulk writes durable.
            self.db.flush_all_column_families();
        }

        // Always re-enable auto-compaction, even if it was already
        // temporarily re-enabled to drain level 0 files (the call is
        // idempotent). This drop runs on every exit path, including panic
        // unwinds; if the process dies without unwinding, the next database
        // open re-enables auto-compaction (see `DiskDb::set_auto_compaction`).
        self.db.set_auto_compaction(true);
    }
}

#[cfg(test)]
mod tests;
