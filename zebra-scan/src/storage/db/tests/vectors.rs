//! Fixed test vectors for the scanner Storage.

use std::collections::BTreeMap;

use zebra_chain::{block::Height, parameters::Network, transaction};
use zebra_state::{SaplingScannedResult, TransactionIndex};

use crate::{storage::db::tests::new_test_storage, tests::ZECPAGES_SAPLING_VIEWING_KEY};

/// Tests that keys are deleted correctly
#[test]
pub fn deletes_keys_and_results_correctly() {
    let mut db = new_test_storage(Network::Mainnet);

    let zec_pages_sapling_efvk = ZECPAGES_SAPLING_VIEWING_KEY.to_string();

    for fake_result_height in [Height::MIN, Height(1), Height::MAX] {
        let mut fake_sapling_results: BTreeMap<TransactionIndex, SaplingScannedResult> =
            BTreeMap::new();
        for transaction_index in [
            TransactionIndex::MIN,
            TransactionIndex::from_index(40),
            TransactionIndex::MAX,
        ] {
            let fake_transaction_id = [0; 32];
            fake_sapling_results.insert(
                transaction_index,
                SaplingScannedResult::from(transaction::Hash::from(fake_transaction_id)),
            );
        }

        db.insert_sapling_results(
            &zec_pages_sapling_efvk,
            fake_result_height,
            fake_sapling_results,
        );
    }

    assert!(
        !db.sapling_results(&zec_pages_sapling_efvk).is_empty(),
        "there should be some results for this key in the db"
    );

    db.delete_sapling_results(vec![zec_pages_sapling_efvk.clone()]);

    assert!(
        db.sapling_results(&zec_pages_sapling_efvk).is_empty(),
        "all results for this key should have been deleted"
    );
}
