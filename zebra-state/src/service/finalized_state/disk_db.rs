//! Module defining access to RocksDB via accessor traits.
//!
//! This module makes sure that:
//! - all disk writes happen inside a RocksDB transaction, and
//! - format-specific invariants are maintained.

use std::fmt::Debug;

use crate::service::finalized_state::disk_format::{FromDisk, IntoDisk};

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

impl ReadDisk for rocksdb::DB {
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
        self.get_pinned_cf(cf, key_bytes)
            .expect("expected that disk errors would not occur")
            .is_some()
    }
}
