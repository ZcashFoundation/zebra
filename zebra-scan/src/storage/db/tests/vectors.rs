//! Fixed test vectors for the scanner Storage.

use zebra_chain::{block::Height, parameters::Network};
use zebra_state::TransactionIndex;

use crate::{
    storage::db::tests::{fake_sapling_results, new_test_storage},
    tests::ZECPAGES_SAPLING_VIEWING_KEY,
};

/// Tests that keys are deleted correctly
#[test]
pub fn deletes_keys_and_results_correctly() {
    let mut db = new_test_storage(Network::Mainnet);

    let zec_pages_sapling_efvk = ZECPAGES_SAPLING_VIEWING_KEY.to_string();

    // Replace the last letter of the zec_pages efvk
    let fake_efvk = format!(
        "{}t",
        &ZECPAGES_SAPLING_VIEWING_KEY[..ZECPAGES_SAPLING_VIEWING_KEY.len() - 1]
    );

    let efvks = [&zec_pages_sapling_efvk, &fake_efvk];

    for efvk in efvks {
        for fake_result_height in [Height::MIN, Height(1), Height::MAX] {
            db.insert_sapling_results(
                efvk,
                fake_result_height,
                fake_sapling_results([
                    TransactionIndex::MIN,
                    TransactionIndex::from_index(40),
                    TransactionIndex::MAX,
                ]),
            );
        }
    }

    for efvk in efvks {
        assert!(
            !db.sapling_results(efvk).is_empty(),
            "there should be some results for this key in the db"
        );

        db.delete_sapling_results(vec![efvk.clone()]);

        assert!(
            db.sapling_results(efvk).is_empty(),
            "all results for this key should have been deleted"
        );
    }
}
