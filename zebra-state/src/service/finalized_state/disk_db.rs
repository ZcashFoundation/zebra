//! Provides low-level access to RocksDB using some database-specific types.
//!
//! This module makes sure that:
//! - all disk writes happen inside a RocksDB transaction
//!   ([`rocksdb::WriteBatch`]), and
//! - format-specific invariants are maintained.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constants must
//! be incremented each time the database format (column, serialization, etc) changes.

use std::{
    collections::{BTreeMap, HashMap},
    fmt::Debug,
    ops::RangeBounds,
    path::Path,
    sync::Arc,
};

use itertools::Itertools;
use rlimit::increase_nofile_limit;

use zebra_chain::parameters::Network;

use crate::{
    service::finalized_state::disk_format::{FromDisk, IntoDisk},
    Config,
};

#[cfg(any(test, feature = "proptest-impl"))]
mod tests;

/// The [`rocksdb::ThreadMode`] used by the database.
pub type DBThreadMode = rocksdb::SingleThreaded;

/// The [`rocksdb`] database type, including thread mode.
///
/// Also the [`rocksdb::DBAccess`] used by database iterators.
pub type DB = rocksdb::DBWithThreadMode<DBThreadMode>;

/// Wrapper struct to ensure low-level database access goes through the correct API.
///
/// `rocksdb` allows concurrent writes through a shared reference,
/// so database instances are cloneable. When the final clone is dropped,
/// the database is closed.
///
/// # Correctness
///
/// Reading transactions from the database using RocksDB iterators causes hangs.
/// But creating iterators and reading the tip height works fine.
///
/// So these hangs are probably caused by holding column family locks to read:
/// - multiple values, or
/// - large values.
///
/// This bug might be fixed by moving database operations to blocking threads (#2188),
/// so that they don't block the tokio executor.
/// (Or it might be fixed by future RocksDB upgrades.)
#[derive(Clone, Debug)]
pub struct DiskDb {
    // Configuration
    //
    // This configuration cannot be modified after the database is initialized,
    // because some clones would have different values.
    //
    /// The configured temporary database setting.
    ///
    /// If true, the database files are deleted on drop.
    ephemeral: bool,

    // Owned State
    //
    // Everything contained in this state must be shared by all clones, or read-only.
    //
    /// The shared inner RocksDB database.
    ///
    /// RocksDB allows reads and writes via a shared reference.
    ///
    /// In [`SingleThreaded`](rocksdb::SingleThreaded) mode,
    /// column family changes and [`Drop`] require exclusive access.
    ///
    /// In [`MultiThreaded`](rocksdb::MultiThreaded) mode,
    /// only [`Drop`] requires exclusive access.
    db: Arc<DB>,
}

/// Wrapper struct to ensure low-level database writes go through the correct API.
///
/// [`rocksdb::WriteBatch`] is a batched set of database updates,
/// which must be written to the database using `DiskDb::write(batch)`.
//
// TODO: move DiskDb, FinalizedBlock, and the source String into this struct,
//       (DiskDb can be cloned),
//       and make them accessible via read-only methods
#[must_use = "batches must be written to the database"]
#[derive(Default)]
pub struct DiskWriteBatch {
    /// The inner RocksDB write batch.
    batch: rocksdb::WriteBatch,
}

/// Helper trait for inserting (Key, Value) pairs into rocksdb with a consistently
/// defined format
//
// TODO: just implement these methods directly on WriteBatch
pub trait WriteDisk {
    /// Serialize and insert the given key and value into a rocksdb column family,
    /// overwriting any existing `value` for `key`.
    fn zs_insert<C, K, V>(&mut self, cf: &C, key: K, value: V)
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + Debug,
        V: IntoDisk;

    /// Remove the given key from rocksdb column family if it exists.
    fn zs_delete<C, K>(&mut self, cf: &C, key: K)
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + Debug;

    /// Remove the given key range from rocksdb column family if it exists.
    fn zs_delete_range<C, K>(&mut self, cf: &C, from: K, to: K)
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + Debug;
}

impl WriteDisk for DiskWriteBatch {
    fn zs_insert<C, K, V>(&mut self, cf: &C, key: K, value: V)
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + Debug,
        V: IntoDisk,
    {
        let key_bytes = key.as_bytes();
        let value_bytes = value.as_bytes();
        self.batch.put_cf(cf, key_bytes, value_bytes);
    }

    fn zs_delete<C, K>(&mut self, cf: &C, key: K)
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + Debug,
    {
        let key_bytes = key.as_bytes();
        self.batch.delete_cf(cf, key_bytes);
    }

    fn zs_delete_range<C, K>(&mut self, cf: &C, from: K, to: K)
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + Debug,
    {
        let from_bytes = from.as_bytes();
        let to_bytes = to.as_bytes();
        self.batch.delete_range_cf(cf, from_bytes, to_bytes);
    }
}

/// Helper trait for retrieving values from rocksdb column familys with a consistently
/// defined format
//
// TODO: just implement these methods directly on DiskDb
//       move this trait, its methods, and support methods to another module
pub trait ReadDisk {
    /// Returns true if a rocksdb column family `cf` does not contain any entries.
    fn zs_is_empty<C>(&self, cf: &C) -> bool
    where
        C: rocksdb::AsColumnFamilyRef;

    /// Returns the value for `key` in the rocksdb column family `cf`, if present.
    fn zs_get<C, K, V>(&self, cf: &C, key: &K) -> Option<V>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk,
        V: FromDisk;

    /// Check if a rocksdb column family `cf` contains the serialized form of `key`.
    fn zs_contains<C, K>(&self, cf: &C, key: &K) -> bool
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk;

    /// Returns the lowest key in `cf`, and the corresponding value.
    ///
    /// Returns `None` if the column family is empty.
    fn zs_first_key_value<C, K, V>(&self, cf: &C) -> Option<(K, V)>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: FromDisk,
        V: FromDisk;

    /// Returns the highest key in `cf`, and the corresponding value.
    ///
    /// Returns `None` if the column family is empty.
    fn zs_last_key_value<C, K, V>(&self, cf: &C) -> Option<(K, V)>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: FromDisk,
        V: FromDisk;

    /// Returns the first key greater than or equal to `lower_bound` in `cf`,
    /// and the corresponding value.
    ///
    /// Returns `None` if there are no keys greater than or equal to `lower_bound`.
    fn zs_next_key_value_from<C, K, V>(&self, cf: &C, lower_bound: &K) -> Option<(K, V)>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + FromDisk,
        V: FromDisk;

    /// Returns the first key less than or equal to `upper_bound` in `cf`,
    /// and the corresponding value.
    ///
    /// Returns `None` if there are no keys less than or equal to `upper_bound`.
    fn zs_prev_key_value_back_from<C, K, V>(&self, cf: &C, upper_bound: &K) -> Option<(K, V)>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + FromDisk,
        V: FromDisk;

    /// Returns the keys and values in `cf` in `range`, in an ordered `BTreeMap`.
    ///
    /// Holding this iterator open might delay block commit transactions.
    fn zs_items_in_range_ordered<C, K, V, R>(&self, cf: &C, range: R) -> BTreeMap<K, V>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + FromDisk + Ord,
        V: FromDisk,
        R: RangeBounds<K>;

    /// Returns the keys and values in `cf` in `range`, in an unordered `HashMap`.
    ///
    /// Holding this iterator open might delay block commit transactions.
    fn zs_items_in_range_unordered<C, K, V, R>(&self, cf: &C, range: R) -> HashMap<K, V>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + FromDisk + Eq + std::hash::Hash,
        V: FromDisk,
        R: RangeBounds<K>;
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
    fn zs_is_empty<C>(&self, cf: &C) -> bool
    where
        C: rocksdb::AsColumnFamilyRef,
    {
        // Empty column families return invalid forward iterators.
        //
        // Checking iterator validity does not seem to cause database hangs.
        let iterator = self.db.iterator_cf(cf, rocksdb::IteratorMode::Start);
        let raw_iterator: rocksdb::DBRawIteratorWithThreadMode<DB> = iterator.into();

        !raw_iterator.valid()
    }

    #[allow(clippy::unwrap_in_result)]
    fn zs_get<C, K, V>(&self, cf: &C, key: &K) -> Option<V>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk,
        V: FromDisk,
    {
        let key_bytes = key.as_bytes();

        // We use `get_pinned_cf` to avoid taking ownership of the serialized
        // value, because we're going to deserialize it anyways, which avoids an
        // extra copy
        let value_bytes = self
            .db
            .get_pinned_cf(cf, key_bytes)
            .expect("unexpected database failure");

        value_bytes.map(V::from_bytes)
    }

    fn zs_contains<C, K>(&self, cf: &C, key: &K) -> bool
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk,
    {
        let key_bytes = key.as_bytes();

        // We use `get_pinned_cf` to avoid taking ownership of the serialized
        // value, because we don't use the value at all. This avoids an extra copy.
        self.db
            .get_pinned_cf(cf, key_bytes)
            .expect("unexpected database failure")
            .is_some()
    }

    #[allow(clippy::unwrap_in_result)]
    fn zs_first_key_value<C, K, V>(&self, cf: &C) -> Option<(K, V)>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: FromDisk,
        V: FromDisk,
    {
        // Reading individual values from iterators does not seem to cause database hangs.
        self.db
            .iterator_cf(cf, rocksdb::IteratorMode::Start)
            .next()?
            .map(|(key_bytes, value_bytes)| {
                Some((K::from_bytes(key_bytes), V::from_bytes(value_bytes)))
            })
            .expect("unexpected database failure")
    }

    #[allow(clippy::unwrap_in_result)]
    fn zs_last_key_value<C, K, V>(&self, cf: &C) -> Option<(K, V)>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: FromDisk,
        V: FromDisk,
    {
        // Reading individual values from iterators does not seem to cause database hangs.
        self.db
            .iterator_cf(cf, rocksdb::IteratorMode::End)
            .next()?
            .map(|(key_bytes, value_bytes)| {
                Some((K::from_bytes(key_bytes), V::from_bytes(value_bytes)))
            })
            .expect("unexpected database failure")
    }

    #[allow(clippy::unwrap_in_result)]
    fn zs_next_key_value_from<C, K, V>(&self, cf: &C, lower_bound: &K) -> Option<(K, V)>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + FromDisk,
        V: FromDisk,
    {
        let lower_bound = lower_bound.as_bytes();
        let from = rocksdb::IteratorMode::From(lower_bound.as_ref(), rocksdb::Direction::Forward);

        // Reading individual values from iterators does not seem to cause database hangs.
        self.db
            .iterator_cf(cf, from)
            .next()?
            .map(|(key_bytes, value_bytes)| {
                Some((K::from_bytes(key_bytes), V::from_bytes(value_bytes)))
            })
            .expect("unexpected database failure")
    }

    #[allow(clippy::unwrap_in_result)]
    fn zs_prev_key_value_back_from<C, K, V>(&self, cf: &C, upper_bound: &K) -> Option<(K, V)>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + FromDisk,
        V: FromDisk,
    {
        let upper_bound = upper_bound.as_bytes();
        let from = rocksdb::IteratorMode::From(upper_bound.as_ref(), rocksdb::Direction::Reverse);

        // Reading individual values from iterators does not seem to cause database hangs.
        self.db
            .iterator_cf(cf, from)
            .next()?
            .map(|(key_bytes, value_bytes)| {
                Some((K::from_bytes(key_bytes), V::from_bytes(value_bytes)))
            })
            .expect("unexpected database failure")
    }

    fn zs_items_in_range_ordered<C, K, V, R>(&self, cf: &C, range: R) -> BTreeMap<K, V>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + FromDisk + Ord,
        V: FromDisk,
        R: RangeBounds<K>,
    {
        self.zs_range_iter(cf, range).collect()
    }

    fn zs_items_in_range_unordered<C, K, V, R>(&self, cf: &C, range: R) -> HashMap<K, V>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + FromDisk + Eq + std::hash::Hash,
        V: FromDisk,
        R: RangeBounds<K>,
    {
        self.zs_range_iter(cf, range).collect()
    }
}

impl DiskWriteBatch {
    /// Creates and returns a new transactional batch write.
    ///
    /// # Correctness
    ///
    /// Each block must be written to the state inside a batch, so that:
    /// - concurrent `ReadStateService` queries don't see half-written blocks, and
    /// - if Zebra calls `exit`, panics, or crashes, half-written blocks are rolled back.
    pub fn new() -> Self {
        DiskWriteBatch {
            batch: rocksdb::WriteBatch::default(),
        }
    }
}

impl DiskDb {
    /// Returns an iterator over the items in `cf` in `range`.
    ///
    /// Holding this iterator open might delay block commit transactions.
    pub fn zs_range_iter<C, K, V, R>(&self, cf: &C, range: R) -> impl Iterator<Item = (K, V)> + '_
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + FromDisk,
        V: FromDisk,
        R: RangeBounds<K>,
    {
        use std::ops::Bound::{self, *};

        // Replace with map() when it stabilises:
        // https://github.com/rust-lang/rust/issues/86026
        let map_to_vec = |bound: Bound<&K>| -> Bound<Vec<u8>> {
            match bound {
                Unbounded => Unbounded,
                Included(x) => Included(x.as_bytes().as_ref().to_vec()),
                Excluded(x) => Excluded(x.as_bytes().as_ref().to_vec()),
            }
        };

        let start_bound = map_to_vec(range.start_bound());
        let end_bound = map_to_vec(range.end_bound());
        let range = (start_bound.clone(), end_bound);

        let start_bound_vec =
            if let Included(ref start_bound) | Excluded(ref start_bound) = start_bound {
                start_bound.clone()
            } else {
                // Actually unused
                Vec::new()
            };

        let start_mode = if matches!(start_bound, Unbounded) {
            // Unbounded iterators start at the first item
            rocksdb::IteratorMode::Start
        } else {
            rocksdb::IteratorMode::From(start_bound_vec.as_slice(), rocksdb::Direction::Forward)
        };

        // Reading multiple items from iterators has caused database hangs,
        // in previous RocksDB versions
        self.db
            .iterator_cf(cf, start_mode)
            .map(|result| result.expect("unexpected database failure"))
            .map(|(key, value)| (key.to_vec(), value))
            // Handle Excluded start and the end bound
            .take_while(move |(key, _value)| range.contains(key))
            .map(|(key, value)| (K::from_bytes(key), V::from_bytes(value)))
    }

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
    /// <https://docs.microsoft.com/en-us/cpp/c-runtime-library/reference/setmaxstdio?view=msvc-160#remarks>
    const MIN_OPEN_FILE_LIMIT: u64 = 512;

    /// The number of files used internally by Zebra.
    ///
    /// Zebra uses file descriptors for OS libraries (10+), polling APIs (10+),
    /// stdio (3), and other OS facilities (2+).
    const RESERVED_FILE_COUNT: u64 = 48;

    /// The size of the database memtable RAM cache in megabytes.
    ///
    /// <https://github.com/facebook/rocksdb/wiki/RocksDB-FAQ#configuration-and-tuning>
    const MEMTABLE_RAM_CACHE_MEGABYTES: usize = 128;

    /// The column families supported by the running database code.
    const COLUMN_FAMILIES_IN_CODE: &[&'static str] = &[
        // Blocks
        "hash_by_height",
        "height_by_hash",
        "block_header_by_height",
        // Transactions
        "tx_by_loc",
        "hash_by_tx_loc",
        "tx_loc_by_hash",
        // Transparent
        "balance_by_transparent_addr",
        "tx_loc_by_transparent_addr_loc",
        "utxo_by_out_loc",
        "utxo_loc_by_transparent_addr_loc",
        // Sprout
        "sprout_nullifiers",
        "sprout_anchors",
        "sprout_note_commitment_tree",
        // Sapling
        "sapling_nullifiers",
        "sapling_anchors",
        "sapling_note_commitment_tree",
        // Orchard
        "orchard_nullifiers",
        "orchard_anchors",
        "orchard_note_commitment_tree",
        // Chain
        "history_tree",
        "tip_chain_value_pool",
    ];

    /// Opens or creates the database at `config.path` for `network`,
    /// and returns a shared low-level database wrapper.
    pub fn new(config: &Config, network: Network) -> DiskDb {
        let path = config.db_path(network);

        let db_options = DiskDb::options();

        // When opening the database in read/write mode, all column families must be opened.
        //
        // To make Zebra forward-compatible with databases updated by later versions,
        // we read any existing column families off the disk, then add any new column families
        // from the current implementation.
        //
        // <https://github.com/facebook/rocksdb/wiki/Column-Families#reference>
        let column_families_on_disk = DB::list_cf(&db_options, &path).unwrap_or_default();
        let column_families_in_code = Self::COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string);

        let column_families = column_families_on_disk
            .into_iter()
            .chain(column_families_in_code)
            .unique()
            .map(|cf_name| rocksdb::ColumnFamilyDescriptor::new(cf_name, db_options.clone()));

        let db_result = DB::open_cf_descriptors(&db_options, &path, column_families);

        match db_result {
            Ok(db) => {
                info!("Opened Zebra state cache at {}", path.display());

                let db = DiskDb {
                    ephemeral: config.ephemeral,
                    db: Arc::new(db),
                };

                db.assert_default_cf_is_empty();

                db
            }

            // TODO: provide a different hint if the disk is full, see #1623
            Err(e) => panic!(
                "Opening database {path:?} failed: {e:?}. \
                 Hint: Check if another zebrad process is running. \
                 Try changing the state cache_dir in the Zebra config.",
            ),
        }
    }

    // Accessor methods

    /// Returns the `Path` where the files used by this database are located.
    pub fn path(&self) -> &Path {
        self.db.path()
    }

    /// Returns the column family handle for `cf_name`.
    pub fn cf_handle(&self, cf_name: &str) -> Option<impl rocksdb::AsColumnFamilyRef + '_> {
        self.db.cf_handle(cf_name)
    }

    // Read methods are located in the ReadDisk trait

    // Write methods
    // Low-level write methods are located in the WriteDisk trait

    /// Writes `batch` to the database.
    pub fn write(&self, batch: DiskWriteBatch) -> Result<(), rocksdb::Error> {
        self.db.write(batch.batch)
    }

    // Private methods

    /// Returns the database options for the finalized state database.
    fn options() -> rocksdb::Options {
        let mut opts = rocksdb::Options::default();
        let mut block_based_opts = rocksdb::BlockBasedOptions::default();

        const ONE_MEGABYTE: usize = 1024 * 1024;

        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        // Use the recommended Ribbon filter setting for all column families.
        //
        // Ribbon filters are faster than Bloom filters in Zebra, as of April 2022.
        // (They aren't needed for single-valued column families, but they don't hurt either.)
        block_based_opts.set_ribbon_filter(9.9);

        // Use the recommended LZ4 compression type.
        //
        // https://github.com/facebook/rocksdb/wiki/Compression#configuration
        opts.set_compression_type(rocksdb::DBCompressionType::Lz4);

        // Tune level-style database file compaction.
        //
        // This improves Zebra's initial sync speed slightly, as of April 2022.
        opts.optimize_level_style_compaction(Self::MEMTABLE_RAM_CACHE_MEGABYTES * ONE_MEGABYTE);

        // Increase the process open file limit if needed,
        // then use it to set RocksDB's limit.
        let open_file_limit = DiskDb::increase_open_file_limit();
        let db_file_limit = DiskDb::get_db_open_file_limit(open_file_limit);

        // If the current limit is very large, set the DB limit using the ideal limit
        let ideal_limit = DiskDb::get_db_open_file_limit(DiskDb::IDEAL_OPEN_FILE_LIMIT)
            .try_into()
            .expect("ideal open file limit fits in a c_int");
        let db_file_limit = db_file_limit.try_into().unwrap_or(ideal_limit);

        opts.set_max_open_files(db_file_limit);

        // Set the block-based options
        opts.set_block_based_table_factory(&block_based_opts);

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
            // This log is verbose during tests.
            #[cfg(not(test))]
            info!(
                min_limit = ?DiskDb::MIN_OPEN_FILE_LIMIT,
                ideal_limit = ?DiskDb::IDEAL_OPEN_FILE_LIMIT,
                "assuming the open file limit is high enough for Zebra",
            );
            #[cfg(test)]
            debug!(
                min_limit = ?DiskDb::MIN_OPEN_FILE_LIMIT,
                ideal_limit = ?DiskDb::IDEAL_OPEN_FILE_LIMIT,
                "assuming the open file limit is high enough for Zebra",
            );
        } else {
            #[cfg(not(test))]
            info!(
                ?current_limit,
                min_limit = ?DiskDb::MIN_OPEN_FILE_LIMIT,
                ideal_limit = ?DiskDb::IDEAL_OPEN_FILE_LIMIT,
                "the open file limit is high enough for Zebra",
            );
            #[cfg(test)]
            debug!(
                ?current_limit,
                min_limit = ?DiskDb::MIN_OPEN_FILE_LIMIT,
                ideal_limit = ?DiskDb::IDEAL_OPEN_FILE_LIMIT,
                "the open file limit is high enough for Zebra",
            );
        }

        current_limit
    }

    // Cleanup methods

    /// Returns the number of shared instances of this database.
    ///
    /// # Concurrency
    ///
    /// The actual number of owners can be higher or lower than the returned value,
    /// because databases can simultaneously be cloned or dropped in other threads.
    ///
    /// However, if the number of owners is 1, and the caller has exclusive access,
    /// the count can't increase unless that caller clones the database.
    pub(crate) fn shared_database_owners(&self) -> usize {
        Arc::strong_count(&self.db) + Arc::weak_count(&self.db)
    }

    /// Shut down the database, cleaning up background tasks and ephemeral data.
    ///
    /// If `force` is true, clean up regardless of any shared references.
    /// `force` can cause errors accessing the database from other shared references.
    /// It should only be used in debugging or test code, immediately before a manual shutdown.
    ///
    /// TODO: make private after the stop height check has moved to the syncer (#3442)
    ///       move shutting down the database to a blocking thread (#2188)
    pub(crate) fn shutdown(&mut self, force: bool) {
        // # Correctness
        //
        // If we're the only owner of the shared database instance,
        // then there are no other threads that can increase the strong or weak count.
        //
        // ## Implementation Requirements
        //
        // This function and all functions that it calls should avoid cloning the shared database
        // instance. If they do, they must drop it before:
        // - shutting down database threads, or
        // - deleting database files.

        if self.shared_database_owners() > 1 {
            let path = self.path();

            let mut ephemeral_note = "";

            if force {
                if self.ephemeral {
                    ephemeral_note = " and removing ephemeral files";
                }

                // This log is verbose during tests.
                #[cfg(not(test))]
                info!(
                    ?path,
                    "forcing shutdown{} of a state database with multiple active instances",
                    ephemeral_note,
                );
                #[cfg(test)]
                debug!(
                    ?path,
                    "forcing shutdown{} of a state database with multiple active instances",
                    ephemeral_note,
                );
            } else {
                if self.ephemeral {
                    ephemeral_note = " and files";
                }

                debug!(
                    ?path,
                    "dropping DiskDb clone, \
                     but keeping shared database instance{} until the last reference is dropped",
                    ephemeral_note,
                );
                return;
            }
        }

        self.assert_default_cf_is_empty();

        // Drop isn't guaranteed to run, such as when we panic, or if the tokio shutdown times out.
        //
        // Zebra's data should be fine if we don't clean up, because:
        // - the database flushes regularly anyway
        // - Zebra commits each block in a database transaction, any incomplete blocks get rolled back
        // - ephemeral files are placed in the os temp dir and should be cleaned up automatically eventually
        let path = self.path();
        debug!(?path, "flushing database to disk");

        // These flushes can fail during forced shutdown or during Drop after a shutdown,
        // particularly in tests. If they fail, there's nothing we can do about it anyway.
        if let Err(error) = self.db.flush() {
            let error = format!("{error:?}");
            if error.to_ascii_lowercase().contains("shutdown in progress") {
                debug!(
                    ?error,
                    ?path,
                    "expected shutdown error flushing database SST files to disk"
                );
            } else {
                info!(
                    ?error,
                    ?path,
                    "unexpected error flushing database SST files to disk during shutdown"
                );
            }
        }

        if let Err(error) = self.db.flush_wal(true) {
            let error = format!("{error:?}");
            if error.to_ascii_lowercase().contains("shutdown in progress") {
                debug!(
                    ?error,
                    ?path,
                    "expected shutdown error flushing database WAL buffer to disk"
                );
            } else {
                info!(
                    ?error,
                    ?path,
                    "unexpected error flushing database WAL buffer to disk during shutdown"
                );
            }
        }

        // # Memory Safety
        //
        // We'd like to call `cancel_all_background_work()` before Zebra exits,
        // but when we call it, we get memory, thread, or C++ errors when the process exits.
        // (This seems to be a bug in RocksDB: cancel_all_background_work() should wait until
        // all the threads have cleaned up.)
        //
        // # Change History
        //
        // We've changed this setting multiple times since 2021, in response to new RocksDB
        // and Rust compiler behaviour.
        //
        // We enabled cancel_all_background_work() due to failures on:
        // - Rust 1.57 on Linux
        //
        // We disabled cancel_all_background_work() due to failures on:
        // - Rust 1.64 on Linux
        //
        // We tried enabling cancel_all_background_work() due to failures on:
        // - Rust 1.70 on macOS 12.6.5 on x86_64
        // but it didn't stop the aborts happening (PR #6820).
        //
        // There weren't any failures with cancel_all_background_work() disabled on:
        // - Rust 1.69 or earlier
        // - Linux with Rust 1.70
        // And with cancel_all_background_work() enabled or disabled on:
        // - macOS 13.2 on aarch64 (M1), native and emulated x86_64, with Rust 1.70
        //
        // # Detailed Description
        //
        // We see these kinds of errors:
        // ```
        // pthread lock: Invalid argument
        // pure virtual method called
        // terminate called without an active exception
        // pthread destroy mutex: Device or resource busy
        // Aborted (core dumped)
        // signal: 6, SIGABRT: process abort signal
        // signal: 11, SIGSEGV: invalid memory reference
        // ```
        //
        // # Reference
        //
        // The RocksDB wiki says:
        // > Q: Is it safe to close RocksDB while another thread is issuing read, write or manual compaction requests?
        // >
        // > A: No. The users of RocksDB need to make sure all functions have finished before they close RocksDB.
        // > You can speed up the waiting by calling CancelAllBackgroundWork().
        //
        // <https://github.com/facebook/rocksdb/wiki/RocksDB-FAQ>
        //
        // > rocksdb::DB instances need to be destroyed before your main function exits.
        // > RocksDB instances usually depend on some internal static variables.
        // > Users need to make sure rocksdb::DB instances are destroyed before those static variables.
        //
        // <https://github.com/facebook/rocksdb/wiki/Known-Issues>
        //
        // # TODO
        //
        // Try re-enabling this code and fixing the underlying concurrency bug.
        //
        //info!(?path, "stopping background database tasks");
        //self.db.cancel_all_background_work(true);

        // We'd like to drop the database before deleting its files,
        // because that closes the column families and the database correctly.
        // But Rust's ownership rules make that difficult,
        // so we just flush and delete ephemeral data instead.
        //
        // This implementation doesn't seem to cause any issues,
        // and the RocksDB Drop implementation handles any cleanup.
        self.delete_ephemeral();
    }

    /// If the database is `ephemeral`, delete its files.
    fn delete_ephemeral(&mut self) {
        // # Correctness
        //
        // This function and all functions that it calls should avoid cloning the shared database
        // instance. See `shutdown()` for details.

        if !self.ephemeral {
            return;
        }

        let path = self.path();

        // This log is verbose during tests.
        #[cfg(not(test))]
        info!(?path, "removing temporary database files");
        #[cfg(test)]
        debug!(?path, "removing temporary database files");

        // We'd like to use `rocksdb::Env::mem_env` for ephemeral databases,
        // but the Zcash blockchain might not fit in memory. So we just
        // delete the database files instead.
        //
        // We'd also like to call `DB::destroy` here, but calling destroy on a
        // live DB is undefined behaviour:
        // https://github.com/facebook/rocksdb/wiki/RocksDB-FAQ#basic-readwrite
        //
        // So we assume that all the database files are under `path`, and
        // delete them using standard filesystem APIs. Deleting open files
        // might cause errors on non-Unix platforms, so we ignore the result.
        // (The OS will delete them eventually anyway, if they are in a temporary directory.)
        let result = std::fs::remove_dir_all(path);

        if result.is_err() {
            // This log is verbose during tests.
            #[cfg(not(test))]
            info!(
                ?result,
                ?path,
                "removing temporary database files caused an error",
            );
            #[cfg(test)]
            debug!(
                ?result,
                ?path,
                "removing temporary database files caused an error",
            );
        } else {
            debug!(
                ?result,
                ?path,
                "successfully removed temporary database files",
            );
        }
    }

    /// Check that the "default" column family is empty.
    ///
    /// # Panics
    ///
    /// If Zebra has a bug where it is storing data in the wrong column family.
    fn assert_default_cf_is_empty(&self) {
        // # Correctness
        //
        // This function and all functions that it calls should avoid cloning the shared database
        // instance. See `shutdown()` for details.

        if let Some(default_cf) = self.cf_handle("default") {
            assert!(
                self.zs_is_empty(&default_cf),
                "Zebra should not store data in the 'default' column family"
            );
        }
    }
}

impl Drop for DiskDb {
    fn drop(&mut self) {
        let path = self.path();
        debug!(?path, "dropping DiskDb instance");

        self.shutdown(false);
    }
}
