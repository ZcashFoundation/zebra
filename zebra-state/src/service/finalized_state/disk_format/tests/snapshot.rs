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
//! # Snapshot Format
//!
//! These snapshots use [RON (Rusty Object Notation)](https://github.com/ron-rs/ron#readme),
//! a text format similar to Rust syntax. Raw byte data is encoded in hexadecimal.
//!
//! Due to `serde` limitations, some object types can't be represented exactly,
//! so RON uses the closest equivalent structure.
//!
//! # TODO
//!
//! Test shielded data, and data activated in Overwinter and later network upgrades.

use std::sync::Arc;

use zebra_chain::{
    block::Block,
    parameters::Network::{self, *},
    serialization::ZcashDeserializeInto,
};

use crate::{
    service::finalized_state::{disk_db::DiskDb, disk_format::tests::KV, FinalizedState},
    Config,
};

/// Snapshot test for RocksDB column families, and their key-value data.
///
/// These snapshots contain the `default` column family, but it is not used by Zebra.
#[test]
fn test_raw_rocksdb_column_families() {
    zebra_test::init();

    test_raw_rocksdb_column_families_with_network(Mainnet);
    test_raw_rocksdb_column_families_with_network(Testnet);
}

/// Snapshot raw column families for `network`.
///
/// See [`test_raw_rocksdb_column_families`].
fn test_raw_rocksdb_column_families_with_network(network: Network) {
    let mut net_suffix = network.to_string();
    net_suffix.make_ascii_lowercase();

    let mut state = FinalizedState::new(&Config::ephemeral(), network);

    // Snapshot the column family names
    let mut cf_names = state.db.list_cf().expect("empty database is valid");

    // The order that RocksDB returns column families is irrelevant,
    // because we always access them by name.
    cf_names.sort();

    // Assert that column family names are the same, regardless of the network.
    // Later, we check they are also the same regardless of the block height.
    insta::assert_ron_snapshot!("column_family_names", cf_names);

    // Assert that empty databases are the same, regardless of the network.
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_suffix("no_blocks");

    settings.bind(|| snapshot_raw_rocksdb_column_family_data(&state.db, &cf_names));

    // Snapshot raw database data for:
    // - mainnet and testnet
    // - genesis, block 1, and block 2
    let blocks = match network {
        Mainnet => &*zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS,
        Testnet => &*zebra_test::vectors::CONTINUOUS_TESTNET_BLOCKS,
    };

    // We limit the number of blocks, because the serialized data is a few kilobytes per block.
    for height in 0..=2 {
        let block: Arc<Block> = blocks
            .get(&height)
            .expect("block height has test data")
            .zcash_deserialize_into()
            .expect("test data deserializes");

        state
            .commit_finalized_direct(block.into(), "snapshot tests")
            .expect("test block is valid");

        let mut settings = insta::Settings::clone_current();
        settings.set_snapshot_suffix(format!("{}_{}", net_suffix, height));

        settings.bind(|| snapshot_raw_rocksdb_column_family_data(&state.db, &cf_names));
    }
}

/// Snapshot the data in each column family, using `cargo insta` and RON serialization.
fn snapshot_raw_rocksdb_column_family_data(db: &DiskDb, original_cf_names: &[String]) {
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

        let mut cf_iter = db.forward_iterator(cf_handle);

        // The default raw data serialization is very verbose, so we hex-encode the bytes.
        let cf_data: Vec<KV> = cf_iter
            .by_ref()
            .map(|(key, value)| KV::new(key, value))
            .collect();

        if cf_name == "default" {
            assert_eq!(cf_data.len(), 0, "default column family is never used");
        } else if cf_data.is_empty() {
            // distinguish column family names from empty column families
            empty_column_families.push(format!("{}: no entries", cf_name));
        } else {
            insta::assert_ron_snapshot!(format!("{}_raw_data", cf_name), cf_data);
        }

        assert_eq!(
            cf_iter.status(),
            Ok(()),
            "unexpected column family iterator error",
        );
    }

    insta::assert_ron_snapshot!("empty_column_families", empty_column_families);
}
