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

/// An RPC clone observes completed measurements without scanning column-family properties.
#[test]
fn disk_size_measurement_is_shared_between_clones() {
    let _init_guard = zebra_test::init();
    let db = DiskDb::new(
        &crate::Config::ephemeral(),
        "state",
        &crate::constants::state_database_format_version_in_code(),
        &zebra_chain::parameters::Network::Mainnet,
        ["size-test".to_owned()],
        false,
    )
    .expect("isolated ephemeral database opens");
    let reader = db.clone();
    let cf = db
        .cf_handle("size-test")
        .expect("test column family exists");
    db.put_cf(cf, b"key", [17; 4096])
        .expect("test write succeeds");
    db.flush_cf(cf).expect("test SST flush completes");
    let measured = db.measure_size();
    assert!(measured > 0, "flushed SST occupies disk space");
    assert_eq!(
        reader.size(),
        0,
        "unrefreshed estimate remains the startup measurement"
    );
    db.export_metrics();
    assert_eq!(
        reader.size(),
        measured,
        "completed refresh reaches existing readers"
    );
    assert_eq!(db.size(), measured);
}

/// Secondary readers observe disk growth when catching up, without a primary metrics task.
#[test]
fn secondary_disk_size_refreshes_after_catch_up() {
    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().expect("isolated cache directory opens");
    let config = crate::Config {
        cache_dir: cache.path().to_path_buf(),
        ephemeral: false,
        ..crate::Config::default()
    };
    let open = |read_only| {
        DiskDb::new(
            &config,
            "state",
            &crate::constants::state_database_format_version_in_code(),
            &zebra_chain::parameters::Network::Mainnet,
            ["secondary-size-test".to_owned()],
            read_only,
        )
        .expect("isolated primary or secondary database opens")
    };
    let primary = open(false);
    let secondary = open(true);
    let reader = secondary.clone();
    let cf = primary
        .cf_handle("secondary-size-test")
        .expect("test column family exists");
    primary
        .put_cf(cf, b"key", [17; 4096])
        .expect("primary write succeeds");
    primary.flush_cf(cf).expect("primary SST flush completes");
    secondary
        .try_catch_up_with_primary()
        .expect("secondary catches up to the flushed primary");
    let measured = secondary.measure_size();
    assert!(measured > 0, "secondary observes the flushed SST");
    assert_eq!(
        reader.size(),
        measured,
        "catch-up refreshes the estimate shared by secondary readers"
    );
}
