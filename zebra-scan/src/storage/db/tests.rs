//! General scanner database tests.

use std::{collections::BTreeMap, sync::Arc};

use zebra_chain::{
    block::{Block, Height},
    parameters::Network::{self, *},
    serialization::ZcashDeserializeInto,
    transaction,
};
use zebra_state::{SaplingScannedResult, TransactionIndex};

use crate::{
    storage::{Storage, INSERT_CONTROL_INTERVAL},
    tests::{FAKE_SAPLING_VIEWING_KEY, ZECPAGES_SAPLING_VIEWING_KEY},
    Config,
};

#[cfg(test)]
mod snapshot;

#[cfg(test)]
mod vectors;

/// Returns an empty `Storage` suitable for testing.
pub fn new_test_storage(network: Network) -> Storage {
    Storage::new(&Config::ephemeral(), network, false)
}

/// Add fake keys to `storage` for testing purposes.
pub fn add_fake_keys(storage: &mut Storage) {
    // Snapshot a birthday that is automatically set to activation height
    storage.add_sapling_key(&ZECPAGES_SAPLING_VIEWING_KEY.to_string(), None);
    // Snapshot a birthday above activation height
    storage.add_sapling_key(&FAKE_SAPLING_VIEWING_KEY.to_string(), Height(1_000_000));
}

/// Add fake results to `storage` for testing purposes.
///
/// If `add_progress_marker` is `true`, adds a progress marker.
/// If it is `false`, adds the transaction hashes from `height`.
pub fn add_fake_results(
    storage: &mut Storage,
    network: Network,
    height: Height,
    add_progress_marker: bool,
) {
    let blocks = match network {
        Mainnet => &*zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS,
        Testnet => &*zebra_test::vectors::CONTINUOUS_TESTNET_BLOCKS,
    };

    let block: Arc<Block> = blocks
        .get(&height.0)
        .expect("block height has test data")
        .zcash_deserialize_into()
        .expect("test data deserializes");

    if add_progress_marker {
        // Fake a progress marker.
        let next_progress_height = height.0.next_multiple_of(INSERT_CONTROL_INTERVAL);
        storage.add_sapling_results(
            &FAKE_SAPLING_VIEWING_KEY.to_string(),
            Height(next_progress_height),
            BTreeMap::new(),
        );
    } else {
        // Fake results from the block at `height`.
        storage.add_sapling_results(
            &ZECPAGES_SAPLING_VIEWING_KEY.to_string(),
            height,
            block
                .transactions
                .iter()
                .enumerate()
                .map(|(index, tx)| (TransactionIndex::from_usize(index), tx.hash().into()))
                .collect(),
        );
    }
}

/// Accepts an iterator of [`TransactionIndex`]es and returns a `BTreeMap` with empty results
pub fn fake_sapling_results<T: IntoIterator<Item = TransactionIndex>>(
    transaction_indexes: T,
) -> BTreeMap<TransactionIndex, SaplingScannedResult> {
    let mut fake_sapling_results = BTreeMap::new();

    for transaction_index in transaction_indexes {
        fake_sapling_results.insert(
            transaction_index,
            SaplingScannedResult::from(transaction::Hash::from([0; 32])),
        );
    }

    fake_sapling_results
}
