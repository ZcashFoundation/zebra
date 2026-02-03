//! Tests and test methods for low-level RocksDB access.

#![allow(clippy::unwrap_in_result)]
#![allow(dead_code)]
#![allow(unused_imports)]

use std::ops::Deref;

use tempfile::TempDir;
use zebra_chain::parameters::Network::Mainnet;

use crate::{
    constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
    service::finalized_state::{
        disk_db::{DiskDb, DB},
        STATE_COLUMN_FAMILIES_IN_CODE,
    },
    Config,
};

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

/// Check that zs_iter_opts returns an upper bound one greater than provided inclusive end bounds.
#[test]
fn zs_iter_opts_increments_key_by_one() {
    let _init_guard = zebra_test::init();

    // TODO: add an empty key (`()` type or `[]` when serialized) test case
    let keys: [u32; 14] = [
        0,
        1,
        200,
        255,
        256,
        257,
        65535,
        65536,
        65537,
        16777215,
        16777216,
        16777217,
        16777218,
        u32::MAX,
    ];

    for key in keys {
        let (_, bytes) = DiskDb::zs_iter_bounds(&..=key.to_be_bytes().to_vec());
        let mut extra_bytes = bytes.expect("there should be an upper bound");
        let bytes = extra_bytes.split_off(extra_bytes.len() - 4);
        let upper_bound = u32::from_be_bytes(bytes.clone().try_into().expect("should be 4 bytes"));
        let expected_upper_bound = key.wrapping_add(1);

        assert_eq!(
            expected_upper_bound, upper_bound,
            "the upper bound should be 1 greater than the original key"
        );

        if expected_upper_bound == 0 {
            assert_eq!(
                extra_bytes,
                vec![1],
                "there should be an extra byte with a value of 1"
            );
        } else {
            assert_eq!(extra_bytes.len(), 0, "there should be no extra bytes");
        }
    }
}

/// Test that a database can be reopened immediately after closing.
///
/// This validates that `wait_for_compact()` in the shutdown sequence properly
/// waits for background jobs to complete, ensuring the lock is released.
///
/// Before the fix, this test would fail with "Database likely already open"
/// because the lock wasn't released immediately when the DB was dropped.
#[test]
fn database_can_be_reopened_immediately_after_close() {
    let _init_guard = zebra_test::init();

    // Create a persistent (non-ephemeral) temp directory that survives across DB instances
    let temp_dir = TempDir::new().expect("failed to create temp dir");

    let config = Config {
        cache_dir: temp_dir.path().to_path_buf(),
        ephemeral: false,
        ..Config::default()
    };

    let network = Mainnet;
    let db_kind = STATE_DATABASE_KIND;
    let format_version = state_database_format_version_in_code();

    let column_families = STATE_COLUMN_FAMILIES_IN_CODE
        .iter()
        .map(ToString::to_string);

    // First open: create the database
    {
        let db = DiskDb::new(
            &config,
            db_kind,
            &format_version,
            &network,
            column_families.clone(),
            false,
        );
        assert!(
            db.path().exists(),
            "database directory should exist after creation"
        );
        // DB is dropped here, triggering shutdown with wait_for_compact
    }

    // Second open: should succeed immediately without "Database likely already open" error
    {
        let db = DiskDb::new(
            &config,
            db_kind,
            &format_version,
            &network,
            column_families.clone(),
            false,
        );
        assert!(
            db.path().exists(),
            "database should be accessible after immediate reopen"
        );
    }
}
