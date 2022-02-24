//! Data snapshot tests for RocksDB column families.
//!
//! These tests check:
//! - the name of each column family
//! - the number of key-value entries
//! - the bytes in each key and value
//!
//! These tests currently use fixed test vectors.
//!
//! # Fixing Test Failures
//!
//! If this test fails, run `cargo insta review` to update the test snapshots,
//! then commit the `test_*.snap` files using git.
//!
//! # TODO
//!
//! Test shielded data, and data activated in Overwinter and later network upgrades.

use zebra_chain::parameters::Network::*;

use crate::{service::finalized_state::FinalizedState, Config};

/// Snapshot test for RocksDB column families, and their key-value data.
///
/// These snapshots contain the `default` column family, but it is not used by Zebra.
#[test]
fn test_raw_rocksdb_column_family_data() {
    zebra_test::init();

    let empty_state = FinalizedState::new(&Config::ephemeral(), Mainnet);
    let db = empty_state.db;

    // TODO: repeat for genesis, block 1, block 2,
    //       asserting that the column family names remain the same each time
    //
    // https://docs.rs/insta/latest/insta/macro.with_settings.html
    // https://docs.rs/insta/latest/insta/struct.Settings.html#method.set_snapshot_suffix

    // Snapshot the column family names

    let mut cf_names = db.list_cf().expect("empty database is valid");

    // The order that RocksDB returns column families is irrelevant,
    // because we always access them by name.
    cf_names.sort();

    insta::assert_ron_snapshot!("column_family_names", cf_names);

    // Snapshot the data in each column family

    for cf_name in cf_names {
        let cf_handle = db
            .cf_handle(&cf_name)
            .expect("RocksDB provided correct names");

        let mut cf_iter = db.forward_iterator(cf_handle);
        let cf_data: Vec<_> = cf_iter.by_ref().collect();

        insta::assert_ron_snapshot!(format!("{}_raw_data", cf_name), cf_data);

        assert_eq!(
            cf_iter.status(),
            Ok(()),
            "unexpected column family iterator error",
        );
    }
}
