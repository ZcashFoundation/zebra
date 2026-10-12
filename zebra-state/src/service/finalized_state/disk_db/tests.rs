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
