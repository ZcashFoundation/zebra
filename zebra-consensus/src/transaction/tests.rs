//! Tests for Zcash transaction consensus checks.
//
// TODO: split fixed test vectors into a `vectors` module?

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, TimeZone, Utc};
use color_eyre::eyre::Report;
use halo2::pasta::{group::ff::PrimeField, pallas};
use tower::{service_fn, ServiceExt};

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, Block, Height},
    orchard::{AuthorizedAction, OrchardVanilla},
    parameters::{Network, NetworkUpgrade},
    primitives::{ed25519, x25519, Groth16Proof},
    sapling,
    serialization::{DateTime32, ZcashDeserialize, ZcashDeserializeInto},
    sprout,
    transaction::{
        arbitrary::{
            fake_v5_transactions_for_network, insert_fake_orchard_shielded_data, test_transactions,
            transactions_from_blocks,
        },
        zip317, Hash, HashType, JoinSplitData, LockTime, Transaction,
    },
    transparent::{self, CoinbaseData},
};

use zebra_state::ValidateContextError;
use zebra_test::mock_service::MockService;

use crate::error::TransactionError;

use super::{check, Request, Verifier};

#[cfg(test)]
mod prop;

#[test]
fn v5_fake_transactions() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        for transaction in fake_v5_transactions_for_network(&network, network.block_iter()) {
            match check::has_inputs_and_outputs(&transaction) {
                Ok(()) => (),
                Err(TransactionError::NoInputs) | Err(TransactionError::NoOutputs) => (),
                Err(_) => panic!("error must be NoInputs or NoOutputs"),
            };

            // make sure there are no joinsplits nor spends in coinbase
            check::coinbase_tx_no_prevout_joinsplit_spend(&transaction)?;
        }
    }

    Ok(())
}

#[test]
fn fake_v5_transaction_with_orchard_actions_has_inputs_and_outputs() {
    // Find a transaction with no inputs or outputs to use as base
    let mut transaction = fake_v5_transactions_for_network(
        &Network::Mainnet,
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
    shielded_data.action_groups.first_mut().flags = zebra_chain::orchard::Flags::ENABLE_SPENDS;

    assert_eq!(
        check::has_inputs_and_outputs(&transaction),
        Err(TransactionError::NoOutputs)
    );

    // If we add ENABLE_OUTPUTS flag it will pass the outputs check but fails with the inputs
    let shielded_data = insert_fake_orchard_shielded_data(&mut transaction);
    shielded_data.action_groups.first_mut().flags = zebra_chain::orchard::Flags::ENABLE_OUTPUTS;

    assert_eq!(
        check::has_inputs_and_outputs(&transaction),
        Err(TransactionError::NoInputs)
    );

    // Finally make it valid by adding both required flags
    let shielded_data = insert_fake_orchard_shielded_data(&mut transaction);
    shielded_data.action_groups.first_mut().flags =
        zebra_chain::orchard::Flags::ENABLE_SPENDS | zebra_chain::orchard::Flags::ENABLE_OUTPUTS;

    assert!(check::has_inputs_and_outputs(&transaction).is_ok());
}

#[test]
fn fake_v5_transaction_with_orchard_actions_has_flags() {
    // Find a transaction with no inputs or outputs to use as base
    let mut transaction = fake_v5_transactions_for_network(
        &Network::Mainnet,
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
        check::has_enough_orchard_flags(&transaction),
        Err(TransactionError::NotEnoughFlags)
    );

    // If we add ENABLE_SPENDS flag it will pass.
    let shielded_data = insert_fake_orchard_shielded_data(&mut transaction);
    shielded_data.action_groups.first_mut().flags = zebra_chain::orchard::Flags::ENABLE_SPENDS;
    assert!(check::has_enough_orchard_flags(&transaction).is_ok());

    // If we add ENABLE_OUTPUTS flag instead, it will pass.
    let shielded_data = insert_fake_orchard_shielded_data(&mut transaction);
    shielded_data.action_groups.first_mut().flags = zebra_chain::orchard::Flags::ENABLE_OUTPUTS;
    assert!(check::has_enough_orchard_flags(&transaction).is_ok());

    // If we add BOTH ENABLE_SPENDS and ENABLE_OUTPUTS flags it will pass.
    let shielded_data = insert_fake_orchard_shielded_data(&mut transaction);
    shielded_data.action_groups.first_mut().flags =
        zebra_chain::orchard::Flags::ENABLE_SPENDS | zebra_chain::orchard::Flags::ENABLE_OUTPUTS;
    assert!(check::has_enough_orchard_flags(&transaction).is_ok());
}

#[test]
fn v5_transaction_with_no_inputs_fails_validation() {
    let transaction = fake_v5_transactions_for_network(
        &Network::Mainnet,
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

#[tokio::test]
async fn mempool_request_with_missing_input_is_rejected() {
    let mut state: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let verifier = Verifier::new(&Network::Mainnet, state.clone());

    let (height, tx) = transactions_from_blocks(zebra_test::vectors::MAINNET_BLOCKS.iter())
        .find(|(_, tx)| !(tx.is_coinbase() || tx.inputs().is_empty()))
        .expect("At least one non-coinbase transaction with transparent inputs in test vectors");

    let input_outpoint = match tx.inputs()[0] {
        transparent::Input::PrevOut { outpoint, .. } => outpoint,
        transparent::Input::Coinbase { .. } => panic!("requires a non-coinbase transaction"),
    };

    tokio::spawn(async move {
        // The first non-coinbase transaction with transparent inputs in our test vectors
        // does not use a lock time, so we don't see Request::BestChainNextMedianTimePast here

        state
            .expect_request(zebra_state::Request::UnspentBestChainUtxo(input_outpoint))
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::UnspentBestChainUtxo(None));

        state
            .expect_request_that(|req| {
                matches!(
                    req,
                    zebra_state::Request::CheckBestChainTipNullifiersAndAnchors(_)
                )
            })
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::ValidBestChainTipNullifiersAndAnchors);
    });

    let verifier_response = verifier
        .oneshot(Request::Mempool {
            transaction: tx.into(),
            height,
        })
        .await;

    assert_eq!(
        verifier_response,
        Err(TransactionError::TransparentInputNotFound)
    );
}

#[tokio::test]
async fn mempool_request_with_present_input_is_accepted() {
    let mut state: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let verifier = Verifier::new(&Network::Mainnet, state.clone());

    let height = NetworkUpgrade::Canopy
        .activation_height(&Network::Mainnet)
        .expect("Canopy activation height is specified");
    let fund_height = (height - 1).expect("fake source fund block height is too small");
    let (input, output, known_utxos) = mock_transparent_transfer(
        fund_height,
        true,
        0,
        Amount::try_from(10001).expect("invalid value"),
    );

    // Create a non-coinbase V4 tx with the last valid expiry height.
    let tx = Transaction::V4 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::unlocked(),
        expiry_height: height,
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let input_outpoint = match tx.inputs()[0] {
        transparent::Input::PrevOut { outpoint, .. } => outpoint,
        transparent::Input::Coinbase { .. } => panic!("requires a non-coinbase transaction"),
    };

    tokio::spawn(async move {
        state
            .expect_request(zebra_state::Request::UnspentBestChainUtxo(input_outpoint))
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::UnspentBestChainUtxo(
                known_utxos
                    .get(&input_outpoint)
                    .map(|utxo| utxo.utxo.clone()),
            ));

        state
            .expect_request_that(|req| {
                matches!(
                    req,
                    zebra_state::Request::CheckBestChainTipNullifiersAndAnchors(_)
                )
            })
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::ValidBestChainTipNullifiersAndAnchors);
    });

    let verifier_response = verifier
        .oneshot(Request::Mempool {
            transaction: tx.into(),
            height,
        })
        .await;

    assert!(
        verifier_response.is_ok(),
        "expected successful verification, got: {verifier_response:?}"
    );
}

#[tokio::test]
async fn mempool_request_with_invalid_lock_time_is_rejected() {
    let mut state: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let verifier = Verifier::new(&Network::Mainnet, state.clone());

    let height = NetworkUpgrade::Canopy
        .activation_height(&Network::Mainnet)
        .expect("Canopy activation height is specified");
    let fund_height = (height - 1).expect("fake source fund block height is too small");
    let (input, output, known_utxos) = mock_transparent_transfer(
        fund_height,
        true,
        0,
        Amount::try_from(1).expect("invalid value"),
    );

    // Create a non-coinbase V4 tx with the last valid expiry height.
    let tx = Transaction::V4 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::max_lock_time_timestamp(),
        expiry_height: height,
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let input_outpoint = match tx.inputs()[0] {
        transparent::Input::PrevOut { outpoint, .. } => outpoint,
        transparent::Input::Coinbase { .. } => panic!("requires a non-coinbase transaction"),
    };

    tokio::spawn(async move {
        state
            .expect_request(zebra_state::Request::BestChainNextMedianTimePast)
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::BestChainNextMedianTimePast(
                DateTime32::from(
                    u32::try_from(LockTime::MIN_TIMESTAMP).expect("min time is valid"),
                ),
            ));

        state
            .expect_request(zebra_state::Request::UnspentBestChainUtxo(input_outpoint))
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::UnspentBestChainUtxo(
                known_utxos
                    .get(&input_outpoint)
                    .map(|utxo| utxo.utxo.clone()),
            ));

        state
            .expect_request_that(|req| {
                matches!(
                    req,
                    zebra_state::Request::CheckBestChainTipNullifiersAndAnchors(_)
                )
            })
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::ValidBestChainTipNullifiersAndAnchors);
    });

    let verifier_response = verifier
        .oneshot(Request::Mempool {
            transaction: tx.into(),
            height,
        })
        .await;

    assert_eq!(
        verifier_response,
        Err(TransactionError::LockedUntilAfterBlockTime(
            Utc.timestamp_opt(u32::MAX.into(), 0).unwrap()
        ))
    );
}

#[tokio::test]
async fn mempool_request_with_unlocked_lock_time_is_accepted() {
    let mut state: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let verifier = Verifier::new(&Network::Mainnet, state.clone());

    let height = NetworkUpgrade::Canopy
        .activation_height(&Network::Mainnet)
        .expect("Canopy activation height is specified");
    let fund_height = (height - 1).expect("fake source fund block height is too small");
    let (input, output, known_utxos) = mock_transparent_transfer(
        fund_height,
        true,
        0,
        Amount::try_from(10001).expect("invalid value"),
    );

    // Create a non-coinbase V4 tx with the last valid expiry height.
    let tx = Transaction::V4 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::unlocked(),
        expiry_height: height,
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let input_outpoint = match tx.inputs()[0] {
        transparent::Input::PrevOut { outpoint, .. } => outpoint,
        transparent::Input::Coinbase { .. } => panic!("requires a non-coinbase transaction"),
    };

    tokio::spawn(async move {
        state
            .expect_request(zebra_state::Request::UnspentBestChainUtxo(input_outpoint))
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::UnspentBestChainUtxo(
                known_utxos
                    .get(&input_outpoint)
                    .map(|utxo| utxo.utxo.clone()),
            ));

        state
            .expect_request_that(|req| {
                matches!(
                    req,
                    zebra_state::Request::CheckBestChainTipNullifiersAndAnchors(_)
                )
            })
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::ValidBestChainTipNullifiersAndAnchors);
    });

    let verifier_response = verifier
        .oneshot(Request::Mempool {
            transaction: tx.into(),
            height,
        })
        .await;

    assert!(
        verifier_response.is_ok(),
        "expected successful verification, got: {verifier_response:?}"
    );
}

#[tokio::test]
async fn mempool_request_with_lock_time_max_sequence_number_is_accepted() {
    let mut state: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let verifier = Verifier::new(&Network::Mainnet, state.clone());

    let height = NetworkUpgrade::Canopy
        .activation_height(&Network::Mainnet)
        .expect("Canopy activation height is specified");
    let fund_height = (height - 1).expect("fake source fund block height is too small");
    let (mut input, output, known_utxos) = mock_transparent_transfer(
        fund_height,
        true,
        0,
        Amount::try_from(10001).expect("invalid value"),
    );

    // Ignore the lock time.
    input.set_sequence(u32::MAX);

    // Create a non-coinbase V4 tx with the last valid expiry height.
    let tx = Transaction::V4 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::max_lock_time_timestamp(),
        expiry_height: height,
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let input_outpoint = match tx.inputs()[0] {
        transparent::Input::PrevOut { outpoint, .. } => outpoint,
        transparent::Input::Coinbase { .. } => panic!("requires a non-coinbase transaction"),
    };

    tokio::spawn(async move {
        state
            .expect_request(zebra_state::Request::UnspentBestChainUtxo(input_outpoint))
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::UnspentBestChainUtxo(
                known_utxos
                    .get(&input_outpoint)
                    .map(|utxo| utxo.utxo.clone()),
            ));

        state
            .expect_request_that(|req| {
                matches!(
                    req,
                    zebra_state::Request::CheckBestChainTipNullifiersAndAnchors(_)
                )
            })
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::ValidBestChainTipNullifiersAndAnchors);
    });

    let verifier_response = verifier
        .oneshot(Request::Mempool {
            transaction: tx.into(),
            height,
        })
        .await;

    assert!(
        verifier_response.is_ok(),
        "expected successful verification, got: {verifier_response:?}"
    );
}

#[tokio::test]
async fn mempool_request_with_past_lock_time_is_accepted() {
    let mut state: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let verifier = Verifier::new(&Network::Mainnet, state.clone());

    let height = NetworkUpgrade::Canopy
        .activation_height(&Network::Mainnet)
        .expect("Canopy activation height is specified");
    let fund_height = (height - 1).expect("fake source fund block height is too small");
    let (input, output, known_utxos) = mock_transparent_transfer(
        fund_height,
        true,
        0,
        Amount::try_from(10001).expect("invalid value"),
    );

    // Create a non-coinbase V4 tx with the last valid expiry height.
    let tx = Transaction::V4 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::min_lock_time_timestamp(),
        expiry_height: height,
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let input_outpoint = match tx.inputs()[0] {
        transparent::Input::PrevOut { outpoint, .. } => outpoint,
        transparent::Input::Coinbase { .. } => panic!("requires a non-coinbase transaction"),
    };

    tokio::spawn(async move {
        state
            .expect_request(zebra_state::Request::BestChainNextMedianTimePast)
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::BestChainNextMedianTimePast(
                DateTime32::MAX,
            ));

        state
            .expect_request(zebra_state::Request::UnspentBestChainUtxo(input_outpoint))
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::UnspentBestChainUtxo(
                known_utxos
                    .get(&input_outpoint)
                    .map(|utxo| utxo.utxo.clone()),
            ));

        state
            .expect_request_that(|req| {
                matches!(
                    req,
                    zebra_state::Request::CheckBestChainTipNullifiersAndAnchors(_)
                )
            })
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::ValidBestChainTipNullifiersAndAnchors);
    });

    let verifier_response = verifier
        .oneshot(Request::Mempool {
            transaction: tx.into(),
            height,
        })
        .await;

    assert!(
        verifier_response.is_ok(),
        "expected successful verification, got: {verifier_response:?}"
    );
}

/// Tests that calls to the transaction verifier with a mempool request that spends
/// immature coinbase outputs will return an error.
#[tokio::test]
async fn mempool_request_with_immature_spend_is_rejected() {
    let _init_guard = zebra_test::init();

    let mut state: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let verifier = Verifier::new(&Network::Mainnet, state.clone());

    let height = NetworkUpgrade::Canopy
        .activation_height(&Network::Mainnet)
        .expect("Canopy activation height is specified");
    let fund_height = (height - 1).expect("fake source fund block height is too small");
    let (input, output, known_utxos) = mock_transparent_transfer(
        fund_height,
        true,
        0,
        Amount::try_from(10001).expect("invalid value"),
    );

    // Create a non-coinbase V4 tx with the last valid expiry height.
    let tx = Transaction::V4 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::min_lock_time_timestamp(),
        expiry_height: height,
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let input_outpoint = match tx.inputs()[0] {
        transparent::Input::PrevOut { outpoint, .. } => outpoint,
        transparent::Input::Coinbase { .. } => panic!("requires a non-coinbase transaction"),
    };

    let spend_restriction = tx.coinbase_spend_restriction(height);

    let coinbase_spend_height = Height(5);

    let utxo = known_utxos
        .get(&input_outpoint)
        .map(|utxo| {
            let mut utxo = utxo.utxo.clone();
            utxo.height = coinbase_spend_height;
            utxo.from_coinbase = true;
            utxo
        })
        .expect("known_utxos should contain the outpoint");

    let expected_error =
        zebra_state::check::transparent_coinbase_spend(input_outpoint, spend_restriction, &utxo)
            .map_err(Box::new)
            .map_err(TransactionError::ValidateContextError)
            .expect_err("check should fail");

    tokio::spawn(async move {
        state
            .expect_request(zebra_state::Request::BestChainNextMedianTimePast)
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::BestChainNextMedianTimePast(
                DateTime32::MAX,
            ));

        state
            .expect_request(zebra_state::Request::UnspentBestChainUtxo(input_outpoint))
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::UnspentBestChainUtxo(
                known_utxos.get(&input_outpoint).map(|utxo| {
                    let mut utxo = utxo.utxo.clone();
                    utxo.height = coinbase_spend_height;
                    utxo.from_coinbase = true;
                    utxo
                }),
            ));

        state
            .expect_request_that(|req| {
                matches!(
                    req,
                    zebra_state::Request::CheckBestChainTipNullifiersAndAnchors(_)
                )
            })
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::ValidBestChainTipNullifiersAndAnchors);
    });

    let verifier_response = verifier
        .oneshot(Request::Mempool {
            transaction: tx.into(),
            height,
        })
        .await
        .expect_err("verification of transaction with immature spend should fail");

    assert_eq!(
        verifier_response, expected_error,
        "expected to fail verification, got: {verifier_response:?}"
    );
}

/// Tests that errors from the read state service are correctly converted into
/// transaction verifier errors.
#[tokio::test]
async fn state_error_converted_correctly() {
    use zebra_state::DuplicateNullifierError;

    let mut state: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let verifier = Verifier::new(&Network::Mainnet, state.clone());

    let height = NetworkUpgrade::Canopy
        .activation_height(&Network::Mainnet)
        .expect("Canopy activation height is specified");
    let fund_height = (height - 1).expect("fake source fund block height is too small");
    let (input, output, known_utxos) = mock_transparent_transfer(
        fund_height,
        true,
        0,
        Amount::try_from(10001).expect("invalid value"),
    );

    // Create a non-coinbase V4 tx with the last valid expiry height.
    let tx = Transaction::V4 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::unlocked(),
        expiry_height: height,
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let input_outpoint = match tx.inputs()[0] {
        transparent::Input::PrevOut { outpoint, .. } => outpoint,
        transparent::Input::Coinbase { .. } => panic!("requires a non-coinbase transaction"),
    };

    let make_validate_context_error =
        || sprout::Nullifier([0; 32].into()).duplicate_nullifier_error(true);

    tokio::spawn(async move {
        state
            .expect_request(zebra_state::Request::UnspentBestChainUtxo(input_outpoint))
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::UnspentBestChainUtxo(
                known_utxos
                    .get(&input_outpoint)
                    .map(|utxo| utxo.utxo.clone()),
            ));

        state
            .expect_request_that(|req| {
                matches!(
                    req,
                    zebra_state::Request::CheckBestChainTipNullifiersAndAnchors(_)
                )
            })
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(Err::<zebra_state::Response, zebra_state::BoxError>(
                make_validate_context_error().into(),
            ));
    });

    let verifier_response = verifier
        .oneshot(Request::Mempool {
            transaction: tx.into(),
            height,
        })
        .await;

    let transaction_error =
        verifier_response.expect_err("expected failed verification, got: {verifier_response:?}");

    assert_eq!(
        TransactionError::from(make_validate_context_error()),
        transaction_error,
        "expected matching state and transaction errors"
    );

    let state_error = zebra_state::BoxError::from(make_validate_context_error())
        .downcast::<ValidateContextError>()
        .map(|boxed| TransactionError::from(*boxed))
        .expect("downcast should succeed");

    assert_eq!(
        state_error, transaction_error,
        "expected matching state and transaction errors"
    );

    let TransactionError::ValidateContextError(propagated_validate_context_error) =
        transaction_error
    else {
        panic!("should be a ValidateContextError variant");
    };

    assert_eq!(
        *propagated_validate_context_error,
        make_validate_context_error(),
        "expected matching state and transaction errors"
    );
}

#[test]
fn v5_transaction_with_no_outputs_fails_validation() {
    let transaction = fake_v5_transactions_for_network(
        &Network::Mainnet,
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
        &Network::Mainnet,
        zebra_test::vectors::MAINNET_BLOCKS.iter(),
    )
    .rev()
    .find(|transaction| transaction.is_coinbase())
    .expect("At least one fake V5 coinbase transaction in the test vectors");

    insert_fake_orchard_shielded_data(&mut transaction);

    assert!(check::coinbase_tx_no_prevout_joinsplit_spend(&transaction).is_ok());
}

#[test]
fn v5_coinbase_transaction_with_enable_spends_flag_fails_validation() {
    let mut transaction = fake_v5_transactions_for_network(
        &Network::Mainnet,
        zebra_test::vectors::MAINNET_BLOCKS.iter(),
    )
    .rev()
    .find(|transaction| transaction.is_coinbase())
    .expect("At least one fake V5 coinbase transaction in the test vectors");

    let shielded_data = insert_fake_orchard_shielded_data(&mut transaction);

    shielded_data.action_groups.first_mut().flags = zebra_chain::orchard::Flags::ENABLE_SPENDS;

    assert_eq!(
        check::coinbase_tx_no_prevout_joinsplit_spend(&transaction),
        Err(TransactionError::CoinbaseHasEnableSpendsOrchard)
    );
}

#[tokio::test]
async fn v5_transaction_is_rejected_before_nu5_activation() {
    const V5_TRANSACTION_VERSION: u32 = 5;

    let canopy = NetworkUpgrade::Canopy;

    for network in Network::iter() {
        let state_service = service_fn(|_| async { unreachable!("Service should not be called") });
        let verifier = Verifier::new(&network, state_service);

        let transaction = fake_v5_transactions_for_network(&network, network.block_iter())
            .next_back()
            .expect("At least one fake V5 transaction in the test vectors");

        let result = verifier
            .oneshot(Request::Block {
                transaction: Arc::new(transaction),
                known_utxos: Arc::new(HashMap::new()),
                height: canopy
                    .activation_height(&network)
                    .expect("Canopy activation height is specified"),
                time: DateTime::<Utc>::MAX_UTC,
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

#[test]
fn v5_transaction_is_accepted_after_nu5_activation_mainnet() {
    v5_transaction_is_accepted_after_nu5_activation_for_network(Network::Mainnet)
}

#[test]
fn v5_transaction_is_accepted_after_nu5_activation_testnet() {
    v5_transaction_is_accepted_after_nu5_activation_for_network(Network::new_default_testnet())
}

fn v5_transaction_is_accepted_after_nu5_activation_for_network(network: Network) {
    let _init_guard = zebra_test::init();
    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        let nu5 = NetworkUpgrade::Nu5;
        let nu5_activation_height = nu5
            .activation_height(&network)
            .expect("NU5 activation height is specified");
        let blocks = network.block_iter();

        let state_service = service_fn(|_| async { unreachable!("Service should not be called") });
        let verifier = Verifier::new(&network, state_service);

        let mut transaction = fake_v5_transactions_for_network(&network, blocks)
            .next_back()
            .expect("At least one fake V5 transaction in the test vectors");
        if transaction
            .expiry_height()
            .expect("V5 must have expiry_height")
            < nu5_activation_height
        {
            let expiry_height = transaction.expiry_height_mut();
            *expiry_height = nu5_activation_height;
        }

        let expected_hash = transaction.unmined_id();
        let expiry_height = transaction
            .expiry_height()
            .expect("V5 must have expiry_height");

        let result = verifier
            .oneshot(Request::Block {
                transaction: Arc::new(transaction),
                known_utxos: Arc::new(HashMap::new()),
                height: expiry_height,
                time: DateTime::<Utc>::MAX_UTC,
            })
            .await;

        assert_eq!(
            result.expect("unexpected error response").tx_id(),
            expected_hash
        );
    })
}

/// Test if V4 transaction with transparent funds is accepted.
#[tokio::test]
async fn v4_transaction_with_transparent_transfer_is_accepted() {
    let network = Network::Mainnet;

    let canopy_activation_height = NetworkUpgrade::Canopy
        .activation_height(&network)
        .expect("Canopy activation height is specified");

    let transaction_block_height =
        (canopy_activation_height + 10).expect("transaction block height is too large");

    let fake_source_fund_height =
        (transaction_block_height - 1).expect("fake source fund block height is too small");

    // Create a fake transparent transfer that should succeed
    let (input, output, known_utxos) = mock_transparent_transfer(
        fake_source_fund_height,
        true,
        0,
        Amount::try_from(1).expect("invalid value"),
    );

    // Create a V4 transaction
    let transaction = Transaction::V4 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::Height(block::Height(0)),
        expiry_height: (transaction_block_height + 1).expect("expiry height is too large"),
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let transaction_hash = transaction.unmined_id();

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new(&network, state_service);

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(known_utxos),
            height: transaction_block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result.expect("unexpected error response").tx_id(),
        transaction_hash
    );
}

/// Tests if a non-coinbase V4 transaction with the last valid expiry height is
/// accepted.
#[tokio::test]
async fn v4_transaction_with_last_valid_expiry_height() {
    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new(&Network::Mainnet, state_service);

    let block_height = NetworkUpgrade::Canopy
        .activation_height(&Network::Mainnet)
        .expect("Canopy activation height is specified");
    let fund_height = (block_height - 1).expect("fake source fund block height is too small");
    let (input, output, known_utxos) = mock_transparent_transfer(
        fund_height,
        true,
        0,
        Amount::try_from(1).expect("invalid value"),
    );

    // Create a non-coinbase V4 tx with the last valid expiry height.
    let transaction = Transaction::V4 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::unlocked(),
        expiry_height: block_height,
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(known_utxos),
            height: block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result.expect("unexpected error response").tx_id(),
        transaction.unmined_id()
    );
}

/// Tests if a coinbase V4 transaction with an expiry height lower than the
/// block height is accepted.
///
/// Note that an expiry height lower than the block height is considered
/// *expired* for *non-coinbase* transactions.
#[tokio::test]
async fn v4_coinbase_transaction_with_low_expiry_height() {
    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new(&Network::Mainnet, state_service);

    let block_height = NetworkUpgrade::Canopy
        .activation_height(&Network::Mainnet)
        .expect("Canopy activation height is specified");

    let (input, output) = mock_coinbase_transparent_output(block_height);

    // This is a correct expiry height for coinbase V4 transactions.
    let expiry_height = (block_height - 1).expect("original block height is too small");

    // Create a coinbase V4 tx.
    let transaction = Transaction::V4 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::unlocked(),
        expiry_height,
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(HashMap::new()),
            height: block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result.expect("unexpected error response").tx_id(),
        transaction.unmined_id()
    );
}

/// Tests if an expired non-coinbase V4 transaction is rejected.
#[tokio::test]
async fn v4_transaction_with_too_low_expiry_height() {
    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new(&Network::Mainnet, state_service);

    let block_height = NetworkUpgrade::Canopy
        .activation_height(&Network::Mainnet)
        .expect("Canopy activation height is specified");

    let fund_height = (block_height - 1).expect("fake source fund block height is too small");
    let (input, output, known_utxos) = mock_transparent_transfer(
        fund_height,
        true,
        0,
        Amount::try_from(1).expect("invalid value"),
    );

    // This expiry height is too low so that the tx should seem expired to the verifier.
    let expiry_height = (block_height - 1).expect("original block height is too small");

    // Create a non-coinbase V4 tx.
    let transaction = Transaction::V4 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::unlocked(),
        expiry_height,
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(known_utxos),
            height: block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result,
        Err(TransactionError::ExpiredTransaction {
            expiry_height,
            block_height,
            transaction_hash: transaction.hash(),
        })
    );
}

/// Tests if a non-coinbase V4 transaction with an expiry height exceeding the
/// maximum is rejected.
#[tokio::test]
async fn v4_transaction_with_exceeding_expiry_height() {
    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new(&Network::Mainnet, state_service);

    let block_height = block::Height::MAX;

    let fund_height = (block_height - 1).expect("fake source fund block height is too small");
    let (input, output, known_utxos) = mock_transparent_transfer(
        fund_height,
        true,
        0,
        Amount::try_from(1).expect("invalid value"),
    );

    // This expiry height exceeds the maximum defined by the specification.
    let expiry_height = block::Height(500_000_000);

    // Create a non-coinbase V4 tx.
    let transaction = Transaction::V4 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::unlocked(),
        expiry_height,
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(known_utxos),
            height: block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result,
        Err(TransactionError::MaximumExpiryHeight {
            expiry_height,
            is_coinbase: false,
            block_height,
            transaction_hash: transaction.hash(),
        })
    );
}

/// Tests if a coinbase V4 transaction with an expiry height exceeding the
/// maximum is rejected.
#[tokio::test]
async fn v4_coinbase_transaction_with_exceeding_expiry_height() {
    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new(&Network::Mainnet, state_service);

    // Use an arbitrary pre-NU5 block height.
    // It can't be NU5-onward because the expiry height limit is not enforced
    // for coinbase transactions (it needs to match the block height instead),
    // which is what is used in this test.
    let block_height = (NetworkUpgrade::Nu5
        .activation_height(&Network::Mainnet)
        .expect("NU5 height must be set")
        - 1)
    .expect("will not underflow");

    let (input, output) = mock_coinbase_transparent_output(block_height);

    // This expiry height exceeds the maximum defined by the specification.
    let expiry_height = block::Height(500_000_000);

    // Create a coinbase V4 tx.
    let transaction = Transaction::V4 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::unlocked(),
        expiry_height,
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(HashMap::new()),
            height: block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result,
        Err(TransactionError::MaximumExpiryHeight {
            expiry_height,
            is_coinbase: true,
            block_height,
            transaction_hash: transaction.hash(),
        })
    );
}

/// Test if V4 coinbase transaction is accepted.
#[tokio::test]
async fn v4_coinbase_transaction_is_accepted() {
    let network = Network::Mainnet;

    let canopy_activation_height = NetworkUpgrade::Canopy
        .activation_height(&network)
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

    let transaction_hash = transaction.unmined_id();

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new(&network, state_service);

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(HashMap::new()),
            height: transaction_block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result.expect("unexpected error response").tx_id(),
        transaction_hash
    );
}

/// Test if V4 transaction with transparent funds is rejected if the source script prevents it.
///
/// This test simulates the case where the script verifier rejects the transaction because the
/// script prevents spending the source UTXO.
#[tokio::test]
async fn v4_transaction_with_transparent_transfer_is_rejected_by_the_script() {
    let network = Network::Mainnet;

    let canopy_activation_height = NetworkUpgrade::Canopy
        .activation_height(&network)
        .expect("Canopy activation height is specified");

    let transaction_block_height =
        (canopy_activation_height + 10).expect("transaction block height is too large");

    let fake_source_fund_height =
        (transaction_block_height - 1).expect("fake source fund block height is too small");

    // Create a fake transparent transfer that should not succeed
    let (input, output, known_utxos) = mock_transparent_transfer(
        fake_source_fund_height,
        false,
        0,
        Amount::try_from(1).expect("invalid value"),
    );

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
    let verifier = Verifier::new(&network, state_service);

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(known_utxos),
            height: transaction_block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result,
        Err(TransactionError::InternalDowncastError(
            "downcast to known transaction error type failed, original error: ScriptInvalid"
                .to_string()
        ))
    );
}

/// Test if V4 transaction with an internal double spend of transparent funds is rejected.
#[tokio::test]
async fn v4_transaction_with_conflicting_transparent_spend_is_rejected() {
    let network = Network::Mainnet;

    let canopy_activation_height = NetworkUpgrade::Canopy
        .activation_height(&network)
        .expect("Canopy activation height is specified");

    let transaction_block_height =
        (canopy_activation_height + 10).expect("transaction block height is too large");

    let fake_source_fund_height =
        (transaction_block_height - 1).expect("fake source fund block height is too small");

    // Create a fake transparent transfer that should succeed
    let (input, output, known_utxos) = mock_transparent_transfer(
        fake_source_fund_height,
        true,
        0,
        Amount::try_from(1).expect("invalid value"),
    );

    // Create a V4 transaction
    let transaction = Transaction::V4 {
        inputs: vec![input.clone(), input.clone()],
        outputs: vec![output],
        lock_time: LockTime::Height(block::Height(0)),
        expiry_height: (transaction_block_height + 1).expect("expiry height is too large"),
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new(&network, state_service);

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(known_utxos),
            height: transaction_block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    let expected_outpoint = input.outpoint().expect("Input should have an outpoint");

    assert_eq!(
        result,
        Err(TransactionError::DuplicateTransparentSpend(
            expected_outpoint
        ))
    );
}

/// Test if V4 transaction with a joinsplit that has duplicate nullifiers is rejected.
#[test]
fn v4_transaction_with_conflicting_sprout_nullifier_inside_joinsplit_is_rejected() {
    let _init_guard = zebra_test::init();
    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        let network = Network::Mainnet;
        let network_upgrade = NetworkUpgrade::Canopy;

        let canopy_activation_height = NetworkUpgrade::Canopy
            .activation_height(&network)
            .expect("Canopy activation height is specified");

        let transaction_block_height =
            (canopy_activation_height + 10).expect("transaction block height is too large");

        // Create a fake Sprout join split
        let (mut joinsplit_data, signing_key) = mock_sprout_join_split_data();

        // Make both nullifiers the same inside the joinsplit transaction
        let duplicate_nullifier = joinsplit_data.first.nullifiers[0];
        joinsplit_data.first.nullifiers[1] = duplicate_nullifier;

        // Create a V4 transaction
        let mut transaction = Transaction::V4 {
            inputs: vec![],
            outputs: vec![],
            lock_time: LockTime::Height(block::Height(0)),
            expiry_height: (transaction_block_height + 1).expect("expiry height is too large"),
            joinsplit_data: Some(joinsplit_data),
            sapling_shielded_data: None,
        };

        // Sign the transaction
        let sighash = transaction.sighash(
            network_upgrade.branch_id().expect("must have branch ID"),
            HashType::ALL,
            &[],
            None,
        );

        match &mut transaction {
            Transaction::V4 {
                joinsplit_data: Some(joinsplit_data),
                ..
            } => joinsplit_data.sig = signing_key.sign(sighash.as_ref()),
            _ => unreachable!("Mock transaction was created incorrectly"),
        }

        let state_service =
            service_fn(|_| async { unreachable!("State service should not be called") });
        let verifier = Verifier::new(&network, state_service);

        let result = verifier
            .oneshot(Request::Block {
                transaction: Arc::new(transaction),
                known_utxos: Arc::new(HashMap::new()),
                height: transaction_block_height,
                time: DateTime::<Utc>::MAX_UTC,
            })
            .await;

        assert_eq!(
            result,
            Err(TransactionError::DuplicateSproutNullifier(
                duplicate_nullifier
            ))
        );
    });
}

/// Test if V4 transaction with duplicate nullifiers across joinsplits is rejected.
#[test]
fn v4_transaction_with_conflicting_sprout_nullifier_across_joinsplits_is_rejected() {
    let _init_guard = zebra_test::init();
    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        let network = Network::Mainnet;
        let network_upgrade = NetworkUpgrade::Canopy;

        let canopy_activation_height = NetworkUpgrade::Canopy
            .activation_height(&network)
            .expect("Canopy activation height is specified");

        let transaction_block_height =
            (canopy_activation_height + 10).expect("transaction block height is too large");

        // Create a fake Sprout join split
        let (mut joinsplit_data, signing_key) = mock_sprout_join_split_data();

        // Duplicate a nullifier from the created joinsplit
        let duplicate_nullifier = joinsplit_data.first.nullifiers[1];

        // Add a new joinsplit with the duplicate nullifier
        let mut new_joinsplit = joinsplit_data.first.clone();
        new_joinsplit.nullifiers[0] = duplicate_nullifier;
        new_joinsplit.nullifiers[1] = sprout::note::Nullifier([2u8; 32].into());

        joinsplit_data.rest.push(new_joinsplit);

        // Create a V4 transaction
        let mut transaction = Transaction::V4 {
            inputs: vec![],
            outputs: vec![],
            lock_time: LockTime::Height(block::Height(0)),
            expiry_height: (transaction_block_height + 1).expect("expiry height is too large"),
            joinsplit_data: Some(joinsplit_data),
            sapling_shielded_data: None,
        };

        // Sign the transaction
        let sighash = transaction.sighash(
            network_upgrade.branch_id().expect("must have branch ID"),
            HashType::ALL,
            &[],
            None,
        );

        match &mut transaction {
            Transaction::V4 {
                joinsplit_data: Some(joinsplit_data),
                ..
            } => joinsplit_data.sig = signing_key.sign(sighash.as_ref()),
            _ => unreachable!("Mock transaction was created incorrectly"),
        }

        let state_service =
            service_fn(|_| async { unreachable!("State service should not be called") });
        let verifier = Verifier::new(&network, state_service);

        let result = verifier
            .oneshot(Request::Block {
                transaction: Arc::new(transaction),
                known_utxos: Arc::new(HashMap::new()),
                height: transaction_block_height,
                time: DateTime::<Utc>::MAX_UTC,
            })
            .await;

        assert_eq!(
            result,
            Err(TransactionError::DuplicateSproutNullifier(
                duplicate_nullifier
            ))
        );
    });
}

/// Test if V5 transaction with transparent funds is accepted.
#[tokio::test]
async fn v5_transaction_with_transparent_transfer_is_accepted() {
    let network = Network::new_default_testnet();
    let network_upgrade = NetworkUpgrade::Nu5;

    let nu5_activation_height = network_upgrade
        .activation_height(&network)
        .expect("NU5 activation height is specified");

    let transaction_block_height =
        (nu5_activation_height + 10).expect("transaction block height is too large");

    let fake_source_fund_height =
        (transaction_block_height - 1).expect("fake source fund block height is too small");

    // Create a fake transparent transfer that should succeed
    let (input, output, known_utxos) = mock_transparent_transfer(
        fake_source_fund_height,
        true,
        0,
        Amount::try_from(1).expect("invalid value"),
    );

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

    let transaction_hash = transaction.unmined_id();

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new(&network, state_service);

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(known_utxos),
            height: transaction_block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result.expect("unexpected error response").tx_id(),
        transaction_hash
    );
}

/// Tests if a non-coinbase V5 transaction with the last valid expiry height is
/// accepted.
#[tokio::test]
async fn v5_transaction_with_last_valid_expiry_height() {
    let network = Network::new_default_testnet();
    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new(&network, state_service);

    let block_height = NetworkUpgrade::Nu5
        .activation_height(&network)
        .expect("Nu5 activation height for testnet is specified");
    let fund_height = (block_height - 1).expect("fake source fund block height is too small");
    let (input, output, known_utxos) = mock_transparent_transfer(
        fund_height,
        true,
        0,
        Amount::try_from(1).expect("invalid value"),
    );

    // Create a non-coinbase V5 tx with the last valid expiry height.
    let transaction = Transaction::V5 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::unlocked(),
        expiry_height: block_height,
        sapling_shielded_data: None,
        orchard_shielded_data: None,
        network_upgrade: NetworkUpgrade::Nu5,
    };

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(known_utxos),
            height: block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result.expect("unexpected error response").tx_id(),
        transaction.unmined_id()
    );
}

/// Tests that a coinbase V5 transaction is accepted only if its expiry height
/// is equal to the height of the block the transaction belongs to.
#[tokio::test]
async fn v5_coinbase_transaction_expiry_height() {
    let network = Network::new_default_testnet();
    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new(&network, state_service);

    let block_height = NetworkUpgrade::Nu5
        .activation_height(&network)
        .expect("Nu5 activation height for testnet is specified");

    let (input, output) = mock_coinbase_transparent_output(block_height);

    // Create a coinbase V5 tx with an expiry height that matches the height of
    // the block. Note that this is the only valid expiry height for a V5
    // coinbase tx.
    let transaction = Transaction::V5 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::unlocked(),
        expiry_height: block_height,
        sapling_shielded_data: None,
        orchard_shielded_data: None,
        network_upgrade: NetworkUpgrade::Nu5,
    };

    let result = verifier
        .clone()
        .oneshot(Request::Block {
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(HashMap::new()),
            height: block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result.expect("unexpected error response").tx_id(),
        transaction.unmined_id()
    );

    // Increment the expiry height so that it becomes invalid.
    let new_expiry_height = (block_height + 1).expect("transaction block height is too large");
    let mut new_transaction = transaction.clone();

    *new_transaction.expiry_height_mut() = new_expiry_height;

    let result = verifier
        .clone()
        .oneshot(Request::Block {
            transaction: Arc::new(new_transaction.clone()),
            known_utxos: Arc::new(HashMap::new()),
            height: block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result,
        Err(TransactionError::CoinbaseExpiryBlockHeight {
            expiry_height: Some(new_expiry_height),
            block_height,
            transaction_hash: new_transaction.hash(),
        })
    );

    // Decrement the expiry height so that it becomes invalid.
    let new_expiry_height = (block_height - 1).expect("transaction block height is too low");
    let mut new_transaction = transaction.clone();

    *new_transaction.expiry_height_mut() = new_expiry_height;

    let result = verifier
        .clone()
        .oneshot(Request::Block {
            transaction: Arc::new(new_transaction.clone()),
            known_utxos: Arc::new(HashMap::new()),
            height: block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result,
        Err(TransactionError::CoinbaseExpiryBlockHeight {
            expiry_height: Some(new_expiry_height),
            block_height,
            transaction_hash: new_transaction.hash(),
        })
    );

    // Test with matching heights again, but using a very high value
    // that is greater than the limit for non-coinbase transactions,
    // to ensure the limit is not being enforced for coinbase transactions.
    let new_expiry_height = Height::MAX;
    let mut new_transaction = transaction.clone();

    *new_transaction.expiry_height_mut() = new_expiry_height;

    let result = verifier
        .clone()
        .oneshot(Request::Block {
            transaction: Arc::new(new_transaction.clone()),
            known_utxos: Arc::new(HashMap::new()),
            height: new_expiry_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result.expect("unexpected error response").tx_id(),
        new_transaction.unmined_id()
    );
}

/// Tests if an expired non-coinbase V5 transaction is rejected.
#[tokio::test]
async fn v5_transaction_with_too_low_expiry_height() {
    let network = Network::new_default_testnet();

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new(&network, state_service);

    let block_height = NetworkUpgrade::Nu5
        .activation_height(&network)
        .expect("Nu5 activation height for testnet is specified");
    let fund_height = (block_height - 1).expect("fake source fund block height is too small");
    let (input, output, known_utxos) = mock_transparent_transfer(
        fund_height,
        true,
        0,
        Amount::try_from(1).expect("invalid value"),
    );

    // This expiry height is too low so that the tx should seem expired to the verifier.
    let expiry_height = (block_height - 1).expect("original block height is too small");

    // Create a non-coinbase V5 tx.
    let transaction = Transaction::V5 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::unlocked(),
        expiry_height,
        sapling_shielded_data: None,
        orchard_shielded_data: None,
        network_upgrade: NetworkUpgrade::Nu5,
    };

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(known_utxos),
            height: block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result,
        Err(TransactionError::ExpiredTransaction {
            expiry_height,
            block_height,
            transaction_hash: transaction.hash(),
        })
    );
}

/// Tests if a non-coinbase V5 transaction with an expiry height exceeding the
/// maximum is rejected.
#[tokio::test]
async fn v5_transaction_with_exceeding_expiry_height() {
    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new(&Network::Mainnet, state_service);

    let block_height = block::Height::MAX;

    let fund_height = (block_height - 1).expect("fake source fund block height is too small");
    let (input, output, known_utxos) = mock_transparent_transfer(
        fund_height,
        true,
        0,
        Amount::try_from(1).expect("invalid value"),
    );

    // This expiry height exceeds the maximum defined by the specification.
    let expiry_height = block::Height(500_000_000);

    // Create a non-coinbase V5 tx.
    let transaction = Transaction::V5 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::unlocked(),
        expiry_height,
        sapling_shielded_data: None,
        orchard_shielded_data: None,
        network_upgrade: NetworkUpgrade::Nu5,
    };

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(known_utxos),
            height: block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result,
        Err(TransactionError::MaximumExpiryHeight {
            expiry_height,
            is_coinbase: false,
            block_height,
            transaction_hash: transaction.hash(),
        })
    );
}

/// Test if V5 coinbase transaction is accepted.
#[tokio::test]
async fn v5_coinbase_transaction_is_accepted() {
    let network = Network::new_default_testnet();
    let network_upgrade = NetworkUpgrade::Nu5;

    let nu5_activation_height = network_upgrade
        .activation_height(&network)
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

    let transaction_hash = transaction.unmined_id();

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new(&network, state_service);

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(known_utxos),
            height: transaction_block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result.expect("unexpected error response").tx_id(),
        transaction_hash
    );
}

/// Test if V5 transaction with transparent funds is rejected if the source script prevents it.
///
/// This test simulates the case where the script verifier rejects the transaction because the
/// script prevents spending the source UTXO.
#[tokio::test]
async fn v5_transaction_with_transparent_transfer_is_rejected_by_the_script() {
    let network = Network::new_default_testnet();
    let network_upgrade = NetworkUpgrade::Nu5;

    let nu5_activation_height = network_upgrade
        .activation_height(&network)
        .expect("NU5 activation height is specified");

    let transaction_block_height =
        (nu5_activation_height + 10).expect("transaction block height is too large");

    let fake_source_fund_height =
        (transaction_block_height - 1).expect("fake source fund block height is too small");

    // Create a fake transparent transfer that should not succeed
    let (input, output, known_utxos) = mock_transparent_transfer(
        fake_source_fund_height,
        false,
        0,
        Amount::try_from(1).expect("invalid value"),
    );

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
    let verifier = Verifier::new(&network, state_service);

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(known_utxos),
            height: transaction_block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result,
        Err(TransactionError::InternalDowncastError(
            "downcast to known transaction error type failed, original error: ScriptInvalid"
                .to_string()
        ))
    );
}

/// Test if V5 transaction with an internal double spend of transparent funds is rejected.
#[tokio::test]
async fn v5_transaction_with_conflicting_transparent_spend_is_rejected() {
    let network = Network::Mainnet;
    let network_upgrade = NetworkUpgrade::Nu5;

    let canopy_activation_height = NetworkUpgrade::Canopy
        .activation_height(&network)
        .expect("Canopy activation height is specified");

    let transaction_block_height =
        (canopy_activation_height + 10).expect("transaction block height is too large");

    let fake_source_fund_height =
        (transaction_block_height - 1).expect("fake source fund block height is too small");

    // Create a fake transparent transfer that should succeed
    let (input, output, known_utxos) = mock_transparent_transfer(
        fake_source_fund_height,
        true,
        0,
        Amount::try_from(1).expect("invalid value"),
    );

    // Create a V4 transaction
    let transaction = Transaction::V5 {
        inputs: vec![input.clone(), input.clone()],
        outputs: vec![output],
        lock_time: LockTime::Height(block::Height(0)),
        expiry_height: (transaction_block_height + 1).expect("expiry height is too large"),
        sapling_shielded_data: None,
        orchard_shielded_data: None,
        network_upgrade,
    };

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new(&network, state_service);

    let result = verifier
        .oneshot(Request::Block {
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(known_utxos),
            height: transaction_block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    let expected_outpoint = input.outpoint().expect("Input should have an outpoint");

    assert_eq!(
        result,
        Err(TransactionError::DuplicateTransparentSpend(
            expected_outpoint
        ))
    );
}

/// Test if signed V4 transaction with a dummy [`sprout::JoinSplit`] is accepted.
///
/// This test verifies if the transaction verifier correctly accepts a signed transaction.
#[test]
fn v4_with_signed_sprout_transfer_is_accepted() {
    let _init_guard = zebra_test::init();
    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        let network = Network::Mainnet;

        let (height, transaction) = test_transactions(&network)
            .rev()
            .filter(|(_, transaction)| {
                !transaction.is_coinbase() && transaction.inputs().is_empty()
            })
            .find(|(_, transaction)| transaction.sprout_groth16_joinsplits().next().is_some())
            .expect("No transaction found with Groth16 JoinSplits");

        let expected_hash = transaction.unmined_id();

        // Initialize the verifier
        let state_service =
            service_fn(|_| async { unreachable!("State service should not be called") });
        let verifier = Verifier::new(&network, state_service);

        // Test the transaction verifier
        let result = verifier
            .clone()
            .oneshot(Request::Block {
                transaction,
                known_utxos: Arc::new(HashMap::new()),
                height,
                time: DateTime::<Utc>::MAX_UTC,
            })
            .await;

        assert_eq!(
            result.expect("unexpected error response").tx_id(),
            expected_hash
        );
    })
}

/// Test if an V4 transaction with a modified [`sprout::JoinSplit`] is rejected.
///
/// This test verifies if the transaction verifier correctly rejects the transaction because of the
/// invalid JoinSplit.
#[test]
fn v4_with_modified_joinsplit_is_rejected() {
    let _init_guard = zebra_test::init();
    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        v4_with_joinsplit_is_rejected_for_modification(
            JoinSplitModification::CorruptSignature,
            // TODO: Fix error downcast
            // Err(TransactionError::Ed25519(ed25519::Error::InvalidSignature))
            TransactionError::InternalDowncastError(
                "downcast to known transaction error type failed, original error: InvalidSignature"
                    .to_string(),
            ),
        )
        .await;

        v4_with_joinsplit_is_rejected_for_modification(
            JoinSplitModification::CorruptProof,
            TransactionError::Groth16("proof verification failed".to_string()),
        )
        .await;

        v4_with_joinsplit_is_rejected_for_modification(
            JoinSplitModification::ZeroProof,
            TransactionError::MalformedGroth16("invalid G1".to_string()),
        )
        .await;
    })
}

async fn v4_with_joinsplit_is_rejected_for_modification(
    modification: JoinSplitModification,
    expected_error: TransactionError,
) {
    let network = Network::Mainnet;

    let (height, mut transaction) = test_transactions(&network)
        .rev()
        .filter(|(_, transaction)| !transaction.is_coinbase() && transaction.inputs().is_empty())
        .find(|(_, transaction)| transaction.sprout_groth16_joinsplits().next().is_some())
        .expect("There should be a tx with Groth16 JoinSplits.");

    let expected_error = Err(expected_error);

    // Modify a JoinSplit in the transaction following the given modification type.
    let tx = Arc::get_mut(&mut transaction).expect("The tx should have only one active reference.");
    match tx {
        Transaction::V4 {
            joinsplit_data: Some(ref mut joinsplit_data),
            ..
        } => modify_joinsplit_data(joinsplit_data, modification),
        _ => unreachable!("Transaction should have some JoinSplit shielded data."),
    }

    // Initialize the verifier
    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called.") });
    let verifier = Verifier::new(&network, state_service);

    // Test the transaction verifier.
    //
    // Note that modifying the JoinSplit data invalidates the tx signatures. The signatures are
    // checked concurrently with the ZK proofs, and when a signature check finishes before the proof
    // check, the verifier reports an invalid signature instead of invalid proof. This race
    // condition happens only occasionally, so we run the verifier in a loop with a small iteration
    // threshold until it returns the correct error.
    let mut i = 1;
    let result = loop {
        let result = verifier
            .clone()
            .oneshot(Request::Block {
                transaction: transaction.clone(),
                known_utxos: Arc::new(HashMap::new()),
                height,
                time: DateTime::<Utc>::MAX_UTC,
            })
            .await;

        if result == expected_error || i >= 100 {
            break result;
        }

        i += 1;
    };

    assert_eq!(result, expected_error);
}

/// Test if a V4 transaction with Sapling spends is accepted by the verifier.
#[test]
fn v4_with_sapling_spends() {
    let _init_guard = zebra_test::init();
    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        let network = Network::Mainnet;

        let (height, transaction) = test_transactions(&network)
            .rev()
            .filter(|(_, transaction)| {
                !transaction.is_coinbase() && transaction.inputs().is_empty()
            })
            .find(|(_, transaction)| transaction.sapling_spends_per_anchor().next().is_some())
            .expect("No transaction found with Sapling spends");

        let expected_hash = transaction.unmined_id();

        // Initialize the verifier
        let state_service =
            service_fn(|_| async { unreachable!("State service should not be called") });
        let verifier = Verifier::new(&network, state_service);

        // Test the transaction verifier
        let result = verifier
            .clone()
            .oneshot(Request::Block {
                transaction,
                known_utxos: Arc::new(HashMap::new()),
                height,
                time: DateTime::<Utc>::MAX_UTC,
            })
            .await;

        assert_eq!(
            result.expect("unexpected error response").tx_id(),
            expected_hash
        );
    });
}

/// Test if a V4 transaction with a duplicate Sapling spend is rejected by the verifier.
#[test]
fn v4_with_duplicate_sapling_spends() {
    let _init_guard = zebra_test::init();
    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        let network = Network::Mainnet;

        let (height, mut transaction) = test_transactions(&network)
            .rev()
            .filter(|(_, transaction)| {
                !transaction.is_coinbase() && transaction.inputs().is_empty()
            })
            .find(|(_, transaction)| transaction.sapling_spends_per_anchor().next().is_some())
            .expect("No transaction found with Sapling spends");

        // Duplicate one of the spends
        let duplicate_nullifier = duplicate_sapling_spend(
            Arc::get_mut(&mut transaction).expect("Transaction only has one active reference"),
        );

        // Initialize the verifier
        let state_service =
            service_fn(|_| async { unreachable!("State service should not be called") });
        let verifier = Verifier::new(&network, state_service);

        // Test the transaction verifier
        let result = verifier
            .clone()
            .oneshot(Request::Block {
                transaction,
                known_utxos: Arc::new(HashMap::new()),
                height,
                time: DateTime::<Utc>::MAX_UTC,
            })
            .await;

        assert_eq!(
            result,
            Err(TransactionError::DuplicateSaplingNullifier(
                duplicate_nullifier
            ))
        );
    });
}

/// Test if a V4 transaction with Sapling outputs but no spends is accepted by the verifier.
#[test]
fn v4_with_sapling_outputs_and_no_spends() {
    let _init_guard = zebra_test::init();
    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        let network = Network::Mainnet;

        let (height, transaction) = test_transactions(&network)
            .rev()
            .filter(|(_, transaction)| {
                !transaction.is_coinbase() && transaction.inputs().is_empty()
            })
            .find(|(_, transaction)| {
                transaction.sapling_spends_per_anchor().next().is_none()
                    && transaction.sapling_outputs().next().is_some()
            })
            .expect("No transaction found with Sapling outputs and no Sapling spends");

        let expected_hash = transaction.unmined_id();

        // Initialize the verifier
        let state_service =
            service_fn(|_| async { unreachable!("State service should not be called") });
        let verifier = Verifier::new(&network, state_service);

        // Test the transaction verifier
        let result = verifier
            .clone()
            .oneshot(Request::Block {
                transaction,
                known_utxos: Arc::new(HashMap::new()),
                height,
                time: DateTime::<Utc>::MAX_UTC,
            })
            .await;

        assert_eq!(
            result.expect("unexpected error response").tx_id(),
            expected_hash
        );
    })
}

/// Test if a V5 transaction with Sapling spends is accepted by the verifier.
#[test]
// TODO: add NU5 mainnet test vectors with Sapling spends, then remove should_panic
#[should_panic]
fn v5_with_sapling_spends() {
    let _init_guard = zebra_test::init();
    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        let network = Network::Mainnet;
        let nu5_activation = NetworkUpgrade::Nu5.activation_height(&network);

        let transaction =
            fake_v5_transactions_for_network(&network, zebra_test::vectors::MAINNET_BLOCKS.iter())
                .rev()
                .filter(|transaction| {
                    !transaction.is_coinbase()
                        && transaction.inputs().is_empty()
                        && transaction.expiry_height() >= nu5_activation
                })
                .find(|transaction| transaction.sapling_spends_per_anchor().next().is_some())
                .expect("No transaction found with Sapling spends");

        let expected_hash = transaction.unmined_id();
        let height = transaction
            .expiry_height()
            .expect("Transaction is missing expiry height");

        // Initialize the verifier
        let state_service =
            service_fn(|_| async { unreachable!("State service should not be called") });
        let verifier = Verifier::new(&network, state_service);

        // Test the transaction verifier
        let result = verifier
            .clone()
            .oneshot(Request::Block {
                transaction: Arc::new(transaction),
                known_utxos: Arc::new(HashMap::new()),
                height,
                time: DateTime::<Utc>::MAX_UTC,
            })
            .await;

        assert_eq!(
            result.expect("unexpected error response").tx_id(),
            expected_hash
        );
    });
}

/// Test if a V5 transaction with a duplicate Sapling spend is rejected by the verifier.
#[test]
fn v5_with_duplicate_sapling_spends() {
    let _init_guard = zebra_test::init();
    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        let network = Network::Mainnet;

        let mut transaction =
            fake_v5_transactions_for_network(&network, zebra_test::vectors::MAINNET_BLOCKS.iter())
                .rev()
                .filter(|transaction| !transaction.is_coinbase() && transaction.inputs().is_empty())
                .find(|transaction| transaction.sapling_spends_per_anchor().next().is_some())
                .expect("No transaction found with Sapling spends");

        let height = transaction
            .expiry_height()
            .expect("Transaction is missing expiry height");

        // Duplicate one of the spends
        let duplicate_nullifier = duplicate_sapling_spend(&mut transaction);

        // Initialize the verifier
        let state_service =
            service_fn(|_| async { unreachable!("State service should not be called") });
        let verifier = Verifier::new(&network, state_service);

        // Test the transaction verifier
        let result = verifier
            .clone()
            .oneshot(Request::Block {
                transaction: Arc::new(transaction),
                known_utxos: Arc::new(HashMap::new()),
                height,
                time: DateTime::<Utc>::MAX_UTC,
            })
            .await;

        assert_eq!(
            result,
            Err(TransactionError::DuplicateSaplingNullifier(
                duplicate_nullifier
            ))
        );
    });
}

/// Test if a V5 transaction with a duplicate Orchard action is rejected by the verifier.
#[test]
fn v5_with_duplicate_orchard_action() {
    let _init_guard = zebra_test::init();
    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        let network = Network::Mainnet;

        // Find a transaction with no inputs or outputs to use as base
        let mut transaction =
            fake_v5_transactions_for_network(&network, zebra_test::vectors::MAINNET_BLOCKS.iter())
                .rev()
                .find(|transaction| {
                    transaction.inputs().is_empty()
                        && transaction.outputs().is_empty()
                        && transaction.sapling_spends_per_anchor().next().is_none()
                        && transaction.sapling_outputs().next().is_none()
                        && transaction.joinsplit_count() == 0
                })
                .expect("At least one fake V5 transaction with no inputs and no outputs");

        let height = transaction
            .expiry_height()
            .expect("Transaction is missing expiry height");

        // Insert fake Orchard shielded data to the transaction, which has at least one action (this is
        // guaranteed structurally by `orchard::ShieldedData`)
        let shielded_data = insert_fake_orchard_shielded_data(&mut transaction);

        // Enable spends
        shielded_data.action_groups.first_mut().flags = zebra_chain::orchard::Flags::ENABLE_SPENDS
            | zebra_chain::orchard::Flags::ENABLE_OUTPUTS;

        // Duplicate the first action
        let duplicate_action = shielded_data.action_groups.first().actions.first().clone();
        let duplicate_nullifier = duplicate_action.action.nullifier;

        shielded_data
            .action_groups
            .first_mut()
            .actions
            .push(duplicate_action);

        // Initialize the verifier
        let state_service =
            service_fn(|_| async { unreachable!("State service should not be called") });
        let verifier = Verifier::new(&network, state_service);

        // Test the transaction verifier
        let result = verifier
            .clone()
            .oneshot(Request::Block {
                transaction: Arc::new(transaction),
                known_utxos: Arc::new(HashMap::new()),
                height,
                time: DateTime::<Utc>::MAX_UTC,
            })
            .await;

        assert_eq!(
            result,
            Err(TransactionError::DuplicateOrchardNullifier(
                duplicate_nullifier
            ))
        );
    });
}

// Utility functions

/// Create a mock transparent transfer to be included in a transaction.
///
/// First, this creates a fake unspent transaction output from a fake transaction included in the
/// specified `previous_utxo_height` block height. This fake [`Utxo`] also contains a simple script
/// that can either accept or reject any spend attempt, depending on if `script_should_succeed` is
/// `true` or `false`. Since the `tx_index_in_block` is irrelevant for blocks that have already
/// been verified, it is set to `1`.
///
/// Then, a [`transparent::Input::PrevOut`] is created that attempts to spend the previously created fake
/// UTXO to a new [`transparent::Output`].
///
/// Finally, the initial fake UTXO is placed in a `known_utxos` [`HashMap`] so that it can be
/// retrieved during verification.
///
/// The function then returns the generated transparent input and output, as well as the
/// `known_utxos` map.
///
/// Note: `known_utxos` is only intended to be used for UTXOs within the same block,
/// so future verification changes might break this mocking function.
fn mock_transparent_transfer(
    previous_utxo_height: block::Height,
    script_should_succeed: bool,
    outpoint_index: u32,
    previous_output_value: Amount<NonNegative>,
) -> (
    transparent::Input,
    transparent::Output,
    HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
) {
    // A script with a single opcode that accepts the transaction (pushes true on the stack)
    let accepting_script = transparent::Script::new(&[1, 1]);
    // A script with a single opcode that rejects the transaction (OP_FALSE)
    let rejecting_script = transparent::Script::new(&[0]);

    // Mock an unspent transaction output
    let previous_outpoint = transparent::OutPoint {
        hash: Hash([1u8; 32]),
        index: outpoint_index,
    };

    let lock_script = if script_should_succeed {
        accepting_script.clone()
    } else {
        rejecting_script.clone()
    };

    let previous_output = transparent::Output {
        value: previous_output_value,
        lock_script,
    };

    let previous_utxo = transparent::OrderedUtxo::new(previous_output, previous_utxo_height, 1);

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
    let first_nullifier = sprout::note::Nullifier([0u8; 32].into());
    let second_nullifier = sprout::note::Nullifier([1u8; 32].into());
    let commitment = sprout::commitment::NoteCommitment::from([0u8; 32]);
    let ephemeral_key =
        x25519::PublicKey::from(&x25519::EphemeralSecret::random_from_rng(rand::thread_rng()));
    let random_seed = sprout::RandomSeed::from([0u8; 32]);
    let mac = sprout::note::Mac::zcash_deserialize(&[0u8; 32][..])
        .expect("Failure to deserialize dummy MAC");
    let zkproof = Groth16Proof([0u8; 192]);
    let encrypted_note = sprout::note::EncryptedNote([0u8; 601]);

    // Create an dummy join split
    let joinsplit = sprout::JoinSplit {
        vpub_old: zero_amount,
        vpub_new: zero_amount,
        anchor,
        nullifiers: [first_nullifier, second_nullifier],
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

/// A type of JoinSplit modification to test.
#[derive(Clone, Copy)]
enum JoinSplitModification {
    // Corrupt a signature, making it invalid.
    CorruptSignature,
    // Corrupt a proof, making it invalid, but still well-formed.
    CorruptProof,
    // Make a proof all-zeroes, making it malformed.
    ZeroProof,
}

/// Modify a [`JoinSplitData`] following the given modification type.
fn modify_joinsplit_data(
    joinsplit_data: &mut JoinSplitData<Groth16Proof>,
    modification: JoinSplitModification,
) {
    match modification {
        JoinSplitModification::CorruptSignature => {
            let mut sig_bytes: [u8; 64] = joinsplit_data.sig.into();
            // Flip a bit from an arbitrary byte of the signature.
            sig_bytes[10] ^= 0x01;
            joinsplit_data.sig = sig_bytes.into();
        }
        JoinSplitModification::CorruptProof => {
            let joinsplit = joinsplit_data
                .joinsplits_mut()
                .next()
                .expect("must have a JoinSplit");
            {
                // A proof is composed of three field elements, the first and last having 48 bytes.
                // (The middle one has 96 bytes.) To corrupt the proof without making it malformed,
                // simply swap those first and last elements.
                let (first, rest) = joinsplit.zkproof.0.split_at_mut(48);
                first.swap_with_slice(&mut rest[96..144]);
            }
        }
        JoinSplitModification::ZeroProof => {
            let joinsplit = joinsplit_data
                .joinsplits_mut()
                .next()
                .expect("must have a JoinSplit");
            joinsplit.zkproof.0 = [0; 192];
        }
    }
}

/// Duplicate a Sapling spend inside a `transaction`.
///
/// Returns the nullifier of the duplicate spend.
///
/// # Panics
///
/// Will panic if the `transaction` does not have Sapling spends.
fn duplicate_sapling_spend(transaction: &mut Transaction) -> sapling::Nullifier {
    match transaction {
        Transaction::V4 {
            sapling_shielded_data: Some(ref mut shielded_data),
            ..
        } => duplicate_sapling_spend_in_shielded_data(shielded_data),
        Transaction::V5 {
            sapling_shielded_data: Some(ref mut shielded_data),
            ..
        } => duplicate_sapling_spend_in_shielded_data(shielded_data),
        _ => unreachable!("Transaction has no Sapling shielded data"),
    }
}

/// Duplicates the first spend of the `shielded_data`.
///
/// Returns the nullifier of the duplicate spend.
///
/// # Panics
///
/// Will panic if `shielded_data` has no spends.
fn duplicate_sapling_spend_in_shielded_data<A: sapling::AnchorVariant + Clone>(
    shielded_data: &mut sapling::ShieldedData<A>,
) -> sapling::Nullifier {
    match shielded_data.transfers {
        sapling::TransferData::SpendsAndMaybeOutputs { ref mut spends, .. } => {
            let duplicate_spend = spends.first().clone();
            let duplicate_nullifier = duplicate_spend.nullifier;

            spends.push(duplicate_spend);

            duplicate_nullifier
        }
        sapling::TransferData::JustOutputs { .. } => {
            unreachable!("Sapling shielded data has no spends")
        }
    }
}

#[test]
fn add_to_sprout_pool_after_nu() {
    let _init_guard = zebra_test::init();

    // get a block that we know it haves a transaction with `vpub_old` field greater than 0.
    let block: Arc<_> = zebra_chain::block::Block::zcash_deserialize(
        &zebra_test::vectors::BLOCK_MAINNET_419199_BYTES[..],
    )
    .unwrap()
    .into();

    // create a block height at canopy activation.
    let network = Network::Mainnet;
    let block_height = NetworkUpgrade::Canopy.activation_height(&network).unwrap();

    // create a zero amount.
    let zero = Amount::<NonNegative>::try_from(0).expect("an amount of 0 is always valid");

    // the coinbase transaction should pass the check.
    assert_eq!(
        check::disabled_add_to_sprout_pool(&block.transactions[0], block_height, &network),
        Ok(())
    );

    // the 2nd transaction has no joinsplits, should pass the check.
    assert_eq!(block.transactions[1].joinsplit_count(), 0);
    assert_eq!(
        check::disabled_add_to_sprout_pool(&block.transactions[1], block_height, &network),
        Ok(())
    );

    // the 5th transaction has joinsplits and the `vpub_old` cumulative is greater than 0,
    // should fail the check.
    assert!(block.transactions[4].joinsplit_count() > 0);
    let vpub_old: Amount<NonNegative> = block.transactions[4]
        .output_values_to_sprout()
        .fold(zero, |acc, &x| (acc + x).unwrap());
    assert!(vpub_old > zero);

    assert_eq!(
        check::disabled_add_to_sprout_pool(&block.transactions[3], block_height, &network),
        Err(TransactionError::DisabledAddToSproutPool)
    );

    // the 8th transaction has joinsplits and the `vpub_old` cumulative is 0,
    // should pass the check.
    assert!(block.transactions[7].joinsplit_count() > 0);
    let vpub_old: Amount<NonNegative> = block.transactions[7]
        .output_values_to_sprout()
        .fold(zero, |acc, &x| (acc + x).unwrap());
    assert_eq!(vpub_old, zero);

    assert_eq!(
        check::disabled_add_to_sprout_pool(&block.transactions[7], block_height, &network),
        Ok(())
    );
}

#[test]
fn coinbase_outputs_are_decryptable_for_historical_blocks() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        coinbase_outputs_are_decryptable_for_historical_blocks_for_network(network)?;
    }

    Ok(())
}

fn coinbase_outputs_are_decryptable_for_historical_blocks_for_network(
    network: Network,
) -> Result<(), Report> {
    let block_iter = network.block_iter();

    let mut tested_coinbase_txs = 0;
    let mut tested_non_coinbase_txs = 0;

    for (height, block) in block_iter {
        let block = block
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");
        let height = Height(*height);
        let heartwood_onward = height
            >= NetworkUpgrade::Heartwood
                .activation_height(&network)
                .unwrap();
        let coinbase_tx = block
            .transactions
            .first()
            .expect("must have coinbase transaction");

        // Check if the coinbase outputs are decryptable with an all-zero key.
        if heartwood_onward
            && (coinbase_tx.sapling_outputs().count() > 0
                || coinbase_tx.orchard_actions().count() > 0)
        {
            // We are only truly decrypting something if it's Heartwood-onward
            // and there are relevant outputs.
            tested_coinbase_txs += 1;
        }
        check::coinbase_outputs_are_decryptable(coinbase_tx, &network, height)
            .expect("coinbase outputs must be decryptable with an all-zero key");

        // For remaining transactions, check if existing outputs are NOT decryptable
        // with an all-zero key, if applicable.
        for tx in block.transactions.iter().skip(1) {
            let has_outputs = tx.sapling_outputs().count() > 0 || tx.orchard_actions().count() > 0;
            if has_outputs && heartwood_onward {
                tested_non_coinbase_txs += 1;
                check::coinbase_outputs_are_decryptable(tx, &network, height).expect_err(
                    "decrypting a non-coinbase output with an all-zero key should fail",
                );
            } else {
                check::coinbase_outputs_are_decryptable(tx, &network, height)
                    .expect("a transaction without outputs, or pre-Heartwood, must be considered 'decryptable'");
            }
        }
    }

    assert!(tested_coinbase_txs > 0, "ensure it was actually tested");
    assert!(tested_non_coinbase_txs > 0, "ensure it was actually tested");

    Ok(())
}

/// Given an Orchard action as a base, fill fields related to note encryption
/// from the given test vector and returned the modified action.
fn fill_action_with_note_encryption_test_vector(
    action: &zebra_chain::orchard::Action<OrchardVanilla>,
    v: &zebra_test::vectors::TestVector,
) -> zebra_chain::orchard::Action<OrchardVanilla> {
    let mut action = action.clone();
    action.cv = v.cv_net.try_into().expect("test vector must be valid");
    action.cm_x = pallas::Base::from_repr(v.cmx).unwrap();
    action.nullifier = v.rho.try_into().expect("test vector must be valid");
    action.ephemeral_key = v
        .ephemeral_key
        .try_into()
        .expect("test vector must be valid");
    action.out_ciphertext = v.c_out.into();
    action.enc_ciphertext = v.c_enc.into();
    action
}

/// Test if shielded coinbase outputs are decryptable with an all-zero outgoing
/// viewing key.
#[test]
fn coinbase_outputs_are_decryptable_for_fake_v5_blocks() {
    let network = Network::new_default_testnet();

    for v in zebra_test::vectors::ORCHARD_NOTE_ENCRYPTION_ZERO_VECTOR.iter() {
        // Find a transaction with no inputs or outputs to use as base
        let mut transaction =
            fake_v5_transactions_for_network(&network, zebra_test::vectors::TESTNET_BLOCKS.iter())
                .rev()
                .find(|transaction| {
                    transaction.inputs().is_empty()
                        && transaction.outputs().is_empty()
                        && transaction.sapling_spends_per_anchor().next().is_none()
                        && transaction.sapling_outputs().next().is_none()
                        && transaction.joinsplit_count() == 0
                })
                .expect("At least one fake V5 transaction with no inputs and no outputs");

        let shielded_data = insert_fake_orchard_shielded_data(&mut transaction);
        shielded_data.action_groups.first_mut().flags = zebra_chain::orchard::Flags::ENABLE_SPENDS
            | zebra_chain::orchard::Flags::ENABLE_OUTPUTS;

        let action = fill_action_with_note_encryption_test_vector(
            &shielded_data.action_groups.first().actions[0].action,
            v,
        );
        let sig = shielded_data.action_groups.first().actions[0].spend_auth_sig;
        shielded_data.action_groups.first_mut().actions =
            vec![AuthorizedAction::from_parts(action, sig)]
                .try_into()
                .unwrap();

        assert_eq!(
            check::coinbase_outputs_are_decryptable(
                &transaction,
                &network,
                NetworkUpgrade::Nu5.activation_height(&network).unwrap(),
            ),
            Ok(())
        );
    }
}

/// Test if random shielded outputs are NOT decryptable with an all-zero outgoing
/// viewing key.
#[test]
fn shielded_outputs_are_not_decryptable_for_fake_v5_blocks() {
    let network = Network::new_default_testnet();

    for v in zebra_test::vectors::ORCHARD_NOTE_ENCRYPTION_VECTOR.iter() {
        // Find a transaction with no inputs or outputs to use as base
        let mut transaction =
            fake_v5_transactions_for_network(&network, zebra_test::vectors::TESTNET_BLOCKS.iter())
                .rev()
                .find(|transaction| {
                    transaction.inputs().is_empty()
                        && transaction.outputs().is_empty()
                        && transaction.sapling_spends_per_anchor().next().is_none()
                        && transaction.sapling_outputs().next().is_none()
                        && transaction.joinsplit_count() == 0
                })
                .expect("At least one fake V5 transaction with no inputs and no outputs");

        let shielded_data = insert_fake_orchard_shielded_data(&mut transaction);
        shielded_data.action_groups.first_mut().flags = zebra_chain::orchard::Flags::ENABLE_SPENDS
            | zebra_chain::orchard::Flags::ENABLE_OUTPUTS;

        let action = fill_action_with_note_encryption_test_vector(
            &shielded_data.action_groups.first().actions[0].action,
            v,
        );
        let sig = shielded_data.action_groups.first().actions[0].spend_auth_sig;
        shielded_data.action_groups.first_mut().actions =
            vec![AuthorizedAction::from_parts(action, sig)]
                .try_into()
                .unwrap();

        assert_eq!(
            check::coinbase_outputs_are_decryptable(
                &transaction,
                &network,
                NetworkUpgrade::Nu5.activation_height(&network).unwrap(),
            ),
            Err(TransactionError::CoinbaseOutputsNotDecryptable)
        );
    }
}

#[tokio::test]
async fn mempool_zip317_error() {
    let mut state: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let verifier = Verifier::new(&Network::Mainnet, state.clone());

    let height = NetworkUpgrade::Nu5
        .activation_height(&Network::Mainnet)
        .expect("Nu5 activation height is specified");
    let fund_height = (height - 1).expect("fake source fund block height is too small");

    // Will produce a small enough miner fee to fail the check.
    let (input, output, known_utxos) = mock_transparent_transfer(
        fund_height,
        true,
        0,
        Amount::try_from(10).expect("valid amount"),
    );

    // Create a non-coinbase V5 tx.
    let tx = Transaction::V5 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::unlocked(),
        network_upgrade: NetworkUpgrade::Nu5,
        expiry_height: height,
        sapling_shielded_data: None,
        orchard_shielded_data: None,
    };

    let input_outpoint = match tx.inputs()[0] {
        transparent::Input::PrevOut { outpoint, .. } => outpoint,
        transparent::Input::Coinbase { .. } => panic!("requires a non-coinbase transaction"),
    };

    tokio::spawn(async move {
        state
            .expect_request(zebra_state::Request::UnspentBestChainUtxo(input_outpoint))
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::UnspentBestChainUtxo(
                known_utxos
                    .get(&input_outpoint)
                    .map(|utxo| utxo.utxo.clone()),
            ));

        state
            .expect_request_that(|req| {
                matches!(
                    req,
                    zebra_state::Request::CheckBestChainTipNullifiersAndAnchors(_)
                )
            })
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::ValidBestChainTipNullifiersAndAnchors);
    });

    let verifier_response = verifier
        .oneshot(Request::Mempool {
            transaction: tx.into(),
            height,
        })
        .await;

    // Mempool refuses to add this transaction into storage.
    assert!(verifier_response.is_err());
    assert_eq!(
        verifier_response.err(),
        Some(TransactionError::Zip317(zip317::Error::UnpaidActions))
    );
}

#[tokio::test]
async fn mempool_zip317_ok() {
    let mut state: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let verifier = Verifier::new(&Network::Mainnet, state.clone());

    let height = NetworkUpgrade::Nu5
        .activation_height(&Network::Mainnet)
        .expect("Nu5 activation height is specified");
    let fund_height = (height - 1).expect("fake source fund block height is too small");

    // Will produce a big enough miner fee to pass the check.
    let (input, output, known_utxos) = mock_transparent_transfer(
        fund_height,
        true,
        0,
        Amount::try_from(10_001).expect("valid amount"),
    );

    // Create a non-coinbase V5 tx.
    let tx = Transaction::V5 {
        inputs: vec![input],
        outputs: vec![output],
        lock_time: LockTime::unlocked(),
        network_upgrade: NetworkUpgrade::Nu5,
        expiry_height: height,
        sapling_shielded_data: None,
        orchard_shielded_data: None,
    };

    let input_outpoint = match tx.inputs()[0] {
        transparent::Input::PrevOut { outpoint, .. } => outpoint,
        transparent::Input::Coinbase { .. } => panic!("requires a non-coinbase transaction"),
    };

    tokio::spawn(async move {
        state
            .expect_request(zebra_state::Request::UnspentBestChainUtxo(input_outpoint))
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::UnspentBestChainUtxo(
                known_utxos
                    .get(&input_outpoint)
                    .map(|utxo| utxo.utxo.clone()),
            ));

        state
            .expect_request_that(|req| {
                matches!(
                    req,
                    zebra_state::Request::CheckBestChainTipNullifiersAndAnchors(_)
                )
            })
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::ValidBestChainTipNullifiersAndAnchors);
    });

    let verifier_response = verifier
        .oneshot(Request::Mempool {
            transaction: tx.into(),
            height,
        })
        .await;

    assert!(
        verifier_response.is_ok(),
        "expected successful verification, got: {verifier_response:?}"
    );
}
