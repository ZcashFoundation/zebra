//! Type-safe column family access.

// When these types aren't exported, they become dead code.
#![cfg_attr(not(any(test, feature = "proptest-impl")), allow(dead_code))]

use std::{
    any::type_name,
    collections::{BTreeMap, HashMap},
    fmt::Debug,
    hash::Hash,
    marker::PhantomData,
    ops::RangeBounds,
};

use crate::service::finalized_state::{DiskWriteBatch, FromDisk, IntoDisk, ReadDisk, WriteDisk};

use super::DiskDb;

/// A type-safe read-only column family reference.
///
/// Use this struct instead of raw [`ReadDisk`] access, because it is type-safe.
/// So you only have to define the types once, and you can't accidentally use different types for
/// reading and writing. (Which is a source of subtle database bugs.)
#[derive(Clone)]
pub struct TypedColumnFamily<'cf, Key, Value>
where
    Key: IntoDisk + FromDisk + Debug,
    Value: IntoDisk + FromDisk,
{
    /// The database.
    db: DiskDb,

    /// The column family reference in the database.
    cf: rocksdb::ColumnFamilyRef<'cf>,

    /// The column family name, only used for debugging and equality checking.
    _cf_name: String,

    /// A marker type used to bind the key and value types to the struct.
    _marker: PhantomData<(Key, Value)>,
}

/// A type-safe and drop-safe batch write to a column family.
///
/// Use this struct instead of raw [`WriteDisk`] access, because it is type-safe.
/// So you only have to define the types once, and you can't accidentally use different types for
/// reading and writing. (Which is a source of subtle database bugs.)
///
/// This type is also drop-safe: unwritten batches have to be specifically ignored.
#[must_use = "batches must be written to the database"]
#[derive(Debug, Eq, PartialEq)]
pub struct WriteTypedBatch<'cf, Key, Value, Batch>
where
    Key: IntoDisk + FromDisk + Debug,
    Value: IntoDisk + FromDisk,
    Batch: WriteDisk,
{
    inner: TypedColumnFamily<'cf, Key, Value>,

    batch: Batch,
}

impl<Key, Value> Debug for TypedColumnFamily<'_, Key, Value>
where
    Key: IntoDisk + FromDisk + Debug,
    Value: IntoDisk + FromDisk,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct(&format!(
            "TypedColumnFamily<{}, {}>",
            type_name::<Key>(),
            type_name::<Value>()
        ))
        .field("db", &self.db)
        .field("cf", &self._cf_name)
        .finish()
    }
}

impl<Key, Value> PartialEq for TypedColumnFamily<'_, Key, Value>
where
    Key: IntoDisk + FromDisk + Debug,
    Value: IntoDisk + FromDisk,
{
    fn eq(&self, other: &Self) -> bool {
        self.db == other.db && self._cf_name == other._cf_name
    }
}

impl<Key, Value> Eq for TypedColumnFamily<'_, Key, Value>
where
    Key: IntoDisk + FromDisk + Debug,
    Value: IntoDisk + FromDisk,
{
}

impl<'cf, Key, Value> TypedColumnFamily<'cf, Key, Value>
where
    Key: IntoDisk + FromDisk + Debug,
    Value: IntoDisk + FromDisk,
{
    // Creation

    /// Returns a new typed column family, if it exists in the database.
    pub fn new(db: &'cf DiskDb, cf_name: &str) -> Option<Self> {
        let cf = db.cf_handle(cf_name)?;

        Some(Self {
            db: db.clone(),
            cf,
            _cf_name: cf_name.to_string(),
            _marker: PhantomData,
        })
    }

    // Writing

    /// Returns a typed writer for this column family for a new batch.
    ///
    /// These methods are the only way to get a `WriteTypedBatch`, which ensures
    /// that the read and write types are consistent.
    pub fn new_batch_for_writing(self) -> WriteTypedBatch<'cf, Key, Value, DiskWriteBatch> {
        WriteTypedBatch {
            inner: self,
            batch: DiskWriteBatch::new(),
        }
    }

    /// Wraps an existing write batch, and returns a typed writer for this column family.
    ///
    /// These methods are the only way to get a `WriteTypedBatch`, which ensures
    /// that the read and write types are consistent.
    pub fn take_batch_for_writing(
        self,
        batch: DiskWriteBatch,
    ) -> WriteTypedBatch<'cf, Key, Value, DiskWriteBatch> {
        WriteTypedBatch { inner: self, batch }
    }

    /// Wraps an existing write batch reference, and returns a typed writer for this column family.
    ///
    /// These methods are the only way to get a `WriteTypedBatch`, which ensures
    /// that the read and write types are consistent.
    pub fn with_batch_for_writing(
        self,
        batch: &mut DiskWriteBatch,
    ) -> WriteTypedBatch<'cf, Key, Value, &mut DiskWriteBatch> {
        WriteTypedBatch { inner: self, batch }
    }

    // Reading

    /// Returns true if this rocksdb column family does not contain any entries.
    pub fn zs_is_empty(&self) -> bool {
        self.db.zs_is_empty(&self.cf)
    }

    /// Returns the value for `key` in this rocksdb column family, if present.
    pub fn zs_get(&self, key: &Key) -> Option<Value> {
        self.db.zs_get(&self.cf, key)
    }

    /// Check if this rocksdb column family contains the serialized form of `key`.
    pub fn zs_contains(&self, key: &Key) -> bool {
        self.db.zs_contains(&self.cf, key)
    }

    /// Returns the lowest key in this column family, and the corresponding value.
    ///
    /// Returns `None` if this column family is empty.
    pub fn zs_first_key_value(&self) -> Option<(Key, Value)> {
        self.db.zs_first_key_value(&self.cf)
    }

    /// Returns the highest key in this column family, and the corresponding value.
    ///
    /// Returns `None` if this column family is empty.
    pub fn zs_last_key_value(&self) -> Option<(Key, Value)> {
        self.db.zs_last_key_value(&self.cf)
    }

    /// Returns the first key greater than or equal to `lower_bound` in this column family,
    /// and the corresponding value.
    ///
    /// Returns `None` if there are no keys greater than or equal to `lower_bound`.
    pub fn zs_next_key_value_from(&self, lower_bound: &Key) -> Option<(Key, Value)> {
        self.db.zs_next_key_value_from(&self.cf, lower_bound)
    }

    /// Returns the first key strictly greater than `lower_bound` in this column family,
    /// and the corresponding value.
    ///
    /// Returns `None` if there are no keys greater than `lower_bound`.
    pub fn zs_next_key_value_strictly_after(&self, lower_bound: &Key) -> Option<(Key, Value)> {
        self.db
            .zs_next_key_value_strictly_after(&self.cf, lower_bound)
    }

    /// Returns the first key less than or equal to `upper_bound` in this column family,
    /// and the corresponding value.
    ///
    /// Returns `None` if there are no keys less than or equal to `upper_bound`.
    pub fn zs_prev_key_value_back_from(&self, upper_bound: &Key) -> Option<(Key, Value)> {
        self.db.zs_prev_key_value_back_from(&self.cf, upper_bound)
    }

    /// Returns the first key strictly less than `upper_bound` in this column family,
    /// and the corresponding value.
    ///
    /// Returns `None` if there are no keys less than `upper_bound`.
    pub fn zs_prev_key_value_strictly_before(&self, upper_bound: &Key) -> Option<(Key, Value)> {
        self.db
            .zs_prev_key_value_strictly_before(&self.cf, upper_bound)
    }

    /// Returns a forward iterator over the items in this column family in `range`.
    ///
    /// Holding this iterator open might delay block commit transactions.
    pub fn zs_forward_range_iter<Range>(
        &self,
        range: Range,
    ) -> impl Iterator<Item = (Key, Value)> + '_
    where
        Range: RangeBounds<Key>,
    {
        self.db.zs_forward_range_iter(&self.cf, range)
    }

    /// Returns a reverse iterator over the items in this column family in `range`.
    ///
    /// Holding this iterator open might delay block commit transactions.
    pub fn zs_reverse_range_iter<Range>(
        &self,
        range: Range,
    ) -> impl Iterator<Item = (Key, Value)> + '_
    where
        Range: RangeBounds<Key>,
    {
        self.db.zs_reverse_range_iter(&self.cf, range)
    }
}

impl<Key, Value> TypedColumnFamily<'_, Key, Value>
where
    Key: IntoDisk + FromDisk + Debug + Ord,
    Value: IntoDisk + FromDisk,
{
    /// Returns the keys and values in this column family in `range`, in an ordered `BTreeMap`.
    ///
    /// Holding this iterator open might delay block commit transactions.
    pub fn zs_items_in_range_ordered<Range>(&self, range: Range) -> BTreeMap<Key, Value>
    where
        Range: RangeBounds<Key>,
    {
        self.db.zs_items_in_range_ordered(&self.cf, range)
    }
}

impl<Key, Value> TypedColumnFamily<'_, Key, Value>
where
    Key: IntoDisk + FromDisk + Debug + Hash + Eq,
    Value: IntoDisk + FromDisk,
{
    /// Returns the keys and values in this column family in `range`, in an unordered `HashMap`.
    ///
    /// Holding this iterator open might delay block commit transactions.
    pub fn zs_items_in_range_unordered<Range>(&self, range: Range) -> HashMap<Key, Value>
    where
        Range: RangeBounds<Key>,
    {
        self.db.zs_items_in_range_unordered(&self.cf, range)
    }
}

impl<Key, Value, Batch> WriteTypedBatch<'_, Key, Value, Batch>
where
    Key: IntoDisk + FromDisk + Debug,
    Value: IntoDisk + FromDisk,
    Batch: WriteDisk,
{
    // Batching before writing

    /// Serialize and insert the given key and value into this column family,
    /// overwriting any existing `value` for `key`.
    pub fn zs_insert(mut self, key: &Key, value: &Value) -> Self {
        self.batch.zs_insert(&self.inner.cf, key, value);

        self
    }

    /// Remove the given key from this column family, if it exists.
    pub fn zs_delete(mut self, key: &Key) -> Self {
        self.batch.zs_delete(&self.inner.cf, key);

        self
    }

    /// Delete the given key range from this rocksdb column family, if it exists, including `from`
    /// and excluding `until_strictly_before`.
    //.
    // TODO: convert zs_delete_range() to take std::ops::RangeBounds
    //       see zs_range_iter() for an example of the edge cases
    pub fn zs_delete_range(mut self, from: &Key, until_strictly_before: &Key) -> Self {
        self.batch
            .zs_delete_range(&self.inner.cf, from, until_strictly_before);

        self
    }
}

// Writing a batch to the database requires an owned batch.
impl<Key, Value> WriteTypedBatch<'_, Key, Value, DiskWriteBatch>
where
    Key: IntoDisk + FromDisk + Debug,
    Value: IntoDisk + FromDisk,
{
    // Writing batches

    /// Writes this batch to this column family in the database,
    /// taking ownership and consuming it.
    pub fn write_batch(self) -> Result<(), rocksdb::Error> {
        self.inner.db.write(self.batch)
    }
}
