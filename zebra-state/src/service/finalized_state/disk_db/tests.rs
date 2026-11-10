//! Tests and test methods for low-level RocksDB access.

#![allow(clippy::unwrap_in_result)]
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

#[test]
fn cached_disk_size_is_initialized_shared_and_refreshed() {
    use semver::Version;
    use zebra_chain::parameters::Network;

    use crate::Config;

    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().expect("the temporary cache directory can be created");
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let open = || {
        DiskDb::new(
            &config,
            "cached-size-test",
            &Version::new(1, 0, 0),
            &Network::Mainnet,
            ["cached_size".to_owned()],
            false,
        )
        .expect("the persistent database configuration is valid")
    };
    let db = open();
    let clone = db.clone();
    let initial_size = db.size();
    assert_eq!(db.cached_size(), initial_size);

    let cf = db
        .cf_handle("cached_size")
        .expect("the test column family was configured");
    db.put_cf(cf, b"key", [0xa5; 4096])
        .expect("the test column family is writable");
    db.flush_cf(cf)
        .expect("the test column family can be flushed");

    let expected_size = db.size();
    assert!(
        expected_size > initial_size,
        "the live measurement must include the new SST file"
    );
    assert_eq!(
        db.cached_size(),
        initial_size,
        "a live size measurement must not refresh the cache"
    );
    assert_eq!(clone.cached_size(), initial_size);

    clone.export_metrics();
    assert_eq!(clone.cached_size(), expected_size);
    assert_eq!(
        db.cached_size(),
        expected_size,
        "metrics exported by a clone must refresh the shared cache"
    );

    drop(clone);
    drop(db);
    let db = open();
    assert!(db.cached_size() > 0);
    assert_eq!(
        db.cached_size(),
        db.size(),
        "opening a populated database must initialize its cached size"
    );
}

#[test]
fn secondary_disk_usage_tracks_primary_growth() {
    use semver::Version;
    use zebra_chain::parameters::Network;

    use crate::Config;

    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().expect("the temporary cache directory can be created");
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let open = |read_only| {
        DiskDb::new(
            &config,
            "secondary-size-test",
            &Version::new(1, 0, 0),
            &Network::Mainnet,
            ["usage_size".to_owned()],
            read_only,
        )
        .expect("the persistent database configuration is valid")
    };
    let primary = open(false);
    let secondary = open(true);
    let initial_size = secondary.cached_size();

    let cf = primary
        .cf_handle("usage_size")
        .expect("the test column family was configured");
    primary
        .put_cf(cf, b"key", [0xa5; 4096])
        .expect("the primary test column family is writable");
    primary
        .flush_cf(cf)
        .expect("the primary test column family can be flushed");
    secondary
        .try_catch_up_with_primary()
        .expect("the secondary can read the primary's flushed data");

    let live_size = secondary.size();
    assert!(
        live_size > initial_size,
        "catch-up must include the new SST file"
    );
    assert_eq!(
        secondary.cached_size(),
        live_size,
        "secondary usage queries must not retain the startup estimate"
    );
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

/// Check that held locks are told apart from other I/O errors, using the messages
/// RocksDB writes on each platform.
#[test]
fn lock_holder_matches_rocksdb_lock_messages() {
    use super::LockHolder;

    let _init_guard = zebra_test::init();

    let cases = [
        (
            "IO error: While lock file: /state/LOCK: Resource temporarily unavailable",
            Some(LockHolder::OtherProcess),
        ),
        (
            "IO error: While lock file: /state/LOCK: Permission denied",
            Some(LockHolder::OtherProcess),
        ),
        (
            "IO error: Failed to create lock file: C:\\state\\LOCK: The process cannot access \
             the file because it is being used by another process.\r\n",
            Some(LockHolder::OtherProcess),
        ),
        (
            "IO error: lock hold by current process, acquire time 1761209419 acquiring thread \
             139890000000000: /state/LOCK: No locks available",
            Some(LockHolder::ThisProcess),
        ),
        (
            "IO error: While lock file: /state/LOCK: No locks available",
            None,
        ),
        (
            "IO error: Failed to create lock file: C:\\state\\LOCK: Access is denied.\r\n",
            None,
        ),
        (
            "IO error: while open a file for lock: /state/LOCK: Is a directory",
            None,
        ),
        (
            "IO error: No space left on device: While appending to file: /state/000001.log",
            None,
        ),
    ];

    for (message, expected) in cases {
        assert_eq!(DiskDb::lock_holder(message), expected, "{message}");
    }
}

/// Check that real RocksDB open errors are classified by who holds the lock.
#[test]
fn open_errors_distinguish_lock_holders_from_io_errors() {
    use super::LockHolder;

    let _init_guard = zebra_test::init();

    let dir = tempfile::tempdir().expect("temporary directory is writable");
    let mut opts = rocksdb::Options::default();
    opts.create_if_missing(true);

    let db_path = dir.path().join("db");
    let _db = DB::open(&opts, &db_path).expect("new database opens in an empty directory");
    let lock_error = DB::open(&opts, &db_path)
        .expect_err("second open fails because the first instance holds the lock");
    let holder = DiskDb::lock_holder(lock_error.as_ref());

    if cfg!(windows) {
        // RocksDB on Windows has no per-process lock table, so a second open in this
        // process fails like an open in another process. The system message is
        // localized, so only English installations are classified as a held lock.
        assert!(
            lock_error.as_ref().contains("Failed to create lock file"),
            "{lock_error}"
        );
        assert!(
            matches!(holder, Some(LockHolder::OtherProcess) | None),
            "{lock_error}"
        );
    } else {
        assert_eq!(holder, Some(LockHolder::ThisProcess), "{lock_error}");
    }

    let blocked_path = dir.path().join("blocked");
    std::fs::create_dir_all(blocked_path.join("LOCK")).expect("temporary directory is writable");
    let io_error = DB::open(&opts, &blocked_path)
        .expect_err("open fails because the lock file path is a directory");
    assert_eq!(io_error.kind(), rocksdb::ErrorKind::IOError, "{io_error}");
    assert_eq!(DiskDb::lock_holder(io_error.as_ref()), None, "{io_error}");
}

/// Check that a read-only secondary removes its temporary directory once its last clone is
/// dropped, and leaves the primary database in place.
#[test]
fn read_only_secondary_removes_its_directory_on_drop() {
    use zebra_chain::parameters::Network;

    use crate::{
        constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
        service::finalized_state::STATE_COLUMN_FAMILIES_IN_CODE,
        Config,
    };

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
            STATE_COLUMN_FAMILIES_IN_CODE
                .iter()
                .map(ToString::to_string),
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
