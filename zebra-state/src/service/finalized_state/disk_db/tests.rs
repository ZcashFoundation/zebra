//! Tests and test methods for low-level RocksDB access.

#![allow(dead_code)]

use std::ops::Deref;

use crate::service::finalized_state::disk_db::{DiskDb, DB};

// Enable older test code to automatically access the inner database via Deref coercion.
impl Deref for DiskDb {
    type Target = DB;

    fn deref(&self) -> &Self::Target {
        &self.db
    }
}

impl DiskDb {
    /// Returns a list of column family names in this database.
    pub fn list_cf(&self) -> Result<Vec<String>, rocksdb::Error> {
        let opts = DiskDb::options();
        let path = self.path();

        rocksdb::DB::list_cf(&opts, path)
    }
}

/// Check that the sprout tree database serialization format has not changed.
#[test]
fn zs_iter_opts_increments_key_by_one() {
    let _init_guard = zebra_test::init();

    let keys: [u32; 13] = [
        0, 1, 200, 255, 256, 257, 65535, 65536, 65537, 16777215, 16777216, 16777217, 16777218,
    ];

    for key in keys {
        let (_, upper_bound_bytes) = DiskDb::zs_iter_bounds(&..=key.to_be_bytes().to_vec());
        let upper_bound_bytes = upper_bound_bytes.expect("there should be an upper bound");
        let upper_bound = u32::from_be_bytes(upper_bound_bytes.try_into().unwrap());
        let expected_upper_bound = key + 1;

        assert_eq!(
            expected_upper_bound, upper_bound,
            "the upper bound should be 1 greater than the original key"
        );
    }
}
