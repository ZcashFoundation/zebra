//! Provides high-level access to the database using [`zebra_chain`] types.
//!
//! This module makes sure that:
//! - all disk writes happen inside a RocksDB transaction, and
//! - format-specific invariants are maintained.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use std::path::Path;

use zebra_chain::parameters::Network;

use crate::{
    config::{database_format_version_in_code, database_format_version_on_disk},
    service::finalized_state::{
        disk_db::DiskDb,
        disk_format::{
            block::MAX_ON_DISK_HEIGHT,
            upgrade::{DbFormatChange, DbFormatChangeThreadHandle},
        },
    },
    Config,
};

pub mod block;
pub mod chain;
pub mod metrics;
pub mod shielded;
pub mod transparent;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod arbitrary;

/// Wrapper struct to ensure high-level typed database access goes through the correct API.
///
/// `rocksdb` allows concurrent writes through a shared reference,
/// so database instances are cloneable. When the final clone is dropped,
/// the database is closed.
#[derive(Clone, Debug)]
pub struct ZebraDb {
    // Owned State
    //
    // Everything contained in this state must be shared by all clones, or read-only.
    //
    /// A handle to a running format change task, which cancels the task when dropped.
    ///
    /// # Concurrency
    ///
    /// This field should be dropped before the database field, so the format upgrade task is
    /// cancelled before the database is dropped. This helps avoid some kinds of deadlocks.
    format_change_handle: Option<DbFormatChangeThreadHandle>,

    /// The inner low-level database wrapper for the RocksDB database.
    db: DiskDb,
}

impl ZebraDb {
    /// Opens or creates the database at `config.path` for `network`,
    /// and returns a shared high-level typed database wrapper.
    pub fn new(config: &Config, network: Network) -> ZebraDb {
        let running_version = database_format_version_in_code();
        let disk_version = database_format_version_on_disk(config, network)
            .expect("unable to read database format version file");

        // Log any format changes before opening the database, in case opening fails.
        let format_change = DbFormatChange::new(running_version, disk_version);

        // Open the database and do initial checks.
        let mut db = ZebraDb {
            format_change_handle: None,
            db: DiskDb::new(config, network),
        };

        db.check_max_on_disk_tip_height();

        // We have to get this height before we spawn the upgrade task, because threads can take
        // a while to start, and new blocks can be committed as soon as we return from this method.
        let initial_tip_height = db.finalized_tip_height();

        // Start any required format changes.
        //
        // TODO: should debug_stop_at_height wait for these upgrades, or not?
        if let Some(format_change) = format_change {
            // Launch the format change and install its handle in the database.
            //
            // `upgrade_db` is a special clone of the database, which can't be used to shut down
            // the upgrade task. (Because the task hasn't been launched yet,
            // `db.format_change_handle` is always None.)
            //
            // It can be a FinalizedState if needed, or the FinalizedState methods needed for
            // upgrades can be moved to ZebraDb.
            let upgrade_db = db.clone();

            let format_change_handle = format_change.spawn_format_change(
                config.clone(),
                network,
                initial_tip_height,
                upgrade_db,
            );

            db.format_change_handle = Some(format_change_handle);
        }

        db
    }

    /// Returns the `Path` where the files used by this database are located.
    pub fn path(&self) -> &Path {
        self.db.path()
    }

    /// Check for panics in code running in spawned threads.
    /// If a thread exited with a panic, resume that panic.
    ///
    /// This method should be called regularly, so that panics are detected as soon as possible.
    pub fn check_for_panics(&mut self) {
        if let Some(format_change_handle) = self.format_change_handle.as_mut() {
            format_change_handle.check_for_panics();
        }

        // This check doesn't panic, but we want to check it regularly anyway.
        self.check_max_on_disk_tip_height();
    }

    /// Shut down the database, cleaning up background tasks and ephemeral data.
    ///
    /// If `force` is true, clean up regardless of any shared references.
    /// `force` can cause errors accessing the database from other shared references.
    /// It should only be used in debugging or test code, immediately before a manual shutdown.
    ///
    /// See [`DiskDb::shutdown`] for details.
    pub fn shutdown(&mut self, force: bool) {
        // # Concurrency
        //
        // The format upgrade task should be cancelled before the database is flushed or shut down.
        // This helps avoid some kinds of deadlocks.
        //
        // See also the correctness note in `DiskDb::shutdown()`.
        if force || self.db.shared_database_owners() <= 1 {
            if let Some(format_change_handle) = self.format_change_handle.as_mut() {
                format_change_handle.force_cancel();
            }
        }

        self.check_for_panics();

        self.db.shutdown(force);
    }

    /// Check that the on-disk height is well below the maximum supported database height.
    ///
    /// Zebra only supports on-disk heights up to 3 bytes.
    ///
    /// # Logs an Error
    ///
    /// If Zebra is storing block heights that are close to [`MAX_ON_DISK_HEIGHT`].
    fn check_max_on_disk_tip_height(&self) {
        if let Some((tip_height, tip_hash)) = self.tip() {
            if tip_height.0 > MAX_ON_DISK_HEIGHT.0 / 2 {
                error!(
                    ?tip_height,
                    ?tip_hash,
                    ?MAX_ON_DISK_HEIGHT,
                    "unexpectedly large tip height, database format upgrade required",
                );
            }
        }
    }
}

impl Drop for ZebraDb {
    fn drop(&mut self) {
        self.shutdown(false);
    }
}
