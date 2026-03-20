//! Provides low-level access to RocksDB using some database-specific types.
//!
//! This module makes sure that:
//! - all disk writes happen inside a RocksDB transaction
//!   ([`rocksdb::WriteBatch`]), and
//! - format-specific invariants are maintained.
//!
//! # Correctness
//!
//! [`crate::constants::state_database_format_version_in_code()`] must be incremented
//! each time the database format (column, serialization, etc) changes.

use std::{
    collections::{BTreeMap, HashMap},
    fmt::{Debug, Write},
    fs,
    ops::RangeBounds,
    path::Path,
    sync::{
        atomic::{self, AtomicBool},
        Arc,
    },
};

use itertools::Itertools;
use rlimit::increase_nofile_limit;

use rocksdb::{ColumnFamilyDescriptor, ErrorKind, Options, ReadOptions};
use semver::Version;
use zebra_chain::{parameters::Network, primitives::byte_array::increment_big_endian};

use crate::{
    database_format_version_on_disk,
    service::finalized_state::disk_format::{FromDisk, IntoDisk},
    write_database_format_version_to_disk, Config,
};

use super::zebra_db::transparent::{
    fetch_add_balance_and_received, BALANCE_BY_TRANSPARENT_ADDR,
    BALANCE_BY_TRANSPARENT_ADDR_MERGE_OP,
};
// Doc-only imports
#[allow(unused_imports)]
use super::{TypedColumnFamily, WriteTypedBatch};

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
    /// The configured database kind for this database.
    db_kind: String,

    /// The format version of the running Zebra code.
    format_version_in_code: Version,

    /// The configured network for this database.
    network: Network,

    /// The configured temporary database setting.
    ///
    /// If true, the database files are deleted on drop.
    ephemeral: bool,

    /// A boolean flag indicating whether the db format change task has finished
    /// applying any format changes that may have been required.
    finished_format_upgrades: Arc<AtomicBool>,

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
#[must_use = "batches must be written to the database"]
#[derive(Default)]
pub struct DiskWriteBatch {
    /// The inner RocksDB write batch.
    batch: rocksdb::WriteBatch,
}

impl Debug for DiskWriteBatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskWriteBatch")
            .field("batch", &format!("{} bytes", self.batch.size_in_bytes()))
            .finish()
    }
}

impl PartialEq for DiskWriteBatch {
    fn eq(&self, other: &Self) -> bool {
        self.batch.data() == other.batch.data()
    }
}

impl Eq for DiskWriteBatch {}

/// Helper trait for inserting serialized typed (Key, Value) pairs into rocksdb.
///
/// # Deprecation
///
/// This trait should not be used in new code, use [`WriteTypedBatch`] instead.
//
// TODO: replace uses of this trait with WriteTypedBatch,
//       implement these methods directly on WriteTypedBatch, and delete the trait.
pub trait WriteDisk {
    /// Serialize and insert the given key and value into a rocksdb column family,
    /// overwriting any existing `value` for `key`.
    fn zs_insert<C, K, V>(&mut self, cf: &C, key: K, value: V)
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + Debug,
        V: IntoDisk;

    /// Serialize and merge the given key and value into a rocksdb column family,
    /// merging with any existing `value` for `key`.
    fn zs_merge<C, K, V>(&mut self, cf: &C, key: K, value: V)
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + Debug,
        V: IntoDisk;

    /// Remove the given key from a rocksdb column family, if it exists.
    fn zs_delete<C, K>(&mut self, cf: &C, key: K)
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + Debug;

    /// Delete the given key range from a rocksdb column family, if it exists, including `from`
    /// and excluding `until_strictly_before`.
    //
    // TODO: convert zs_delete_range() to take std::ops::RangeBounds
    //       see zs_range_iter() for an example of the edge cases
    fn zs_delete_range<C, K>(&mut self, cf: &C, from: K, until_strictly_before: K)
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + Debug;
}

/// # Deprecation
///
/// These impls should not be used in new code, use [`WriteTypedBatch`] instead.
//
// TODO: replace uses of these impls with WriteTypedBatch,
//       implement these methods directly on WriteTypedBatch, and delete the trait.
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

    fn zs_merge<C, K, V>(&mut self, cf: &C, key: K, value: V)
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + Debug,
        V: IntoDisk,
    {
        let key_bytes = key.as_bytes();
        let value_bytes = value.as_bytes();
        self.batch.merge_cf(cf, key_bytes, value_bytes);
    }

    fn zs_delete<C, K>(&mut self, cf: &C, key: K)
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + Debug,
    {
        let key_bytes = key.as_bytes();
        self.batch.delete_cf(cf, key_bytes);
    }

    // TODO: convert zs_delete_range() to take std::ops::RangeBounds
    //       see zs_range_iter() for an example of the edge cases
    fn zs_delete_range<C, K>(&mut self, cf: &C, from: K, until_strictly_before: K)
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + Debug,
    {
        let from_bytes = from.as_bytes();
        let until_strictly_before_bytes = until_strictly_before.as_bytes();
        self.batch
            .delete_range_cf(cf, from_bytes, until_strictly_before_bytes);
    }
}

// Allow &mut DiskWriteBatch as well as owned DiskWriteBatch
impl<T> WriteDisk for &mut T
where
    T: WriteDisk,
{
    fn zs_insert<C, K, V>(&mut self, cf: &C, key: K, value: V)
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + Debug,
        V: IntoDisk,
    {
        (*self).zs_insert(cf, key, value)
    }

    fn zs_merge<C, K, V>(&mut self, cf: &C, key: K, value: V)
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + Debug,
        V: IntoDisk,
    {
        (*self).zs_merge(cf, key, value)
    }

    fn zs_delete<C, K>(&mut self, cf: &C, key: K)
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + Debug,
    {
        (*self).zs_delete(cf, key)
    }

    fn zs_delete_range<C, K>(&mut self, cf: &C, from: K, until_strictly_before: K)
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + Debug,
    {
        (*self).zs_delete_range(cf, from, until_strictly_before)
    }
}

/// Helper trait for retrieving and deserializing values from rocksdb column families.
///
/// # Deprecation
///
/// This trait should not be used in new code, use [`TypedColumnFamily`] instead.
//
// TODO: replace uses of this trait with TypedColumnFamily,
//       implement these methods directly on DiskDb, and delete the trait.
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
        K: IntoDisk + FromDisk,
        V: FromDisk;

    /// Returns the highest key in `cf`, and the corresponding value.
    ///
    /// Returns `None` if the column family is empty.
    fn zs_last_key_value<C, K, V>(&self, cf: &C) -> Option<(K, V)>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + FromDisk,
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

    /// Returns the first key strictly greater than `lower_bound` in `cf`,
    /// and the corresponding value.
    ///
    /// Returns `None` if there are no keys greater than `lower_bound`.
    fn zs_next_key_value_strictly_after<C, K, V>(&self, cf: &C, lower_bound: &K) -> Option<(K, V)>
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

    /// Returns the first key strictly less than `upper_bound` in `cf`,
    /// and the corresponding value.
    ///
    /// Returns `None` if there are no keys less than `upper_bound`.
    fn zs_prev_key_value_strictly_before<C, K, V>(&self, cf: &C, upper_bound: &K) -> Option<(K, V)>
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
                self.network, other.network,
                "database with same path but different network configs",
            );
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

/// # Deprecation
///
/// These impls should not be used in new code, use [`TypedColumnFamily`] instead.
//
// TODO: replace uses of these impls with TypedColumnFamily,
//       implement these methods directly on DiskDb, and delete the trait.
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

    fn zs_first_key_value<C, K, V>(&self, cf: &C) -> Option<(K, V)>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + FromDisk,
        V: FromDisk,
    {
        // Reading individual values from iterators does not seem to cause database hangs.
        self.zs_forward_range_iter(cf, ..).next()
    }

    fn zs_last_key_value<C, K, V>(&self, cf: &C) -> Option<(K, V)>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + FromDisk,
        V: FromDisk,
    {
        // Reading individual values from iterators does not seem to cause database hangs.
        self.zs_reverse_range_iter(cf, ..).next()
    }

    fn zs_next_key_value_from<C, K, V>(&self, cf: &C, lower_bound: &K) -> Option<(K, V)>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + FromDisk,
        V: FromDisk,
    {
        self.zs_forward_range_iter(cf, lower_bound..).next()
    }

    fn zs_next_key_value_strictly_after<C, K, V>(&self, cf: &C, lower_bound: &K) -> Option<(K, V)>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + FromDisk,
        V: FromDisk,
    {
        use std::ops::Bound::*;

        // There is no standard syntax for an excluded start bound.
        self.zs_forward_range_iter(cf, (Excluded(lower_bound), Unbounded))
            .next()
    }

    fn zs_prev_key_value_back_from<C, K, V>(&self, cf: &C, upper_bound: &K) -> Option<(K, V)>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + FromDisk,
        V: FromDisk,
    {
        self.zs_reverse_range_iter(cf, ..=upper_bound).next()
    }

    fn zs_prev_key_value_strictly_before<C, K, V>(&self, cf: &C, upper_bound: &K) -> Option<(K, V)>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + FromDisk,
        V: FromDisk,
    {
        self.zs_reverse_range_iter(cf, ..upper_bound).next()
    }

    fn zs_items_in_range_ordered<C, K, V, R>(&self, cf: &C, range: R) -> BTreeMap<K, V>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + FromDisk + Ord,
        V: FromDisk,
        R: RangeBounds<K>,
    {
        self.zs_forward_range_iter(cf, range).collect()
    }

    fn zs_items_in_range_unordered<C, K, V, R>(&self, cf: &C, range: R) -> HashMap<K, V>
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + FromDisk + Eq + std::hash::Hash,
        V: FromDisk,
        R: RangeBounds<K>,
    {
        self.zs_forward_range_iter(cf, range).collect()
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
    /// Prints rocksdb metrics for each column family along with total database disk size, live data disk size and database memory size.
    pub fn print_db_metrics(&self) {
        let mut total_size_on_disk = 0;
        let mut total_live_size_on_disk = 0;
        let mut total_size_in_mem = 0;
        let db: &Arc<DB> = &self.db;
        let db_options = DiskDb::options();
        let column_families = DiskDb::construct_column_families(db_options, db.path(), []);
        let mut column_families_log_string = String::from("");

        write!(column_families_log_string, "Column families and sizes: ").unwrap();

        for cf_descriptor in column_families {
            let cf_name = &cf_descriptor.name();
            let cf_handle = db
                .cf_handle(cf_name)
                .expect("Column family handle must exist");
            let live_data_size = db
                .property_int_value_cf(cf_handle, "rocksdb.estimate-live-data-size")
                .unwrap_or(Some(0));
            let total_sst_files_size = db
                .property_int_value_cf(cf_handle, "rocksdb.total-sst-files-size")
                .unwrap_or(Some(0));
            let cf_disk_size = total_sst_files_size.unwrap_or(0);
            total_size_on_disk += cf_disk_size;
            total_live_size_on_disk += live_data_size.unwrap_or(0);
            let mem_table_size = db
                .property_int_value_cf(cf_handle, "rocksdb.size-all-mem-tables")
                .unwrap_or(Some(0));
            total_size_in_mem += mem_table_size.unwrap_or(0);

            write!(
                column_families_log_string,
                "{} (Disk: {}, Memory: {})",
                cf_name,
                human_bytes::human_bytes(cf_disk_size as f64),
                human_bytes::human_bytes(mem_table_size.unwrap_or(0) as f64)
            )
            .unwrap();
        }

        debug!("{}", column_families_log_string);
        info!(
            "Total Database Disk Size: {}",
            human_bytes::human_bytes(total_size_on_disk as f64)
        );
        info!(
            "Total Live Data Disk Size: {}",
            human_bytes::human_bytes(total_live_size_on_disk as f64)
        );
        info!(
            "Total Database Memory Size: {}",
            human_bytes::human_bytes(total_size_in_mem as f64)
        );
    }

    /// Exports RocksDB metrics to Prometheus.
    ///
    /// This function collects database statistics and exposes them as Prometheus metrics.
    /// Call this periodically (e.g., every 30 seconds) from a background task.
    pub(crate) fn export_metrics(&self) {
        let db: &Arc<DB> = &self.db;
        let db_options = DiskDb::options();
        let column_families = DiskDb::construct_column_families(db_options, db.path(), []);

        let mut total_disk: u64 = 0;
        let mut total_live: u64 = 0;
        let mut total_mem: u64 = 0;

        for cf_descriptor in column_families {
            let cf_name = cf_descriptor.name().to_string();
            if let Some(cf_handle) = db.cf_handle(&cf_name) {
                let disk = db
                    .property_int_value_cf(cf_handle, "rocksdb.total-sst-files-size")
                    .ok()
                    .flatten()
                    .unwrap_or(0);
                let live = db
                    .property_int_value_cf(cf_handle, "rocksdb.estimate-live-data-size")
                    .ok()
                    .flatten()
                    .unwrap_or(0);
                let mem = db
                    .property_int_value_cf(cf_handle, "rocksdb.size-all-mem-tables")
                    .ok()
                    .flatten()
                    .unwrap_or(0);

                total_disk += disk;
                total_live += live;
                total_mem += mem;

                metrics::gauge!("zebra.state.rocksdb.cf_disk_size_bytes", "cf" => cf_name.clone())
                    .set(disk as f64);
                metrics::gauge!("zebra.state.rocksdb.cf_memory_size_bytes", "cf" => cf_name)
                    .set(mem as f64);
            }
        }

        metrics::gauge!("zebra.state.rocksdb.total_disk_size_bytes").set(total_disk as f64);
        metrics::gauge!("zebra.state.rocksdb.live_data_size_bytes").set(total_live as f64);
        metrics::gauge!("zebra.state.rocksdb.total_memory_size_bytes").set(total_mem as f64);

        // Compaction metrics - these use database-wide properties (not per-column-family)
        if let Ok(Some(pending)) = db.property_int_value("rocksdb.compaction-pending") {
            metrics::gauge!("zebra.state.rocksdb.compaction.pending_bytes").set(pending as f64);
        }

        if let Ok(Some(running)) = db.property_int_value("rocksdb.num-running-compactions") {
            metrics::gauge!("zebra.state.rocksdb.compaction.running").set(running as f64);
        }

        if let Ok(Some(cache)) = db.property_int_value("rocksdb.block-cache-usage") {
            metrics::gauge!("zebra.state.rocksdb.block_cache_usage_bytes").set(cache as f64);
        }

        // Level-by-level file counts (RocksDB typically has up to 7 levels)
        for level in 0..7 {
            let prop = format!("rocksdb.num-files-at-level{level}");
            if let Ok(Some(count)) = db.property_int_value(&prop) {
                metrics::gauge!("zebra.state.rocksdb.num_files_at_level", "level" => level.to_string())
                    .set(count as f64);
            }
        }
    }

    /// Returns the estimated total disk space usage of the database.
    pub fn size(&self) -> u64 {
        let db: &Arc<DB> = &self.db;
        let db_options = DiskDb::options();
        let mut total_size_on_disk = 0;
        for cf_descriptor in DiskDb::construct_column_families(db_options, db.path(), []) {
            let cf_name = &cf_descriptor.name();
            let cf_handle = db
                .cf_handle(cf_name)
                .expect("Column family handle must exist");

            total_size_on_disk += db
                .property_int_value_cf(cf_handle, "rocksdb.total-sst-files-size")
                .ok()
                .flatten()
                .unwrap_or(0);
        }

        total_size_on_disk
    }

    /// Sets `finished_format_upgrades` to true to indicate that Zebra has
    /// finished applying any required db format upgrades.
    pub fn mark_finished_format_upgrades(&self) {
        self.finished_format_upgrades
            .store(true, atomic::Ordering::SeqCst);
    }

    /// Returns true if the `finished_format_upgrades` flag has been set to true to
    /// indicate that Zebra has finished applying any required db format upgrades.
    pub fn finished_format_upgrades(&self) -> bool {
        self.finished_format_upgrades.load(atomic::Ordering::SeqCst)
    }

    /// When called with a secondary DB instance, tries to catch up with the primary DB instance
    pub fn try_catch_up_with_primary(&self) -> Result<(), rocksdb::Error> {
        self.db.try_catch_up_with_primary()
    }

    /// Returns a forward iterator over the items in `cf` in `range`.
    ///
    /// Holding this iterator open might delay block commit transactions.
    pub fn zs_forward_range_iter<C, K, V, R>(
        &self,
        cf: &C,
        range: R,
    ) -> impl Iterator<Item = (K, V)> + '_
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + FromDisk,
        V: FromDisk,
        R: RangeBounds<K>,
    {
        self.zs_range_iter_with_direction(cf, range, false)
    }

    /// Returns a reverse iterator over the items in `cf` in `range`.
    ///
    /// Holding this iterator open might delay block commit transactions.
    pub fn zs_reverse_range_iter<C, K, V, R>(
        &self,
        cf: &C,
        range: R,
    ) -> impl Iterator<Item = (K, V)> + '_
    where
        C: rocksdb::AsColumnFamilyRef,
        K: IntoDisk + FromDisk,
        V: FromDisk,
        R: RangeBounds<K>,
    {
        self.zs_range_iter_with_direction(cf, range, true)
    }

    /// Returns an iterator over the items in `cf` in `range`.
    ///
    /// RocksDB iterators are ordered by increasing key bytes by default.
    /// Otherwise, if `reverse` is `true`, the iterator is ordered by decreasing key bytes.
    ///
    /// Holding this iterator open might delay block commit transactions.
    fn zs_range_iter_with_direction<C, K, V, R>(
        &self,
        cf: &C,
        range: R,
        reverse: bool,
    ) -> impl Iterator<Item = (K, V)> + '_
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
        let range = (start_bound, end_bound);

        let mode = Self::zs_iter_mode(&range, reverse);
        let opts = Self::zs_iter_opts(&range);

        // Reading multiple items from iterators has caused database hangs,
        // in previous RocksDB versions
        self.db
            .iterator_cf_opt(cf, opts, mode)
            .map(|result| result.expect("unexpected database failure"))
            .map(|(key, value)| (key.to_vec(), value))
            // Skip excluded "from" bound and empty ranges. The `mode` already skips keys
            // strictly before the "from" bound.
            .skip_while({
                let range = range.clone();
                move |(key, _value)| !range.contains(key)
            })
            // Take until the excluded "to" bound is reached,
            // or we're after the included "to" bound.
            .take_while(move |(key, _value)| range.contains(key))
            .map(|(key, value)| (K::from_bytes(key), V::from_bytes(value)))
    }

    /// Returns the RocksDB ReadOptions with a lower and upper bound for a range.
    fn zs_iter_opts<R>(range: &R) -> ReadOptions
    where
        R: RangeBounds<Vec<u8>>,
    {
        let mut opts = ReadOptions::default();
        let (lower_bound, upper_bound) = Self::zs_iter_bounds(range);

        if let Some(bound) = lower_bound {
            opts.set_iterate_lower_bound(bound);
        };

        if let Some(bound) = upper_bound {
            opts.set_iterate_upper_bound(bound);
        };

        opts
    }

    /// Returns a lower and upper iterate bounds for a range.
    ///
    /// Note: Since upper iterate bounds are always exclusive in RocksDB, this method
    ///       will increment the upper bound by 1 if the end bound of the provided range
    ///       is inclusive.
    fn zs_iter_bounds<R>(range: &R) -> (Option<Vec<u8>>, Option<Vec<u8>>)
    where
        R: RangeBounds<Vec<u8>>,
    {
        use std::ops::Bound::*;

        let lower_bound = match range.start_bound() {
            Included(bound) | Excluded(bound) => Some(bound.clone()),
            Unbounded => None,
        };

        let upper_bound = match range.end_bound().cloned() {
            Included(mut bound) => {
                // Increment the last byte in the upper bound that is less than u8::MAX, and
                // clear any bytes after it to increment the next key in lexicographic order
                // (next big-endian number). RocksDB uses lexicographic order for keys.
                let is_wrapped_overflow = increment_big_endian(&mut bound);

                if is_wrapped_overflow {
                    bound.insert(0, 0x01)
                }

                Some(bound)
            }
            Excluded(bound) => Some(bound),
            Unbounded => None,
        };

        (lower_bound, upper_bound)
    }

    /// Returns the RocksDB iterator "from" mode for `range`.
    ///
    /// RocksDB iterators are ordered by increasing key bytes by default.
    /// Otherwise, if `reverse` is `true`, the iterator is ordered by decreasing key bytes.
    fn zs_iter_mode<R>(range: &R, reverse: bool) -> rocksdb::IteratorMode<'_>
    where
        R: RangeBounds<Vec<u8>>,
    {
        use std::ops::Bound::*;

        let from_bound = if reverse {
            range.end_bound()
        } else {
            range.start_bound()
        };

        match from_bound {
            Unbounded => {
                if reverse {
                    // Reversed unbounded iterators start from the last item
                    rocksdb::IteratorMode::End
                } else {
                    // Unbounded iterators start from the first item
                    rocksdb::IteratorMode::Start
                }
            }

            Included(bound) | Excluded(bound) => {
                let direction = if reverse {
                    rocksdb::Direction::Reverse
                } else {
                    rocksdb::Direction::Forward
                };

                rocksdb::IteratorMode::From(bound.as_slice(), direction)
            }
        }
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

    /// Build a vector of current column families on the disk and optionally any new column families.
    /// Returns an iterable collection of all column families.
    fn construct_column_families(
        db_options: Options,
        path: &Path,
        column_families_in_code: impl IntoIterator<Item = String>,
    ) -> impl Iterator<Item = ColumnFamilyDescriptor> {
        // When opening the database in read/write mode, all column families must be opened.
        //
        // To make Zebra forward-compatible with databases updated by later versions,
        // we read any existing column families off the disk, then add any new column families
        // from the current implementation.
        //
        // <https://github.com/facebook/rocksdb/wiki/Column-Families#reference>
        let column_families_on_disk = DB::list_cf(&db_options, path).unwrap_or_default();
        let column_families_in_code = column_families_in_code.into_iter();

        column_families_on_disk
            .into_iter()
            .chain(column_families_in_code)
            .unique()
            .map(move |cf_name: String| {
                let mut cf_options = db_options.clone();

                if cf_name == BALANCE_BY_TRANSPARENT_ADDR {
                    cf_options.set_merge_operator_associative(
                        BALANCE_BY_TRANSPARENT_ADDR_MERGE_OP,
                        fetch_add_balance_and_received,
                    );
                }

                rocksdb::ColumnFamilyDescriptor::new(cf_name, cf_options.clone())
            })
    }

    /// Opens or creates the database at a path based on the kind, major version and network,
    /// with the supplied column families, preserving any existing column families,
    /// and returns a shared low-level database wrapper.
    ///
    /// # Panics
    ///
    /// - If the cache directory does not exist and can't be created.
    /// - If the database cannot be opened for whatever reason.
    pub fn new(
        config: &Config,
        db_kind: impl AsRef<str>,
        format_version_in_code: &Version,
        network: &Network,
        column_families_in_code: impl IntoIterator<Item = String>,
        read_only: bool,
    ) -> DiskDb {
        // If the database is ephemeral, we don't need to check the cache directory.
        if !config.ephemeral {
            DiskDb::validate_cache_dir(&config.cache_dir);
        }

        let db_kind = db_kind.as_ref();
        let path = config.db_path(db_kind, format_version_in_code.major, network);

        let db_options = DiskDb::options();

        let column_families =
            DiskDb::construct_column_families(db_options.clone(), &path, column_families_in_code);

        let db_result = if read_only {
            // Use a tempfile for the secondary instance cache directory
            let secondary_config = Config {
                ephemeral: true,
                ..config.clone()
            };
            let secondary_path =
                secondary_config.db_path("secondary_state", format_version_in_code.major, network);
            let create_dir_result = std::fs::create_dir_all(&secondary_path);

            info!(?create_dir_result, "creating secondary db directory");

            DB::open_cf_descriptors_as_secondary(
                &db_options,
                &path,
                &secondary_path,
                column_families,
            )
        } else {
            DB::open_cf_descriptors(&db_options, &path, column_families)
        };

        match db_result {
            Ok(db) => {
                info!("Opened Zebra state cache at {}", path.display());

                let db = DiskDb {
                    db_kind: db_kind.to_string(),
                    format_version_in_code: format_version_in_code.clone(),
                    network: network.clone(),
                    ephemeral: config.ephemeral,
                    db: Arc::new(db),
                    finished_format_upgrades: Arc::new(AtomicBool::new(false)),
                };

                db.assert_default_cf_is_empty();

                db
            }

            Err(e) if matches!(e.kind(), ErrorKind::Busy | ErrorKind::IOError) => panic!(
                "Database likely already open {path:?} \
                         Hint: Check if another zebrad process is running."
            ),

            Err(e) => panic!(
                "Opening database {path:?} failed. \
                        Hint: Try changing the state cache_dir in the Zebra config. \
                        Error: {e}",
            ),
        }
    }

    // Accessor methods

    /// Returns the configured database kind for this database.
    pub fn db_kind(&self) -> String {
        self.db_kind.clone()
    }

    /// Returns the format version of the running code that created this `DiskDb` instance in memory.
    pub fn format_version_in_code(&self) -> Version {
        self.format_version_in_code.clone()
    }

    /// Returns the fixed major version for this database.
    pub fn major_version(&self) -> u64 {
        self.format_version_in_code().major
    }

    /// Returns the configured network for this database.
    pub fn network(&self) -> Network {
        self.network.clone()
    }

    /// Returns the `Path` where the files used by this database are located.
    pub fn path(&self) -> &Path {
        self.db.path()
    }

    /// Returns the low-level rocksdb inner database.
    #[allow(dead_code)]
    fn inner(&self) -> &Arc<DB> {
        &self.db
    }

    /// Returns the column family handle for `cf_name`.
    pub fn cf_handle(&self, cf_name: &str) -> Option<rocksdb::ColumnFamilyRef<'_>> {
        // Note: the lifetime returned by this method is subtly wrong. As of December 2023 it is
        // the shorter of &self and &str, but RocksDB clones column family names internally, so it
        // should just be &self. To avoid this restriction, clone the string before passing it to
        // this method. Currently Zebra uses static strings, so this doesn't matter.
        self.db.cf_handle(cf_name)
    }

    // Read methods are located in the ReadDisk trait

    // Write methods
    // Low-level write methods are located in the WriteDisk trait

    /// Writes `batch` to the database.
    pub(crate) fn write(&self, batch: DiskWriteBatch) -> Result<(), rocksdb::Error> {
        self.db.write(batch.batch)
    }

    // Private methods

    /// Tries to reuse an existing db after a major upgrade.
    ///
    /// If the current db version belongs to `restorable_db_versions`, the function moves a previous
    /// db to a new path so it can be used again. It does so by merely trying to rename the path
    /// corresponding to the db version directly preceding the current version to the path that is
    /// used by the current db. If successful, it also deletes the db version file.
    ///
    /// Returns the old disk version if one existed and the db directory was renamed, or None otherwise.
    // TODO: Update this function to rename older major db format version to the current version (#9565).
    #[allow(clippy::unwrap_in_result)]
    pub(crate) fn try_reusing_previous_db_after_major_upgrade(
        restorable_db_versions: &[u64],
        format_version_in_code: &Version,
        config: &Config,
        db_kind: impl AsRef<str>,
        network: &Network,
    ) -> Option<Version> {
        if let Some(&major_db_ver) = restorable_db_versions
            .iter()
            .find(|v| **v == format_version_in_code.major)
        {
            let db_kind = db_kind.as_ref();

            let old_major_db_ver = major_db_ver - 1;
            let old_path = config.db_path(db_kind, old_major_db_ver, network);
            // Exit early if the path doesn't exist or there's an error checking it.
            if !fs::exists(&old_path).unwrap_or(false) {
                return None;
            }

            let new_path = config.db_path(db_kind, major_db_ver, network);

            let old_path = match fs::canonicalize(&old_path) {
                Ok(canonicalized_old_path) => canonicalized_old_path,
                Err(e) => {
                    warn!("could not canonicalize {old_path:?}: {e}");
                    return None;
                }
            };

            let cache_path = match fs::canonicalize(&config.cache_dir) {
                Ok(canonicalized_cache_path) => canonicalized_cache_path,
                Err(e) => {
                    warn!("could not canonicalize {:?}: {e}", config.cache_dir);
                    return None;
                }
            };

            // # Correctness
            //
            // Check that the path we're about to move is inside the cache directory.
            //
            // If the user has symlinked the state directory to a non-cache directory, we don't want
            // to move it, because it might contain other files.
            //
            // We don't attempt to guard against malicious symlinks created by attackers
            // (TOCTOU attacks). Zebra should not be run with elevated privileges.
            if !old_path.starts_with(&cache_path) {
                info!("skipped reusing previous state cache: state is outside cache directory");
                return None;
            }

            let opts = DiskDb::options();
            let old_db_exists = DB::list_cf(&opts, &old_path).is_ok_and(|cf| !cf.is_empty());
            let new_db_exists = DB::list_cf(&opts, &new_path).is_ok_and(|cf| !cf.is_empty());

            if old_db_exists && !new_db_exists {
                // Create the parent directory for the new db. This is because we can't directly
                // rename e.g. `state/v25/mainnet/` to `state/v26/mainnet/` with `fs::rename()` if
                // `state/v26/` does not exist.
                match fs::create_dir_all(
                    new_path
                        .parent()
                        .expect("new state cache must have a parent path"),
                ) {
                    Ok(()) => info!("created new directory for state cache at {new_path:?}"),
                    Err(e) => {
                        warn!(
                            "could not create new directory for state cache at {new_path:?}: {e}"
                        );
                        return None;
                    }
                };

                match fs::rename(&old_path, &new_path) {
                    Ok(()) => {
                        info!("moved state cache from {old_path:?} to {new_path:?}");

                        let mut disk_version =
                            database_format_version_on_disk(config, db_kind, major_db_ver, network)
                                .expect("unable to read database format version file")
                                .expect("unable to parse database format version");

                        disk_version.major = old_major_db_ver;

                        write_database_format_version_to_disk(
                            config,
                            db_kind,
                            major_db_ver,
                            &disk_version,
                            network,
                        )
                        .expect("unable to write database format version file to disk");

                        // Get the parent of the old path, e.g. `state/v25/` and delete it if it is
                        // empty.
                        let old_path = old_path
                            .parent()
                            .expect("old state cache must have parent path");

                        if fs::read_dir(old_path)
                            .expect("cached state dir needs to be readable")
                            .next()
                            .is_none()
                        {
                            match fs::remove_dir_all(old_path) {
                                Ok(()) => {
                                    info!("removed empty old state cache directory at {old_path:?}")
                                }
                                Err(e) => {
                                    warn!(
                                        "could not remove empty old state cache directory \
                                           at {old_path:?}: {e}"
                                    )
                                }
                            }
                        }

                        return Some(disk_version);
                    }
                    Err(e) => {
                        warn!("could not move state cache from {old_path:?} to {new_path:?}: {e}");
                    }
                };
            }
        };

        None
    }

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
            debug!(
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

    // Validates a cache directory and creates it if it doesn't exist.
    // If the directory cannot be created, it panics with a specific error message.
    fn validate_cache_dir(cache_dir: &std::path::PathBuf) {
        if let Err(e) = fs::create_dir_all(cache_dir) {
            match e.kind() {
                std::io::ErrorKind::PermissionDenied => panic!(
                    "Permission denied creating {cache_dir:?}. \
                     Hint: check if cache directory exist and has write permissions."
                ),
                std::io::ErrorKind::StorageFull => panic!(
                    "No space left on device creating {cache_dir:?}. \
                     Hint: check if the disk is full."
                ),
                _ => panic!("Could not create cache dir {cache_dir:?}: {e}"),
            }
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
