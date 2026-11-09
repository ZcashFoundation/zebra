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

/// Check that compacting a column family drops entries that were written and then deleted.
#[test]
fn compact_cf_range_drops_deleted_entries() {
    use crate::{
        constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
        service::finalized_state::{
            zebra_db::transparent::UTXO_LOC_BY_TRANSPARENT_ADDR_LOC, STATE_COLUMN_FAMILIES_IN_CODE,
        },
        Config,
    };

    let _init_guard = zebra_test::init();

    let db = DiskDb::new(
        &Config::ephemeral(),
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        &zebra_chain::parameters::Network::Mainnet,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    )
    .expect("opening an ephemeral database should succeed");
    let cf = db.cf_handle(UTXO_LOC_BY_TRANSPARENT_ADDR_LOC).unwrap();
    let sst_size = || {
        db.property_int_value_cf(&cf, "rocksdb.total-sst-files-size")
            .unwrap()
            .unwrap()
    };

    // Like an address whose outputs were all spent: every entry is created, then deleted.
    for i in 0u32..1_000 {
        db.put_cf(&cf, i.to_be_bytes(), []).unwrap();
    }
    db.flush_cf(&cf).unwrap();
    for i in 0u32..1_000 {
        db.delete_cf(&cf, i.to_be_bytes()).unwrap();
    }
    db.flush_cf(&cf).unwrap();
    assert!(
        sst_size() > 0,
        "the entries and their deletion markers are on disk"
    );

    db.compact_cf_range(UTXO_LOC_BY_TRANSPARENT_ADDR_LOC, None, None);

    assert_eq!(
        sst_size(),
        0,
        "compaction drops both the entries and the deletion markers"
    );
}

/// Check that the address UTXO index compacts a deletion-heavy file without a manual compaction.
#[test]
fn deletion_heavy_utxo_index_files_are_compacted_automatically() {
    use std::time::{Duration, Instant};

    use crate::{
        constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
        service::finalized_state::{
            zebra_db::transparent::UTXO_LOC_BY_TRANSPARENT_ADDR_LOC, STATE_COLUMN_FAMILIES_IN_CODE,
        },
        Config,
    };

    let _init_guard = zebra_test::init();

    let db = DiskDb::new(
        &Config::ephemeral(),
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        &zebra_chain::parameters::Network::Mainnet,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    )
    .expect("opening an ephemeral database should succeed");
    let cf = db.cf_handle(UTXO_LOC_BY_TRANSPARENT_ADDR_LOC).unwrap();
    let level0_files = || {
        db.property_int_value_cf(&cf, "rocksdb.num-files-at-level0")
            .unwrap()
            .unwrap()
    };

    // One flush of only deletion markers makes a single level-0 file, below Zebra's level-0
    // compaction trigger of two files, so only the deletion-triggered collector compacts it.
    for i in 0u32..20_000 {
        db.delete_cf(&cf, i.to_be_bytes()).unwrap();
    }
    db.flush_cf(&cf).unwrap();

    let deadline = Instant::now() + Duration::from_secs(30);
    while level0_files() > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(level0_files(), 0, "the deletion-heavy file was compacted");
}
