//! Raw data snapshot tests for the scanner database format.
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
//! If this test fails, run:
//! ```sh
//! cd zebra-scan
//! cargo insta test --review --features shielded-scan
//! ```
//! to update the test snapshots, then commit the `test_*.snap` files using git.
//!
//! # Snapshot Format
//!
//! These snapshots use [RON (Rusty Object Notation)](https://github.com/ron-rs/ron#readme),
//! a text format similar to Rust syntax. Raw byte data is encoded in hexadecimal.
//!
//! Due to `serde` limitations, some object types can't be represented exactly,
//! so RON uses the closest equivalent structure.

use std::collections::BTreeMap;

use zebra_chain::{
    block::Height,
    parameters::Network::{self, *},
};
use zebra_state::{RawBytes, ReadDisk, SaplingScannedDatabaseIndex, TransactionLocation, KV};

use crate::storage::{db::ScannerDb, Storage};

/// Snapshot test for RocksDB column families, and their raw key-value data.
///
/// These snapshots contain the `default` column family, but it is not used by Zebra.
#[test]
fn test_raw_rocksdb_column_families() {
    let _init_guard = zebra_test::init();

    test_raw_rocksdb_column_families_with_network(Mainnet);
    test_raw_rocksdb_column_families_with_network(Testnet);
}

/// Snapshot raw column families for `network`.
///
/// See [`test_raw_rocksdb_column_families`].
fn test_raw_rocksdb_column_families_with_network(network: Network) {
    let mut net_suffix = network.to_string();
    net_suffix.make_ascii_lowercase();

    let mut storage = super::new_test_storage(network);

    // Snapshot the column family names
    let mut cf_names = storage.db.list_cf().expect("empty database is valid");

    // The order that RocksDB returns column families is irrelevant,
    // because we always access them by name.
    cf_names.sort();

    // Assert that column family names are the same, regardless of the network.
    // Later, we check they are also the same regardless of the block height.
    insta::assert_ron_snapshot!("column_family_names", cf_names);

    // Assert that empty databases are the same, regardless of the network.
    let mut settings = insta::Settings::clone_current();

    settings.set_snapshot_suffix("empty");
    settings.bind(|| snapshot_raw_rocksdb_column_family_data(&storage.db, &cf_names));

    super::add_fake_keys(&mut storage);

    // Assert that the key format doesn't change.
    settings.set_snapshot_suffix(format!("{net_suffix}_keys"));
    settings.bind(|| snapshot_raw_rocksdb_column_family_data(&storage.db, &cf_names));

    // Snapshot raw database data for:
    // - mainnet and testnet
    // - genesis, block 1, and block 2
    //
    // We limit the number of blocks, because we create 2 snapshots per block, one for each network.
    for height in 0..=2 {
        super::add_fake_results(&mut storage, network, Height(height));

        let mut settings = insta::Settings::clone_current();
        settings.set_snapshot_suffix(format!("{net_suffix}_{height}"));

        // Assert that the result format doesn't change.
        settings.bind(|| snapshot_raw_rocksdb_column_family_data(&storage.db, &cf_names));
    }

    // TODO: add an empty marker result after PR #8080 merges
}

/// Snapshot the data in each column family, using `cargo insta` and RON serialization.
fn snapshot_raw_rocksdb_column_family_data(db: &ScannerDb, original_cf_names: &[String]) {
    let mut new_cf_names = db.list_cf().expect("empty database is valid");
    new_cf_names.sort();

    // Assert that column family names are the same, regardless of the network or block height.
    assert_eq!(
        original_cf_names, new_cf_names,
        "unexpected extra column families",
    );

    let mut empty_column_families = Vec::new();

    // Now run the data snapshots
    for cf_name in original_cf_names {
        let cf_handle = db
            .cf_handle(cf_name)
            .expect("RocksDB API provides correct names");

        // Correctness: Multi-key iteration causes hangs in concurrent code, but seems ok in tests.
        let cf_items: BTreeMap<RawBytes, RawBytes> = db.zs_items_in_range_ordered(&cf_handle, ..);

        // The default raw data serialization is very verbose, so we hex-encode the bytes.
        let cf_data: Vec<KV> = cf_items
            .iter()
            .map(|(key, value)| KV::new(key.raw_bytes(), value.raw_bytes()))
            .collect();

        if cf_name == "default" {
            assert_eq!(cf_data.len(), 0, "default column family is never used");
        } else if cf_data.is_empty() {
            // distinguish column family names from empty column families
            empty_column_families.push(format!("{cf_name}: no entries"));
        } else {
            // The note commitment tree snapshots will change if the trees do not have cached roots.
            // But we expect them to always have cached roots,
            // because those roots are used to populate the anchor column families.
            insta::assert_ron_snapshot!(format!("{cf_name}_raw_data"), cf_data);
        }
    }

    insta::assert_ron_snapshot!("empty_column_families", empty_column_families);
}
