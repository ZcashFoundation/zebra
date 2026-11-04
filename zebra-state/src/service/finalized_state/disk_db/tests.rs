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
    /// Returns the column family names of the state database at `major` under `config`.
    pub fn list_cf_at(
        config: &crate::Config,
        major: u64,
        network: &zebra_chain::parameters::Network,
    ) -> Vec<String> {
        rocksdb::DB::list_cf(&DiskDb::options(), config.db_path("state", major, network))
            .unwrap_or_default()
    }

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

#[test]
fn read_only_reuse_rejects_an_interrupted_older_major_upgrade() {
    read_only_reuse_rejects_recorded_version(semver::Version::new(27, 0, 0));
}

#[test]
fn read_only_reuse_rejects_a_compatible_major_mismatch() {
    read_only_reuse_rejects_recorded_version(semver::Version::new(29, 0, 0));
}

fn read_only_reuse_rejects_recorded_version(recorded_version: semver::Version) {
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
    let old_path = config.db_path("state", 28, &network);
    let new_path = config.db_path("state", 29, &network);
    let db = DiskDb::new(
        &config,
        "state",
        &Version::new(28, 0, 0),
        &network,
        ["utxo_loc_by_transparent_addr_loc".to_owned()],
        false,
    )
    .unwrap();
    drop(db);

    // A directory's recorded version can still describe an unfinished format transition.
    write_database_format_version_to_disk(&config, "state", 28, &recorded_version, &network)
        .unwrap();
    let files = || {
        fs::read_dir(&old_path)
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
            StateInitError::ReadOnlyDatabaseNeedsUpgrade { ref path } if path == &old_path
        ),
        "read-only reuse must distinguish an unfinished transition from a missing database: {error}"
    );
    assert_eq!(files(), before, "the old directory must remain unmodified");
    assert!(!new_path.parent().unwrap().exists());
}

/// A read-only start reads an older-major database from before 30.1.0 in place, although that
/// database has no `snapshot_base` column family and a secondary instance can not create one.
#[test]
fn read_only_reuse_reads_a_database_without_the_snapshot_base_column_family() {
    use semver::Version;
    use zebra_chain::parameters::Network;

    use crate::{
        config::write_database_format_version_to_disk,
        service::finalized_state::{
            can_read_without_migration, SNAPSHOT_BASE, STATE_COLUMN_FAMILIES_IN_CODE,
        },
        Config, StateInitError, ZebraDb,
    };

    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let network = Network::Mainnet;
    let disk_version = Version::new(30, 0, 0);
    let running_version = Version::new(32, 0, 0);
    let in_code = || {
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string)
    };

    let db = DiskDb::new(
        &config,
        "state",
        &disk_version,
        &network,
        in_code().filter(|cf| cf != SNAPSHOT_BASE),
        false,
    )
    .unwrap();
    drop(db);
    write_database_format_version_to_disk(&config, "state", 30, &disk_version, &network).unwrap();

    let result = ZebraDb::new(
        &config,
        "state",
        &running_version,
        &network,
        true,
        in_code(),
        true,
    );
    if can_read_without_migration(&disk_version, &running_version) {
        let db = result.expect("a compatible 30.0.0 database opens read-only");
        assert_eq!(db.snapshot_base(), None);
    } else {
        // A later record migration makes the source need a writable start first.
        assert!(matches!(
            result.unwrap_err(),
            StateInitError::ReadOnlyDatabaseNeedsUpgrade { .. }
        ));
    }
    assert!(!config.db_path("state", 32, &network).exists());
    assert!(DiskDb::list_cf_at(&config, 30, &network)
        .iter()
        .all(|cf| cf != SNAPSHOT_BASE));
}

/// The reusable source is the newest directory that holds a database: an empty newer directory
/// does not hide an older database.
#[test]
fn reusable_source_skips_a_directory_without_a_database() {
    use semver::Version;
    use zebra_chain::parameters::Network;

    use crate::{service::finalized_state::STATE_COLUMN_FAMILIES_IN_CODE, Config};

    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let network = Network::Mainnet;
    let db = DiskDb::new(
        &config,
        "state",
        &Version::new(29, 0, 0),
        &network,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    )
    .unwrap();
    drop(db);
    std::fs::create_dir_all(config.db_path("state", 30, &network)).unwrap();

    assert_eq!(
        DiskDb::newest_reusable_source(&config, "state", &network, 32),
        Some(29)
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
