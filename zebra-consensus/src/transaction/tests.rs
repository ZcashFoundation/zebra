//! Tests for Zcash transaction consensus checks.

#![allow(clippy::unwrap_in_result)]

//
// TODO: split fixed test vectors into a `vectors` module?

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use chrono::{DateTime, TimeZone, Utc};
use color_eyre::eyre::Report;
use futures::{FutureExt, TryFutureExt};
use halo2::pasta::{group::ff::PrimeField, pallas};
use tokio::time::timeout;
use tower::{buffer::Buffer, service_fn, ServiceExt};

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, Block, Height},
    orchard::Action,
    parameters::{testnet::ConfiguredActivationHeights, Network, NetworkUpgrade},
    primitives::{ed25519, x25519, Groth16Proof},
    sapling,
    serialization::{DateTime32, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
    sprout,
    transaction::{
        arbitrary::{test_transactions, transactions_from_blocks, v5_transactions},
        zip317, Hash, HashType, JoinSplitData, LockTime, Transaction,
    },
    transparent::{self, CoinbaseData, CoinbaseSpendRestriction},
};

use zebra_node_services::mempool;
use zebra_state::ValidateContextError;
use zebra_test::mock_service::MockService;

use crate::{error::TransactionError, transaction::POLL_MEMPOOL_DELAY};

use super::{check, Request, Verifier};

#[cfg(test)]
mod prop;

/// Returns the timeout duration for tests, extended when running under coverage
/// instrumentation to account for the performance overhead.
fn test_timeout() -> std::time::Duration {
    // Check if we're running under cargo-llvm-cov by looking for its environment variables
    if std::env::var("LLVM_COV_FLAGS").is_ok() || std::env::var("CARGO_LLVM_COV").is_ok() {
        // Use a 5x longer timeout when running with coverage (150 seconds)
        std::time::Duration::from_secs(150)
    } else {
        std::time::Duration::from_secs(30)
    }
}

#[test]
fn v5_transactions_basic_check() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        for transaction in v5_transactions(network.block_iter()) {
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
fn v5_transaction_with_orchard_actions_has_inputs_and_outputs() {
    for net in Network::iter() {
        let tx = v5_transactions(net.block_iter())
            .find(|transaction| {
                transaction.inputs().is_empty()
                    && transaction.outputs().is_empty()
                    && transaction.sapling_spends_count() == 0
                    && transaction.sapling_outputs().next().is_none()
                    && transaction.joinsplit_count() == 0
            })
            .expect("V5 tx with only Orchard shielded data");

        let tx_bytes = tx
            .zcash_serialize_to_vec()
            .expect("transaction serialization should succeed");

        // Find the orchard flags offset
        let flags_offset = find_v5_orchard_flags_offset(&tx_bytes);

        // Test with empty flags (no spends, no outputs)
        let mut modified = tx_bytes.clone();
        modified[flags_offset] = 0x00; // Flags::empty()
        let modified_tx = Transaction::zcash_deserialize(modified.as_slice())
            .expect("modified transaction should deserialize");
        assert_eq!(
            check::has_inputs_and_outputs(&modified_tx),
            Err(TransactionError::NoInputs)
        );

        // ENABLE_SPENDS only -> passes inputs check but fails outputs
        let mut modified = tx_bytes.clone();
        modified[flags_offset] = 0x01; // Flags::ENABLE_SPENDS
        let modified_tx = Transaction::zcash_deserialize(modified.as_slice())
            .expect("modified transaction should deserialize");
        assert_eq!(
            check::has_inputs_and_outputs(&modified_tx),
            Err(TransactionError::NoOutputs)
        );

        // ENABLE_OUTPUTS only -> passes outputs check but fails inputs
        let mut modified = tx_bytes.clone();
        modified[flags_offset] = 0x02; // Flags::ENABLE_OUTPUTS
        let modified_tx = Transaction::zcash_deserialize(modified.as_slice())
            .expect("modified transaction should deserialize");
        assert_eq!(
            check::has_inputs_and_outputs(&modified_tx),
            Err(TransactionError::NoInputs)
        );

        // Both flags -> valid
        let mut modified = tx_bytes.clone();
        modified[flags_offset] = 0x03; // ENABLE_SPENDS | ENABLE_OUTPUTS
        let modified_tx = Transaction::zcash_deserialize(modified.as_slice())
            .expect("modified transaction should deserialize");
        assert!(check::has_inputs_and_outputs(&modified_tx).is_ok());
    }
}

#[test]
fn v5_transaction_with_orchard_actions_has_flags() {
    for net in Network::iter() {
        let tx = v5_transactions(net.block_iter())
            .find(|transaction| {
                transaction.inputs().is_empty()
                    && transaction.outputs().is_empty()
                    && transaction.sapling_spends_count() == 0
                    && transaction.sapling_outputs().next().is_none()
                    && transaction.joinsplit_count() == 0
            })
            .expect("V5 tx with only Orchard actions");

        let tx_bytes = tx
            .zcash_serialize_to_vec()
            .expect("transaction serialization should succeed");

        let flags_offset = find_v5_orchard_flags_offset(&tx_bytes);

        // Empty flags -> fails
        let mut modified = tx_bytes.clone();
        modified[flags_offset] = 0x00;
        let modified_tx = Transaction::zcash_deserialize(modified.as_slice())
            .expect("modified transaction should deserialize");
        assert_eq!(
            check::has_enough_orchard_flags(&modified_tx),
            Err(TransactionError::NotEnoughFlags)
        );

        // ENABLE_SPENDS only -> passes
        let mut modified = tx_bytes.clone();
        modified[flags_offset] = 0x01;
        let modified_tx = Transaction::zcash_deserialize(modified.as_slice())
            .expect("modified transaction should deserialize");
        assert!(check::has_enough_orchard_flags(&modified_tx).is_ok());

        // ENABLE_OUTPUTS only -> passes
        let mut modified = tx_bytes.clone();
        modified[flags_offset] = 0x02;
        let modified_tx = Transaction::zcash_deserialize(modified.as_slice())
            .expect("modified transaction should deserialize");
        assert!(check::has_enough_orchard_flags(&modified_tx).is_ok());

        // Both flags -> passes
        let mut modified = tx_bytes.clone();
        modified[flags_offset] = 0x03;
        let modified_tx = Transaction::zcash_deserialize(modified.as_slice())
            .expect("modified transaction should deserialize");
        assert!(check::has_enough_orchard_flags(&modified_tx).is_ok());
    }
}

#[test]
fn v5_transaction_with_no_inputs_fails_verification() {
    let (_, output, _) = mock_transparent_transfer(
        Height(1),
        true,
        0,
        Amount::try_from(1).expect("valid value"),
    );

    for net in Network::iter() {
        let transaction = Transaction::test_v5(
            NetworkUpgrade::Nu5,
            vec![],
            vec![output.clone()],
            LockTime::Height(block::Height(0)),
            NetworkUpgrade::Nu5.activation_height(&net).expect("height"),
        );

        assert_eq!(
            check::has_inputs_and_outputs(&transaction),
            Err(TransactionError::NoInputs)
        );
    }
}

#[test]
fn v5_transaction_with_no_outputs_fails_verification() {
    let (input, _, _) = mock_transparent_transfer(
        Height(1),
        true,
        0,
        Amount::try_from(1).expect("valid value"),
    );

    for net in Network::iter() {
        let transaction = Transaction::test_v5(
            NetworkUpgrade::Nu5,
            vec![input.clone()],
            vec![],
            LockTime::Height(block::Height(0)),
            NetworkUpgrade::Nu5.activation_height(&net).expect("height"),
        );

        assert_eq!(
            check::has_inputs_and_outputs(&transaction),
            Err(TransactionError::NoOutputs)
        );
    }
}

#[tokio::test]
async fn mempool_request_with_missing_input_is_rejected() {
    let mut state: MockService<_, _, _, _> = MockService::build().for_unit_tests();

    for net in Network::iter() {
        let verifier = Verifier::new_for_tests(&net, state.clone());

        let (height, tx) = transactions_from_blocks(net.block_iter())
            .find(|(_, tx)| !(tx.is_coinbase() || tx.inputs().is_empty()))
            .expect(
                "At least one non-coinbase transaction with transparent inputs in test vectors",
            );

        let input_outpoint = match tx.inputs()[0] {
            transparent::Input::PrevOut { outpoint, .. } => outpoint,
            transparent::Input::Coinbase { .. } => panic!("requires a non-coinbase transaction"),
        };

        // The first non-coinbase transaction with transparent inputs in our test vectors
        // does not use a lock time, so we don't see Request::BestChainNextMedianTimePast here

        let state_req = state
            .expect_request(zebra_state::Request::UnspentBestChainUtxo(input_outpoint))
            .map(|responder| responder.respond(zebra_state::Response::UnspentBestChainUtxo(None)));

        let verifier_req = verifier.oneshot(Request::Mempool {
            transaction: tx.into(),
            height,
        });

        let (rsp, _) = futures::join!(verifier_req, state_req);

        assert_eq!(rsp, Err(TransactionError::TransparentInputNotFound));
    }
}

#[tokio::test]
async fn mempool_request_with_present_input_is_accepted() {
    let mut state: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let verifier = Verifier::new_for_tests(&Network::Mainnet, state.clone());

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
    let tx = Transaction::test_v4(vec![input], vec![output], LockTime::unlocked(), height);

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
            transaction: std::sync::Arc::new(tx).into(),
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
    let verifier = Verifier::new_for_tests(&Network::Mainnet, state.clone());

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
    let tx = Transaction::test_v4(
        vec![input],
        vec![output],
        LockTime::max_lock_time_timestamp(),
        height,
    );

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
            transaction: std::sync::Arc::new(tx).into(),
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
    let verifier = Verifier::new_for_tests(&Network::Mainnet, state.clone());

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
    let tx = Transaction::test_v4(vec![input], vec![output], LockTime::unlocked(), height);

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
            transaction: std::sync::Arc::new(tx).into(),
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
    let verifier = Verifier::new_for_tests(&Network::Mainnet, state.clone());

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
    let tx = Transaction::test_v4(
        vec![input],
        vec![output],
        LockTime::max_lock_time_timestamp(),
        height,
    );

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
            transaction: std::sync::Arc::new(tx).into(),
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
    let verifier = Verifier::new_for_tests(&Network::Mainnet, state.clone());

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
    let tx = Transaction::test_v4(
        vec![input],
        vec![output],
        LockTime::min_lock_time_timestamp(),
        height,
    );

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
            transaction: std::sync::Arc::new(tx).into(),
            height,
        })
        .await;

    assert!(
        verifier_response.is_ok(),
        "expected successful verification, got: {verifier_response:?}"
    );
}

#[tokio::test]
async fn mempool_request_with_unmined_output_spends_is_accepted() {
    let mut state: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let mempool: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let (mempool_setup_tx, mempool_setup_rx) = tokio::sync::oneshot::channel();
    let verifier = Verifier::new(&Network::Mainnet, state.clone(), mempool_setup_rx);
    mempool_setup_tx
        .send(mempool.clone())
        .ok()
        .expect("send should succeed");

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
    let tx = Transaction::test_v4(
        vec![input],
        vec![output],
        LockTime::min_lock_time_timestamp(),
        height,
    );

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

    let mut mempool_clone = mempool.clone();
    tokio::spawn(async move {
        mempool_clone
            .expect_request(mempool::Request::AwaitOutput(input_outpoint))
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(mempool::Response::UnspentOutput(
                known_utxos
                    .get(&input_outpoint)
                    .expect("input outpoint should exist in known_utxos")
                    .utxo
                    .output
                    .clone(),
            ));
    });

    let verifier_response = verifier
        .oneshot(Request::Mempool {
            transaction: std::sync::Arc::new(tx).into(),
            height,
        })
        .await;

    assert!(
        verifier_response.is_ok(),
        "expected successful verification, got: {verifier_response:?}"
    );

    let crate::transaction::Response::Mempool {
        transaction: _,
        spent_mempool_outpoints,
    } = verifier_response.expect("already checked that response is ok")
    else {
        panic!("unexpected response variant from transaction verifier for Mempool request")
    };

    assert_eq!(
        spent_mempool_outpoints,
        vec![input_outpoint],
        "spent_mempool_outpoints in tx verifier response should match input_outpoint"
    );

    tokio::time::sleep(POLL_MEMPOOL_DELAY * 2).await;
    // polled before AwaitOutput request and after a mempool transaction with transparent outputs
    // is successfully verified
    assert_eq!(
        mempool.poll_count(),
        2,
        "the mempool service should have been polled twice"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn skips_verification_of_block_transactions_in_mempool() {
    let mut state: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let mempool: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let (mempool_setup_tx, mempool_setup_rx) = tokio::sync::oneshot::channel();
    let verifier = Verifier::new(&Network::Mainnet, state.clone(), mempool_setup_rx);
    let verifier = Buffer::new(verifier, 1);

    mempool_setup_tx
        .send(mempool.clone())
        .ok()
        .expect("send should succeed");

    let height = NetworkUpgrade::Nu6
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
    let tx = Transaction::test_v5(
        NetworkUpgrade::Nu6,
        vec![input],
        vec![output],
        LockTime::min_lock_time_timestamp(),
        height,
    );

    let tx_hash = tx.hash();
    let input_outpoint = match tx.inputs()[0] {
        transparent::Input::PrevOut { outpoint, .. } => outpoint,
        transparent::Input::Coinbase { .. } => panic!("requires a non-coinbase transaction"),
    };

    let mut state_clone = state.clone();
    tokio::spawn(async move {
        state_clone
            .expect_request(zebra_state::Request::BestChainNextMedianTimePast)
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::BestChainNextMedianTimePast(
                DateTime32::MAX,
            ));

        state_clone
            .expect_request(zebra_state::Request::UnspentBestChainUtxo(input_outpoint))
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::UnspentBestChainUtxo(None));

        state_clone
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

    let utxo = known_utxos
        .get(&input_outpoint)
        .expect("input outpoint should exist in known_utxos")
        .utxo
        .clone();

    let mut mempool_clone = mempool.clone();
    let output = utxo.output.clone();
    tokio::spawn(async move {
        mempool_clone
            .expect_request(mempool::Request::AwaitOutput(input_outpoint))
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(mempool::Response::UnspentOutput(output));
    });

    // Briefly yield and sleep so the spawned task can first expect an await output request.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let verifier_response = verifier
        .clone()
        .oneshot(Request::Mempool {
            transaction: std::sync::Arc::new(tx.clone()).into(),
            height,
        })
        .await;

    assert!(
        verifier_response.is_ok(),
        "expected successful verification, got: {verifier_response:?}"
    );

    let crate::transaction::Response::Mempool {
        transaction,
        spent_mempool_outpoints,
    } = verifier_response.expect("already checked that response is ok")
    else {
        panic!("unexpected response variant from transaction verifier for Mempool request")
    };

    assert_eq!(
        spent_mempool_outpoints,
        vec![input_outpoint],
        "spent_mempool_outpoints in tx verifier response should match input_outpoint"
    );

    let mut mempool_clone = mempool.clone();
    tokio::spawn(async move {
        for _ in 0..2 {
            mempool_clone
                .expect_request(mempool::Request::TransactionWithDepsByMinedId(tx_hash))
                .await
                .expect("verifier should call mock mempool service with correct request")
                .respond(mempool::Response::TransactionWithDeps {
                    transaction: transaction.clone(),
                    dependencies: [input_outpoint.hash].into(),
                });
        }
    });

    let make_request = |known_outpoint_hashes| Request::Block {
        transaction_hash: tx_hash,
        transaction: Arc::new(tx),
        known_outpoint_hashes,
        known_utxos: Arc::new(HashMap::new()),
        height,
        time: Utc::now(),
    };

    // Briefly yield and sleep so the spawned task can first expect the requests.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let crate::transaction::Response::Block { .. } = verifier
        .clone()
        .oneshot(make_request.clone()(Arc::new([input_outpoint.hash].into())))
        .await
        .expect("should return Ok without calling state service")
    else {
        panic!("unexpected response variant from transaction verifier for Block request")
    };

    tokio::spawn(async move {
        state
            .expect_request(zebra_state::Request::AwaitUtxo(input_outpoint))
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::Utxo(utxo));
    });

    let crate::transaction::Response::Block { .. } = verifier
        .clone()
        .oneshot(make_request.clone()(Arc::new(HashSet::new())))
        .await
        .expect("should succeed after calling state service")
    else {
        panic!("unexpected response variant from transaction verifier for Block request")
    };

    tokio::time::sleep(POLL_MEMPOOL_DELAY * 2).await;
    // polled before AwaitOutput request, after a mempool transaction with transparent outputs,
    // is successfully verified, and twice more when checking if a transaction in a block is
    // already the mempool.
    assert_eq!(
        mempool.poll_count(),
        4,
        "the mempool service should have been polled 4 times"
    );
}

/// Tests that calls to the transaction verifier with a mempool request that spends
/// immature coinbase outputs will return an error.
#[tokio::test]
async fn mempool_request_with_immature_spend_is_rejected() {
    let _init_guard = zebra_test::init();

    let mut state: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let verifier = Verifier::new_for_tests(&Network::Mainnet, state.clone());

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
    let tx = Transaction::test_v4(
        vec![input],
        vec![output],
        LockTime::min_lock_time_timestamp(),
        height,
    );

    let input_outpoint = match tx.inputs()[0] {
        transparent::Input::PrevOut { outpoint, .. } => outpoint,
        transparent::Input::Coinbase { .. } => panic!("requires a non-coinbase transaction"),
    };

    let spend_restriction = tx.coinbase_spend_restriction(&Network::Mainnet, height);

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
            transaction: std::sync::Arc::new(tx).into(),
            height,
        })
        .await
        .expect_err("verification of transaction with immature spend should fail");

    assert_eq!(
        verifier_response, expected_error,
        "expected to fail verification, got: {verifier_response:?}"
    );
}

/// Tests that calls to the transaction verifier with a mempool request that spends
/// mature coinbase outputs to transparent outputs will return Ok() on Regtest.
#[tokio::test]
async fn mempool_request_with_transparent_coinbase_spend_is_accepted_on_regtest() {
    let _init_guard = zebra_test::init();

    let network = Network::new_regtest(
        ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(100),
            nu6: Some(1_000),
            ..Default::default()
        }
        .into(),
    );
    let mut state: MockService<_, _, _, _> = MockService::build().for_unit_tests();
    let verifier = Verifier::new_for_tests(&network, state.clone());

    let height = NetworkUpgrade::Nu6
        .activation_height(&network)
        .expect("NU6 activation height is specified");
    let fund_height = (height - 1).expect("fake source fund block height is too small");
    let (input, output, known_utxos) = mock_transparent_transfer(
        fund_height,
        true,
        0,
        Amount::try_from(10001).expect("invalid value"),
    );

    // Create a non-coinbase V5 tx with the last valid expiry height.
    let tx = Transaction::test_v5(
        NetworkUpgrade::Nu6,
        vec![input],
        vec![output],
        LockTime::min_lock_time_timestamp(),
        height,
    );

    let input_outpoint = match tx.inputs()[0] {
        transparent::Input::PrevOut { outpoint, .. } => outpoint,
        transparent::Input::Coinbase { .. } => panic!("requires a non-coinbase transaction"),
    };

    let spend_restriction = tx.coinbase_spend_restriction(&network, height);

    assert_eq!(
        spend_restriction,
        CoinbaseSpendRestriction::CheckCoinbaseMaturity {
            spend_height: height
        }
    );

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

    zebra_state::check::transparent_coinbase_spend(input_outpoint, spend_restriction, &utxo)
        .expect("check should pass");

    tokio::spawn(async move {
        state
            .expect_request(zebra_state::Request::BestChainNextMedianTimePast)
            .await
            .respond(zebra_state::Response::BestChainNextMedianTimePast(
                DateTime32::MAX,
            ));

        state
            .expect_request(zebra_state::Request::UnspentBestChainUtxo(input_outpoint))
            .await
            .respond(zebra_state::Response::UnspentBestChainUtxo(Some(utxo)));

        state
            .expect_request_that(|req| {
                matches!(
                    req,
                    zebra_state::Request::CheckBestChainTipNullifiersAndAnchors(_)
                )
            })
            .await
            .respond(zebra_state::Response::ValidBestChainTipNullifiersAndAnchors);
    });

    verifier
        .oneshot(Request::Mempool {
            transaction: std::sync::Arc::new(tx).into(),
            height,
        })
        .await
        .expect("verification of transaction with mature spend to transparent outputs should pass");
}

/// Tests that errors from the read state service are correctly converted into
/// transaction verifier errors.
#[tokio::test]
async fn state_error_converted_correctly() {
    use zebra_state::DuplicateNullifierError;

    let mut state: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let verifier = Verifier::new_for_tests(&Network::Mainnet, state.clone());

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
    let tx = Transaction::test_v4(vec![input], vec![output], LockTime::unlocked(), height);

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
            transaction: std::sync::Arc::new(tx).into(),
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
fn v5_coinbase_transaction_without_enable_spends_flag_passes_validation() {
    for net in Network::iter() {
        let coinbase_tx = v5_transactions(net.block_iter())
            .find(|transaction| transaction.is_coinbase())
            .expect("V5 coinbase tx");

        // Graft orchard data from a non-coinbase V5 tx onto the coinbase tx
        let tx = graft_orchard_data_onto_v5_tx(&coinbase_tx, &net, Some(0x00)); // flags = empty

        assert!(check::coinbase_tx_no_prevout_joinsplit_spend(&tx).is_ok());
    }
}

#[test]
fn v5_coinbase_transaction_with_enable_spends_flag_fails_validation() {
    for net in Network::iter() {
        let coinbase_tx = v5_transactions(net.block_iter())
            .find(|transaction| transaction.is_coinbase())
            .expect("V5 coinbase tx");

        // Graft orchard data with ENABLE_SPENDS flag set
        let tx = graft_orchard_data_onto_v5_tx(&coinbase_tx, &net, Some(0x01)); // flags = ENABLE_SPENDS

        assert_eq!(
            check::coinbase_tx_no_prevout_joinsplit_spend(&tx),
            Err(TransactionError::CoinbaseHasEnableSpendsOrchard)
        );
    }
}

#[tokio::test]
async fn v5_transaction_is_rejected_before_nu5_activation() {
    let sapling = NetworkUpgrade::Sapling;

    for net in Network::iter() {
        let verifier = Verifier::new_for_tests(
            &net,
            service_fn(|_| async { unreachable!("Service should not be called") }),
        );

        let tx = v5_transactions(net.block_iter()).next().expect("V5 tx");

        assert_eq!(
            verifier
                .oneshot(Request::Block {
                    transaction_hash: tx.hash(),
                    transaction: Arc::new(tx),
                    known_utxos: Arc::new(HashMap::new()),
                    known_outpoint_hashes: Arc::new(HashSet::new()),
                    height: sapling.activation_height(&net).expect("height"),
                    time: DateTime::<Utc>::MAX_UTC,
                })
                .await,
            Err(TransactionError::UnsupportedByNetworkUpgrade(5, sapling))
        );
    }
}

#[tokio::test]
async fn v5_transaction_is_accepted_after_nu5_activation() {
    let _init_guard = zebra_test::init();

    for net in Network::iter() {
        let state = service_fn(|_| async { unreachable!("Service should not be called") });
        let tx = v5_transactions(net.block_iter()).next().expect("V5 tx");
        let tx_height = tx.expiry_height().expect("V5 must have expiry_height");
        let expected = tx.unmined_id();

        assert!(tx_height >= NetworkUpgrade::Nu5.activation_height(&net).expect("height"));

        let verif_res = Verifier::new_for_tests(&net, state)
            .oneshot(Request::Block {
                transaction_hash: tx.hash(),
                transaction: Arc::new(tx),
                known_utxos: Arc::new(HashMap::new()),
                known_outpoint_hashes: Arc::new(HashSet::new()),
                height: tx_height,
                time: DateTime::<Utc>::MAX_UTC,
            })
            .await;

        assert_eq!(verif_res.expect("success").tx_id(), expected);
    }
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
    let transaction = Transaction::test_v4(
        vec![input],
        vec![output],
        LockTime::Height(block::Height(0)),
        (transaction_block_height + 1).expect("expiry height is too large"),
    );

    let transaction_hash = transaction.unmined_id();

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new_for_tests(&network, state_service);

    let result = verifier
        .oneshot(Request::Block {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(known_utxos),
            known_outpoint_hashes: Arc::new(HashSet::new()),
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
    let verifier = Verifier::new_for_tests(&Network::Mainnet, state_service);

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
    let transaction = Transaction::test_v4(
        vec![input],
        vec![output],
        LockTime::unlocked(),
        block_height,
    );

    let result = verifier
        .oneshot(Request::Block {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(known_utxos),
            known_outpoint_hashes: Arc::new(HashSet::new()),
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
    let verifier = Verifier::new_for_tests(&Network::Mainnet, state_service);

    let block_height = NetworkUpgrade::Canopy
        .activation_height(&Network::Mainnet)
        .expect("Canopy activation height is specified");

    let (input, output) = mock_coinbase_transparent_output(block_height);

    // This is a correct expiry height for coinbase V4 transactions.
    let expiry_height = (block_height - 1).expect("original block height is too small");

    // Create a coinbase V4 tx.
    let transaction = Transaction::test_v4(
        vec![input],
        vec![output],
        LockTime::unlocked(),
        expiry_height,
    );

    let result = verifier
        .oneshot(Request::Block {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(HashMap::new()),
            known_outpoint_hashes: Arc::new(HashSet::new()),
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
    let verifier = Verifier::new_for_tests(&Network::Mainnet, state_service);

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
    let transaction = Transaction::test_v4(
        vec![input],
        vec![output],
        LockTime::unlocked(),
        expiry_height,
    );

    let result = verifier
        .oneshot(Request::Block {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(known_utxos),
            known_outpoint_hashes: Arc::new(HashSet::new()),
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
    let verifier = Verifier::new_for_tests(&Network::Mainnet, state_service);

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
    let transaction = Transaction::test_v4(
        vec![input],
        vec![output],
        LockTime::unlocked(),
        expiry_height,
    );

    let result = verifier
        .oneshot(Request::Block {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(known_utxos),
            known_outpoint_hashes: Arc::new(HashSet::new()),
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
    let verifier = Verifier::new_for_tests(&Network::Mainnet, state_service);

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
    let transaction = Transaction::test_v4(
        vec![input],
        vec![output],
        LockTime::unlocked(),
        expiry_height,
    );

    let result = verifier
        .oneshot(Request::Block {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(HashMap::new()),
            known_outpoint_hashes: Arc::new(HashSet::new()),
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
    let transaction = Transaction::test_v4(
        vec![input],
        vec![output],
        LockTime::Height(block::Height(0)),
        transaction_block_height,
    );

    let transaction_hash = transaction.unmined_id();

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new_for_tests(&network, state_service);

    let result = verifier
        .oneshot(Request::Block {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(HashMap::new()),
            known_outpoint_hashes: Arc::new(HashSet::new()),
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
    let transaction = Transaction::test_v4(
        vec![input],
        vec![output],
        LockTime::Height(block::Height(0)),
        (transaction_block_height + 1).expect("expiry height is too large"),
    );

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new_for_tests(&network, state_service);

    let result = verifier
        .oneshot(Request::Block {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(known_utxos),
            known_outpoint_hashes: Arc::new(HashSet::new()),
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
    let transaction = Transaction::test_v4(
        vec![input.clone(), input.clone()],
        vec![output],
        LockTime::Height(block::Height(0)),
        (transaction_block_height + 1).expect("expiry height is too large"),
    );

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new_for_tests(&network, state_service);

    let result = verifier
        .oneshot(Request::Block {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(known_utxos),
            known_outpoint_hashes: Arc::new(HashSet::new()),
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
        let nu = NetworkUpgrade::Canopy;

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

        // Build a signed V4 transaction with the joinsplit data
        let transaction = build_signed_v4_tx_with_joinsplit_data(
            joinsplit_data,
            &signing_key,
            nu,
            (transaction_block_height + 1).expect("expiry height is too large"),
        );

        let state_service =
            service_fn(|_| async { unreachable!("State service should not be called") });
        let verifier = Verifier::new_for_tests(&network, state_service);

        let result = verifier
            .oneshot(Request::Block {
                transaction_hash: transaction.hash(),
                transaction: Arc::new(transaction),
                known_utxos: Arc::new(HashMap::new()),
                known_outpoint_hashes: Arc::new(HashSet::new()),
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
        let nu = NetworkUpgrade::Canopy;

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

        // Build a signed V4 transaction with the joinsplit data
        let transaction = build_signed_v4_tx_with_joinsplit_data(
            joinsplit_data,
            &signing_key,
            nu,
            (transaction_block_height + 1).expect("expiry height is too large"),
        );

        let state_service =
            service_fn(|_| async { unreachable!("State service should not be called") });
        let verifier = Verifier::new_for_tests(&network, state_service);

        let result = verifier
            .oneshot(Request::Block {
                transaction_hash: transaction.hash(),
                transaction: Arc::new(transaction),
                known_utxos: Arc::new(HashMap::new()),
                known_outpoint_hashes: Arc::new(HashSet::new()),
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
    let transaction = Transaction::test_v5(
        network_upgrade,
        vec![input],
        vec![output],
        LockTime::Height(block::Height(0)),
        (transaction_block_height + 1).expect("expiry height is too large"),
    );

    let transaction_hash = transaction.unmined_id();

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new_for_tests(&network, state_service);

    let result = verifier
        .oneshot(Request::Block {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(known_utxos),
            known_outpoint_hashes: Arc::new(HashSet::new()),
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
    let verifier = Verifier::new_for_tests(&network, state_service);

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
    let transaction = Transaction::test_v5(
        NetworkUpgrade::Nu5,
        vec![input],
        vec![output],
        LockTime::unlocked(),
        block_height,
    );

    let result = verifier
        .oneshot(Request::Block {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(known_utxos),
            known_outpoint_hashes: Arc::new(HashSet::new()),
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
    let verifier = Verifier::new_for_tests(&network, state_service);
    let verifier = Buffer::new(verifier, 10);

    let block_height = NetworkUpgrade::Nu5
        .activation_height(&network)
        .expect("Nu5 activation height for testnet is specified");

    let (input, output) = mock_coinbase_transparent_output(block_height);

    // Create a coinbase V5 tx with an expiry height that matches the height of
    // the block. Note that this is the only valid expiry height for a V5
    // coinbase tx.
    let transaction = Transaction::test_v5(
        NetworkUpgrade::Nu5,
        vec![input],
        vec![output],
        LockTime::unlocked(),
        block_height,
    );

    let result = verifier
        .clone()
        .oneshot(Request::Block {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(HashMap::new()),
            known_outpoint_hashes: Arc::new(HashSet::new()),
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

    new_transaction.set_expiry_height(new_expiry_height);

    let result = verifier
        .clone()
        .oneshot(Request::Block {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(new_transaction.clone()),
            known_utxos: Arc::new(HashMap::new()),
            known_outpoint_hashes: Arc::new(HashSet::new()),
            height: block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await
        .map_err(|err| {
            *err.downcast()
                .expect("error type should be TransactionError")
        });

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

    new_transaction.set_expiry_height(new_expiry_height);

    let result = verifier
        .clone()
        .oneshot(Request::Block {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(new_transaction.clone()),
            known_utxos: Arc::new(HashMap::new()),
            known_outpoint_hashes: Arc::new(HashSet::new()),
            height: block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await
        .map_err(|err| {
            *err.downcast()
                .expect("error type should be TransactionError")
        });

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

    new_transaction.set_expiry_height(new_expiry_height);

    // Setting the new expiry height as the block height will activate NU6, so we need to set NU6
    // for the tx as well.
    let height = new_expiry_height;
    new_transaction.set_network_upgrade(NetworkUpgrade::current(&network, height));

    let verification_result = verifier
        .clone()
        .oneshot(Request::Block {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(new_transaction.clone()),
            known_utxos: Arc::new(HashMap::new()),
            known_outpoint_hashes: Arc::new(HashSet::new()),
            height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        verification_result
            .expect("successful verification")
            .tx_id(),
        new_transaction.unmined_id()
    );
}

/// Tests if an expired non-coinbase V5 transaction is rejected.
#[tokio::test]
async fn v5_transaction_with_too_low_expiry_height() {
    let network = Network::new_default_testnet();

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new_for_tests(&network, state_service);

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
    let transaction = Transaction::test_v5(
        NetworkUpgrade::Nu5,
        vec![input],
        vec![output],
        LockTime::unlocked(),
        expiry_height,
    );

    let result = verifier
        .oneshot(Request::Block {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(known_utxos),
            known_outpoint_hashes: Arc::new(HashSet::new()),
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

/// Tests if a non-coinbase V5 transaction with an expiry height exceeding the maximum is rejected.
#[tokio::test]
async fn v5_transaction_with_exceeding_expiry_height() {
    let state = service_fn(|_| async { unreachable!("State service should not be called") });

    let height_max = block::Height::MAX;

    let (input, output, known_utxos) = mock_transparent_transfer(
        height_max.previous().expect("valid height"),
        true,
        0,
        Amount::try_from(1).expect("valid amount"),
    );

    // This expiry height exceeds the maximum defined by the specification.
    let expiry_height = block::Height(500_000_000);

    // Create a non-coinbase V5 tx.
    let transaction = Transaction::test_v5(
        NetworkUpgrade::Nu6_1,
        vec![input],
        vec![output],
        LockTime::unlocked(),
        expiry_height,
    );

    let transaction_hash = transaction.hash();

    let verification_result = Verifier::new_for_tests(&Network::Mainnet, state)
        .oneshot(Request::Block {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(known_utxos),
            known_outpoint_hashes: Arc::new(HashSet::new()),
            height: height_max,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        verification_result,
        Err(TransactionError::MaximumExpiryHeight {
            expiry_height,
            is_coinbase: false,
            block_height: height_max,
            transaction_hash,
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
    let transaction = Transaction::test_v5(
        network_upgrade,
        vec![input],
        vec![output],
        LockTime::Height(block::Height(0)),
        transaction_block_height,
    );

    let transaction_hash = transaction.unmined_id();

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new_for_tests(&network, state_service);

    let result = verifier
        .oneshot(Request::Block {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(known_utxos),
            known_outpoint_hashes: Arc::new(HashSet::new()),
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
    let transaction = Transaction::test_v5(
        network_upgrade,
        vec![input],
        vec![output],
        LockTime::Height(block::Height(0)),
        (transaction_block_height + 1).expect("expiry height is too large"),
    );

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = Verifier::new_for_tests(&network, state_service);

    let result = verifier
        .oneshot(Request::Block {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(known_utxos),
            known_outpoint_hashes: Arc::new(HashSet::new()),
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
    for network in Network::iter() {
        let canopy_activation_height = NetworkUpgrade::Canopy
            .activation_height(&network)
            .expect("Canopy activation height is specified");

        let height = (canopy_activation_height + 10).expect("valid height");

        // Create a fake transparent transfer that should succeed
        let (input, output, known_utxos) = mock_transparent_transfer(
            height.previous().expect("valid height"),
            true,
            0,
            Amount::try_from(1).expect("valid amount"),
        );

        let transaction = Transaction::test_v5(
            NetworkUpgrade::Nu5,
            vec![input.clone(), input.clone()],
            vec![output],
            LockTime::Height(block::Height(0)),
            height.next().expect("valid height"),
        );

        let state = service_fn(|_| async { unreachable!("State service should not be called") });

        let verification_result = Verifier::new_for_tests(&network, state)
            .oneshot(Request::Block {
                transaction_hash: transaction.hash(),
                transaction: Arc::new(transaction),
                known_utxos: Arc::new(known_utxos),
                known_outpoint_hashes: Arc::new(HashSet::new()),
                height,
                time: DateTime::<Utc>::MAX_UTC,
            })
            .await;

        assert_eq!(
            verification_result,
            Err(TransactionError::DuplicateTransparentSpend(
                input.outpoint().expect("Input should have an outpoint")
            ))
        );
    }
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
            .find(|(_, transaction)| transaction.has_sprout_joinsplit_data())
            .expect("No transaction found with Groth16 JoinSplits");

        let expected_hash = transaction.unmined_id();

        // Initialize the verifier
        let state_service =
            service_fn(|_| async { unreachable!("State service should not be called") });
        let verifier = Verifier::new_for_tests(&network, state_service);

        // Test the transaction verifier
        let result = verifier
            .oneshot(Request::Block {
                transaction_hash: transaction.hash(),
                transaction,
                known_utxos: Arc::new(HashMap::new()),
                known_outpoint_hashes: Arc::new(HashSet::new()),
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

    let (height, transaction) = test_transactions(&network)
        .rev()
        .filter(|(_, tx)| {
            !tx.is_coinbase() && tx.inputs().is_empty() && !tx.has_sapling_shielded_data()
        })
        .find(|(_, tx)| tx.joinsplit_count() > 0)
        .expect("There should be a tx with Groth16 JoinSplits.");

    let expected_error = Err(expected_error);

    // Serialize the transaction, apply byte-level modifications, and re-deserialize.
    let mut tx_bytes = transaction
        .zcash_serialize_to_vec()
        .expect("transaction serialization should succeed");

    match modification {
        JoinSplitModification::CorruptSignature => {
            // The joinsplit signature is the last 64 bytes of the serialized transaction.
            let sig_offset = tx_bytes.len() - 64;
            // Flip a bit from an arbitrary byte of the signature.
            tx_bytes[sig_offset + 10] ^= 0x01;
        }
        JoinSplitModification::CorruptProof => {
            // Find the first joinsplit proof in the serialized bytes and corrupt it.
            // After the Sapling data, the joinsplit section starts with nJoinSplit (compact_size),
            // then each JoinSplit contains: vpub_old(8) + vpub_new(8) + anchor(32) +
            // nullifiers(2*32) + commitments(2*32) + ephemeral_key(32) + random_seed(32) +
            // vmacs(2*32) + zkproof(192) + enc_ciphertexts(2*601)
            // We locate the proof by finding the offset of the first 192-byte proof field.
            let proof_offset = find_first_joinsplit_proof_offset(&tx_bytes);
            // A proof is composed of three field elements: first(48) + middle(96) + last(48).
            // To corrupt without making malformed, swap first and last elements.
            let (first, rest) = tx_bytes[proof_offset..proof_offset + 192].split_at_mut(48);
            let last_start = 96;
            let mut first_copy = [0u8; 48];
            first_copy.copy_from_slice(first);
            first[..48].copy_from_slice(&rest[last_start..last_start + 48]);
            rest[last_start..last_start + 48].copy_from_slice(&first_copy);
        }
        JoinSplitModification::ZeroProof => {
            let proof_offset = find_first_joinsplit_proof_offset(&tx_bytes);
            tx_bytes[proof_offset..proof_offset + 192].fill(0);
        }
    }

    let transaction: Arc<Transaction> = Arc::new(
        Transaction::zcash_deserialize(tx_bytes.as_slice())
            .expect("modified transaction should deserialize"),
    );

    // Initialize the verifier
    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called.") });
    let verifier = Verifier::new_for_tests(&network, state_service);
    let verifier = Buffer::new(verifier, 10);

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
                transaction_hash: transaction.hash(),
                transaction: transaction.clone(),
                known_utxos: Arc::new(HashMap::new()),
                known_outpoint_hashes: Arc::new(HashSet::new()),
                height,
                time: DateTime::<Utc>::MAX_UTC,
            })
            .await
            .map_err(|err| {
                *err.downcast()
                    .expect("error type should be TransactionError")
            });

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
            .find(|(_, transaction)| transaction.sapling_spends_count() > 0)
            .expect("No transaction found with Sapling spends");

        let expected_hash = transaction.unmined_id();

        // Initialize the verifier
        let state_service =
            service_fn(|_| async { unreachable!("State service should not be called") });
        let verifier = Verifier::new_for_tests(&network, state_service);

        // Test the transaction verifier
        let result = timeout(
            test_timeout(),
            verifier.oneshot(Request::Block {
                transaction_hash: transaction.hash(),
                transaction,
                known_utxos: Arc::new(HashMap::new()),
                known_outpoint_hashes: Arc::new(HashSet::new()),
                height,
                time: DateTime::<Utc>::MAX_UTC,
            }),
        )
        .await
        .expect("timeout expired");

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
            .find(|(_, transaction)| transaction.sapling_spends_count() > 0)
            .expect("No transaction found with Sapling spends");

        // Duplicate one of the spends
        let duplicate_nullifier = duplicate_sapling_spend(
            Arc::get_mut(&mut transaction).expect("Transaction only has one active reference"),
        );

        // Initialize the verifier
        let state_service =
            service_fn(|_| async { unreachable!("State service should not be called") });
        let verifier = Verifier::new_for_tests(&network, state_service);

        // Test the transaction verifier
        let result = verifier
            .oneshot(Request::Block {
                transaction_hash: transaction.hash(),
                transaction,
                known_utxos: Arc::new(HashMap::new()),
                known_outpoint_hashes: Arc::new(HashSet::new()),
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
                transaction.sapling_spends_count() == 0
                    && transaction.sapling_outputs().next().is_some()
            })
            .expect("No transaction found with Sapling outputs and no Sapling spends");

        let expected_hash = transaction.unmined_id();

        // Initialize the verifier
        let state_service =
            service_fn(|_| async { unreachable!("State service should not be called") });
        let verifier = Verifier::new_for_tests(&network, state_service);

        // Test the transaction verifier
        let result = verifier
            .oneshot(Request::Block {
                transaction_hash: transaction.hash(),
                transaction,
                known_utxos: Arc::new(HashMap::new()),
                known_outpoint_hashes: Arc::new(HashSet::new()),
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
#[tokio::test]
async fn v5_with_sapling_spends() {
    let _init_guard = zebra_test::init();

    for net in Network::iter() {
        let nu5_activation = NetworkUpgrade::Nu5.activation_height(&net);

        let tx = v5_transactions(net.block_iter())
            .filter(|tx| {
                !tx.is_coinbase() && tx.inputs().is_empty() && tx.expiry_height() >= nu5_activation
            })
            .find(|tx| tx.sapling_spends_count() > 0)
            .expect("V5 tx with Sapling spends");

        let expected_hash = tx.unmined_id();
        let height = tx.expiry_height().expect("expiry height");

        let verifier = Verifier::new_for_tests(
            &net,
            service_fn(|_| async { unreachable!("State service should not be called") }),
        );

        assert_eq!(
            timeout(
                test_timeout(),
                verifier.oneshot(Request::Block {
                    transaction_hash: tx.hash(),
                    transaction: Arc::new(tx),
                    known_utxos: Arc::new(HashMap::new()),
                    known_outpoint_hashes: Arc::new(HashSet::new()),
                    height,
                    time: DateTime::<Utc>::MAX_UTC,
                })
            )
            .await
            .expect("timeout expired")
            .expect("unexpected error response")
            .tx_id(),
            expected_hash
        );
    }
}

/// Test if a V5 transaction with a duplicate Sapling spend is rejected by the verifier.
#[tokio::test]
async fn v5_with_duplicate_sapling_spends() {
    let _init_guard = zebra_test::init();

    for net in Network::iter() {
        let mut tx = v5_transactions(net.block_iter())
            .filter(|tx| !tx.is_coinbase() && tx.inputs().is_empty())
            .find(|tx| tx.sapling_spends_count() > 0)
            .expect("V5 tx with Sapling spends");

        let height = tx.expiry_height().expect("expiry height");

        // Duplicate one of the spends
        let duplicate_nullifier = duplicate_sapling_spend(&mut tx);

        let verifier = Verifier::new_for_tests(
            &net,
            service_fn(|_| async { unreachable!("State service should not be called") }),
        );

        assert_eq!(
            verifier
                .oneshot(Request::Block {
                    transaction_hash: tx.hash(),
                    transaction: Arc::new(tx),
                    known_utxos: Arc::new(HashMap::new()),
                    known_outpoint_hashes: Arc::new(HashSet::new()),
                    height,
                    time: DateTime::<Utc>::MAX_UTC,
                })
                .await,
            Err(TransactionError::DuplicateSaplingNullifier(
                duplicate_nullifier
            ))
        );
    }
}

/// Test if a V5 transaction with a duplicate Orchard action is rejected by the verifier.
// TODO: Rewrite when Transaction API supports mutating orchard shielded data.
#[tokio::test]
#[ignore = "duplicating an orchard action requires modifying the orchard bundle proofs and action count, which is not feasible with byte-level manipulation"]
async fn v5_with_duplicate_orchard_action() {
    // TODO: restore when orchard bundle construction is available (see discussion #10463)
}

/// Checks that the tx verifier handles consensus branch ids in V5 txs correctly.
#[tokio::test]
async fn v5_consensus_branch_ids() {
    let mut state = MockService::build().for_unit_tests();

    let (input, output, known_utxos) = mock_transparent_transfer(
        Height(1),
        true,
        0,
        Amount::try_from(10001).expect("valid amount"),
    );

    let known_utxos = Arc::new(known_utxos);

    // NU5 is the first network upgrade that supports V5 txs.
    let mut network_upgrade = NetworkUpgrade::Nu5;

    let mut tx = Transaction::test_v5(
        network_upgrade,
        vec![input],
        vec![output],
        LockTime::unlocked(),
        Height::MAX_EXPIRY_HEIGHT,
    );

    let outpoint = match tx.inputs()[0] {
        transparent::Input::PrevOut { outpoint, .. } => outpoint,
        transparent::Input::Coinbase { .. } => panic!("requires a non-coinbase transaction"),
    };

    for network in Network::iter() {
        let verifier = Buffer::new(Verifier::new_for_tests(&network, state.clone()), 10);

        while let Some(next_nu) = network_upgrade.next_upgrade() {
            // Check an outdated network upgrade.
            let Some(height) = next_nu.activation_height(&network) else {
                tracing::warn!(?next_nu, "missing activation height",);
                // Shift the network upgrade for the next loop iteration.
                network_upgrade = next_nu;
                continue;
            };

            let block_req = verifier
                .clone()
                .oneshot(Request::Block {
                    transaction_hash: tx.hash(),
                    transaction: Arc::new(tx.clone()),
                    known_utxos: known_utxos.clone(),
                    known_outpoint_hashes: Arc::new(HashSet::new()),
                    // The consensus branch ID of the tx is outdated for this height.
                    height,
                    time: DateTime::<Utc>::MAX_UTC,
                })
                .map_err(|err| *err.downcast().expect("`TransactionError` type"));

            let mempool_req = verifier
                .clone()
                .oneshot(Request::Mempool {
                    transaction: std::sync::Arc::new(tx.clone()).into(),
                    // The consensus branch ID of the tx is outdated for this height.
                    height,
                })
                .map_err(|err| *err.downcast().expect("`TransactionError` type"));

            let (block_rsp, mempool_rsp) = futures::join!(block_req, mempool_req);

            assert_eq!(block_rsp, Err(TransactionError::WrongConsensusBranchId));
            assert_eq!(mempool_rsp, Err(TransactionError::WrongConsensusBranchId));

            // Check the currently supported network upgrade.
            let height = network_upgrade.activation_height(&network).expect("height");

            let block_req = verifier
                .clone()
                .oneshot(Request::Block {
                    transaction_hash: tx.hash(),
                    transaction: Arc::new(tx.clone()),
                    known_utxos: known_utxos.clone(),
                    known_outpoint_hashes: Arc::new(HashSet::new()),
                    // The consensus branch ID of the tx is supported by this height.
                    height,
                    time: DateTime::<Utc>::MAX_UTC,
                })
                .map_ok(|rsp| rsp.tx_id())
                .map_err(|e| format!("{e}"));

            let mempool_req = verifier
                .clone()
                .oneshot(Request::Mempool {
                    transaction: std::sync::Arc::new(tx.clone()).into(),
                    // The consensus branch ID of the tx is supported by this height.
                    height,
                })
                .map_ok(|rsp| rsp.tx_id())
                .map_err(|e| format!("{e}"));

            let state_req = async {
                state
                    .expect_request(zebra_state::Request::UnspentBestChainUtxo(outpoint))
                    .map(|r| {
                        r.respond(zebra_state::Response::UnspentBestChainUtxo(
                            known_utxos.get(&outpoint).map(|utxo| utxo.utxo.clone()),
                        ))
                    })
                    .await;

                state
                    .expect_request_that(|req| {
                        matches!(
                            req,
                            zebra_state::Request::CheckBestChainTipNullifiersAndAnchors(_)
                        )
                    })
                    .map(|r| {
                        r.respond(zebra_state::Response::ValidBestChainTipNullifiersAndAnchors)
                    })
                    .await;
            };

            let (block_rsp, mempool_rsp, _) = futures::join!(block_req, mempool_req, state_req);
            let txid = tx.unmined_id();

            assert_eq!(block_rsp, Ok(txid));
            assert_eq!(mempool_rsp, Ok(txid));

            // Check a network upgrade that Zebra doesn't support yet.
            tx.set_network_upgrade(next_nu);

            let height = network_upgrade.activation_height(&network).expect("height");

            let block_req = verifier
                .clone()
                .oneshot(Request::Block {
                    transaction_hash: tx.hash(),
                    transaction: Arc::new(tx.clone()),
                    known_utxos: known_utxos.clone(),
                    known_outpoint_hashes: Arc::new(HashSet::new()),
                    // The consensus branch ID of the tx is not supported by this height.
                    height,
                    time: DateTime::<Utc>::MAX_UTC,
                })
                .map_err(|err| *err.downcast().expect("`TransactionError` type"));

            let mempool_req = verifier
                .clone()
                .oneshot(Request::Mempool {
                    transaction: std::sync::Arc::new(tx.clone()).into(),
                    // The consensus branch ID of the tx is not supported by this height.
                    height,
                })
                .map_err(|err| *err.downcast().expect("`TransactionError` type"));

            let (block_rsp, mempool_rsp) = futures::join!(block_req, mempool_req);

            assert_eq!(block_rsp, Err(TransactionError::WrongConsensusBranchId));
            assert_eq!(mempool_rsp, Err(TransactionError::WrongConsensusBranchId));

            // Shift the network upgrade for the next loop iteration.
            network_upgrade = next_nu;
        }
    }
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

/// Build a V4 transaction from joinsplit data using byte-level serialization.
///
/// Creates a minimal Sapling V4 transaction (no transparent inputs/outputs, no sapling data)
/// containing the given joinsplit data.
fn build_v4_tx_with_joinsplit_data(
    joinsplit_data: Option<JoinSplitData<Groth16Proof>>,
    expiry_height: block::Height,
) -> Transaction {
    let mut bytes: Vec<u8> = Vec::new();

    // V4 overwintered header: version=4, overwintered flag set (= 0x80000004 LE)
    bytes.extend_from_slice(&0x8000_0004u32.to_le_bytes());
    // versionGroupId = SAPLING_VERSION_GROUP_ID = 0x892F2085 LE
    bytes.extend_from_slice(&0x892F_2085u32.to_le_bytes());
    // nTransparentInputs = 0 (compact_size)
    bytes.push(0x00);
    // nTransparentOutputs = 0 (compact_size)
    bytes.push(0x00);
    // nLockTime = 0
    bytes.extend_from_slice(&0u32.to_le_bytes());
    // nExpiryHeight
    bytes.extend_from_slice(&expiry_height.0.to_le_bytes());
    // valueBalanceSapling = 0 (i64 LE)
    bytes.extend_from_slice(&0i64.to_le_bytes());
    // nSpendsSapling = 0 (compact_size)
    bytes.push(0x00);
    // nOutputsSapling = 0 (compact_size)
    bytes.push(0x00);

    // Joinsplit data
    if let Some(ref jsd) = joinsplit_data {
        jsd.zcash_serialize(&mut bytes)
            .expect("joinsplit_data serialization should succeed");
    } else {
        // nJoinSplit = 0 (compact_size)
        bytes.push(0x00);
    }

    Transaction::zcash_deserialize(bytes.as_slice())
        .expect("manually constructed V4 transaction should deserialize")
}

/// Build a V4 transaction with joinsplit data and a valid ed25519 signature.
///
/// Constructs the transaction, computes the sighash, signs it, and patches the signature
/// into the serialized bytes before re-deserializing.
fn build_signed_v4_tx_with_joinsplit_data(
    joinsplit_data: JoinSplitData<Groth16Proof>,
    signing_key: &ed25519::SigningKey,
    network_upgrade: NetworkUpgrade,
    expiry_height: block::Height,
) -> Transaction {
    // Build the initial transaction with a dummy (zero) signature
    let tx = build_v4_tx_with_joinsplit_data(Some(joinsplit_data), expiry_height);

    // Compute the sighash
    let sighash = tx
        .sighash(network_upgrade, HashType::ALL, Arc::new(Vec::new()), None)
        .expect("sighash computation should succeed");

    // Sign the sighash
    let sig = signing_key.sign(sighash.as_ref());
    let sig_bytes: [u8; 64] = sig.into();

    // Serialize the transaction, patch the signature (last 64 bytes), and re-deserialize
    let mut tx_bytes = tx
        .zcash_serialize_to_vec()
        .expect("transaction serialization should succeed");
    let sig_offset = tx_bytes.len() - 64;
    tx_bytes[sig_offset..].copy_from_slice(&sig_bytes);

    Transaction::zcash_deserialize(tx_bytes.as_slice())
        .expect("signed V4 transaction should deserialize")
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

/// Find the byte offset of the first JoinSplit proof (zkproof) in a serialized V4 transaction.
///
/// Parses past the V4 header, transparent data, lock time, expiry height, sapling data,
/// and the first joinsplit fields to reach the 192-byte proof field.
fn find_first_joinsplit_proof_offset(tx_bytes: &[u8]) -> usize {
    // Parse past V4 header
    let mut pos = 8usize; // nVersion(4) + nVersionGroupId(4)

    // Parse transparent inputs
    let n_inputs = parse_compact_size(tx_bytes, &mut pos);
    for _ in 0..n_inputs {
        pos += 36; // outpoint (32 hash + 4 index)
        let script_len = parse_compact_size(tx_bytes, &mut pos);
        pos += script_len as usize + 4; // scriptSig + nSequence
    }

    // Parse transparent outputs
    let n_outputs = parse_compact_size(tx_bytes, &mut pos);
    for _ in 0..n_outputs {
        pos += 8; // value
        let script_len = parse_compact_size(tx_bytes, &mut pos);
        pos += script_len as usize; // scriptPubKey
    }

    // nLockTime(4) + nExpiryHeight(4)
    pos += 8;

    // valueBalanceSapling(8)
    pos += 8;

    // Parse Sapling spends
    let n_spends = parse_compact_size(tx_bytes, &mut pos);
    for _ in 0..n_spends {
        // cv(32) + anchor(32) + nullifier(32) + rk(32) + zkproof(192) + spendAuthSig(64)
        pos += 32 + 32 + 32 + 32 + 192 + 64;
    }

    // Parse Sapling outputs
    let n_sapling_outputs = parse_compact_size(tx_bytes, &mut pos);
    for _ in 0..n_sapling_outputs {
        // cv(32) + cmu(32) + ephemeralKey(32) + encCiphertext(580) + outCiphertext(80) + zkproof(192)
        pos += 32 + 32 + 32 + 580 + 80 + 192;
    }

    // Parse nJoinSplit
    let n_joinsplits = parse_compact_size(tx_bytes, &mut pos);
    assert!(n_joinsplits > 0, "expected at least one joinsplit");

    // First JoinSplit fields before zkproof:
    // vpub_old(8) + vpub_new(8) + anchor(32) + nullifiers(2*32) + commitments(2*32) +
    // ephemeral_key(32) + random_seed(32) + vmacs(2*32)
    pos += 8 + 8 + 32 + 64 + 64 + 32 + 32 + 64;

    // Now pos points to the first zkproof (192 bytes)
    pos
}

/// Parse a CompactSize integer from `bytes` at the given `pos`, advancing `pos`.
fn parse_compact_size(bytes: &[u8], pos: &mut usize) -> u64 {
    let first = bytes[*pos];
    *pos += 1;
    match first {
        0xfd => {
            let val = u16::from_le_bytes([bytes[*pos], bytes[*pos + 1]]);
            *pos += 2;
            val as u64
        }
        0xfe => {
            let val = u32::from_le_bytes([
                bytes[*pos],
                bytes[*pos + 1],
                bytes[*pos + 2],
                bytes[*pos + 3],
            ]);
            *pos += 4;
            val as u64
        }
        0xff => {
            let val = u64::from_le_bytes([
                bytes[*pos],
                bytes[*pos + 1],
                bytes[*pos + 2],
                bytes[*pos + 3],
                bytes[*pos + 4],
                bytes[*pos + 5],
                bytes[*pos + 6],
                bytes[*pos + 7],
            ]);
            *pos += 8;
            val
        }
        n => n as u64,
    }
}

/// Duplicate the first Sapling spend inside a transaction using byte-level serialization.
///
/// Serializes the transaction, parses to the sapling spends section, duplicates the first
/// spend description bytes, increments the spend count, and re-deserializes.
///
/// Returns the zebra `sapling::Nullifier` of the duplicated spend.
///
/// # Panics
///
/// Will panic if the transaction does not have Sapling spends.
fn duplicate_sapling_spend(transaction: &mut Transaction) -> sapling::Nullifier {
    let tx_bytes = transaction
        .zcash_serialize_to_vec()
        .expect("transaction serialization should succeed");

    let is_v5 = transaction.version() >= 5;

    if is_v5 {
        // V5 sapling spends are split across multiple sections:
        // 1. Compact spend descriptions: cv(32) + nf(32) + rk(32) = 96 bytes each
        // 2. Compact output descriptions: cv(32) + cmu(32) + epk(32) + enc(580) + out(80) = 756 bytes each
        // 3. valueBalanceSapling (8 bytes, if spends or outputs exist)
        // 4. Anchor (32 bytes, if spends > 0)
        // 5. Spend proofs (192 bytes each)
        // 6. Spend auth sigs (64 bytes each)
        // 7. Output proofs (192 bytes each)
        // 8. Binding sig (64 bytes, if spends or outputs exist)

        let mut pos = skip_v5_header_and_transparent(&tx_bytes);

        // nSpendsSapling
        let spend_count_pos = pos;
        let n_spends = parse_compact_size(&tx_bytes, &mut pos);
        assert!(n_spends > 0, "expected sapling spends");

        let first_spend_start = pos;
        let first_spend_bytes = tx_bytes[first_spend_start..first_spend_start + 96].to_vec();

        // Extract nullifier from first spend (at offset cv(32) = 32)
        let mut nf_bytes = [0u8; 32];
        nf_bytes.copy_from_slice(&tx_bytes[first_spend_start + 32..first_spend_start + 64]);
        let duplicate_nullifier = sapling::Nullifier::from(nf_bytes);

        let spends_end = first_spend_start + (n_spends as usize * 96);
        pos = spends_end;

        // nOutputsSapling + compact outputs
        let outputs_section_start = pos;
        let n_sapling_outputs = parse_compact_size(&tx_bytes, &mut pos);
        pos += n_sapling_outputs as usize * 756;

        let has_sapling_data = n_spends > 0 || n_sapling_outputs > 0;

        // valueBalanceSapling
        if has_sapling_data {
            pos += 8;
        }

        // Anchor
        if n_spends > 0 {
            pos += 32;
        }

        // Save the position after outputs+valueBalance+anchor (before proofs)
        let pre_proofs_end = pos;

        // Spend proofs
        let proof_section_start = pos;
        let first_proof_bytes = tx_bytes[proof_section_start..proof_section_start + 192].to_vec();
        pos += n_spends as usize * 192;

        // Spend auth sigs
        let sig_section_start = pos;
        let first_sig_bytes = tx_bytes[sig_section_start..sig_section_start + 64].to_vec();
        pos += n_spends as usize * 64;

        // Remainder: output proofs + binding sig + orchard section
        let remainder = tx_bytes[pos..].to_vec();

        // Rebuild with duplicated first spend
        let new_n_spends = n_spends + 1;
        let mut new_bytes = Vec::new();

        // Everything before the spend count
        new_bytes.extend_from_slice(&tx_bytes[..spend_count_pos]);

        // New spend count
        write_compact_size(&mut new_bytes, new_n_spends);

        // Original compact spends + duplicate first
        new_bytes.extend_from_slice(&tx_bytes[first_spend_start..spends_end]);
        new_bytes.extend_from_slice(&first_spend_bytes);

        // Outputs section + valueBalance + anchor (unchanged)
        new_bytes.extend_from_slice(&tx_bytes[outputs_section_start..pre_proofs_end]);

        // Spend proofs: original + duplicate first
        new_bytes.extend_from_slice(
            &tx_bytes[proof_section_start..proof_section_start + n_spends as usize * 192],
        );
        new_bytes.extend_from_slice(&first_proof_bytes);

        // Spend auth sigs: original + duplicate first
        new_bytes.extend_from_slice(
            &tx_bytes[sig_section_start..sig_section_start + n_spends as usize * 64],
        );
        new_bytes.extend_from_slice(&first_sig_bytes);

        // Remainder (output proofs, binding sig, orchard)
        new_bytes.extend_from_slice(&remainder);

        *transaction = Transaction::zcash_deserialize(new_bytes.as_slice())
            .expect("modified V5 transaction with duplicated sapling spend should deserialize");

        duplicate_nullifier
    } else {
        // V4 transaction layout:
        // After header, transparent data, locktime, expiryHeight, valueBalanceSapling,
        // nSpendsSapling (compact_size), then each spend: cv(32) + anchor(32) + nullifier(32) + rk(32) + zkproof(192) + spendAuthSig(64) = 384 bytes

        let mut pos = 8usize; // nVersion(4) + nVersionGroupId(4)

        // Parse transparent inputs
        let n_inputs = parse_compact_size(&tx_bytes, &mut pos);
        for _ in 0..n_inputs {
            pos += 36;
            let script_len = parse_compact_size(&tx_bytes, &mut pos);
            pos += script_len as usize + 4;
        }

        // Parse transparent outputs
        let n_outputs = parse_compact_size(&tx_bytes, &mut pos);
        for _ in 0..n_outputs {
            pos += 8;
            let script_len = parse_compact_size(&tx_bytes, &mut pos);
            pos += script_len as usize;
        }

        // nLockTime(4) + nExpiryHeight(4) + valueBalanceSapling(8)
        pos += 16;

        // nSpendsSapling
        let spend_count_pos = pos;
        let n_spends = parse_compact_size(&tx_bytes, &mut pos);
        assert!(n_spends > 0, "expected sapling spends");

        let first_spend_start = pos;
        // Each V4 spend is 384 bytes: cv(32) + anchor(32) + nullifier(32) + rk(32) + zkproof(192) + spendAuthSig(64)
        let spend_size = 384;
        let first_spend_bytes =
            tx_bytes[first_spend_start..first_spend_start + spend_size].to_vec();

        // Extract nullifier from first spend (at offset cv(32) + anchor(32) = 64)
        let nf_start = first_spend_start + 64;
        let mut nf_bytes = [0u8; 32];
        nf_bytes.copy_from_slice(&tx_bytes[nf_start..nf_start + 32]);
        let duplicate_nullifier = sapling::Nullifier::from(nf_bytes);

        let spends_end = first_spend_start + (n_spends as usize * spend_size);

        // Rebuild with duplicated first spend
        let new_n_spends = n_spends + 1;
        let mut new_bytes = Vec::new();

        // Everything before the spend count
        new_bytes.extend_from_slice(&tx_bytes[..spend_count_pos]);

        // New spend count
        write_compact_size(&mut new_bytes, new_n_spends);

        // Original spends + duplicate first
        new_bytes.extend_from_slice(&tx_bytes[first_spend_start..spends_end]);
        new_bytes.extend_from_slice(&first_spend_bytes);

        // Everything after spends
        new_bytes.extend_from_slice(&tx_bytes[spends_end..]);

        *transaction = Transaction::zcash_deserialize(new_bytes.as_slice())
            .expect("modified V4 transaction with duplicated sapling spend should deserialize");

        duplicate_nullifier
    }
}

/// Graft orchard data from a donor V5 transaction onto a target V5 transaction.
///
/// Finds a V5 non-coinbase transaction with orchard data from the network's test blocks,
/// extracts the orchard section, replaces the target's orchard section with it,
/// and optionally overrides the flags byte.
fn graft_orchard_data_onto_v5_tx(
    target: &Transaction,
    net: &Network,
    override_flags: Option<u8>,
) -> Transaction {
    // Find a V5 tx with orchard data to use as donor
    let donor = v5_transactions(net.block_iter())
        .find(|tx| tx.has_orchard_shielded_data())
        .expect("V5 tx with orchard data");

    let target_bytes = target
        .zcash_serialize_to_vec()
        .expect("target serialization should succeed");
    let donor_bytes = donor
        .zcash_serialize_to_vec()
        .expect("donor serialization should succeed");

    // Find the start of the orchard section in both transactions
    let target_orchard_start = find_v5_orchard_section_start(&target_bytes);
    let donor_orchard_start = find_v5_orchard_section_start(&donor_bytes);

    // Build the new transaction: target header + transparent + sapling, then donor orchard
    let mut new_bytes = target_bytes[..target_orchard_start].to_vec();
    new_bytes.extend_from_slice(&donor_bytes[donor_orchard_start..]);

    // Override flags if requested
    if let Some(flags) = override_flags {
        let flags_offset = find_v5_orchard_flags_offset(&new_bytes);
        new_bytes[flags_offset] = flags;
    }

    Transaction::zcash_deserialize(new_bytes.as_slice())
        .expect("grafted V5 transaction should deserialize")
}

/// Skip past the V5 header and transparent section, returning the position
/// just after transparent outputs (at the start of the sapling section).
fn skip_v5_header_and_transparent(tx_bytes: &[u8]) -> usize {
    // V5 header: version(4) + versionGroupId(4) + consensusBranchId(4) + lockTime(4) + expiryHeight(4)
    let mut pos = 20usize;

    // Parse transparent inputs
    let n_inputs = parse_compact_size(tx_bytes, &mut pos);
    for _ in 0..n_inputs {
        pos += 36;
        let script_len = parse_compact_size(tx_bytes, &mut pos);
        pos += script_len as usize + 4;
    }

    // Parse transparent outputs
    let n_outputs = parse_compact_size(tx_bytes, &mut pos);
    for _ in 0..n_outputs {
        pos += 8;
        let script_len = parse_compact_size(tx_bytes, &mut pos);
        pos += script_len as usize;
    }

    pos
}

/// Skip past the V5 sapling section, returning the position of the orchard section.
///
/// `pos` should be at the start of the sapling section.
fn skip_v5_sapling_section(tx_bytes: &[u8], pos: &mut usize) {
    let n_spends = parse_compact_size(tx_bytes, pos);
    *pos += n_spends as usize * 96; // compact spends: cv(32) + nf(32) + rk(32)

    let n_sapling_outputs = parse_compact_size(tx_bytes, pos);
    *pos += n_sapling_outputs as usize * 756; // compact outputs: cv(32) + cmu(32) + epk(32) + enc(580) + out(80)

    let has_sapling_data = n_spends > 0 || n_sapling_outputs > 0;

    if has_sapling_data {
        *pos += 8; // valueBalanceSapling (i64)
    }

    if n_spends > 0 {
        *pos += 32; // anchor
    }
    *pos += n_spends as usize * 192; // spend proofs
    *pos += n_spends as usize * 64; // spend auth sigs
    *pos += n_sapling_outputs as usize * 192; // output proofs

    if has_sapling_data {
        *pos += 64; // binding sig
    }
}

/// Find the byte offset where the orchard section starts in a serialized V5 transaction.
///
/// This is the position of nActionsOrchard (compact_size).
fn find_v5_orchard_section_start(tx_bytes: &[u8]) -> usize {
    let mut pos = skip_v5_header_and_transparent(tx_bytes);
    skip_v5_sapling_section(tx_bytes, &mut pos);
    pos
}

/// Find the byte offset of the orchard flags byte in a serialized V5 transaction.
///
/// Parses past the V5 header, transparent section, and sapling section to the orchard
/// section, then past the actions to the flags byte.
fn find_v5_orchard_flags_offset(tx_bytes: &[u8]) -> usize {
    let mut pos = find_v5_orchard_section_start(tx_bytes);

    // Orchard section: nActionsOrchard
    let n_actions = parse_compact_size(tx_bytes, &mut pos);
    assert!(n_actions > 0, "expected orchard actions");

    // Each action: cv(32) + nullifier(32) + rk(32) + cmx(32) + ephemeralKey(32) +
    //              encCiphertext(580) + outCiphertext(80) = 820 bytes
    pos += n_actions as usize * 820;

    // Now pos points to flagsOrchard (1 byte)
    pos
}

/// Write a CompactSize integer to `bytes`.
fn write_compact_size(bytes: &mut Vec<u8>, value: u64) {
    if value < 0xfd {
        bytes.push(value as u8);
    } else if value <= 0xffff {
        bytes.push(0xfd);
        bytes.extend_from_slice(&(value as u16).to_le_bytes());
    } else if value <= 0xffff_ffff {
        bytes.push(0xfe);
        bytes.extend_from_slice(&(value as u32).to_le_bytes());
    } else {
        bytes.push(0xff);
        bytes.extend_from_slice(&value.to_le_bytes());
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
    let vpub_old_sum: i64 = block.transactions[4]
        .output_values_to_sprout()
        .into_iter()
        .sum();
    let vpub_old = Amount::<NonNegative>::try_from(vpub_old_sum).expect("valid vpub_old sum");
    assert!(vpub_old > zero);

    assert_eq!(
        check::disabled_add_to_sprout_pool(&block.transactions[3], block_height, &network),
        Err(TransactionError::DisabledAddToSproutPool)
    );

    // the 8th transaction has joinsplits and the `vpub_old` cumulative is 0,
    // should pass the check.
    assert!(block.transactions[7].joinsplit_count() > 0);
    let vpub_old_sum: i64 = block.transactions[7]
        .output_values_to_sprout()
        .into_iter()
        .sum();
    let vpub_old = Amount::<NonNegative>::try_from(vpub_old_sum).expect("valid vpub_old sum");
    assert_eq!(vpub_old, zero);

    assert_eq!(
        check::disabled_add_to_sprout_pool(&block.transactions[7], block_height, &network),
        Ok(())
    );
}

/// Checks that Heartwood onward, all Sapling and Orchard outputs in coinbase txs decrypt to a note
/// plaintext, i.e. the procedure in § 4.20.3 ‘Decryption using a Full Viewing Key (Sapling and
/// Orchard )’ does not return ⊥, using a sequence of 32 zero bytes as the outgoing viewing key. We
/// will refer to such a sequence as the _zero key_.
#[test]
fn coinbase_outputs_are_decryptable() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    for net in Network::iter() {
        let mut tested_post_heartwood_shielded_coinbase_tx = false;
        let mut tested_pre_heartwood_shielded_coinbase_tx = false;

        let mut tested_post_heartwood_unshielded_coinbase_tx = false;
        let mut tested_pre_heartwood_unshielded_coinbase_tx = false;

        let mut tested_post_heartwood_shielded_non_coinbase_tx = false;
        let mut tested_pre_heartwood_shielded_non_coinbase_tx = false;

        let mut tested_post_heartwood_unshielded_non_coinbase_tx = false;
        let mut tested_pre_heartwood_unshielded_non_coinbase_tx = false;

        for (height, block) in net.block_iter() {
            let block = block.zcash_deserialize_into::<Block>().expect("block");
            let height = Height(*height);
            let is_heartwood = height >= NetworkUpgrade::Heartwood.activation_height(&net).unwrap();
            let coinbase = block.transactions.first().expect("coinbase transaction");

            if coinbase.has_shielded_outputs() && is_heartwood {
                tested_post_heartwood_shielded_coinbase_tx = true;
                check::coinbase_outputs_are_decryptable(coinbase, &net, height).expect(
                    "post-Heartwood shielded coinbase outputs must be decryptable with the zero key",
                );
            }

            if coinbase.has_shielded_outputs() && !is_heartwood {
                tested_pre_heartwood_shielded_coinbase_tx = true;
                check::coinbase_outputs_are_decryptable(coinbase, &net, height)
                    .expect("the consensus rule does not apply to pre-Heartwood txs");
            }

            if !coinbase.has_shielded_outputs() && is_heartwood {
                tested_post_heartwood_unshielded_coinbase_tx = true;
                check::coinbase_outputs_are_decryptable(coinbase, &net, height)
                    .expect("the consensus rule does not apply to txs with no shielded outputs");
            }

            if !coinbase.has_shielded_outputs() && !is_heartwood {
                tested_pre_heartwood_unshielded_coinbase_tx = true;
                check::coinbase_outputs_are_decryptable(coinbase, &net, height)
                    .expect("the consensus rule does not apply to pre-Heartwood txs");
            }

            // For non-coinbase txs, check if existing outputs are NOT decryptable with an all-zero
            // key, if applicable.
            for non_coinbase in block.transactions.iter().skip(1) {
                if non_coinbase.has_shielded_outputs() && is_heartwood {
                    tested_post_heartwood_shielded_non_coinbase_tx = true;
                    assert_eq!(
                        check::coinbase_outputs_are_decryptable(non_coinbase, &net, height),
                        Err(TransactionError::NotCoinbase)
                    )
                }

                if non_coinbase.has_shielded_outputs() && !is_heartwood {
                    tested_pre_heartwood_shielded_non_coinbase_tx = true;
                    check::coinbase_outputs_are_decryptable(non_coinbase, &net, height)
                        .expect("the consensus rule does not apply to pre-Heartwood txs");
                }

                if !non_coinbase.has_shielded_outputs() && is_heartwood {
                    tested_post_heartwood_unshielded_non_coinbase_tx = true;
                    check::coinbase_outputs_are_decryptable(non_coinbase, &net, height).expect(
                        "the consensus rule does not apply to txs with no shielded outputs",
                    );
                }

                if !non_coinbase.has_shielded_outputs() && !is_heartwood {
                    tested_pre_heartwood_unshielded_non_coinbase_tx = true;
                    check::coinbase_outputs_are_decryptable(non_coinbase, &net, height)
                        .expect("the consensus rule does not apply to pre-Heartwood txs");
                }
            }
        }

        assert!(tested_post_heartwood_shielded_coinbase_tx);
        // We have no pre-Heartwood shielded coinbase txs.
        assert!(!tested_pre_heartwood_shielded_coinbase_tx);
        assert!(tested_post_heartwood_unshielded_coinbase_tx);
        assert!(tested_pre_heartwood_unshielded_coinbase_tx);
        assert!(tested_post_heartwood_shielded_non_coinbase_tx);
        assert!(tested_pre_heartwood_shielded_non_coinbase_tx);
        assert!(tested_post_heartwood_unshielded_non_coinbase_tx);
        assert!(tested_pre_heartwood_unshielded_non_coinbase_tx);
    }

    Ok(())
}

/// Given an Orchard action as a base, fill fields related to note encryption
/// from the given test vector and returned the modified action.
#[allow(dead_code)] // Only used by #[ignore]d tests pending insert_fake_orchard_shielded_data
fn fill_action_with_note_encryption_test_vector(
    action: &Action,
    v: &zebra_test::vectors::TestVector,
) -> Action {
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

/// Test if shielded coinbase outputs are decryptable with an all-zero outgoing viewing key.
// TODO: Rewrite when Transaction API supports insert_fake_orchard_shielded_data.
#[test]
#[ignore = "requires constructing custom orchard actions with specific note encryption test vector fields, not feasible with byte-level manipulation"]
fn coinbase_outputs_are_decryptable_for_fake_v5_blocks() {
    // TODO: restore when orchard bundle construction is available (see discussion #10463)
}

/// Test if random shielded outputs are NOT decryptable with an all-zero outgoing viewing key.
// TODO: Rewrite when Transaction API supports insert_fake_orchard_shielded_data.
#[test]
#[ignore = "requires constructing custom orchard actions with specific note encryption test vector fields, not feasible with byte-level manipulation"]
fn shielded_outputs_are_not_decryptable_for_fake_v5_blocks() {
    // TODO: restore when orchard bundle construction is available (see discussion #10463)
}

#[tokio::test]
async fn mempool_zip317_error() {
    let mut state: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let verifier = Verifier::new_for_tests(&Network::Mainnet, state.clone());

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
    let tx = Transaction::test_v5(
        NetworkUpgrade::Nu5,
        vec![input],
        vec![output],
        LockTime::unlocked(),
        height,
    );

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
            transaction: std::sync::Arc::new(tx).into(),
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
    let verifier = Verifier::new_for_tests(&Network::Mainnet, state.clone());

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
    let tx = Transaction::test_v5(
        NetworkUpgrade::Nu5,
        vec![input],
        vec![output],
        LockTime::unlocked(),
        height,
    );

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
            transaction: std::sync::Arc::new(tx).into(),
            height,
        })
        .await;

    assert!(
        verifier_response.is_ok(),
        "expected successful verification, got: {verifier_response:?}"
    );
}

// TODO: Rewrite when Transaction API supports constructing/mutating orchard shielded data.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires constructing and modifying orchard bundles with garbage proofs, not feasible with byte-level manipulation"]
async fn mempool_skip_accepts_block_with_garbage_orchard_proofs() {
    // TODO: restore when orchard bundle construction is available (see discussion #10463)
}
