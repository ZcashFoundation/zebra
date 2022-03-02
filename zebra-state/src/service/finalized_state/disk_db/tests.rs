//! Tests and test methods for low-level RocksDB access.

#![allow(dead_code)]

use crate::service::finalized_state::DiskDb;

impl DiskDb {
    /// Returns a list of column family names in this database.
    pub fn list_cf(&self) -> Result<Vec<String>, rocksdb::Error> {
        let opts = DiskDb::options();
        let path = self.path();

        rocksdb::DB::list_cf(&opts, path)
    }
}
