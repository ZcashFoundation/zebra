use std::convert::TryFrom;

use zebra_chain::{
    amount::Amount,
    at_least_one, orchard,
    parameters::Network,
    primitives::{redpallas, Halo2Proof},
    serialization::{AtLeastOne, ZcashDeserialize},
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

    // Create a dummy action, it doesn't need to be valid
    let dummy_action_bytes = [0u8; 820];
    let mut dummy_action_bytes_reader = &dummy_action_bytes[..];
    let dummy_action = orchard::Action::zcash_deserialize(&mut dummy_action_bytes_reader)
        .expect("Dummy action should deserialize");

    // Pair the dummy action with a fake signature
    let dummy_authorized_action = orchard::AuthorizedAction {
        action: dummy_action,
        spend_auth_sig: redpallas::Signature::from([0u8; 64]),
    };

    // Place the dummy action inside the Orchard shielded data
    let dummy_shielded_data = orchard::ShieldedData {
        flags: orchard::Flags::empty(),
        value_balance: Amount::try_from(0).expect("invalid transaction amount"),
        shared_anchor: orchard::tree::Root::default(),
        proof: Halo2Proof(vec![]),
        actions: at_least_one![dummy_authorized_action],
        binding_sig: redpallas::Signature::from([0u8; 64]),
    };

    // Replace the shielded data in the transaction so that it has one action
    match &mut transaction {
        Transaction::V5 {
            orchard_shielded_data,
            ..
        } => *orchard_shielded_data = Some(dummy_shielded_data),
        _ => panic!("Fake V5 transaction is not V5"),
    }

    // If a transaction has at least one Orhcard shielded action, it should be considered to have
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
