use zebra_chain::{
    parameters::Network,
    transaction::{arbitrary::fake_v5_transactions_for_network, Transaction},
};

use super::check;

use crate::error::TransactionError;
use color_eyre::eyre::Report;

#[test]
fn v5_fake_transactions() -> Result<(), Report> {
    zebra_test::init();

    let networks = vec![
        (Network::Mainnet, zebra_test::vectors::MAINNET_BLOCKS.iter()),
        (Network::Testnet, zebra_test::vectors::TESTNET_BLOCKS.iter()),
    ];

    for (network, blocks) in networks {
        for transaction in fake_v5_transactions_for_network(network, blocks) {
            match check::has_inputs_and_outputs(&transaction) {
                Ok(()) => (),
                Err(TransactionError::NoInputs) | Err(TransactionError::NoOutputs) => (),
                Err(_) => panic!("error must be NoInputs or NoOutputs"),
            };

            // make sure there are no joinsplits nor spends in coinbase
            check::coinbase_tx_no_prevout_joinsplit_spend(&transaction)?;

            // validate the sapling shielded data
            match transaction {
                Transaction::V5 {
                    sapling_shielded_data,
                    ..
                } => {
                    if let Some(s) = sapling_shielded_data {
                        for spend in s.spends_per_anchor() {
                            check::spend_cv_rk_not_small_order(&spend)?
                        }
                        for output in s.outputs() {
                            check::output_cv_epk_not_small_order(&output)?;
                        }
                    }
                }
                _ => panic!("we should have no tx other than 5"),
            }
        }
    }

    Ok(())
}

#[test]
fn v5_transaction_with_no_inputs_fails_validation() {
    let transaction = fake_v5_transactions_for_network(
        Network::Mainnet,
        zebra_test::vectors::MAINNET_BLOCKS.iter(),
    )
    .rev()
    .find(|transaction| {
        transaction.inputs().is_empty()
            && transaction.sapling_spends_per_anchor().next().is_none()
            && transaction.orchard_actions().next().is_none()
            && transaction.joinsplit_count() == 0
            && (!transaction.outputs().is_empty() || transaction.sapling_outputs().next().is_some())
    })
    .expect("At least one fake v5 transaction with no inputs in the test vectors");

    assert_eq!(
        check::has_inputs_and_outputs(&transaction),
        Err(TransactionError::NoInputs)
    );
}

#[test]
fn v5_transaction_with_no_outputs_fails_validation() {
    let transaction = fake_v5_transactions_for_network(
        Network::Mainnet,
        zebra_test::vectors::MAINNET_BLOCKS.iter(),
    )
    .rev()
    .find(|transaction| {
        transaction.outputs().is_empty()
            && transaction.sapling_outputs().next().is_none()
            && transaction.orchard_actions().next().is_none()
            && transaction.joinsplit_count() == 0
            && (!transaction.inputs().is_empty()
                || transaction.sapling_spends_per_anchor().next().is_some())
    })
    .expect("At least one fake v5 transaction with no outputs in the test vectors");

    assert_eq!(
        check::has_inputs_and_outputs(&transaction),
        Err(TransactionError::NoOutputs)
    );
}
