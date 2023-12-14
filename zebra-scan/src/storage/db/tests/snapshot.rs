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

use itertools::Itertools;

use zebra_chain::{
    block::Height,
    parameters::Network::{self, *},
};
use zebra_state::{RawBytes, ReadDisk, SaplingScannedDatabaseIndex, TransactionLocation, KV};

use crate::storage::{db::ScannerDb, Storage};

/// Snapshot test for:
/// - RocksDB column families, and their raw key-value data, and
/// - typed scanner result data using high-level storage methods.
///
/// These snapshots contain the `default` column family, but it is not used by Zebra.
#[test]
fn test_database_format() {
    let _init_guard = zebra_test::init();

    test_database_format_with_network(Mainnet);
    test_database_format_with_network(Testnet);
}

/// Snapshot raw and typed database formats for `network`.
///
/// See [`test_database_format()`] for details.
fn test_database_format_with_network(network: Network) {
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
    settings.bind(|| snapshot_typed_result_data(&storage));

    super::add_fake_keys(&mut storage);

    // Assert that the key format doesn't change.
    settings.set_snapshot_suffix(format!("{net_suffix}_keys"));
    settings.bind(|| snapshot_raw_rocksdb_column_family_data(&storage.db, &cf_names));
    settings.bind(|| snapshot_typed_result_data(&storage));

    // Snapshot raw database data for:
    // - mainnet and testnet
    // - genesis, block 1, and block 2
    //
    // We limit the number of blocks, because we create 2 snapshots per block, one for each network.
    for height in 0..=2 {
        super::add_fake_results(&mut storage, network, Height(height), true);
        super::add_fake_results(&mut storage, network, Height(height), false);

        let mut settings = insta::Settings::clone_current();
        settings.set_snapshot_suffix(format!("{net_suffix}_{height}"));

        // Assert that the result format doesn't change.
        settings.bind(|| snapshot_raw_rocksdb_column_family_data(&storage.db, &cf_names));
        settings.bind(|| snapshot_typed_result_data(&storage));
    }
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
            // Distinguish column family names from empty column families
            empty_column_families.push(format!("{cf_name}: no entries"));
        } else {
            // Make sure the raw format doesn't accidentally change.
            insta::assert_ron_snapshot!(format!("{cf_name}_raw_data"), cf_data);
        }
    }

    insta::assert_ron_snapshot!("empty_column_families", empty_column_families);
}

/// Snapshot typed scanner result data using high-level storage methods,
/// using `cargo insta` and RON serialization.
fn snapshot_typed_result_data(storage: &Storage) {
    // Make sure the typed key format doesn't accidentally change.
    let sapling_keys_last_heights = storage.sapling_keys_last_heights();

    // HashMap has an unstable order across Rust releases, so we need to sort it here.
    insta::assert_ron_snapshot!(
        "sapling_keys",
        sapling_keys_last_heights,
        {
                "." => insta::sorted_redaction()
        }
    );

    // HashMap has an unstable order across Rust releases, so we need to sort it here as well.
    for (key_index, (sapling_key, last_height)) in
        sapling_keys_last_heights.iter().sorted().enumerate()
    {
        let sapling_results = storage.sapling_results(sapling_key);

        assert_eq!(sapling_results.keys().max(), Some(last_height));

        // Check internal database method consistency
        for (height, results) in sapling_results.iter() {
            let sapling_index_and_results =
                storage.sapling_results_for_key_and_height(sapling_key, *height);

            // The list of results for each height must match the results queried by that height.
            let sapling_results_for_height: Vec<_> = sapling_index_and_results
                .values()
                .flatten()
                .cloned()
                .collect();
            assert_eq!(results, &sapling_results_for_height);

            for (index, result) in sapling_index_and_results {
                let index = SaplingScannedDatabaseIndex {
                    sapling_key: sapling_key.clone(),
                    tx_loc: TransactionLocation::from_parts(*height, index),
                };

                // The result for each index must match the result queried by that index.
                let sapling_result_for_index = storage.sapling_result_for_index(&index);
                assert_eq!(result, sapling_result_for_index);
            }
        }

        // Make sure the typed result format doesn't accidentally change.
        insta::assert_ron_snapshot!(format!("sapling_key_{key_index}_results"), sapling_results);
    }
}
