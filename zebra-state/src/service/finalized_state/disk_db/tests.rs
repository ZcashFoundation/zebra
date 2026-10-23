//! Tests and test methods for low-level RocksDB access.

#![allow(clippy::unwrap_in_result)]
#![allow(dead_code)]

use std::ops::Deref;

use zebra_chain::parameters::Network;

use crate::{
    constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
    service::finalized_state::disk_db::{DiskDb, DB},
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

/// Check that lock conflicts are told apart from other I/O errors when opening a database.
#[test]
fn open_errors_distinguish_lock_conflicts_from_io_errors() {
    let _init_guard = zebra_test::init();

    let dir = tempfile::tempdir().expect("temporary directory is writable");
    let mut opts = rocksdb::Options::default();
    opts.create_if_missing(true);

    let db_path = dir.path().join("db");
    let _db = DB::open(&opts, &db_path).expect("new database opens in an empty directory");
    let lock_error = DB::open(&opts, &db_path)
        .expect_err("second open fails because the first instance holds the lock");
    assert!(
        DiskDb::is_lock_conflict(&lock_error),
        "lock conflict was not detected: {lock_error}"
    );

    let blocked_path = dir.path().join("blocked");
    std::fs::create_dir_all(blocked_path.join("LOCK")).expect("temporary directory is writable");
    let io_error = DB::open(&opts, &blocked_path)
        .expect_err("open fails because the lock file path is a directory");
    assert_eq!(io_error.kind(), rocksdb::ErrorKind::IOError, "{io_error}");
    assert!(
        !DiskDb::is_lock_conflict(&io_error),
        "I/O error was reported as a lock conflict: {io_error}"
    );
}

/// Check that a read-only secondary removes its temporary directory once its last clone is
/// dropped, and leaves the primary database in place.
#[test]
fn read_only_secondary_removes_its_directory_on_drop() {
    let _init_guard = zebra_test::init();

    let cache_dir = tempfile::tempdir().expect("temporary directory is writable");
    let config = Config {
        cache_dir: cache_dir.path().to_path_buf(),
        ephemeral: false,
        ..Config::default()
    };
    let network = Network::Mainnet;
    let version = state_database_format_version_in_code();
    let open = |read_only| {
        DiskDb::new(
            &config,
            STATE_DATABASE_KIND,
            &version,
            &network,
            [],
            read_only,
        )
        .expect("database opens in a fresh cache directory")
    };

    let primary = open(false);
    let secondary = open(true);
    let secondary_path = secondary
        .secondary_dir
        .as_ref()
        .expect("read-only instances have a secondary directory")
        .path()
        .to_path_buf();
    assert!(secondary_path.exists());

    drop(secondary.clone());
    assert!(
        secondary_path.exists(),
        "dropping a clone must keep the secondary directory"
    );

    drop(secondary);
    assert!(
        !secondary_path.exists(),
        "secondary directory {secondary_path:?} was not removed"
    );
    assert!(
        primary.path().exists(),
        "dropping a secondary must keep the primary database"
    );
}
