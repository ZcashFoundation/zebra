//! Provides low-level access to RocksDB using some database-specific types.
//!
//! This module makes sure that:
//! - all disk writes happen inside a RocksDB transaction ([`WriteBatch`]), and
//! - format-specific invariants are maintained.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use std::{fmt::Debug, path::Path, sync::Arc};

use rlimit::increase_nofile_limit;

use zebra_chain::parameters::Network;

use crate::{
    service::finalized_state::disk_format::{FromDisk, IntoDisk},
    Config,
};

#[cfg(any(test, feature = "proptest-impl"))]
mod tests;

/// Wrapper struct to ensure low-level database access goes through the correct API.
#[derive(Clone, Debug)]
pub struct DiskDb {
    /// The shared inner RocksDB database.
    ///
    /// RocksDB allows reads and writes via a shared reference.
    /// Only column family changes and [`Drop`] require exclusive access.
    db: Arc<rocksdb::DB>,

    /// The configured temporary database setting.
    ///
    /// If true, the database files are deleted on drop.
    ephemeral: bool,
}

/// Wrapper struct to ensure low-level database writes go through the correct API.
///
/// [`rocksdb::WriteBatch`] is a batched set of database updates,
/// which must be written to the database using `DiskDb::write(batch)`.
#[must_use = "batches must be written to the database"]
pub struct DiskWriteBatch {
    /// The inner RocksDB write batch.
    batch: rocksdb::WriteBatch,
}

/// Helper trait for inserting (Key, Value) pairs into rocksdb with a consistently
/// defined format
pub trait WriteDisk {
    /// Serialize and insert the given key and value into a rocksdb column family,
    /// overwriting any existing `value` for `key`.
    fn zs_insert<K, V>(&mut self, cf: &rocksdb::ColumnFamily, key: K, value: V)
    where
        K: IntoDisk + Debug,
        V: IntoDisk;

    /// Remove the given key form rocksdb column family if it exists.
    fn zs_delete<K>(&mut self, cf: &rocksdb::ColumnFamily, key: K)
    where
        K: IntoDisk + Debug;
}

impl WriteDisk for DiskWriteBatch {
    fn zs_insert<K, V>(&mut self, cf: &rocksdb::ColumnFamily, key: K, value: V)
    where
        K: IntoDisk + Debug,
        V: IntoDisk,
    {
        let key_bytes = key.as_bytes();
        let value_bytes = value.as_bytes();
        self.batch.put_cf(cf, key_bytes, value_bytes);
    }

    fn zs_delete<K>(&mut self, cf: &rocksdb::ColumnFamily, key: K)
    where
        K: IntoDisk + Debug,
    {
        let key_bytes = key.as_bytes();
        self.batch.delete_cf(cf, key_bytes);
    }
}

/// Helper trait for retrieving values from rocksdb column familys with a consistently
/// defined format
pub trait ReadDisk {
    /// Returns the value for `key` in the rocksdb column family `cf`, if present.
    fn zs_get<K, V>(&self, cf: &rocksdb::ColumnFamily, key: &K) -> Option<V>
    where
        K: IntoDisk,
        V: FromDisk;

    /// Check if a rocksdb column family `cf` contains the serialized form of `key`.
    fn zs_contains<K>(&self, cf: &rocksdb::ColumnFamily, key: &K) -> bool
    where
        K: IntoDisk;
}

impl PartialEq for DiskDb {
    fn eq(&self, other: &Self) -> bool {
        if self.db.path() == other.db.path() {
            assert_eq!(
                self.ephemeral, other.ephemeral,
                "database with same path but different ephemeral configs",
            );
            return true;
        }

        false
    }
}

impl Eq for DiskDb {}

impl ReadDisk for DiskDb {
    fn zs_get<K, V>(&self, cf: &rocksdb::ColumnFamily, key: &K) -> Option<V>
    where
        K: IntoDisk,
        V: FromDisk,
    {
        let key_bytes = key.as_bytes();

        // We use `get_pinned_cf` to avoid taking ownership of the serialized
        // value, because we're going to deserialize it anyways, which avoids an
        // extra copy
        //
        // TODO: move disk reads to a blocking thread (#2188)
        let value_bytes = self
            .db
            .get_pinned_cf(cf, key_bytes)
            .expect("expected that disk errors would not occur");

        value_bytes.map(V::from_bytes)
    }

    fn zs_contains<K>(&self, cf: &rocksdb::ColumnFamily, key: &K) -> bool
    where
        K: IntoDisk,
    {
        let key_bytes = key.as_bytes();

        // We use `get_pinned_cf` to avoid taking ownership of the serialized
        // value, because we don't use the value at all. This avoids an extra copy.
        //
        // TODO: move disk reads to a blocking thread (#2188)
        self.db
            .get_pinned_cf(cf, key_bytes)
            .expect("expected that disk errors would not occur")
            .is_some()
    }
}

impl DiskWriteBatch {
    pub fn new() -> Self {
        DiskWriteBatch {
            batch: rocksdb::WriteBatch::default(),
        }
    }
}

impl DiskDb {
    /// The ideal open file limit for Zebra
    const IDEAL_OPEN_FILE_LIMIT: u64 = 1024;

    /// The minimum number of open files for Zebra to operate normally. Also used
    /// as the default open file limit, when the OS doesn't tell us how many
    /// files we can use.
    ///
    /// We want 100+ file descriptors for peers, and 100+ for the database.
    ///
    /// On Windows, the default limit is 512 high-level I/O files, and 8192
    /// low-level I/O files:
    /// https://docs.microsoft.com/en-us/cpp/c-runtime-library/reference/setmaxstdio?view=msvc-160#remarks
    const MIN_OPEN_FILE_LIMIT: u64 = 512;

    /// The number of files used internally by Zebra.
    ///
    /// Zebra uses file descriptors for OS libraries (10+), polling APIs (10+),
    /// stdio (3), and other OS facilities (2+).
    const RESERVED_FILE_COUNT: u64 = 48;

    pub fn new(config: &Config, network: Network) -> DiskDb {
        let path = config.db_path(network);
        let db_options = DiskDb::options();

        let column_families = vec![
            rocksdb::ColumnFamilyDescriptor::new("hash_by_height", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("height_by_hash", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("block_by_height", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("tx_by_hash", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("utxo_by_outpoint", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("sprout_nullifiers", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("sapling_nullifiers", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("orchard_nullifiers", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("sprout_anchors", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("sapling_anchors", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("orchard_anchors", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("sprout_note_commitment_tree", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new(
                "sapling_note_commitment_tree",
                db_options.clone(),
            ),
            rocksdb::ColumnFamilyDescriptor::new(
                "orchard_note_commitment_tree",
                db_options.clone(),
            ),
            rocksdb::ColumnFamilyDescriptor::new("history_tree", db_options.clone()),
            rocksdb::ColumnFamilyDescriptor::new("tip_chain_value_pool", db_options.clone()),
        ];

        // TODO: move opening the database to a blocking thread (#2188)
        let db_result = rocksdb::DB::open_cf_descriptors(&db_options, &path, column_families);

        match db_result {
            Ok(db) => {
                info!("Opened Zebra state cache at {}", path.display());

                let db = DiskDb {
                    db: Arc::new(db),
                    ephemeral: config.ephemeral,
                };

                db.assert_default_cf_is_empty();

                db
            }

            // TODO: provide a different hint if the disk is full, see #1623
            Err(e) => panic!(
                "Opening database {:?} failed: {:?}. \
                 Hint: Check if another zebrad process is running. \
                 Try changing the state cache_dir in the Zebra config.",
                path, e,
            ),
        }
    }

    // Read methods

    /// Returns the `Path` where the files used by this database are located.
    pub fn path(&self) -> &Path {
        self.db.path()
    }

    /// Returns the column family handle for `cf_name`.
    pub fn cf_handle(&self, cf_name: &str) -> Option<&rocksdb::ColumnFamily> {
        self.db.cf_handle(cf_name)
    }

    /// Returns an iterator over the keys in `cf_name`, starting from the first key.
    ///
    /// TODO: add an iterator wrapper struct that does disk reads in a blocking thread (#2188)
    pub fn forward_iterator(&self, cf_handle: &rocksdb::ColumnFamily) -> rocksdb::DBIterator {
        self.db.iterator_cf(cf_handle, rocksdb::IteratorMode::Start)
    }

    /// Returns a reverse iterator over the keys in `cf_name`, starting from the last key.
    ///
    /// TODO: add an iterator wrapper struct that does disk reads in a blocking thread (#2188)
    pub fn reverse_iterator(&self, cf_handle: &rocksdb::ColumnFamily) -> rocksdb::DBIterator {
        self.db.iterator_cf(cf_handle, rocksdb::IteratorMode::End)
    }

    /// Returns true if `cf` does not contain any entries.
    pub fn is_empty(&self, cf_handle: &rocksdb::ColumnFamily) -> bool {
        // Empty column families return invalid iterators.
        !self.forward_iterator(cf_handle).valid()
    }

    // Write methods

    /// Writes `batch` to the database.
    pub fn write(&self, batch: DiskWriteBatch) -> Result<(), rocksdb::Error> {
        // TODO: move writing to the database to a blocking thread (#2188)
        self.db.write(batch.batch)
    }

    // Private methods

    /// Returns the database options for the finalized state database.
    fn options() -> rocksdb::Options {
        let mut opts = rocksdb::Options::default();

        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        let open_file_limit = DiskDb::increase_open_file_limit();
        let db_file_limit = DiskDb::get_db_open_file_limit(open_file_limit);

        // If the current limit is very large, set the DB limit using the ideal limit
        let ideal_limit = DiskDb::get_db_open_file_limit(DiskDb::IDEAL_OPEN_FILE_LIMIT)
            .try_into()
            .expect("ideal open file limit fits in a c_int");
        let db_file_limit = db_file_limit.try_into().unwrap_or(ideal_limit);

        opts.set_max_open_files(db_file_limit);

        opts
    }

    /// Calculate the database's share of `open_file_limit`
    fn get_db_open_file_limit(open_file_limit: u64) -> u64 {
        // Give the DB half the files, and reserve half the files for peers
        (open_file_limit - DiskDb::RESERVED_FILE_COUNT) / 2
    }

    /// Increase the open file limit for this process to `IDEAL_OPEN_FILE_LIMIT`.
    /// If that fails, try `MIN_OPEN_FILE_LIMIT`.
    ///
    /// If the current limit is above `IDEAL_OPEN_FILE_LIMIT`, leaves it
    /// unchanged.
    ///
    /// Returns the current limit, after any successful increases.
    ///
    /// # Panics
    ///
    /// If the open file limit can not be increased to `MIN_OPEN_FILE_LIMIT`.
    fn increase_open_file_limit() -> u64 {
        // Zebra mainly uses TCP sockets (`zebra-network`) and low-level files
        // (`zebra-state` database).
        //
        // On Unix-based platforms, `increase_nofile_limit` changes the limit for
        // both database files and TCP connections.
        //
        // But it doesn't do anything on Windows in rlimit 0.7.0.
        //
        // On Windows, the default limits are:
        // - 512 high-level stream I/O files (via the C standard functions),
        // - 8192 low-level I/O files (via the Unix C functions), and
        // - 1000 TCP Control Block entries (network connections).
        //
        // https://docs.microsoft.com/en-us/cpp/c-runtime-library/reference/setmaxstdio?view=msvc-160#remarks
        // http://smallvoid.com/article/winnt-tcpip-max-limit.html
        //
        // `zebra-state`'s `IDEAL_OPEN_FILE_LIMIT` is much less than
        // the Windows low-level I/O file limit.
        //
        // The [`setmaxstdio` and `getmaxstdio`](https://docs.rs/rlimit/latest/rlimit/#windows)
        // functions from the `rlimit` crate only change the high-level I/O file limit.
        //
        // `zebra-network`'s default connection limit is much less than
        // the TCP Control Block limit on Windows.

        // We try setting the ideal limit, then the minimum limit.
        let current_limit = match increase_nofile_limit(DiskDb::IDEAL_OPEN_FILE_LIMIT) {
            Ok(current_limit) => current_limit,
            Err(limit_error) => {
                // These errors can happen due to sandboxing or unsupported system calls,
                // even if the file limit is high enough.
                info!(
                    ?limit_error,
                    min_limit = ?DiskDb::MIN_OPEN_FILE_LIMIT,
                    ideal_limit = ?DiskDb::IDEAL_OPEN_FILE_LIMIT,
                    "unable to increase the open file limit, \
                     assuming Zebra can open a minimum number of files"
                );

                return DiskDb::MIN_OPEN_FILE_LIMIT;
            }
        };

        if current_limit < DiskDb::MIN_OPEN_FILE_LIMIT {
            panic!(
                "open file limit too low: \
                 unable to set the number of open files to {}, \
                 the minimum number of files required by Zebra. \
                 Current limit is {:?}. \
                 Hint: Increase the open file limit to {} before launching Zebra",
                DiskDb::MIN_OPEN_FILE_LIMIT,
                current_limit,
                DiskDb::IDEAL_OPEN_FILE_LIMIT
            );
        } else if current_limit < DiskDb::IDEAL_OPEN_FILE_LIMIT {
            warn!(
                ?current_limit,
                min_limit = ?DiskDb::MIN_OPEN_FILE_LIMIT,
                ideal_limit = ?DiskDb::IDEAL_OPEN_FILE_LIMIT,
                "the maximum number of open files is below Zebra's ideal limit. \
                 Hint: Increase the open file limit to {} before launching Zebra",
                DiskDb::IDEAL_OPEN_FILE_LIMIT
            );
        } else if cfg!(windows) {
            info!(
                min_limit = ?DiskDb::MIN_OPEN_FILE_LIMIT,
                ideal_limit = ?DiskDb::IDEAL_OPEN_FILE_LIMIT,
                "assuming the open file limit is high enough for Zebra",
            );
        } else {
            info!(
                ?current_limit,
                min_limit = ?DiskDb::MIN_OPEN_FILE_LIMIT,
                ideal_limit = ?DiskDb::IDEAL_OPEN_FILE_LIMIT,
                "the open file limit is high enough for Zebra",
            );
        }

        current_limit
    }

    /// Shut down the database, cleaning up background tasks and ephemeral data.
    ///
    /// If `force` is true, clean up regardless of any shared references.
    /// `force` can cause errors accessing the database from other shared references.
    /// It should only be used in debugging or test code, immediately before a manual shutdown.
    ///
    /// TODO: make private after the stop height check has moved to the syncer (#3442)
    ///       move shutting down the database to a blocking thread (#2188),
    ///            and remove `force` and the manual flush
    pub(crate) fn shutdown(&mut self, force: bool) {
        // Prevent a race condition where another thread clones the Arc,
        // right after we've checked we're the only holder of the Arc.
        let clone_prevention_guard = Arc::get_mut(&mut self.db);

        if clone_prevention_guard.is_none() && !force {
            debug!(
                "dropping cloned DiskDb, \
                 but keeping shared database until the last reference is dropped",
            );

            return;
        }

        self.assert_default_cf_is_empty();

        // Drop isn't guaranteed to run, such as when we panic, or if the tokio shutdown times out.
        //
        // Zebra's data should be fine if we don't clean up, because:
        // - the database flushes regularly anyway
        // - Zebra commits each block in a database transaction, any incomplete blocks get rolled back
        // - ephemeral files are placed in the os temp dir and should be cleaned up automatically eventually
        info!("flushing database to disk");
        self.db.flush().expect("flush is successful");

        // But we should call `cancel_all_background_work` before Zebra exits.
        // If we don't, we see these kinds of errors:
        // ```
        // pthread lock: Invalid argument
        // pure virtual method called
        // terminate called without an active exception
        // pthread destroy mutex: Device or resource busy
        // Aborted (core dumped)
        // ```
        //
        // The RocksDB wiki says:
        // > Q: Is it safe to close RocksDB while another thread is issuing read, write or manual compaction requests?
        // >
        // > A: No. The users of RocksDB need to make sure all functions have finished before they close RocksDB.
        // > You can speed up the waiting by calling CancelAllBackgroundWork().
        //
        // https://github.com/facebook/rocksdb/wiki/RocksDB-FAQ
        info!("stopping background database tasks");
        self.db.cancel_all_background_work(true);

        // We'd like to drop the database before deleting its files,
        // because that closes the column families and the database correctly.
        // But Rust's ownership rules make that difficult,
        // so we just flush and delete ephemeral data instead.
        //
        // The RocksDB wiki says:
        // > rocksdb::DB instances need to be destroyed before your main function exits.
        // > RocksDB instances usually depend on some internal static variables.
        // > Users need to make sure rocksdb::DB instances are destroyed before those static variables.
        //
        // https://github.com/facebook/rocksdb/wiki/Known-Issues
        //
        // But our current code doesn't seem to cause any issues.
        // We might want to explicitly drop the database as part of graceful shutdown (#1678).
        self.delete_ephemeral(force);
    }

    /// If the database is `ephemeral`, delete it.
    ///
    /// If `force` is true, clean up regardless of any shared references.
    /// `force` can cause errors accessing the database from other shared references.
    /// It should only be used in debugging or test code, immediately before a manual shutdown.
    fn delete_ephemeral(&mut self, force: bool) {
        if !self.ephemeral {
            return;
        }

        // Prevent a race condition where another thread clones the Arc,
        // right after we've checked we're the only holder of the Arc.
        let clone_prevention_guard = Arc::get_mut(&mut self.db);

        if clone_prevention_guard.is_none() && !force {
            debug!(
                "dropping cloned DiskDb, \
                 but keeping shared database files until the last reference is dropped",
            );

            return;
        }

        let path = self.path();
        info!(cache_path = ?path, "removing temporary database files");

        // We'd like to use `rocksdb::Env::mem_env` for ephemeral databases,
        // but the Zcash blockchain might not fit in memory. So we just
        // delete the database files instead.
        //
        // We'd like to call `DB::destroy` here, but calling destroy on a
        // live DB is undefined behaviour:
        // https://github.com/facebook/rocksdb/wiki/RocksDB-FAQ#basic-readwrite
        //
        // So we assume that all the database files are under `path`, and
        // delete them using standard filesystem APIs. Deleting open files
        // might cause errors on non-Unix platforms, so we ignore the result.
        // (The OS will delete them eventually anyway.)
        let res = std::fs::remove_dir_all(path);

        // TODO: downgrade to debug once bugs like #2905 are fixed
        //       but leave any errors at "info" level
        info!(?res, "removed temporary database files");
    }

    /// Check that the "default" column family is empty.
    ///
    /// # Panics
    ///
    /// If Zebra has a bug where it is storing data in the wrong column family.
    fn assert_default_cf_is_empty(&self) {
        if let Some(default_cf) = self.cf_handle("default") {
            assert!(
                self.is_empty(default_cf),
                "Zebra should not store data in the 'default' column family"
            );
        }
    }
}

impl Drop for DiskDb {
    fn drop(&mut self) {
        self.shutdown(false);
    }
}
