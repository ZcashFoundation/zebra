//! Module defining access to RocksDB via accessor traits.
//!
//! This module makes sure that:
//! - all disk writes happen inside a RocksDB transaction, and
//! - format-specific invariants are maintained.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use std::fmt::Debug;

use rlimit::increase_nofile_limit;

use zebra_chain::parameters::Network;

use crate::{
    service::finalized_state::disk_format::{FromDisk, IntoDisk},
    Config,
};

/// Wrapper struct to ensure low-level disk reads go through
/// the correct API.
pub struct DiskDb(rocksdb::DB);

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

impl WriteDisk for rocksdb::WriteBatch {
    fn zs_insert<K, V>(&mut self, cf: &rocksdb::ColumnFamily, key: K, value: V)
    where
        K: IntoDisk + Debug,
        V: IntoDisk,
    {
        let key_bytes = key.as_bytes();
        let value_bytes = value.as_bytes();
        self.put_cf(cf, key_bytes, value_bytes);
    }

    fn zs_delete<K>(&mut self, cf: &rocksdb::ColumnFamily, key: K)
    where
        K: IntoDisk + Debug,
    {
        let key_bytes = key.as_bytes();
        self.delete_cf(cf, key_bytes);
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
        let value_bytes = self
            .0
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
        self.0
            .get_pinned_cf(cf, key_bytes)
            .expect("expected that disk errors would not occur")
            .is_some()
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

        let db_result = rocksdb::DB::open_cf_descriptors(&db_options, &path, column_families);

        match db_result {
            Ok(d) => {
                tracing::info!("Opened Zebra state cache at {}", path.display());
                DiskDb(d)
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

    /// Returns the database options for the finalized state database
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
        // `increase_nofile_limit` doesn't do anything on Windows in rlimit 0.7.0.
        //
        // On Windows, the default limit is:
        // - 512 high-level stream I/O files (via the C standard functions), and
        // - 8192 low-level I/O files (via the Unix C functions).
        // https://docs.microsoft.com/en-us/cpp/c-runtime-library/reference/setmaxstdio?view=msvc-160#remarks
        //
        // If we need more high-level I/O files on Windows,
        // use `setmaxstdio` and `getmaxstdio` from the `rlimit` crate:
        // https://docs.rs/rlimit/latest/rlimit/#windows
        //
        // Then panic if `setmaxstdio` fails to set the minimum value,
        // and `getmaxstdio` is below the minimum value.

        // We try setting the ideal limit, then the minimum limit.
        let current_limit = match increase_nofile_limit(DiskDb::IDEAL_OPEN_FILE_LIMIT) {
            Ok(current_limit) => current_limit,
            Err(limit_error) => {
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
}
