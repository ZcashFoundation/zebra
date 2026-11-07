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
fn read_only_reuse_rejects_an_interrupted_older_major_upgrade() {
    read_only_reuse_rejects_recorded_version(semver::Version::new(27, 0, 0), 28, false);
}

#[test]
fn read_only_reuse_rejects_a_compatible_major_mismatch() {
    read_only_reuse_rejects_recorded_version(semver::Version::new(29, 0, 0), 28, false);
}

#[test]
fn read_only_current_directory_rejects_an_interrupted_upgrade() {
    read_only_reuse_rejects_recorded_version(semver::Version::new(27, 0, 0), 29, false);
}

#[test]
fn read_only_current_directory_rejects_an_unfinished_directory_transition() {
    read_only_reuse_rejects_recorded_version(semver::Version::new(28, 0, 0), 29, false);
}

#[test]
fn writable_reuse_preserves_interrupted_upgrade_version() {
    read_only_reuse_rejects_recorded_version(semver::Version::new(27, 0, 0), 28, true);
}

fn read_only_reuse_rejects_recorded_version(
    recorded_version: semver::Version,
    directory_major: u64,
    reuse_first: bool,
) {
    use std::{collections::BTreeMap, fs};

    use semver::Version;
    use zebra_chain::parameters::Network;

    use crate::{config::write_database_format_version_to_disk, Config, StateInitError, ZebraDb};

    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let network = Network::Mainnet;
    let old_path = config.db_path("state", directory_major, &network);
    let new_path = config.db_path("state", 29, &network);
    let db = DiskDb::new(
        &config,
        "state",
        &Version::new(directory_major, 0, 0),
        &network,
        ["utxo_loc_by_transparent_addr_loc".to_owned()],
        false,
    )
    .unwrap();
    drop(db);

    // A directory's recorded version can still describe an unfinished format transition.
    write_database_format_version_to_disk(
        &config,
        "state",
        directory_major,
        &recorded_version,
        &network,
    )
    .unwrap();
    let selected_path = if reuse_first {
        assert_eq!(
            DiskDb::try_reusing_previous_db_after_major_upgrade(
                &[29],
                &Version::new(29, 0, 0),
                &config,
                "state",
                &network,
                false,
            ),
            Some(recorded_version),
        );
        assert!(!old_path.exists());
        &new_path
    } else {
        &old_path
    };
    let files = || {
        fs::read_dir(selected_path)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let bytes = fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect::<BTreeMap<_, _>>()
    };
    let before = files();
    let result = ZebraDb::new(
        &config,
        "state",
        &Version::new(29, 0, 0),
        &network,
        true,
        ["utxo_loc_by_transparent_addr_loc".to_owned()],
        true,
    );
    let error = result.unwrap_err();
    assert!(
        matches!(
            error,
            StateInitError::ReadOnlyDatabaseNeedsUpgrade { ref path } if path == selected_path
        ),
        "read-only reuse must distinguish an unfinished transition from a missing database: {error}"
    );
    assert_eq!(files(), before, "the old directory must remain unmodified");
    if directory_major == 28 && !reuse_first {
        assert!(!new_path.parent().unwrap().exists());
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
