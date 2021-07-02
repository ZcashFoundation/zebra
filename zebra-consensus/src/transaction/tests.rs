use std::{collections::HashMap, convert::TryFrom, convert::TryInto, sync::Arc};

use tower::{service_fn, ServiceExt};

use zebra_chain::{
    amount::{Amount, NonNegative},
    block, orchard,
    parameters::{Network, NetworkUpgrade},
    primitives::{ed25519, x25519, Groth16Proof},
    serialization::ZcashDeserialize,
    sprout,
    transaction::{
        arbitrary::{
            fake_v5_transactions_for_network, insert_fake_orchard_shielded_data, test_transactions,
        },
        Hash, HashType, JoinSplitData, LockTime, Transaction,
    },
    transparent::{self, CoinbaseData},
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

    // The check will fail if the transaction has no flags
    assert_eq!(
        check::has_inputs_and_outputs(&transaction),
        Err(TransactionError::NoInputs)
    );

    // If we add ENABLE_SPENDS flag it will pass the inputs check but fails with the outputs
    // TODO: Avoid new calls to `insert_fake_orchard_shielded_data` for each check #2409.
    let shielded_data = insert_fake_orchard_shielded_data(&mut transaction);
    shielded_data.flags = orchard::Flags::ENABLE_SPENDS;

    assert_eq!(
        check::has_inputs_and_outputs(&transaction),
        Err(TransactionError::NoOutputs)
    );

    // If we add ENABLE_OUTPUTS flag it will pass the outputs check but fails with the inputs
    let shielded_data = insert_fake_orchard_shielded_data(&mut transaction);
    shielded_data.flags = orchard::Flags::ENABLE_OUTPUTS;

    assert_eq!(
        check::has_inputs_and_outputs(&transaction),
        Err(TransactionError::NoInputs)
    );

    // Finally make it valid by adding both required flags
    let shielded_data = insert_fake_orchard_shielded_data(&mut transaction);
    shielded_data.flags = orchard::Flags::ENABLE_SPENDS | orchard::Flags::ENABLE_OUTPUTS;

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
// TODO: Remove `should_panic` once V5 transaction sighash is implemened by the merge of #2165.
#[should_panic]
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

/// Test if V4 transaction with transparent funds is accepted.
#[tokio::test]
async fn v4_transaction_with_transparent_transfer_is_accepted() {
    let network = Network::Mainnet;

    let canopy_activation_height = NetworkUpgrade::Canopy
        .activation_height(network)
        .expect("Canopy activation height is specified");

    let transaction_block_height =
        (canopy_activation_height + 10).expect("transaction block height is too large");

    let fake_source_fund_height =
        (transaction_block_height - 1).expect("fake source fund block height is too small");

    // Create a fake transparent transfer that should succeed
    let (input, output, known_utxos) = mock_transparent_transfer(fake_source_fund_height, true);

    // Create a V4 transaction
    let transaction = Transaction::V4 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::Height(block::Height(0)),
        expiry_height: (transaction_block_height + 1).expect("expiry height is too large"),
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let transaction_hash = transaction.hash();

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let script_verifier = script::Verifier::new(state_service);
    let verifier = Verifier::new(network, script_verifier);

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(known_utxos),
            height: transaction_block_height,
        })
        .await;

    assert_eq!(result, Ok(transaction_hash));
}

/// Test if V4 coinbase transaction is accepted.
#[tokio::test]
async fn v4_coinbase_transaction_is_accepted() {
    let network = Network::Mainnet;

    let canopy_activation_height = NetworkUpgrade::Canopy
        .activation_height(network)
        .expect("Canopy activation height is specified");

    let transaction_block_height =
        (canopy_activation_height + 10).expect("transaction block height is too large");

    // Create a fake transparent coinbase that should succeed
    let (input, output) = mock_coinbase_transparent_output(transaction_block_height);

    // Create a V4 coinbase transaction
    let transaction = Transaction::V4 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::Height(block::Height(0)),
        expiry_height: transaction_block_height,
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let transaction_hash = transaction.hash();

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let script_verifier = script::Verifier::new(state_service);
    let verifier = Verifier::new(network, script_verifier);

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(HashMap::new()),
            height: transaction_block_height,
        })
        .await;

    assert_eq!(result, Ok(transaction_hash));
}

/// Test if V4 transaction with transparent funds is rejected if the source script prevents it.
///
/// This test simulates the case where the script verifier rejects the transaction because the
/// script prevents spending the source UTXO.
#[tokio::test]
async fn v4_transaction_with_transparent_transfer_is_rejected_by_the_script() {
    let network = Network::Mainnet;

    let canopy_activation_height = NetworkUpgrade::Canopy
        .activation_height(network)
        .expect("Canopy activation height is specified");

    let transaction_block_height =
        (canopy_activation_height + 10).expect("transaction block height is too large");

    let fake_source_fund_height =
        (transaction_block_height - 1).expect("fake source fund block height is too small");

    // Create a fake transparent transfer that should not succeed
    let (input, output, known_utxos) = mock_transparent_transfer(fake_source_fund_height, false);

    // Create a V4 transaction
    let transaction = Transaction::V4 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::Height(block::Height(0)),
        expiry_height: (transaction_block_height + 1).expect("expiry height is too large"),
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let script_verifier = script::Verifier::new(state_service);
    let verifier = Verifier::new(network, script_verifier);

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(known_utxos),
            height: transaction_block_height,
        })
        .await;

    assert_eq!(
        result,
        Err(TransactionError::InternalDowncastError(
            "downcast to redjubjub::Error failed, original error: ScriptInvalid".to_string()
        ))
    );
}

/// Test if V5 transaction with transparent funds is accepted.
#[tokio::test]
// TODO: Remove `should_panic` once the NU5 activation heights for testnet and mainnet have been
// defined.
#[should_panic]
async fn v5_transaction_with_transparent_transfer_is_accepted() {
    let network = Network::Testnet;
    let network_upgrade = NetworkUpgrade::Nu5;

    let nu5_activation_height = network_upgrade
        .activation_height(network)
        .expect("NU5 activation height is specified");

    let transaction_block_height =
        (nu5_activation_height + 10).expect("transaction block height is too large");

    let fake_source_fund_height =
        (transaction_block_height - 1).expect("fake source fund block height is too small");

    // Create a fake transparent transfer that should succeed
    let (input, output, known_utxos) = mock_transparent_transfer(fake_source_fund_height, true);

    // Create a V5 transaction
    let transaction = Transaction::V5 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::Height(block::Height(0)),
        expiry_height: (transaction_block_height + 1).expect("expiry height is too large"),
        sapling_shielded_data: None,
        orchard_shielded_data: None,
        network_upgrade,
    };

    let transaction_hash = transaction.hash();

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let script_verifier = script::Verifier::new(state_service);
    let verifier = Verifier::new(network, script_verifier);

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(known_utxos),
            height: transaction_block_height,
        })
        .await;

    assert_eq!(result, Ok(transaction_hash));
}

/// Test if V5 coinbase transaction is accepted.
#[tokio::test]
// TODO: Remove `should_panic` once the NU5 activation heights for testnet and mainnet have been
// defined.
#[should_panic]
async fn v5_coinbase_transaction_is_accepted() {
    let network = Network::Testnet;
    let network_upgrade = NetworkUpgrade::Nu5;

    let nu5_activation_height = network_upgrade
        .activation_height(network)
        .expect("NU5 activation height is specified");

    let transaction_block_height =
        (nu5_activation_height + 10).expect("transaction block height is too large");

    // Create a fake transparent coinbase that should succeed
    let (input, output) = mock_coinbase_transparent_output(transaction_block_height);
    let known_utxos = HashMap::new();

    // Create a V5 coinbase transaction
    let transaction = Transaction::V5 {
        network_upgrade,
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::Height(block::Height(0)),
        expiry_height: transaction_block_height,
        sapling_shielded_data: None,
        orchard_shielded_data: None,
    };

    let transaction_hash = transaction.hash();

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let script_verifier = script::Verifier::new(state_service);
    let verifier = Verifier::new(network, script_verifier);

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(known_utxos),
            height: transaction_block_height,
        })
        .await;

    assert_eq!(result, Ok(transaction_hash));
}

/// Test if V5 transaction with transparent funds is rejected if the source script prevents it.
///
/// This test simulates the case where the script verifier rejects the transaction because the
/// script prevents spending the source UTXO.
#[tokio::test]
// TODO: Remove `should_panic` once the NU5 activation heights for testnet and mainnet have been
// defined.
#[should_panic]
async fn v5_transaction_with_transparent_transfer_is_rejected_by_the_script() {
    let network = Network::Testnet;
    let network_upgrade = NetworkUpgrade::Nu5;

    let nu5_activation_height = network_upgrade
        .activation_height(network)
        .expect("NU5 activation height is specified");

    let transaction_block_height =
        (nu5_activation_height + 10).expect("transaction block height is too large");

    let fake_source_fund_height =
        (transaction_block_height - 1).expect("fake source fund block height is too small");

    // Create a fake transparent transfer that should not succeed
    let (input, output, known_utxos) = mock_transparent_transfer(fake_source_fund_height, false);

    // Create a V5 transaction
    let transaction = Transaction::V5 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::Height(block::Height(0)),
        expiry_height: (transaction_block_height + 1).expect("expiry height is too large"),
        sapling_shielded_data: None,
        orchard_shielded_data: None,
        network_upgrade,
    };

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let script_verifier = script::Verifier::new(state_service);
    let verifier = Verifier::new(network, script_verifier);

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(known_utxos),
            height: transaction_block_height,
        })
        .await;

    assert_eq!(
        result,
        Err(TransactionError::InternalDowncastError(
            "downcast to redjubjub::Error failed, original error: ScriptInvalid".to_string()
        ))
    );
}

/// Test if signed V4 transaction with a dummy [`sprout::JoinSplit`] is accepted.
/// - Test if an unsigned V4 transaction with a dummy [`sprout::JoinSplit`] is rejected.
///
/// This test verifies if the transaction verifier correctly accepts a signed transaction.
#[test]
fn v4_with_signed_sprout_transfer_is_accepted() {
    zebra_test::init();
    zebra_test::RUNTIME.block_on(async {
        let network = Network::Mainnet;
        let network_upgrade = NetworkUpgrade::Canopy;

        let canopy_activation_height = network_upgrade
            .activation_height(network)
            .expect("Canopy activation height is not set");

        let transaction_block_height =
            (canopy_activation_height + 10).expect("Canopy activation height is too large");

        // Initialize the verifier
        let state_service =
            service_fn(|_| async { unreachable!("State service should not be called") });
        let script_verifier = script::Verifier::new(state_service);
        let verifier = Verifier::new(network, script_verifier);

        // Create a fake Sprout join split
        let (joinsplit_data, signing_key) = mock_sprout_join_split_data();

        let mut transaction = Transaction::V4 {
            inputs: vec![],
            outputs: vec![],
            lock_time: LockTime::Height(block::Height(0)),
            expiry_height: (transaction_block_height + 1).expect("expiry height is too large"),
            joinsplit_data: Some(joinsplit_data),
            sapling_shielded_data: None,
        };

        // Sign the transaction
        let sighash = transaction.sighash(network_upgrade, HashType::ALL, None);

        match &mut transaction {
            Transaction::V4 {
                joinsplit_data: Some(joinsplit_data),
                ..
            } => joinsplit_data.sig = signing_key.sign(sighash.as_bytes()),
            _ => unreachable!("Mock transaction was created incorrectly"),
        }

        let expected_hash = transaction.hash();

        // Test the transaction verifier
        let result = verifier
            .clone()
            .oneshot(Request::Block {
                transaction: Arc::new(transaction),
                known_utxos: Arc::new(HashMap::new()),
                height: transaction_block_height,
            })
            .await;

        assert_eq!(result, Ok(expected_hash));
    });
}

/// Test if an unsigned V4 transaction with a dummy [`sprout::JoinSplit`] is rejected.
///
/// This test verifies if the transaction verifier correctly rejects the transaction because of the
/// invalid signature.
#[test]
fn v4_with_unsigned_sprout_transfer_is_rejected() {
    zebra_test::init();
    zebra_test::RUNTIME.block_on(async {
        let network = Network::Mainnet;
        let network_upgrade = NetworkUpgrade::Canopy;

        let canopy_activation_height = network_upgrade
            .activation_height(network)
            .expect("Canopy activation height is not set");

        let transaction_block_height =
            (canopy_activation_height + 10).expect("Canopy activation height is too large");

        // Initialize the verifier
        let state_service =
            service_fn(|_| async { unreachable!("State service should not be called") });
        let script_verifier = script::Verifier::new(state_service);
        let verifier = Verifier::new(network, script_verifier);

        // Create a fake Sprout join split
        let (joinsplit_data, _) = mock_sprout_join_split_data();

        let transaction = Transaction::V4 {
            inputs: vec![],
            outputs: vec![],
            lock_time: LockTime::Height(block::Height(0)),
            expiry_height: (transaction_block_height + 1).expect("expiry height is too large"),
            joinsplit_data: Some(joinsplit_data),
            sapling_shielded_data: None,
        };

        // Test the transaction verifier
        let result = verifier
            .clone()
            .oneshot(Request::Block {
                transaction: Arc::new(transaction),
                known_utxos: Arc::new(HashMap::new()),
                height: transaction_block_height,
            })
            .await;

        assert_eq!(
            result,
            Err(
                // TODO: Fix error downcast
                // Err(TransactionError::Ed25519(ed25519::Error::InvalidSignature))
                TransactionError::InternalDowncastError(
                    "downcast to redjubjub::Error failed, original error: InvalidSignature"
                        .to_string(),
                )
            )
        );
    });
}

/// Test if a V4 transaction with Sapling spends is accepted by the verifier.
#[test]
fn v4_with_sapling_spends() {
    zebra_test::init();
    zebra_test::RUNTIME.block_on(async {
        let network = Network::Mainnet;

        let (height, transaction) = test_transactions(network)
            .rev()
            .filter(|(_, transaction)| {
                !transaction.is_coinbase() && transaction.inputs().is_empty()
            })
            .find(|(_, transaction)| transaction.sapling_spends_per_anchor().next().is_some())
            .expect("No transaction found with Sapling spends");

        let expected_hash = transaction.hash();

        // Initialize the verifier
        let state_service =
            service_fn(|_| async { unreachable!("State service should not be called") });
        let script_verifier = script::Verifier::new(state_service);
        let verifier = Verifier::new(network, script_verifier);

        // Test the transaction verifier
        let result = verifier
            .clone()
            .oneshot(Request::Block {
                transaction,
                known_utxos: Arc::new(HashMap::new()),
                height,
            })
            .await;

        assert_eq!(result, Ok(expected_hash));
    });
}

/// Test if a V4 transaction with Sapling outputs but no spends is accepted by the verifier.
#[test]
fn v4_with_sapling_outputs_and_no_spends() {
    zebra_test::init();
    zebra_test::RUNTIME.block_on(async {
        let network = Network::Mainnet;

        let (height, transaction) = test_transactions(network)
            .rev()
            .filter(|(_, transaction)| {
                !transaction.is_coinbase() && transaction.inputs().is_empty()
            })
            .find(|(_, transaction)| {
                transaction.sapling_spends_per_anchor().next().is_none()
                    && transaction.sapling_outputs().next().is_some()
            })
            .expect("No transaction found with Sapling outputs and no Sapling spends");

        let expected_hash = transaction.hash();

        // Initialize the verifier
        let state_service =
            service_fn(|_| async { unreachable!("State service should not be called") });
        let script_verifier = script::Verifier::new(state_service);
        let verifier = Verifier::new(network, script_verifier);

        // Test the transaction verifier
        let result = verifier
            .clone()
            .oneshot(Request::Block {
                transaction,
                known_utxos: Arc::new(HashMap::new()),
                height,
            })
            .await;

        assert_eq!(result, Ok(expected_hash));
    });
}

/// Test if a V5 transaction with Sapling spends is accepted by the verifier.
#[test]
// TODO: Remove `should_panic` once V5 transaction verification is complete.
#[should_panic]
fn v5_with_sapling_spends() {
    zebra_test::init();
    zebra_test::RUNTIME.block_on(async {
        let network = Network::Mainnet;

        let transaction =
            fake_v5_transactions_for_network(network, zebra_test::vectors::MAINNET_BLOCKS.iter())
                .rev()
                .filter(|transaction| !transaction.is_coinbase() && transaction.inputs().is_empty())
                .find(|transaction| transaction.sapling_spends_per_anchor().next().is_some())
                .expect("No transaction found with Sapling spends");

        let expected_hash = transaction.hash();
        let height = transaction
            .expiry_height()
            .expect("Transaction is missing expiry height");

        // Initialize the verifier
        let state_service =
            service_fn(|_| async { unreachable!("State service should not be called") });
        let script_verifier = script::Verifier::new(state_service);
        let verifier = Verifier::new(network, script_verifier);

        // Test the transaction verifier
        let result = verifier
            .clone()
            .oneshot(Request::Block {
                transaction: Arc::new(transaction),
                known_utxos: Arc::new(HashMap::new()),
                height,
            })
            .await;

        assert_eq!(result, Ok(expected_hash));
    });
}

// Utility functions

/// Create a mock transparent transfer to be included in a transaction.
///
/// First, this creates a fake unspent transaction output from a fake transaction included in the
/// specified `previous_utxo_height` block height. This fake [`Utxo`] also contains a simple script
/// that can either accept or reject any spend attempt, depending on if `script_should_succeed` is
/// `true` or `false`.
///
/// Then, a [`transparent::Input::PrevOut`] is created that attempts to spend the previously created fake
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

/// Create a mock coinbase input with a transparent output.
///
/// Create a [`transparent::Input::Coinbase`] at `coinbase_height`.
/// Then create UTXO with a [`transparent::Output`] spending some coinbase funds.
///
/// Returns the generated coinbase input and transparent output.
fn mock_coinbase_transparent_output(
    coinbase_height: block::Height,
) -> (transparent::Input, transparent::Output) {
    // A script with a single opcode that rejects the transaction (OP_FALSE)
    let rejecting_script = transparent::Script::new(&[0]);

    let input = transparent::Input::Coinbase {
        height: coinbase_height,
        data: CoinbaseData::new(Vec::new()),
        sequence: u32::MAX,
    };

    // The output resulting from the transfer
    // Using the rejecting script pretends the amount is burned because it can't be spent again
    let output = transparent::Output {
        value: Amount::try_from(1).expect("1 is an invalid amount"),
        lock_script: rejecting_script,
    };

    (input, output)
}

/// Create a mock [`sprout::JoinSplit`] and include it in a [`transaction::JoinSplitData`].
///
/// This creates a dummy join split. By itself it is invalid, but it is useful for including in a
/// transaction to check the signatures.
///
/// The [`transaction::JoinSplitData`] with the dummy [`sprout::JoinSplit`] is returned together
/// with the [`ed25519::SigningKey`] that can be used to create a signature to later add to the
/// returned join split data.
fn mock_sprout_join_split_data() -> (JoinSplitData<Groth16Proof>, ed25519::SigningKey) {
    // Prepare dummy inputs for the join split
    let zero_amount = 0_i32
        .try_into()
        .expect("Invalid JoinSplit transparent input");
    let anchor = sprout::tree::Root::default();
    let nullifier = sprout::note::Nullifier([0u8; 32]);
    let commitment = sprout::commitment::NoteCommitment::from([0u8; 32]);
    let ephemeral_key =
        x25519::PublicKey::from(&x25519::EphemeralSecret::new(rand07::thread_rng()));
    let random_seed = [0u8; 32];
    let mac = sprout::note::Mac::zcash_deserialize(&[0u8; 32][..])
        .expect("Failure to deserialize dummy MAC");
    let zkproof = Groth16Proof([0u8; 192]);
    let encrypted_note = sprout::note::EncryptedNote([0u8; 601]);

    // Create an dummy join split
    let joinsplit = sprout::JoinSplit {
        vpub_old: zero_amount,
        vpub_new: zero_amount,
        anchor,
        nullifiers: [nullifier; 2],
        commitments: [commitment; 2],
        ephemeral_key,
        random_seed,
        vmacs: [mac.clone(), mac],
        zkproof,
        enc_ciphertexts: [encrypted_note; 2],
    };

    // Create a usable signing key
    let signing_key = ed25519::SigningKey::new(rand::thread_rng());
    let verification_key = ed25519::VerificationKey::from(&signing_key);

    // Populate join split data with the dummy join split.
    let joinsplit_data = JoinSplitData {
        first: joinsplit,
        rest: vec![],
        pub_key: verification_key.into(),
        sig: [0u8; 64].into(),
    };

    (joinsplit_data, signing_key)
}

#[test]
fn add_to_sprout_pool_after_nu() {
    zebra_test::init();

    // get a block that we know it haves a transaction with `vpub_old` field greater than 0.
    let block: Arc<_> = zebra_chain::block::Block::zcash_deserialize(
        &zebra_test::vectors::BLOCK_MAINNET_419199_BYTES[..],
    )
    .unwrap()
    .into();

    // create a block height at canopy activation.
    let network = Network::Mainnet;
    let block_height = NetworkUpgrade::Canopy.activation_height(network).unwrap();

    // create a zero amount.
    let zero = Amount::<NonNegative>::try_from(0).expect("an amount of 0 is always valid");

    // the coinbase transaction should pass the check.
    assert_eq!(
        check::disabled_add_to_sprout_pool(&block.transactions[0], block_height, network),
        Ok(())
    );

    // the 2nd transaction has no joinsplits, should pass the check.
    assert_eq!(block.transactions[1].joinsplit_count(), 0);
    assert_eq!(
        check::disabled_add_to_sprout_pool(&block.transactions[1], block_height, network),
        Ok(())
    );

    // the 5th transaction has joinsplits and the `vpub_old` cumulative is greater than 0,
    // should fail the check.
    assert!(block.transactions[4].joinsplit_count() > 0);
    let vpub_old: Amount<NonNegative> = block.transactions[4]
        .sprout_pool_added_values()
        .fold(zero, |acc, &x| (acc + x).unwrap());
    assert!(vpub_old > zero);

    assert_eq!(
        check::disabled_add_to_sprout_pool(&block.transactions[3], block_height, network),
        Err(TransactionError::DisabledAddToSproutPool)
    );

    // the 8th transaction has joinsplits and the `vpub_old` cumulative is 0,
    // should pass the check.
    assert!(block.transactions[7].joinsplit_count() > 0);
    let vpub_old: Amount<NonNegative> = block.transactions[7]
        .sprout_pool_added_values()
        .fold(zero, |acc, &x| (acc + x).unwrap());
    assert_eq!(vpub_old, zero);

    assert_eq!(
        check::disabled_add_to_sprout_pool(&block.transactions[7], block_height, network),
        Ok(())
    );
}
