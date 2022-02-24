//! Data snapshot tests for RocksDB column families.
//!
//! These tests check:
//! - the name of each column family
//! - the number of key-value entries
//! - the bytes in each key and value
//!
//! These tests currently use fixed test vectors.
//! TODO: test shielded data, and data activated in Overwinter and later network upgrades

use zebra_chain::parameters::Network::*;

use crate::{service::finalized_state::FinalizedState, Config};

/// Snapshot test for RocksDB column families, and their key-value data.
#[test]
fn test_raw_rocksdb_column_family_data() {
    // TODO: move to zebra_test::init()
    let mut settings = insta::Settings::clone_current();
    settings.set_prepend_module_to_snapshot(false);
    settings.bind_to_thread();

    zebra_test::init();

    let empty_state = FinalizedState::new(&Config::ephemeral(), Mainnet);
    let mut cf_names = empty_state.db.list_cf().expect("empty database is valid");

    // The order that RocksDB returns column families is irrelevant,
    // because we always access them by name.
    //
    // TODO: use insta::Settings::sort_selector() instead?
    cf_names.sort();

    insta::assert_ron_snapshot!(cf_names);
}
