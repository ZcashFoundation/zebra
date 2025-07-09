//! Provides high-level access to the database using [`zebra_chain`] types.
//!
//! This module makes sure that:
//! - all disk writes happen inside a RocksDB transaction, and
//! - format-specific invariants are maintained.
//!
//! # Correctness
//!
//! [`crate::constants::state_database_format_version_in_code()`] must be incremented
//! each time the database format (column, serialization, etc) changes.

use std::{path::Path, sync::Arc};

use crossbeam_channel::bounded;
use semver::Version;

use zebra_chain::{diagnostic::task::WaitForPanics, parameters::Network};

use crate::{
    config::database_format_version_on_disk,
    service::finalized_state::{
        disk_db::DiskDb,
        disk_format::{
            block::MAX_ON_DISK_HEIGHT,
            upgrade::{DbFormatChange, DbFormatChangeThreadHandle},
        },
    },
    write_database_format_version_to_disk, BoxError, Config,
};

use super::disk_format::upgrade::restorable_db_versions;

pub mod block;
pub mod chain;
pub mod metrics;
pub mod shielded;
pub mod transparent;

#[cfg(any(test, feature = "proptest-impl"))]
// TODO: when the database is split out of zebra-state, always expose these methods.
pub mod arbitrary;

/// Wrapper struct to ensure high-level `zebra-state` database access goes through the correct API.
///
/// `rocksdb` allows concurrent writes through a shared reference,
/// so database instances are cloneable. When the final clone is dropped,
/// the database is closed.
#[derive(Clone, Debug)]
pub struct ZebraDb {
    // Configuration
    //
    // This configuration cannot be modified after the database is initialized,
    // because some clones would have different values.
    //
    /// The configuration for the database.
    //
    // TODO: move the config to DiskDb
    config: Arc<Config>,

    /// Should format upgrades and format checks be skipped for this instance?
    /// Only used in test code.
    //
    // TODO: move this to DiskDb
    debug_skip_format_upgrades: bool,

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
    //
    // TODO: move the generic upgrade code and fields to DiskDb
    format_change_handle: Option<DbFormatChangeThreadHandle>,

    /// The inner low-level database wrapper for the RocksDB database.
    db: DiskDb,
}

impl ZebraDb {
    /// Opens or creates the database at a path based on the kind, major version and network,
    /// with the supplied column families, preserving any existing column families,
    /// and returns a shared high-level typed database wrapper.
    ///
    /// If `debug_skip_format_upgrades` is true, don't do any format upgrades or format checks.
    /// This argument is only used when running tests, it is ignored in production code.
    //
    // TODO: rename to StateDb and remove the db_kind and column_families_in_code arguments
    pub fn new(
        config: &Config,
        db_kind: impl AsRef<str>,
        format_version_in_code: &Version,
        network: &Network,
        debug_skip_format_upgrades: bool,
        column_families_in_code: impl IntoIterator<Item = String>,
        read_only: bool,
    ) -> ZebraDb {
        let disk_version = DiskDb::try_reusing_previous_db_after_major_upgrade(
            &restorable_db_versions(),
            format_version_in_code,
            config,
            &db_kind,
            network,
        )
        .or_else(|| {
            database_format_version_on_disk(config, &db_kind, format_version_in_code.major, network)
                .expect("unable to read database format version file")
        });

        // Log any format changes before opening the database, in case opening fails.
        let format_change = DbFormatChange::open_database(format_version_in_code, disk_version);

        // Format upgrades try to write to the database, so we always skip them
        // if `read_only` is `true`.
        //
        // We also allow skipping them when we are running tests.
        let debug_skip_format_upgrades = read_only || (cfg!(test) && debug_skip_format_upgrades);

        // Open the database and do initial checks.
        let mut db = ZebraDb {
            config: Arc::new(config.clone()),
            debug_skip_format_upgrades,
            format_change_handle: None,
            // After the database directory is created, a newly created database temporarily
            // changes to the default database version. Then we set the correct version in the
            // upgrade thread. We need to do the version change in this order, because the version
            // file can only be changed while we hold the RocksDB database lock.
            db: DiskDb::new(
                config,
                db_kind,
                format_version_in_code,
                network,
                column_families_in_code,
                read_only,
            ),
        };

        db.spawn_format_change(format_change);

        db
    }

    /// Launch any required format changes or format checks, and store their thread handle.
    pub fn spawn_format_change(&mut self, format_change: DbFormatChange) {
        if self.debug_skip_format_upgrades {
            return;
        }

        // We have to get this height before we spawn the upgrade task, because threads can take
        // a while to start, and new blocks can be committed as soon as we return from this method.
        let initial_tip_height = self.finalized_tip_height();

        // `upgrade_db` is a special clone of this database, which can't be used to shut down
        // the upgrade task. (Because the task hasn't been launched yet,
        // its `db.format_change_handle` is always None.)
        let upgrade_db = self.clone();

        // TODO:
        // - should debug_stop_at_height wait for the upgrade task to finish?
        let format_change_handle =
            format_change.spawn_format_change(upgrade_db, initial_tip_height);

        self.format_change_handle = Some(format_change_handle);
    }

    /// Returns config for this database.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Returns the configured database kind for this database.
    pub fn db_kind(&self) -> String {
        self.db.db_kind()
    }

    /// Returns the format version of the running code that created this `ZebraDb` instance in memory.
    pub fn format_version_in_code(&self) -> Version {
        self.db.format_version_in_code()
    }

    /// Returns the fixed major version for this database.
    pub fn major_version(&self) -> u64 {
        self.db.major_version()
    }

    /// Returns the format version of this database on disk.
    ///
    /// See `database_format_version_on_disk()` for details.
    pub fn format_version_on_disk(&self) -> Result<Option<Version>, BoxError> {
        database_format_version_on_disk(
            self.config(),
            self.db_kind(),
            self.major_version(),
            &self.network(),
        )
    }

    /// Updates the format of this database on disk to the suppled version.
    ///
    /// See `write_database_format_version_to_disk()` for details.
    pub(crate) fn update_format_version_on_disk(
        &self,
        new_version: &Version,
    ) -> Result<(), BoxError> {
        write_database_format_version_to_disk(
            self.config(),
            self.db_kind(),
            self.major_version(),
            new_version,
            &self.network(),
        )
    }

    /// Returns the configured network for this database.
    pub fn network(&self) -> Network {
        self.db.network()
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
    }

    /// When called with a secondary DB instance, tries to catch up with the primary DB instance
    pub fn try_catch_up_with_primary(&self) -> Result<(), rocksdb::Error> {
        self.db.try_catch_up_with_primary()
    }

    /// Spawns a blocking task to try catching up with the primary DB instance.
    pub async fn spawn_try_catch_up_with_primary(&self) -> Result<(), rocksdb::Error> {
        let db = self.clone();
        tokio::task::spawn_blocking(move || {
            let result = db.try_catch_up_with_primary();
            if let Err(catch_up_error) = &result {
                tracing::warn!(?catch_up_error, "failed to catch up to primary");
            }
            result
        })
        .wait_for_panics()
        .await
    }

    /// Shut down the database, cleaning up background tasks and ephemeral data.
    ///
    /// If `force` is true, clean up regardless of any shared references.
    /// `force` can cause errors accessing the database from other shared references.
    /// It should only be used in debugging or test code, immediately before a manual shutdown.
    ///
    /// See [`DiskDb::shutdown`] for details.
    pub fn shutdown(&mut self, force: bool) {
        // Are we shutting down the underlying database instance?
        let is_shutdown = force || self.db.shared_database_owners() <= 1;

        // # Concurrency
        //
        // The format upgrade task should be cancelled before the database is flushed or shut down.
        // This helps avoid some kinds of deadlocks.
        //
        // See also the correctness note in `DiskDb::shutdown()`.
        if !self.debug_skip_format_upgrades && is_shutdown {
            if let Some(format_change_handle) = self.format_change_handle.as_mut() {
                format_change_handle.force_cancel();
            }

            // # Correctness
            //
            // Check that the database format is correct before shutting down.
            // This lets users know to delete and re-sync their database immediately,
            // rather than surprising them next time Zebra starts up.
            //
            // # Testinng
            //
            // In Zebra's CI, panicking here stops us writing invalid cached states,
            // which would then make unrelated PRs fail when Zebra starts up.

            // If the upgrade has completed, or we've done a downgrade, check the state is valid.
            let disk_version = database_format_version_on_disk(
                &self.config,
                self.db_kind(),
                self.major_version(),
                &self.network(),
            )
            .expect("unexpected invalid or unreadable database version file");

            if let Some(disk_version) = disk_version {
                // We need to keep the cancel handle until the format check has finished,
                // because dropping it cancels the format check.
                let (_never_cancel_handle, never_cancel_receiver) = bounded(1);

                // We block here because the checks are quick and database validity is
                // consensus-critical.
                if disk_version >= self.db.format_version_in_code() {
                    DbFormatChange::check_new_blocks(self)
                        .run_format_change_or_check(
                            self,
                            // The initial tip height is not used by the new blocks format check.
                            None,
                            &never_cancel_receiver,
                        )
                        .expect("cancel handle is never used");
                }
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
    pub(crate) fn check_max_on_disk_tip_height(&self) -> Result<(), String> {
        if let Some((tip_height, tip_hash)) = self.tip() {
            if tip_height.0 > MAX_ON_DISK_HEIGHT.0 / 2 {
                let err = Err(format!(
                    "unexpectedly large tip height, database format upgrade required: \
                     tip height: {tip_height:?}, tip hash: {tip_hash:?}, \
                     max height: {MAX_ON_DISK_HEIGHT:?}"
                ));
                error!(?err);
                return err;
            }
        }

        Ok(())
    }

    /// Logs metrics related to the underlying RocksDB instance.
    ///
    /// This function prints various metrics and statistics about the RocksDB database,
    /// such as disk usage, memory usage, and other performance-related metrics.
    pub fn print_db_metrics(&self) {
        self.db.print_db_metrics();
    }

    /// Returns the estimated total disk space usage of the database.
    pub fn size(&self) -> u64 {
        self.db.size()
    }
}

impl Drop for ZebraDb {
    fn drop(&mut self) {
        self.shutdown(false);
    }
}
