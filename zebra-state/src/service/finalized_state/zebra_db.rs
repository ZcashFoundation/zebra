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

use zebra_chain::{block::Height, diagnostic::task::WaitForPanics, parameters::Network};

use crate::{
    config::database_format_version_on_disk,
    service::finalized_state::{
        disk_db::DiskDb,
        disk_format::{
            block::MAX_ON_DISK_HEIGHT,
            transparent::AddressLocation,
            upgrade::{DbFormatChange, DbFormatChangeThreadHandle},
        },
    },
    snapshot_consume::{SnapshotConsumeLoadError, SnapshotConsumeState},
    write_database_format_version_to_disk, BoxError, Config, StateInitError,
};

use super::disk_format::upgrade::restorable_db_versions;

pub mod block;
pub mod chain;
pub mod known_hash;
pub mod metrics;
pub mod rpc_index;
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

    /// The optional snapshot-consume state for known-hash / checkpoint
    /// assumeUTXO sync, loaded once at construction from
    /// [`Config::snapshot_consume`].
    ///
    /// `None` for a normal sync. When `Some`, the finalized write path consumes
    /// a verified state snapshot at `H_max` instead of deriving it (direct tree
    /// writes, skipped per-block balances, and survivor-set elision). Shared by
    /// all clones (read-only after construction).
    snapshot_consume: Option<Arc<SnapshotConsumeState>>,

    /// The optional separate "RPC index" database, holding only the RPC-only
    /// column families (transparent address / balance / spent-tx indexes),
    /// written by a thread that trails this consensus database.
    ///
    /// `None` (the default) when
    /// [`Config::separate_rpc_index_db`](crate::Config::separate_rpc_index_db)
    /// is off — every column family lives in this `ZebraDb` and the RPC-only
    /// read accessors read from it directly.
    ///
    /// When `Some`, this `ZebraDb` is the consensus database (opened with the
    /// consensus-only column families) and the RPC-only read accessors consult
    /// this handle instead. Boxed to break the recursive type (a `ZebraDb`
    /// containing a `ZebraDb`); the inner handle's own `rpc_index_db` is always
    /// `None`. Shared by all clones via the inner `DiskDb`'s `Arc`.
    //
    // # Correctness
    //
    // The inner database is a normal `ZebraDb` with its own `Drop`, so the last
    // clone closes it exactly like the consensus database.
    rpc_index_db: Option<Box<ZebraDb>>,
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
    #[allow(clippy::unwrap_in_result)]
    pub fn new(
        config: &Config,
        db_kind: impl AsRef<str>,
        format_version_in_code: &Version,
        network: &Network,
        debug_skip_format_upgrades: bool,
        column_families_in_code: impl IntoIterator<Item = String>,
        read_only: bool,
    ) -> Result<ZebraDb, StateInitError> {
        // A read-only secondary instance must never modify the primary's cache directory, so it
        // skips the post-major-upgrade DB reuse (which can create directories and rename the
        // on-disk database) and reads the on-disk format version directly. The cache directory is
        // checked for readability first, so a missing or unreadable directory returns a typed
        // `ReadOnlyCacheDirUnreadable` error here instead of panicking on the version-file read.
        let disk_version = if read_only {
            DiskDb::check_cache_dir_readable(&config.cache_dir)?;

            database_format_version_on_disk(config, &db_kind, format_version_in_code.major, network)
                .expect("unable to read database format version file")
        } else {
            DiskDb::try_reusing_previous_db_after_major_upgrade(
                &restorable_db_versions(),
                format_version_in_code,
                config,
                &db_kind,
                network,
            )
            .or_else(|| {
                database_format_version_on_disk(
                    config,
                    &db_kind,
                    format_version_in_code.major,
                    network,
                )
                .expect("unable to read database format version file")
            })
        };

        // Log any format changes before opening the database, in case opening fails.
        let format_change = DbFormatChange::open_database(format_version_in_code, disk_version);

        // A read-only secondary instance cannot create a database. If there's no database on
        // disk, fail with a clear, actionable error instead of silently "creating" one.
        //
        // The read-write path is unaffected: creating a new database is the correct behavior there.
        if read_only && format_change.is_newly_created() {
            let db_path = config.db_path(&db_kind, format_version_in_code.major, network);
            return Err(StateInitError::ReadOnlyDatabaseNotFound { path: db_path });
        }

        // Format upgrades try to write to the database, so we always skip them
        // if `read_only` is `true`.
        //
        // We also allow skipping them when we are running tests.
        let debug_skip_format_upgrades = read_only || (cfg!(test) && debug_skip_format_upgrades);

        // Open the low-level database and do initial checks.
        //
        // After the database directory is created, a newly created database temporarily
        // changes to the default database version. Then we set the correct version in the
        // upgrade thread. We need to do the version change in this order, because the version
        // file can only be changed while we hold the RocksDB database lock.
        let disk_db = DiskDb::new(
            config,
            db_kind,
            format_version_in_code,
            network,
            column_families_in_code,
            read_only,
        )?;

        let mut db = ZebraDb {
            config: Arc::new(config.clone()),
            debug_skip_format_upgrades,
            format_change_handle: None,
            db: disk_db,
            snapshot_consume: None,
            rpc_index_db: None,
        };

        // Open the separate RPC index database, if configured. It lives under
        // the consensus database directory, so it shares the version / network
        // path and is cleaned up with it. Only the top-level (consensus)
        // database opens one; the recursive call passes `false`.
        if config.separate_rpc_index_db && !read_only {
            db.rpc_index_db = Some(Box::new(Self::open_rpc_index_db(
                config,
                &db_kind,
                format_version_in_code,
                network,
                debug_skip_format_upgrades,
            )));
        }

        // Load the optional snapshot-consume state, if configured. This is done
        // after the database is open so the fresh-DB guard can read the tip. A
        // misconfigured assumeUTXO sync is a fatal startup error with an
        // actionable message (a clean typed error rather than a raw panic),
        // because continuing would silently corrupt the finalized state.
        db.snapshot_consume = db
            .load_snapshot_consume(config, network)
            .unwrap_or_else(|error| {
                // The snapshot-consume safety guards must hold before any block
                // is committed; failing them is unrecoverable, so stop startup
                // with the typed error's actionable message.
                panic!("cannot enable snapshot-consume (assumeUTXO) sync: {error}");
            });

        let zero_location_utxos =
            db.address_utxo_locations(AddressLocation::from_usize(Height(0), 0, 0));
        if !zero_location_utxos.is_empty() {
            warn!(
                "You have been impacted by the Zebra 2.4.0 address indexer corruption bug. \
                If you rely on the data from the RPC interface, you will need to recover your database. \
                Follow the instructions in the 2.4.1 release notes: https://github.com/ZcashFoundation/zebra/releases/tag/v2.4.1 \
                If you just run the node for consensus and don't use data from the RPC interface, you can ignore this warning."
            )
        }

        db.spawn_format_change(format_change);

        Ok(db)
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

    /// Sets `finished_format_upgrades` to true on the inner [`DiskDb`] to indicate that Zebra has
    /// finished applying any required db format upgrades.
    pub fn mark_finished_format_upgrades(&self) {
        self.db.mark_finished_format_upgrades();
    }

    /// Returns true if the `finished_format_upgrades` flag has been set to true on the inner [`DiskDb`] to
    /// indicate that Zebra has finished applying any required db format upgrades.
    pub fn finished_format_upgrades(&self) -> bool {
        self.db.finished_format_upgrades()
    }

    /// Returns config for this database.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Returns the loaded snapshot-consume state, if assumeUTXO sync is
    /// configured ([`Config::snapshot_consume`]).
    ///
    /// `None` for a normal sync. The write path consults this to decide whether
    /// to consume a snapshot (direct tree writes, skip per-block balances,
    /// survivor-set elision) instead of deriving state.
    pub fn snapshot_consume(&self) -> Option<&Arc<SnapshotConsumeState>> {
        self.snapshot_consume.as_ref()
    }

    /// Opens the separate RPC index database under the consensus database path.
    ///
    /// The RPC index database holds only the RPC-only column families
    /// ([`RPC_INDEX_COLUMN_FAMILIES_IN_CODE`]) plus its own durable tip marker.
    /// It is placed at `<consensus-db-path>/rpc-index` by deriving a config
    /// whose `cache_dir` is the consensus database's full version / network
    /// directory and whose `db_kind` directory is [`RPC_INDEX_DB_DIR`], so it is
    /// co-located with, and cleaned up alongside, the consensus database.
    ///
    /// The inner database never opens its own RPC index database (the derived
    /// config has `separate_rpc_index_db` cleared), avoiding infinite recursion.
    ///
    /// [`RPC_INDEX_COLUMN_FAMILIES_IN_CODE`]: crate::service::finalized_state::RPC_INDEX_COLUMN_FAMILIES_IN_CODE
    /// [`RPC_INDEX_DB_DIR`]: crate::service::finalized_state::RPC_INDEX_DB_DIR
    fn open_rpc_index_db(
        config: &Config,
        db_kind: impl AsRef<str>,
        format_version_in_code: &Version,
        network: &Network,
        debug_skip_format_upgrades: bool,
    ) -> ZebraDb {
        use crate::service::finalized_state::{
            RPC_INDEX_COLUMN_FAMILIES_IN_CODE, RPC_INDEX_DB_DIR,
        };

        // The consensus database's full version / network directory becomes the
        // RPC index database's cache_dir, so the inner db_path resolves to
        // <consensus-db-path>/rpc-index/v<major>/<network>.
        let consensus_db_path = config.db_path(&db_kind, format_version_in_code.major, network);

        let rpc_index_config = Config {
            cache_dir: consensus_db_path,
            // The inner database must never open its own RPC index database.
            separate_rpc_index_db: false,
            // The RPC index is non-consensus data; never skip its WAL or load a
            // snapshot into it.
            disable_wal_during_ibd: false,
            snapshot_consume: None,
            // It is a real on-disk database co-located with the consensus one,
            // even when the consensus database is ephemeral the parent dir is a
            // real temp dir, so keep ephemeral matched to the parent.
            ephemeral: config.ephemeral,
            ..config.clone()
        };

        ZebraDb::new(
            &rpc_index_config,
            RPC_INDEX_DB_DIR,
            format_version_in_code,
            network,
            debug_skip_format_upgrades,
            RPC_INDEX_COLUMN_FAMILIES_IN_CODE
                .iter()
                .map(ToString::to_string),
            false,
        )
    }

    /// Returns the separate RPC index database handle, if the consensus / RPC
    /// write split is enabled
    /// ([`Config::separate_rpc_index_db`](crate::Config::separate_rpc_index_db)).
    ///
    /// `None` for the single-database default path, in which the RPC-only column
    /// families live in this database.
    pub fn rpc_index_db(&self) -> Option<&ZebraDb> {
        self.rpc_index_db.as_deref()
    }

    /// Returns the database holding the RPC-only column families: the separate
    /// RPC index database when the split is enabled, otherwise this database.
    ///
    /// RPC-only read accessors use this so they transparently read from the
    /// correct database in both configurations.
    pub fn rpc_index_or_self(&self) -> &ZebraDb {
        self.rpc_index_db.as_deref().unwrap_or(self)
    }

    /// Returns `true` if this database's own handle does not have the column
    /// family `cf_name` (used to assert RPC-only column families are absent from
    /// the consensus database in the write-split tests).
    #[cfg(test)]
    pub(crate) fn raw_cf_handle_is_none_for_test(&self, cf_name: &str) -> bool {
        self.db.cf_handle(cf_name).is_none()
    }

    /// Returns the inner low-level [`DiskDb`] handle, for tests that drive the
    /// `&DiskDb`-taking batch builders directly (e.g. the write-split spend
    /// resolution test).
    #[cfg(test)]
    pub(crate) fn disk_db_for_test(&self) -> &DiskDb {
        &self.db
    }

    /// Sets the snapshot-consume state directly, for tests.
    ///
    /// Bypasses loading from a survivor-set file so tests can drive the
    /// snapshot-consume write path with a synthetic [`SnapshotConsumeState`].
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn set_snapshot_consume(&mut self, snapshot_consume: Option<Arc<SnapshotConsumeState>>) {
        self.snapshot_consume = snapshot_consume;
    }

    /// Loads the optional snapshot-consume state from `config` for `network`.
    ///
    /// Returns `Ok(None)` if snapshot-consume is not configured. Returns a clean,
    /// typed [`SnapshotConsumeLoadError`] (never a panic) if it is configured but
    /// fails a safety guard or can't be loaded, so a misconfigured assumeUTXO
    /// sync fails fast at startup with an actionable error rather than silently
    /// corrupting state. The caller turns the error into a fatal startup failure.
    ///
    /// # Guards
    ///
    /// - **Network match.** The configured network must match this database's
    ///   network; a per-network artifact applied to the wrong chain would mark
    ///   the wrong outputs.
    /// - **Fresh DB only.** Snapshot-consume is only loaded against an empty
    ///   database (a from-genesis sync). Loading it against a non-empty database
    ///   could elide outputs the database already holds, or rely on an in-memory
    ///   spend-resolution cache that is cold after a restart — both unsafe (see
    ///   `docs/design/utxo-elision.md` §4.3). The other consume behaviours
    ///   (direct tree writes, skipping per-block balances) are also only enabled
    ///   on a fresh database for simplicity and to keep the snapshot's `H_max`
    ///   meaningful. Resuming an in-progress assumeUTXO sync is a recorded
    ///   follow-up (it needs the deferred-durability machinery to be
    ///   restart-safe).
    fn load_snapshot_consume(
        &self,
        config: &Config,
        network: &Network,
    ) -> Result<Option<Arc<SnapshotConsumeState>>, SnapshotConsumeLoadError> {
        let Some(consume_config) = config.snapshot_consume.as_ref() else {
            return Ok(None);
        };

        // Network match: a per-network artifact applied to the wrong chain marks
        // the wrong outputs.
        let db_network = self.network();
        if &db_network != network {
            return Err(SnapshotConsumeLoadError::NetworkMismatch {
                configured: network.clone(),
                database: db_network,
            });
        }

        // Fresh DB only: refuse to enable any snapshot-consume behaviour against
        // a database that already holds blocks. A normal resync from genesis
        // creates a fresh database, so this is the expected state for assumeUTXO
        // sync.
        if !self.is_empty() {
            return Err(SnapshotConsumeLoadError::NonEmptyDatabase {
                tip_height: self.finalized_tip_height(),
            });
        }

        let consume_state = SnapshotConsumeState::load(consume_config, network)?;

        tracing::info!(
            h_max = ?consume_state.h_max(),
            survivors = consume_state.survivor_set().map(|s| s.len()),
            elide_utxo_bytes = consume_state.elide_utxo_bytes(),
            "loaded snapshot-consume (assumeUTXO) state",
        );

        Ok(Some(Arc::new(consume_state)))
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

    /// Enables or disables RocksDB auto-compaction on every column family.
    ///
    /// See [`DiskDb::set_auto_compaction`] for details.
    pub(crate) fn set_auto_compaction(&self, enabled: bool) -> Result<(), rocksdb::Error> {
        self.db.set_auto_compaction(enabled)
    }

    /// Returns true if the last successful [`ZebraDb::set_auto_compaction`]
    /// call disabled auto-compaction.
    #[cfg(test)]
    pub(crate) fn auto_compaction_disabled(&self) -> bool {
        self.db.auto_compaction_disabled()
    }

    /// Enables or disables WAL skipping for future database writes.
    ///
    /// See [`DiskDb::set_skip_wal`] for the correctness requirements.
    pub(crate) fn set_skip_wal(&self, skip_wal: bool) {
        self.db.set_skip_wal(skip_wal);
    }

    /// Returns true if database writes currently skip the write-ahead log.
    #[cfg(test)]
    pub(crate) fn skip_wal(&self) -> bool {
        self.db.skip_wal()
    }

    /// Returns the largest number of level 0 SST files in any column family.
    ///
    /// See [`DiskDb::level0_file_count`] for details.
    pub(crate) fn level0_file_count(&self) -> u64 {
        self.db.level0_file_count()
    }

    /// Flushes every column family's memtables to SST files on disk, waiting
    /// for the flushes to finish.
    ///
    /// See [`DiskDb::flush_all_column_families`] for details.
    pub(crate) fn flush_all_column_families(&self) -> Result<(), rocksdb::Error> {
        self.db.flush_all_column_families()
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

    /// Exports RocksDB metrics to Prometheus.
    ///
    /// This function collects database statistics and exposes them as Prometheus metrics.
    /// Call this periodically (e.g., every 30 seconds) from a background task.
    pub(crate) fn export_metrics(&self) {
        self.db.export_metrics();
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

#[cfg(test)]
mod load_snapshot_consume_tests {
    use std::sync::Arc;

    use zebra_chain::{block::Block, parameters::Network, serialization::ZcashDeserializeInto};

    use crate::{
        service::finalized_state::FinalizedState,
        snapshot_consume::{SnapshotConsumeConfig, SnapshotConsumeLoadError},
        CheckpointVerifiedBlock, Config,
    };

    /// `load_snapshot_consume` returns a clean typed error (never panics
    /// internally) when assumeUTXO sync is configured against a database that
    /// already holds blocks (finding #4). The caller (`ZebraDb::new`) turns the
    /// error into a fatal startup failure with an actionable message.
    #[test]
    fn refuses_non_empty_database_with_clean_error() {
        let _init_guard = zebra_test::init();

        let network = Network::Mainnet;

        // A fresh state, made non-empty by committing genesis.
        let mut state = FinalizedState::new_with_debug(
            &Config::ephemeral(),
            &network,
            true,
            #[cfg(feature = "elasticsearch")]
            false,
            false,
        );
        let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
            .zcash_deserialize_into::<Arc<Block>>()
            .expect("genesis deserializes");
        state
            .commit_finalized_direct(
                CheckpointVerifiedBlock::from(genesis).into(),
                None,
                "load_snapshot_consume non-empty-db test",
            )
            .expect("genesis commits");

        // A config that enables snapshot-consume (no survivor set needed: the
        // fresh-DB guard runs before the survivor set is loaded).
        let config = Config {
            snapshot_consume: Some(SnapshotConsumeConfig::default()),
            ..Config::ephemeral()
        };

        let result = state.db.load_snapshot_consume(&config, &network);
        assert!(
            matches!(
                result,
                Err(SnapshotConsumeLoadError::NonEmptyDatabase { .. })
            ),
            "a non-empty database must be a clean NonEmptyDatabase error, got {result:?}",
        );
    }

    /// `load_snapshot_consume` returns a clean typed error on a network mismatch
    /// between the configured network and the database's network (finding #4).
    #[test]
    fn refuses_network_mismatch_with_clean_error() {
        let _init_guard = zebra_test::init();

        // An empty Mainnet database.
        let state = FinalizedState::new_with_debug(
            &Config::ephemeral(),
            &Network::Mainnet,
            true,
            #[cfg(feature = "elasticsearch")]
            false,
            false,
        );

        let config = Config {
            snapshot_consume: Some(SnapshotConsumeConfig::default()),
            ..Config::ephemeral()
        };

        // Asking to consume a Testnet snapshot against a Mainnet database is a
        // clean error, not a panic.
        let result = state
            .db
            .load_snapshot_consume(&config, &Network::new_default_testnet());
        assert!(
            matches!(
                result,
                Err(SnapshotConsumeLoadError::NetworkMismatch { .. })
            ),
            "a network mismatch must be a clean NetworkMismatch error, got {result:?}",
        );
    }

    /// With snapshot-consume not configured, the loader returns `Ok(None)` on any
    /// database (the normal-sync path).
    #[test]
    fn unconfigured_returns_none() {
        let _init_guard = zebra_test::init();

        let network = Network::Mainnet;
        let state = FinalizedState::new_with_debug(
            &Config::ephemeral(),
            &network,
            true,
            #[cfg(feature = "elasticsearch")]
            false,
            false,
        );

        let result = state
            .db
            .load_snapshot_consume(&Config::ephemeral(), &network);
        assert!(
            matches!(result, Ok(None)),
            "unconfigured snapshot-consume returns Ok(None), got {result:?}",
        );
    }
}
