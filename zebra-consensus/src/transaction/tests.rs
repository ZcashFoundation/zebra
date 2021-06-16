use std::{collections::HashMap, convert::TryFrom, sync::Arc};

use tower::{service_fn, ServiceExt};

use zebra_chain::{
    amount::Amount,
    block, orchard,
    parameters::{Network, NetworkUpgrade},
    transaction::{
        arbitrary::{fake_v5_transactions_for_network, insert_fake_orchard_shielded_data},
        Hash, Transaction,
    },
    transparent,
};
use zebra_state::Utxo;

use super::{check, Request, Verifier};

use crate::{error::TransactionError, script};
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
                            check::output_cv_epk_not_small_order(output)?;
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
fn fake_v5_transaction_with_orchard_actions_has_inputs_and_outputs() {
    // Find a transaction with no inputs or outputs to use as base
    let mut transaction = fake_v5_transactions_for_network(
        Network::Mainnet,
        zebra_test::vectors::MAINNET_BLOCKS.iter(),
    )
    .rev()
    .find(|transaction| {
        transaction.inputs().is_empty()
            && transaction.outputs().is_empty()
            && transaction.sapling_spends_per_anchor().next().is_none()
            && transaction.sapling_outputs().next().is_none()
            && transaction.joinsplit_count() == 0
    })
    .expect("At least one fake V5 transaction with no inputs and no outputs");

    // Insert fake Orchard shielded data to the transaction, which has at least one action (this is
    // guaranteed structurally by `orchard::ShieldedData`)
    insert_fake_orchard_shielded_data(&mut transaction);

    // If a transaction has at least one Orchard shielded action, it should be considered to have
    // inputs and/or outputs
    assert!(check::has_inputs_and_outputs(&transaction).is_ok());
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

#[test]
fn v5_coinbase_transaction_without_enable_spends_flag_passes_validation() {
    let mut transaction = fake_v5_transactions_for_network(
        Network::Mainnet,
        zebra_test::vectors::MAINNET_BLOCKS.iter(),
    )
    .rev()
    .find(|transaction| transaction.is_coinbase())
    .expect("At least one fake V5 coinbase transaction in the test vectors");

    insert_fake_orchard_shielded_data(&mut transaction);

    assert!(check::coinbase_tx_no_prevout_joinsplit_spend(&transaction).is_ok(),);
}

#[test]
fn v5_coinbase_transaction_with_enable_spends_flag_fails_validation() {
    let mut transaction = fake_v5_transactions_for_network(
        Network::Mainnet,
        zebra_test::vectors::MAINNET_BLOCKS.iter(),
    )
    .rev()
    .find(|transaction| transaction.is_coinbase())
    .expect("At least one fake V5 coinbase transaction in the test vectors");

    let shielded_data = insert_fake_orchard_shielded_data(&mut transaction);

    shielded_data.flags = orchard::Flags::ENABLE_SPENDS;

    assert_eq!(
        check::coinbase_tx_no_prevout_joinsplit_spend(&transaction),
        Err(TransactionError::CoinbaseHasEnableSpendsOrchard)
    );
}

#[tokio::test]
async fn v5_transaction_is_rejected_before_nu5_activation() {
    const V5_TRANSACTION_VERSION: u32 = 5;

    let canopy = NetworkUpgrade::Canopy;
    let networks = vec![
        (Network::Mainnet, zebra_test::vectors::MAINNET_BLOCKS.iter()),
        (Network::Testnet, zebra_test::vectors::TESTNET_BLOCKS.iter()),
    ];

    for (network, blocks) in networks {
        let state_service = service_fn(|_| async { unreachable!("Service should not be called") });
        let script_verifier = script::Verifier::new(state_service);
        let verifier = Verifier::new(network, script_verifier);

        let transaction = fake_v5_transactions_for_network(network, blocks)
            .rev()
            .next()
            .expect("At least one fake V5 transaction in the test vectors");

        let result = verifier
            .oneshot(Request::Block {
                transaction: Arc::new(transaction),
                known_utxos: Arc::new(HashMap::new()),
                height: canopy
                    .activation_height(network)
                    .expect("Canopy activation height is specified"),
            })
            .await;

        assert_eq!(
            result,
            Err(TransactionError::UnsupportedByNetworkUpgrade(
                V5_TRANSACTION_VERSION,
                canopy
            ))
        );
    }
}

#[tokio::test]
// TODO: Remove `should_panic` once the NU5 activation heights for testnet and mainnet have been
// defined.
#[should_panic]
async fn v5_transaction_is_accepted_after_nu5_activation() {
    let nu5 = NetworkUpgrade::Nu5;
    let networks = vec![
        (Network::Mainnet, zebra_test::vectors::MAINNET_BLOCKS.iter()),
        (Network::Testnet, zebra_test::vectors::TESTNET_BLOCKS.iter()),
    ];

    for (network, blocks) in networks {
        let state_service = service_fn(|_| async { unreachable!("Service should not be called") });
        let script_verifier = script::Verifier::new(state_service);
        let verifier = Verifier::new(network, script_verifier);

        let transaction = fake_v5_transactions_for_network(network, blocks)
            .rev()
            .next()
            .expect("At least one fake V5 transaction in the test vectors");

        let expected_hash = transaction.hash();

        let result = verifier
            .oneshot(Request::Block {
                transaction: Arc::new(transaction),
                known_utxos: Arc::new(HashMap::new()),
                height: nu5
                    .activation_height(network)
                    .expect("NU5 activation height is specified"),
            })
            .await;

        assert_eq!(result, Ok(expected_hash));
    }
}

// Utility functions

/// Create a mock transparent transfer to be included in a transaction.
///
/// First, this creates a fake unspent transaction output from a fake transaction included in the
/// specified `previous_utxo_height` block height. This fake [`Utxo`] also contains a simple script
/// that can either accept or reject any spend attempt, depending on if `script_should_succeed` is
/// `true` or `false`.
///
/// Then, a [`transparent::Input`] is created that attempts to spends the previously created fake
/// UTXO. A new UTXO is created with the [`transparent::Output`] resulting from the spend.
///
/// Finally, the initial fake UTXO is placed in a `known_utxos` [`HashMap`] so that it can be
/// retrieved during verification.
///
/// The function then returns the generated transparent input and output, as well as the
/// `known_utxos` map.
fn mock_transparent_transfer(
    previous_utxo_height: block::Height,
    script_should_succeed: bool,
) -> (
    transparent::Input,
    transparent::Output,
    HashMap<transparent::OutPoint, Utxo>,
) {
    // A script with a single opcode that accepts the transaction (pushes true on the stack)
    let accepting_script = transparent::Script::new(&[1, 1]);
    // A script with a single opcode that rejects the transaction (OP_FALSE)
    let rejecting_script = transparent::Script::new(&[0]);

    // Mock an unspent transaction output
    let previous_outpoint = transparent::OutPoint {
        hash: Hash([1u8; 32]),
        index: 0,
    };

    let lock_script = if script_should_succeed {
        accepting_script.clone()
    } else {
        rejecting_script.clone()
    };

    let previous_output = transparent::Output {
        value: Amount::try_from(1).expect("1 is an invalid amount"),
        lock_script,
    };

    let previous_utxo = Utxo {
        output: previous_output,
        height: previous_utxo_height,
        from_coinbase: false,
    };

    // Use the `previous_outpoint` as input
    let input = transparent::Input::PrevOut {
        outpoint: previous_outpoint,
        unlock_script: accepting_script,
        sequence: 0,
    };

    // The output resulting from the transfer
    // Using the rejecting script pretends the amount is burned because it can't be spent again
    let output = transparent::Output {
        value: Amount::try_from(1).expect("1 is an invalid amount"),
        lock_script: rejecting_script,
    };

    // Cache the source of the fund so that it can be used during verification
    let mut known_utxos = HashMap::new();
    known_utxos.insert(previous_outpoint, previous_utxo);

    (input, output, known_utxos)
}
