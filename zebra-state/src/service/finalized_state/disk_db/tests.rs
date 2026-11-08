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

/// Both lock conflicts and other I/O failures must preserve RocksDB's actual error.
#[test]
fn database_open_errors_preserve_the_cause() {
    use semver::Version;
    use zebra_chain::parameters::Network;

    use crate::Config;

    let _init_guard = zebra_test::init();
    for locked in [false, true] {
        let cache = tempfile::tempdir().expect("the temporary cache directory can be created");
        let config = Config {
            cache_dir: cache.path().to_path_buf(),
            ..Config::default()
        };
        let version = Version::new(1, 0, 0);
        let path = config.db_path("open-error-test", version.major, &Network::Mainnet);
        let open = || {
            DiskDb::new(
                &config,
                "open-error-test",
                &version,
                &Network::Mainnet,
                [],
                false,
            )
        };
        let _held_db = if locked {
            Some(open().expect("the first database open should succeed"))
        } else {
            std::fs::create_dir_all(path.join("LOCK"))
                .expect("a directory can occupy the database's lock file path");
            None
        };

        let panic = std::panic::catch_unwind(open).expect_err("the database cannot be opened");
        let message = panic
            .downcast_ref::<String>()
            .expect("the database open panic contains a formatted message");
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(message.contains("Error: IO error:"), "{message}");
        assert!(
            !message.contains("Database likely already open"),
            "{message}"
        );
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
