//! Tests for Zcash transaction consensus checks.

#![allow(clippy::unwrap_in_result)]

//
// TODO: split fixed test vectors into a `vectors` module?

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, TimeZone, Utc};
use color_eyre::eyre::Report;
use futures::{FutureExt, TryFutureExt};
use tokio::time::timeout;
use tower::{buffer::Buffer, service_fn, ServiceExt};

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, Block, Height},
    parameters::{
        testnet::{ConfiguredActivationHeights, Parameters},
        Network, NetworkUpgrade,
    },
    primitives::{ed25519, x25519, Groth16Proof},
    sapling,
    serialization::{DateTime32, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
    sprout,
    transaction::{
        arbitrary::{
            insert_fake_orchard_shielded_data, test_transactions, transactions_from_blocks,
            v5_transactions, with_garbage_orchard_authorization, with_orchard_flags,
            with_orchard_value_balance,
        },
        zip317, Hash, HashType, JoinSplitData, LockTime, Transaction,
    },
    transparent::{self, CoinbaseSpendRestriction},
};

use zebra_node_services::mempool;
use zebra_state::ValidateContextError;
use zebra_test::mock_service::MockService;

use crate::{error::TransactionError, transaction::POLL_MEMPOOL_DELAY};

use super::{check, BlockRequest, BlockTxVerifier, MempoolRequest, MempoolTxVerifier};

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
            Err(TransactionError::NotEnoughOrchardFlags)
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

/// Tests the `[NU6.3 onward] valueBalanceOrchard MUST be nonnegative` rule: from NU6.3 the Orchard
/// pool is frozen against new inflows (newly shielded value is routed to Ironwood).
#[test]
fn orchard_value_balance_frozen_at_nu6_3() {
    let _init_guard = zebra_test::init();

    // NU6.3 is unscheduled on Mainnet/Testnet, so the rule is unreachable there; use a network
    // that schedules it.
    let network = Network::new_regtest(
        ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(2),
            nu6: Some(3),
            nu6_1: Some(4),
            nu6_2: Some(5),
            nu6_3: Some(10),
            ..Default::default()
        }
        .into(),
    );

    let nu6_3_height = NetworkUpgrade::Nu6_3
        .activation_height(&network)
        .expect("NU6.3 activation height is configured");
    let pre_nu6_3_height = NetworkUpgrade::Nu6_2
        .activation_height(&network)
        .expect("NU6.2 activation height is configured");

    // A real V5 transaction carrying an Orchard bundle; its value balance is overridden below.
    let mut tx = v5_transactions(Network::Mainnet.block_iter())
        .find(|tx| tx.has_orchard_shielded_data())
        .expect("a V5 transaction with an Orchard bundle");

    // A net-negative `valueBalanceOrchard` shields new value into the Orchard pool. The check has
    // no coinbase exemption, so it applies to coinbase transactions too.
    tx = with_orchard_value_balance(tx, -1);

    // Rejected from NU6.3 onward,
    assert_eq!(
        check::orchard_value_balance_non_negative(
            &tx,
            NetworkUpgrade::current(&network, nu6_3_height)
        ),
        Err(TransactionError::NegativeOrchardValueBalance),
    );
    // but allowed before NU6.3, where the Orchard pool is not yet frozen.
    assert!(check::orchard_value_balance_non_negative(
        &tx,
        NetworkUpgrade::current(&network, pre_nu6_3_height)
    )
    .is_ok());

    // A zero balance (Orchard-to-Orchard note management) is allowed at NU6.3.
    tx = with_orchard_value_balance(tx, 0);
    assert!(check::orchard_value_balance_non_negative(
        &tx,
        NetworkUpgrade::current(&network, nu6_3_height)
    )
    .is_ok());

    // A positive balance (Orchard-to-transparent unshielding) is allowed at NU6.3.
    tx = with_orchard_value_balance(tx, 1);
    assert!(check::orchard_value_balance_non_negative(
        &tx,
        NetworkUpgrade::current(&network, nu6_3_height)
    )
    .is_ok());

    // A transaction with no Orchard bundle is unaffected at NU6.3.
    let no_orchard_tx = Transaction::test_v5(
        NetworkUpgrade::Nu5,
        Vec::new(),
        Vec::new(),
        LockTime::Height(Height(0)),
        Height(0),
    );
    assert!(check::orchard_value_balance_non_negative(
        &no_orchard_tx,
        NetworkUpgrade::current(&network, nu6_3_height)
    )
    .is_ok());
}

/// Tests the `[NU6.3 onward] if there are Ironwood actions, at least one of enableSpendsIronwood
/// and enableOutputsIronwood MUST be 1` rule.
///
/// `orchard::Flags` cannot represent a flag set with neither spends nor outputs enabled, so such
/// a bundle can only enter the node over the wire. The test therefore builds a valid transaction
/// and clears the Ironwood flag byte before re-parsing it, which is the path a peer would take.
#[test]
fn v6_transaction_with_ironwood_actions_must_have_flags() {
    use zebra_chain::{
        serialization::{ZcashDeserializeInto, ZcashSerialize},
        transaction::arbitrary::{
            fake_bundle_for_branch, fake_v6_transaction, v6_ironwood_flags_offset,
        },
    };

    let branch_id = zcash_protocol::consensus::BranchId::Nu6_3;
    let ironwood = fake_bundle_for_branch(branch_id, ::orchard::ValuePool::Ironwood, 1, 5)
        .expect("the Ironwood pool is defined at NU6.3");

    // With flags set, the rule passes.
    let tx = fake_v6_transaction(NetworkUpgrade::Nu6_3, None, Some(ironwood));
    assert!(check::has_enough_ironwood_flags(&tx).is_ok());

    // Clearing the Ironwood flag byte disables both spends and outputs, which the rule rejects.
    let mut bytes = tx
        .zcash_serialize_to_vec()
        .expect("v6 transaction serializes");
    bytes[v6_ironwood_flags_offset(1)] = 0;

    let tx: Transaction = bytes
        .zcash_deserialize_into()
        .expect("a bundle with no flags parses; the rule is enforced by the verifier");
    assert_eq!(
        check::has_enough_ironwood_flags(&tx),
        Err(TransactionError::NotEnoughIronwoodFlags),
    );

    // A transaction with no Ironwood bundle is a no-op.
    let tx = fake_v6_transaction(NetworkUpgrade::Nu6_3, None, None);
    assert!(check::has_enough_ironwood_flags(&tx).is_ok());
}

/// Tests the `[NU6.3 onward] the enableCrossAddress flag of flagsOrchard MUST be 0` rule.
///
/// `orchard::Flags` cannot represent an Orchard-pool bundle with `enableCrossAddress` set under
/// `BundleVersion::orchard_v3`, and the wire codec rejects the flag bit for that pool, so this
/// consensus check is defense-in-depth that no reachable transaction can trip. Both of those
/// guards are covered by `v6_orchard_bundle_rejects_cross_address_flag_on_the_wire` in
/// zebra-chain; here we check that a well-formed v6 Orchard bundle passes the rule.
#[test]
fn v6_orchard_bundle_must_not_enable_cross_address() {
    use zebra_chain::transaction::arbitrary::{fake_bundle_for_branch, fake_v6_transaction};

    let branch_id = zcash_protocol::consensus::BranchId::Nu6_3;
    let orchard = fake_bundle_for_branch(branch_id, ::orchard::ValuePool::Orchard, 1, 9)
        .expect("the Orchard pool is defined at NU6.3");

    assert!(
        !orchard.flags().cross_address_enabled(),
        "an orchard_v3 bundle cannot enable cross-address transfers",
    );

    let tx = fake_v6_transaction(NetworkUpgrade::Nu6_3, Some(orchard), None);
    assert!(check::orchard_cross_address_disabled(&tx).is_ok());
}

/// Tests the `[NU6.3 onward] coinbase transactions MUST have an empty Orchard component` rule
/// (ZIP-229). The rule applies to every transaction version, so a v5 coinbase carrying Orchard
/// actions is rejected from NU6.3 onward, even though the v5 format itself is unchanged.
#[test]
fn coinbase_orchard_component_empty_at_nu6_3() {
    let _init_guard = zebra_test::init();

    // NU6.3 is unscheduled on Mainnet/Testnet, so use a network that schedules it.
    let network = Network::new_regtest(
        ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(2),
            nu6: Some(3),
            nu6_1: Some(4),
            nu6_2: Some(5),
            nu6_3: Some(10),
            ..Default::default()
        }
        .into(),
    );

    let nu6_3_height = NetworkUpgrade::Nu6_3
        .activation_height(&network)
        .expect("NU6.3 activation height is configured");
    let pre_nu6_3_height = NetworkUpgrade::Nu6_2
        .activation_height(&network)
        .expect("NU6.2 activation height is configured");

    // A real V5 coinbase transaction; its Orchard component is inserted below.
    let tx = v5_transactions(Network::Mainnet.block_iter())
        .find(|transaction| transaction.is_coinbase())
        .expect("a V5 coinbase transaction");

    // A coinbase with no Orchard component is always accepted.
    assert!(check::coinbase_orchard_component_empty(
        &tx,
        NetworkUpgrade::current(&network, nu6_3_height)
    )
    .is_ok());

    // Give the coinbase a (non-empty) Orchard component.
    let tx = insert_fake_orchard_shielded_data(tx);
    assert!(tx.is_coinbase() && tx.has_orchard_shielded_data());

    // Rejected from NU6.3 onward,
    assert_eq!(
        check::coinbase_orchard_component_empty(
            &tx,
            NetworkUpgrade::current(&network, nu6_3_height)
        ),
        Err(TransactionError::CoinbaseHasOrchardActions),
    );
    // but allowed before NU6.3, where coinbase Orchard outputs are still permitted.
    assert!(check::coinbase_orchard_component_empty(
        &tx,
        NetworkUpgrade::current(&network, pre_nu6_3_height)
    )
    .is_ok());

    // The rule only constrains coinbase transactions: a non-coinbase tx with an Orchard component
    // is unaffected at NU6.3.
    let non_coinbase = v5_transactions(Network::Mainnet.block_iter())
        .find(|transaction| !transaction.is_coinbase())
        .expect("a non-coinbase V5 transaction");
    let non_coinbase = insert_fake_orchard_shielded_data(non_coinbase);
    assert!(check::coinbase_orchard_component_empty(
        &non_coinbase,
        NetworkUpgrade::current(&network, nu6_3_height)
    )
    .is_ok());
}

/// Tests that a transaction revealing the same Ironwood nullifier twice is rejected as a
/// double-spend, and that the Ironwood and Orchard nullifier sets are checked separately.
#[test]
fn v6_transaction_with_duplicate_ironwood_nullifier_is_rejected() {
    use ::orchard::bundle::{BundleVersion, Flags as OrchardFlags};
    use zcash_protocol::value::ZatBalance;
    use zebra_chain::transaction::arbitrary::{
        fake_bundle_for_branch, fake_orchard_bundle_duplicate_nullifiers, fake_v6_transaction,
    };

    let zero = ZatBalance::from_i64(0).expect("zero is a valid balance");
    let branch_id = zcash_protocol::consensus::BranchId::Nu6_3;

    // Two Ironwood actions sharing a nullifier are a double-spend.
    let ironwood = fake_orchard_bundle_duplicate_nullifiers(
        OrchardFlags::ENABLED,
        zero,
        2,
        11,
        BundleVersion::ironwood_v3(),
    );
    let tx = fake_v6_transaction(NetworkUpgrade::Nu6_3, None, Some(ironwood));
    assert!(matches!(
        check::spend_conflicts(&tx),
        Err(TransactionError::DuplicateIronwoodNullifier(_)),
    ));

    // Distinct nullifiers have no conflict.
    let ironwood = fake_bundle_for_branch(branch_id, ::orchard::ValuePool::Ironwood, 2, 11)
        .expect("the Ironwood pool is defined at NU6.3");
    let tx = fake_v6_transaction(NetworkUpgrade::Nu6_3, None, Some(ironwood));
    assert!(check::spend_conflicts(&tx).is_ok());
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
        let verifier = MempoolTxVerifier::new_for_tests(&net, state.clone());

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

        let verifier_req = verifier.oneshot(MempoolRequest {
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
    let verifier = MempoolTxVerifier::new_for_tests(&Network::Mainnet, state.clone());

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
        .oneshot(MempoolRequest {
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
    let verifier = MempoolTxVerifier::new_for_tests(&Network::Mainnet, state.clone());

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
        .oneshot(MempoolRequest {
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
    let verifier = MempoolTxVerifier::new_for_tests(&Network::Mainnet, state.clone());

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
        .oneshot(MempoolRequest {
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
    let verifier = MempoolTxVerifier::new_for_tests(&Network::Mainnet, state.clone());

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
        .oneshot(MempoolRequest {
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
    let verifier = MempoolTxVerifier::new_for_tests(&Network::Mainnet, state.clone());

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
        .oneshot(MempoolRequest {
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
    let verifier = MempoolTxVerifier::new(&Network::Mainnet, state.clone(), mempool_setup_rx);
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
        .oneshot(MempoolRequest {
            transaction: std::sync::Arc::new(tx).into(),
            height,
        })
        .await;

    assert!(
        verifier_response.is_ok(),
        "expected successful verification, got: {verifier_response:?}"
    );

    let crate::transaction::MempoolResponse {
        transaction: _,
        spent_mempool_outpoints,
    } = verifier_response.expect("already checked that response is ok");

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

/// Confirms block verification always queries the state service fresh, even
/// when the same transaction was already verified and cached by the mempool
/// verifier. `BlockTxVerifier` has no dependency on any mempool service, so
/// this is now also enforced structurally; this test additionally checks the
/// runtime behavior when both verifiers share a state backend.
#[tokio::test(flavor = "multi_thread")]
async fn block_verification_does_not_use_mempool_verified_state() {
    let mut state: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let mempool: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let (mempool_setup_tx, mempool_setup_rx) = tokio::sync::oneshot::channel();
    let mempool_verifier = Buffer::new(
        MempoolTxVerifier::new(&Network::Mainnet, state.clone(), mempool_setup_rx),
        1,
    );
    let block_verifier = Buffer::new(BlockTxVerifier::new(&Network::Mainnet, state.clone()), 1);

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

    let verifier_response = mempool_verifier
        .clone()
        .oneshot(MempoolRequest {
            transaction: std::sync::Arc::new(tx.clone()).into(),
            height,
        })
        .await;

    assert!(
        verifier_response.is_ok(),
        "expected successful verification, got: {verifier_response:?}"
    );

    let crate::transaction::MempoolResponse {
        transaction: _,
        spent_mempool_outpoints,
    } = verifier_response.expect("already checked that response is ok");

    assert_eq!(
        spent_mempool_outpoints,
        vec![input_outpoint],
        "spent_mempool_outpoints in tx verifier response should match input_outpoint"
    );

    let make_request = || BlockRequest {
        transaction_hash: tx_hash,
        transaction: Arc::new(tx.clone()),
        known_utxos: Arc::new(HashMap::new()),
        height,
        time: Utc::now(),
    };

    // The mempool has already verified this transaction, and it is now submitted twice as a block
    // request. Each request runs full block verification independently, which for this transaction
    // means fetching the spent UTXO from the state service, so two AwaitUtxo responses are queued
    // below. Reuse of the mempool's result — or of the first block request's result — is prevented
    // structurally: BlockTxVerifier holds no mempool handle and caches nothing across requests.
    let utxo_clone = utxo.clone();
    tokio::spawn(async move {
        state
            .expect_request(zebra_state::Request::AwaitUtxo(input_outpoint))
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::Utxo(utxo_clone));

        state
            .expect_request(zebra_state::Request::AwaitUtxo(input_outpoint))
            .await
            .expect("verifier should call mock state service with correct request")
            .respond(zebra_state::Response::Utxo(utxo));
    });

    // Briefly yield and sleep so the spawned task can first expect the requests.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let crate::transaction::BlockResponse { .. } = block_verifier
        .clone()
        .oneshot(make_request())
        .await
        .expect("should succeed after calling state service");

    let crate::transaction::BlockResponse { .. } = block_verifier
        .clone()
        .oneshot(make_request())
        .await
        .expect("should succeed after calling state service");

    tokio::time::sleep(POLL_MEMPOOL_DELAY * 2).await;
    // polled before AwaitOutput request and after a mempool transaction with transparent outputs
    // is successfully verified.
    assert_eq!(
        mempool.poll_count(),
        2,
        "the mempool service should have been polled twice"
    );
}

/// Tests that calls to the transaction verifier with a mempool request that spends
/// immature coinbase outputs will return an error.
#[tokio::test]
async fn mempool_request_with_immature_spend_is_rejected() {
    let _init_guard = zebra_test::init();

    let mut state: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let verifier = MempoolTxVerifier::new_for_tests(&Network::Mainnet, state.clone());

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
        .oneshot(MempoolRequest {
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
    let verifier = MempoolTxVerifier::new_for_tests(&network, state.clone());

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
        .oneshot(MempoolRequest {
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
    let verifier = MempoolTxVerifier::new_for_tests(&Network::Mainnet, state.clone());

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
        .oneshot(MempoolRequest {
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
        let verifier = BlockTxVerifier::new(
            &net,
            service_fn(|_| async { unreachable!("Service should not be called") }),
        );

        let tx = v5_transactions(net.block_iter()).next().expect("V5 tx");

        assert_eq!(
            verifier
                .oneshot(BlockRequest {
                    transaction_hash: tx.hash(),
                    transaction: Arc::new(tx),
                    known_utxos: Arc::new(HashMap::new()),
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

        let verif_res = BlockTxVerifier::new(&net, state)
            .oneshot(BlockRequest {
                transaction_hash: tx.hash(),
                transaction: Arc::new(tx),
                known_utxos: Arc::new(HashMap::new()),
                height: tx_height,
                time: DateTime::<Utc>::MAX_UTC,
            })
            .await;

        assert_eq!(verif_res.expect("success").tx_id, expected);
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
    let verifier = BlockTxVerifier::new(&network, state_service);

    let result = verifier
        .oneshot(BlockRequest {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(known_utxos),
            height: transaction_block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result.expect("unexpected error response").tx_id,
        transaction_hash
    );
}

/// Tests if a non-coinbase V4 transaction with the last valid expiry height is
/// accepted.
#[tokio::test]
async fn v4_transaction_with_last_valid_expiry_height() {
    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = BlockTxVerifier::new(&Network::Mainnet, state_service);

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
        .oneshot(BlockRequest {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(known_utxos),
            height: block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result.expect("unexpected error response").tx_id,
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
    let verifier = BlockTxVerifier::new(&Network::Mainnet, state_service);

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
        .oneshot(BlockRequest {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(HashMap::new()),
            height: block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result.expect("unexpected error response").tx_id,
        transaction.unmined_id()
    );
}

/// Tests if an expired non-coinbase V4 transaction is rejected.
#[tokio::test]
async fn v4_transaction_with_too_low_expiry_height() {
    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = BlockTxVerifier::new(&Network::Mainnet, state_service);

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
        .oneshot(BlockRequest {
            transaction_hash: transaction.hash(),
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
    let verifier = BlockTxVerifier::new(&Network::Mainnet, state_service);

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
        .oneshot(BlockRequest {
            transaction_hash: transaction.hash(),
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
    let verifier = BlockTxVerifier::new(&Network::Mainnet, state_service);

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
        .oneshot(BlockRequest {
            transaction_hash: transaction.hash(),
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
    let transaction = Transaction::test_v4(
        vec![input],
        vec![output],
        LockTime::Height(block::Height(0)),
        transaction_block_height,
    );

    let transaction_hash = transaction.unmined_id();

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = BlockTxVerifier::new(&network, state_service);

    let result = verifier
        .oneshot(BlockRequest {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(HashMap::new()),
            height: transaction_block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result.expect("unexpected error response").tx_id,
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
    let verifier = BlockTxVerifier::new(&network, state_service);

    let result = verifier
        .oneshot(BlockRequest {
            transaction_hash: transaction.hash(),
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
    let transaction = Transaction::test_v4(
        vec![input.clone(), input.clone()],
        vec![output],
        LockTime::Height(block::Height(0)),
        (transaction_block_height + 1).expect("expiry height is too large"),
    );

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = BlockTxVerifier::new(&network, state_service);

    let result = verifier
        .oneshot(BlockRequest {
            transaction_hash: transaction.hash(),
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
        let verifier = BlockTxVerifier::new(&network, state_service);

        let result = verifier
            .oneshot(BlockRequest {
                transaction_hash: transaction.hash(),
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
        let verifier = BlockTxVerifier::new(&network, state_service);

        let result = verifier
            .oneshot(BlockRequest {
                transaction_hash: transaction.hash(),
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
    let verifier = BlockTxVerifier::new(&network, state_service);

    let result = verifier
        .oneshot(BlockRequest {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(known_utxos),
            height: transaction_block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result.expect("unexpected error response").tx_id,
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
    let verifier = BlockTxVerifier::new(&network, state_service);

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
        .oneshot(BlockRequest {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(known_utxos),
            height: block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result.expect("unexpected error response").tx_id,
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
    let verifier = BlockTxVerifier::new(&network, state_service);
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
        .oneshot(BlockRequest {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(HashMap::new()),
            height: block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result.expect("unexpected error response").tx_id,
        transaction.unmined_id()
    );

    // Increment the expiry height so that it becomes invalid.
    let new_expiry_height = (block_height + 1).expect("transaction block height is too large");
    let mut new_transaction = transaction.clone();

    new_transaction.set_expiry_height(new_expiry_height);

    let result = verifier
        .clone()
        .oneshot(BlockRequest {
            transaction_hash: new_transaction.hash(),
            transaction: Arc::new(new_transaction.clone()),
            known_utxos: Arc::new(HashMap::new()),
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
        .oneshot(BlockRequest {
            transaction_hash: new_transaction.hash(),
            transaction: Arc::new(new_transaction.clone()),
            known_utxos: Arc::new(HashMap::new()),
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
        .oneshot(BlockRequest {
            transaction_hash: new_transaction.hash(),
            transaction: Arc::new(new_transaction.clone()),
            known_utxos: Arc::new(HashMap::new()),
            height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        verification_result.expect("successful verification").tx_id,
        new_transaction.unmined_id()
    );
}

/// Tests if an expired non-coinbase V5 transaction is rejected.
#[tokio::test]
async fn v5_transaction_with_too_low_expiry_height() {
    let network = Network::new_default_testnet();

    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called") });
    let verifier = BlockTxVerifier::new(&network, state_service);

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
        .oneshot(BlockRequest {
            transaction_hash: transaction.hash(),
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

    // Create a non-coinbase V5 tx. Its branch must be the one active at `height_max`, so the
    // expiry-height rule is what rejects it rather than the consensus branch ID check.
    let transaction = Transaction::test_v5(
        NetworkUpgrade::Nu6_3,
        vec![input],
        vec![output],
        LockTime::unlocked(),
        expiry_height,
    );

    let transaction_hash = transaction.hash();

    let verification_result = BlockTxVerifier::new(&Network::Mainnet, state)
        .oneshot(BlockRequest {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction.clone()),
            known_utxos: Arc::new(known_utxos),
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
    let verifier = BlockTxVerifier::new(&network, state_service);

    let result = verifier
        .oneshot(BlockRequest {
            transaction_hash: transaction.hash(),
            transaction: Arc::new(transaction),
            known_utxos: Arc::new(known_utxos),
            height: transaction_block_height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert_eq!(
        result.expect("unexpected error response").tx_id,
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
    let verifier = BlockTxVerifier::new(&network, state_service);

    let result = verifier
        .oneshot(BlockRequest {
            transaction_hash: transaction.hash(),
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

        let verification_result = BlockTxVerifier::new(&network, state)
            .oneshot(BlockRequest {
                transaction_hash: transaction.hash(),
                transaction: Arc::new(transaction),
                known_utxos: Arc::new(known_utxos),
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
        let verifier = BlockTxVerifier::new(&network, state_service);

        // Test the transaction verifier
        let result = verifier
            .oneshot(BlockRequest {
                transaction_hash: transaction.hash(),
                transaction,
                known_utxos: Arc::new(HashMap::new()),
                height,
                time: DateTime::<Utc>::MAX_UTC,
            })
            .await;

        assert_eq!(
            result.expect("unexpected error response").tx_id,
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
            TransactionError::Ed25519(ed25519::Error::InvalidSignature),
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

    // Serialize the transaction, apply byte-level modifications (re-signing the JoinSplit
    // signature where necessary so that exactly one check fails deterministically), and
    // re-deserialize.
    let mut tx_bytes = transaction
        .zcash_serialize_to_vec()
        .expect("transaction serialization should succeed");

    modify_joinsplit_bytes_and_resign(&mut tx_bytes, &network, height, modification);

    let transaction: Arc<Transaction> = Arc::new(
        Transaction::zcash_deserialize(tx_bytes.as_slice())
            .expect("modified transaction should deserialize"),
    );

    // Initialize the verifier
    let state_service =
        service_fn(|_| async { unreachable!("State service should not be called.") });
    let verifier = BlockTxVerifier::new(&network, state_service);

    // Test the transaction verifier.
    //
    // Because the modification leaves exactly one of the proof or signature checks failing (the
    // other is kept valid by re-signing), the verifier returns the same error every time, with no
    // race between the concurrent proof and signature checks.
    let result = verifier
        .oneshot(BlockRequest {
            transaction_hash: transaction.hash(),
            transaction: transaction.clone(),
            known_utxos: Arc::new(HashMap::new()),
            height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

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
        let verifier = BlockTxVerifier::new(&network, state_service);

        // Test the transaction verifier
        let result = timeout(
            test_timeout(),
            verifier.oneshot(BlockRequest {
                transaction_hash: transaction.hash(),
                transaction,
                known_utxos: Arc::new(HashMap::new()),
                height,
                time: DateTime::<Utc>::MAX_UTC,
            }),
        )
        .await
        .expect("timeout expired");

        assert_eq!(
            result.expect("unexpected error response").tx_id,
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
        let verifier = BlockTxVerifier::new(&network, state_service);

        // Test the transaction verifier
        let result = verifier
            .oneshot(BlockRequest {
                transaction_hash: transaction.hash(),
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
                transaction.sapling_spends_count() == 0
                    && transaction.sapling_outputs().next().is_some()
            })
            .expect("No transaction found with Sapling outputs and no Sapling spends");

        let expected_hash = transaction.unmined_id();

        // Initialize the verifier
        let state_service =
            service_fn(|_| async { unreachable!("State service should not be called") });
        let verifier = BlockTxVerifier::new(&network, state_service);

        // Test the transaction verifier
        let result = verifier
            .oneshot(BlockRequest {
                transaction_hash: transaction.hash(),
                transaction,
                known_utxos: Arc::new(HashMap::new()),
                height,
                time: DateTime::<Utc>::MAX_UTC,
            })
            .await;

        assert_eq!(
            result.expect("unexpected error response").tx_id,
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

        let verifier = BlockTxVerifier::new(
            &net,
            service_fn(|_| async { unreachable!("State service should not be called") }),
        );

        assert_eq!(
            timeout(
                test_timeout(),
                verifier.oneshot(BlockRequest {
                    transaction_hash: tx.hash(),
                    transaction: Arc::new(tx),
                    known_utxos: Arc::new(HashMap::new()),
                    height,
                    time: DateTime::<Utc>::MAX_UTC,
                })
            )
            .await
            .expect("timeout expired")
            .expect("unexpected error response")
            .tx_id,
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

        let verifier = BlockTxVerifier::new(
            &net,
            service_fn(|_| async { unreachable!("State service should not be called") }),
        );

        assert_eq!(
            verifier
                .oneshot(BlockRequest {
                    transaction_hash: tx.hash(),
                    transaction: Arc::new(tx),
                    known_utxos: Arc::new(HashMap::new()),
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
#[tokio::test]
async fn v5_with_duplicate_orchard_action() {
    use ::orchard::bundle::{Bundle as OrchardBundle, Flags as OrchardFlags};
    use nonempty::NonEmpty;

    let _init_guard = zebra_test::init();

    for net in Network::iter() {
        let tx = v5_transactions(net.block_iter())
            .rev()
            .find(|tx| {
                tx.inputs().is_empty()
                    && tx.outputs().is_empty()
                    && tx.sapling_spends_count() == 0
                    && tx.sapling_outputs().next().is_none()
                    && tx.joinsplit_count() == 0
                    && tx.has_orchard_shielded_data()
            })
            .expect("V5 tx with only Orchard actions");

        let height = tx.expiry_height().expect("expiry height");
        let duplicate_nullifier = tx
            .orchard_nullifiers()
            .next()
            .expect("tx has at least one orchard action");

        // Duplicate the first action by rebuilding the orchard bundle. The
        // bundle's binding proof/signature will no longer cover the new action
        // count, but the duplicate-nullifier check runs before orchard proof
        // verification, so the verifier short-circuits on the duplication.
        let orig_bundle = tx
            .orchard_bundle()
            .expect("filter guarantees orchard shielded data");
        let first_action = orig_bundle.actions().first().clone();
        let mut actions: Vec<_> = orig_bundle.actions().iter().cloned().collect();
        actions.push(first_action);
        let actions = NonEmpty::from_vec(actions).expect("non-empty");

        let new_bundle = OrchardBundle::try_from_parts(
            actions,
            // Enable spends so the nullifier participates in consensus checks.
            OrchardFlags::ENABLED,
            *orig_bundle.value_balance(),
            *orig_bundle.anchor(),
            orig_bundle.authorization().clone(),
            orig_bundle.bundle_version(),
        )
        .expect("the duplicated action keeps the proof size and flags valid");

        let tx = tx.with_orchard_bundle(Some(new_bundle));

        let verifier = BlockTxVerifier::new(
            &net,
            service_fn(|_| async { unreachable!("State service should not be called") }),
        );

        assert_eq!(
            verifier
                .oneshot(BlockRequest {
                    transaction_hash: tx.hash(),
                    transaction: Arc::new(tx),
                    known_utxos: Arc::new(HashMap::new()),
                    height,
                    time: DateTime::<Utc>::MAX_UTC,
                })
                .await,
            Err(TransactionError::DuplicateOrchardNullifier(
                duplicate_nullifier
            ))
        );
    }
}

/// Checks the activation boundary of the temporary Orchard-disabling soft fork:
/// it is inactive below the configured height and active at and above it, can be
/// disabled entirely, and Mainnet uses its fixed activation height.
#[test]
fn orchard_disabling_soft_fork_activation_boundary() {
    let _init_guard = zebra_test::init();

    let soft_fork_height = Height(2_000_000);

    // A Testnet with the soft fork configured to activate at `soft_fork_height`.
    let network = Parameters::build()
        .with_temporary_orchard_disabling_soft_fork_height(soft_fork_height)
        .to_network()
        .expect("failed to build configured network");

    assert!(
        !network.temporary_orchard_disabling_soft_fork_active(Height(1_999_999)),
        "soft fork must be inactive below the configured height",
    );
    assert!(
        network.temporary_orchard_disabling_soft_fork_active(soft_fork_height),
        "soft fork must be active at the configured height",
    );
    assert!(
        network.temporary_orchard_disabling_soft_fork_active(Height(2_000_001)),
        "soft fork must be active above the configured height",
    );

    // A Testnet with the soft fork disabled is never active.
    let disabled = Parameters::build()
        .disable_temporary_orchard_disabling_soft_fork()
        .to_network()
        .expect("failed to build configured network");

    assert!(
        !disabled.temporary_orchard_disabling_soft_fork_active(Height(4_042_000)),
        "a disabled soft fork must never be active",
    );

    // Mainnet uses a fixed activation height (3_363_426).
    assert!(
        !Network::Mainnet.temporary_orchard_disabling_soft_fork_active(Height(3_363_425)),
        "Mainnet soft fork must be inactive below its fixed height",
    );
    assert!(
        Network::Mainnet.temporary_orchard_disabling_soft_fork_active(Height(3_363_426)),
        "Mainnet soft fork must be active at its fixed height",
    );
}

/// The temporary Orchard-disabling soft fork must reject transactions that
/// contain Orchard actions once it is active, in both block and mempool
/// verification contexts.
#[tokio::test]
async fn orchard_disabling_soft_fork_rejects_orchard_actions_in_blocks_and_mempool() {
    let _init_guard = zebra_test::init();

    // Find a V5 transaction whose only shielded data is Orchard, so it both
    // contains Orchard actions and can pass `has_inputs_and_outputs` once the
    // Orchard flags are set below.
    let default_testnet = Network::new_default_testnet();
    let tx = v5_transactions(default_testnet.block_iter())
        .rev()
        .find(|transaction| {
            transaction.inputs().is_empty()
                && transaction.outputs().is_empty()
                && transaction.sapling_spends().next().is_none()
                && transaction.sapling_outputs().next().is_none()
                && transaction.joinsplit_count() == 0
        })
        .expect("V5 tx with only Orchard actions");

    // Enable spends and outputs so the transaction passes `has_inputs_and_outputs`
    // and `has_enough_orchard_flags`, reaching the soft-fork check.
    let tx = with_orchard_flags(tx, ::orchard::bundle::Flags::ENABLED);

    // Verify at the transaction's own expiry height, where its NU5 consensus
    // branch id is valid on the default Testnet activation schedule.
    let height = tx.expiry_height().expect("V5 tx has an expiry height");

    // Configure a Testnet identical to the default public Testnet except that the
    // Orchard-disabling soft fork activates at `height`, so it is active for this
    // transaction.
    let network = Parameters::build()
        .with_temporary_orchard_disabling_soft_fork_height(height)
        .to_network()
        .expect("failed to build configured network");

    assert!(
        network.temporary_orchard_disabling_soft_fork_active(height),
        "soft fork must be active at the transaction's height",
    );

    let expected_err =
        TransactionError::Other("transaction has Orchard actions (temporarily disabled)".into());

    // The soft-fork check runs before any state-service query, so the state
    // service must never be called.
    let block_response = BlockTxVerifier::new(
        &network,
        service_fn(|_| async { unreachable!("state service should not be called") }),
    )
    .oneshot(BlockRequest {
        transaction_hash: tx.hash(),
        transaction: Arc::new(tx.clone()),
        known_utxos: Arc::new(HashMap::new()),
        height,
        time: DateTime::<Utc>::MAX_UTC,
    })
    .await;

    assert_eq!(
        block_response,
        Err(expected_err.clone()),
        "block verification must reject a transaction with Orchard actions after the soft fork",
    );

    let mempool_response = MempoolTxVerifier::new_for_tests(
        &network,
        service_fn(|_| async { unreachable!("state service should not be called") }),
    )
    .oneshot(MempoolRequest {
        transaction: Arc::new(tx).into(),
        height,
    })
    .await;

    assert_eq!(
        mempool_response,
        Err(expected_err),
        "mempool verification must reject a transaction with Orchard actions after the soft fork",
    );
}

/// Negative control mirroring the zcashd test: a transaction without Orchard
/// actions is unaffected by the soft fork and is still accepted while it is
/// active.
#[tokio::test]
async fn orchard_disabling_soft_fork_accepts_non_orchard_transactions() {
    let _init_guard = zebra_test::init();

    // A Testnet with the Orchard-disabling soft fork active from height 1.
    let network = Parameters::build()
        .with_temporary_orchard_disabling_soft_fork_height(Height(1))
        .to_network()
        .expect("failed to build configured network");

    let mut state: MockService<_, _, _, _> = MockService::build().for_prop_tests();

    let canopy_activation_height = NetworkUpgrade::Canopy
        .activation_height(&network)
        .expect("Canopy activation height is specified");

    let transaction_block_height =
        (canopy_activation_height + 10).expect("transaction block height is too large");
    let fake_source_fund_height =
        (transaction_block_height - 1).expect("fake source fund block height is too small");

    assert!(
        network.temporary_orchard_disabling_soft_fork_active(transaction_block_height),
        "soft fork must be active at the transaction's height",
    );

    // A transparent transfer has no Orchard actions, so the soft fork must not
    // affect it. The input must exceed the output by enough to pay the ZIP-317
    // conventional fee, so the transaction is otherwise valid.
    let (input, output, known_utxos) = mock_transparent_transfer(
        fake_source_fund_height,
        true,
        0,
        Amount::try_from(10001).expect("valid amount"),
    );

    let transaction = Transaction::test_v4(
        vec![input],
        vec![output],
        LockTime::Height(block::Height(0)),
        (transaction_block_height + 1).expect("expiry height is too large"),
    );

    let input_outpoint = match transaction.inputs()[0] {
        transparent::Input::PrevOut { outpoint, .. } => outpoint,
        transparent::Input::Coinbase { .. } => panic!("requires a non-coinbase transaction"),
    };

    let verifier = MempoolTxVerifier::new_for_tests(&network, state.clone());

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

    let response = verifier
        .oneshot(MempoolRequest {
            transaction: Arc::new(transaction).into(),
            height: transaction_block_height,
        })
        .await;

    assert!(
        response.is_ok(),
        "non-Orchard transaction must be accepted while the soft fork is active, got: {response:?}",
    );
}

/// Mirrors the zcashd boundary test: the soft fork must accept an Orchard
/// transaction one block below its activation height but reject the same
/// transaction at the activation height.
#[tokio::test]
async fn orchard_disabling_soft_fork_accepts_orchard_actions_below_activation_height() {
    let _init_guard = zebra_test::init();

    // Use an unmodified Orchard-only V5 transaction from the test vectors so its
    // proofs remain valid for the acceptance path.
    let default_testnet = Network::new_default_testnet();
    let tx = v5_transactions(default_testnet.block_iter())
        .rev()
        .find(|transaction| {
            transaction.inputs().is_empty()
                && transaction.outputs().is_empty()
                && transaction.sapling_spends().next().is_none()
                && transaction.sapling_outputs().next().is_none()
                && transaction.joinsplit_count() == 0
        })
        .expect("V5 tx with only Orchard actions");

    assert!(
        tx.has_orchard_shielded_data(),
        "test transaction must contain Orchard actions",
    );

    let height = tx.expiry_height().expect("V5 tx has an expiry height");

    // The soft fork activates one block above the transaction's height, so it is
    // inactive for this transaction and verification proceeds normally.
    let accepting_network = Parameters::build()
        .with_temporary_orchard_disabling_soft_fork_height(
            (height + 1).expect("height is too large"),
        )
        .to_network()
        .expect("failed to build configured network");

    assert!(
        !accepting_network.temporary_orchard_disabling_soft_fork_active(height),
        "soft fork must be inactive below its activation height",
    );

    // The only state request for an Orchard-only transaction verified as part of
    // a block is the nullifier and anchor check.
    let mut state: MockService<zebra_state::Request, zebra_state::Response, _, _> =
        MockService::build().for_prop_tests();
    let accept_verifier = BlockTxVerifier::new(&accepting_network, state.clone());

    tokio::spawn(async move {
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

    let accept_response = accept_verifier
        .oneshot(BlockRequest {
            transaction_hash: tx.hash(),
            transaction: Arc::new(tx.clone()),
            known_utxos: Arc::new(HashMap::new()),
            height,
            time: DateTime::<Utc>::MAX_UTC,
        })
        .await;

    assert!(
        accept_response.is_ok(),
        "Orchard transaction must be accepted below the soft fork height, got: {accept_response:?}",
    );

    // At the activation height the same transaction is rejected. The soft-fork
    // check runs before any state query, so the state service is never called.
    let rejecting_network = Parameters::build()
        .with_temporary_orchard_disabling_soft_fork_height(height)
        .to_network()
        .expect("failed to build configured network");

    let reject_response = BlockTxVerifier::new(
        &rejecting_network,
        service_fn(|_| async { unreachable!("state service should not be called") }),
    )
    .oneshot(BlockRequest {
        transaction_hash: tx.hash(),
        transaction: Arc::new(tx),
        known_utxos: Arc::new(HashMap::new()),
        height,
        time: DateTime::<Utc>::MAX_UTC,
    })
    .await;

    assert_eq!(
        reject_response,
        Err(TransactionError::Other(
            "transaction has Orchard actions (temporarily disabled)".into()
        )),
        "Orchard transaction must be rejected at the soft fork height",
    );
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
        let block_verifier = Buffer::new(BlockTxVerifier::new(&network, state.clone()), 10);
        let mempool_verifier = Buffer::new(
            MempoolTxVerifier::new_for_tests(&network, state.clone()),
            10,
        );

        while let Some(next_nu) = network_upgrade.next_upgrade() {
            // Check an outdated network upgrade.
            let Some(height) = next_nu.activation_height(&network) else {
                tracing::warn!(?next_nu, "missing activation height",);
                // Shift the network upgrade for the next loop iteration.
                network_upgrade = next_nu;
                continue;
            };

            let block_req = block_verifier
                .clone()
                .oneshot(BlockRequest {
                    transaction_hash: tx.hash(),
                    transaction: Arc::new(tx.clone()),
                    known_utxos: known_utxos.clone(),
                    // The consensus branch ID of the tx is outdated for this height.
                    height,
                    time: DateTime::<Utc>::MAX_UTC,
                })
                .map_err(|err| *err.downcast().expect("`TransactionError` type"));

            let mempool_req = mempool_verifier
                .clone()
                .oneshot(MempoolRequest {
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

            let block_req = block_verifier
                .clone()
                .oneshot(BlockRequest {
                    transaction_hash: tx.hash(),
                    transaction: Arc::new(tx.clone()),
                    known_utxos: known_utxos.clone(),
                    // The consensus branch ID of the tx is supported by this height.
                    height,
                    time: DateTime::<Utc>::MAX_UTC,
                })
                .map_ok(|rsp| rsp.tx_id)
                .map_err(|e| format!("{e}"));

            let mempool_req = mempool_verifier
                .clone()
                .oneshot(MempoolRequest {
                    transaction: std::sync::Arc::new(tx.clone()).into(),
                    // The consensus branch ID of the tx is supported by this height.
                    height,
                })
                .map_ok(|rsp| rsp.transaction.transaction.id)
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

            let block_req = block_verifier
                .clone()
                .oneshot(BlockRequest {
                    transaction_hash: tx.hash(),
                    transaction: Arc::new(tx.clone()),
                    known_utxos: known_utxos.clone(),
                    // The consensus branch ID of the tx is not supported by this height.
                    height,
                    time: DateTime::<Utc>::MAX_UTC,
                })
                .map_err(|err| *err.downcast().expect("`TransactionError` type"));

            let mempool_req = mempool_verifier
                .clone()
                .oneshot(MempoolRequest {
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
    // A standard, signature-free P2SH input that the script interpreter accepts. The redeem
    // script is OP_TRUE, so the spend is valid without a signature, while the spent output is a
    // standard P2SH `scriptPubKey`. This lets the input pass the mempool `AreInputsStandard`
    // gate (`check::mempool_standard_input_scripts`) that now runs before script verification,
    // as well as the interpreter itself.
    const OP_TRUE: u8 = 0x51;
    // `scriptSig`: a single push of the OP_TRUE redeem script (push-only, one stack item).
    let accepting_unlock_script = transparent::Script::new(&[0x01, OP_TRUE]);
    // `scriptPubKey`: OP_HASH160 <HASH160(OP_TRUE)> OP_EQUAL. The 20-byte hash is precomputed
    // (RIPEMD160(SHA256([OP_TRUE]))) so the P2SH hash check passes during verification.
    let mut p2sh_lock_bytes = vec![0xa9, 0x14];
    p2sh_lock_bytes.extend_from_slice(&[
        0xda, 0x17, 0x45, 0xe9, 0xb5, 0x49, 0xbd, 0x0b, 0xfa, 0x1a, 0x56, 0x99, 0x71, 0xc7, 0x7e,
        0xba, 0x30, 0xcd, 0x5a, 0x4b,
    ]);
    p2sh_lock_bytes.push(0x87);
    let accepting_lock_script = transparent::Script::new(&p2sh_lock_bytes);
    // A script with a single opcode that rejects the transaction (OP_FALSE). Only used for block
    // requests (the standardness gate is mempool-only), so it need not be a standard type.
    let rejecting_script = transparent::Script::new(&[0]);

    // Mock an unspent transaction output
    let previous_outpoint = transparent::OutPoint {
        hash: Hash([1u8; 32]),
        index: outpoint_index,
    };

    let (lock_script, unlock_script) = if script_should_succeed {
        (accepting_lock_script, accepting_unlock_script)
    } else {
        // The spent output's OP_FALSE `scriptPubKey` makes verification fail regardless of the
        // `scriptSig`, so the `scriptSig` value is irrelevant here.
        (rejecting_script.clone(), accepting_unlock_script)
    };

    let previous_output = transparent::Output {
        value: previous_output_value,
        lock_script,
    };

    let previous_utxo = transparent::OrderedUtxo::new(previous_output, previous_utxo_height, 1);

    // Use the `previous_outpoint` as input
    let input = transparent::Input::PrevOut {
        outpoint: previous_outpoint,
        unlock_script,
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
        data: vec![],
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
    let mut tx = Transaction::test_v4_with_joinsplit_data(joinsplit_data.as_ref());
    tx.set_expiry_height(expiry_height);
    tx
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

/// Modify a JoinSplit in the serialized bytes of a V4 transaction following the given
/// modification type, re-signing the JoinSplit signature where necessary so that exactly one of
/// the proof or signature checks fails.
///
/// The JoinSplit signature covers the transaction sighash, which commits to the JoinSplit proofs
/// (and to `joinSplitPubKey`). So corrupting a proof also invalidates the original signature, and
/// the verifier checks the proof and signature concurrently — whichever fails first is reported.
/// To test proof verification in isolation, deterministically, this takes over the JoinSplit key
/// pair for the proof-corruption cases and re-signs the modified transaction, leaving a *valid*
/// signature over an *invalid* proof, so only the proof check fails. Conversely,
/// [`JoinSplitModification::CorruptSignature`] leaves the proof (and its signature) valid and then
/// invalidates only the signature.
fn modify_joinsplit_bytes_and_resign(
    tx_bytes: &mut Vec<u8>,
    network: &Network,
    height: block::Height,
    modification: JoinSplitModification,
) {
    if let JoinSplitModification::CorruptSignature = modification {
        // The joinsplit signature is the last 64 bytes of the serialized transaction.
        // Flip a bit from an arbitrary byte of the signature; no re-signing is needed because
        // the proof and the sighash stay valid, so only the signature check fails.
        let sig_offset = tx_bytes.len() - 64;
        tx_bytes[sig_offset + 10] ^= 0x01;
        return;
    }

    // Proof-corruption cases: corrupt the proof, take over the JoinSplit key pair, then re-sign.
    let proof_offset = find_first_joinsplit_proof_offset(tx_bytes);
    match modification {
        JoinSplitModification::CorruptProof => {
            // A proof is composed of three field elements: first(48) + middle(96) + last(48).
            // To corrupt without making it malformed, swap the first and last elements.
            let (first, rest) = tx_bytes[proof_offset..proof_offset + 192].split_at_mut(48);
            let mut first_copy = [0u8; 48];
            first_copy.copy_from_slice(first);
            first.copy_from_slice(&rest[96..144]);
            rest[96..144].copy_from_slice(&first_copy);
        }
        JoinSplitModification::ZeroProof => {
            // An all-zero proof is malformed (not a valid curve point).
            tx_bytes[proof_offset..proof_offset + 192].fill(0);
        }
        JoinSplitModification::CorruptSignature => {
            unreachable!("the signature case returns early above")
        }
    }

    // The sighash commits to `joinSplitPubKey` (but not to `joinSplitSig`), so write the new
    // public key before computing the sighash to sign below. In a V4 transaction with JoinSplits,
    // `joinSplitPubKey` is the 32 bytes preceding the final 64-byte `joinSplitSig`.
    let signing_key = ed25519::SigningKey::new(rand::thread_rng());
    let verification_key = ed25519::VerificationKey::from(&signing_key);
    let pub_key_offset = tx_bytes.len() - 96;
    tx_bytes[pub_key_offset..pub_key_offset + 32]
        .copy_from_slice(&<[u8; 32]>::from(verification_key));

    // Re-sign the modified transaction so the JoinSplit signature is valid over the invalid proof.
    let modified = Transaction::zcash_deserialize(tx_bytes.as_slice())
        .expect("modified transaction should deserialize");
    let nu = NetworkUpgrade::current(network, height);
    let sighash = modified
        .sighash(nu, HashType::ALL, Arc::new(Vec::new()), None)
        .expect("network upgrade should be valid for tx");
    let sig = signing_key.sign(sighash.as_ref());
    let sig_offset = tx_bytes.len() - 64;
    tx_bytes[sig_offset..].copy_from_slice(&<[u8; 64]>::from(sig));
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

/// Replaces `tx`'s Orchard bundle with a single-action bundle built from a note-encryption
/// test vector, keeping the bundle version consistent with the transaction's consensus branch.
fn with_note_encryption_vector(
    tx: Transaction,
    v: &zebra_test::vectors::TestVector,
) -> Transaction {
    use zcash_protocol::value::ZatBalance;
    use zebra_chain::transaction::arbitrary::{
        fake_orchard_bundle_with_note, outputs_enabled_flags,
    };

    let bundle_version =
        zcash_primitives::transaction::components::orchard::bundle_version_for_branch(
            tx.consensus_branch_id(),
            ::orchard::ValuePool::Orchard,
        )
        .expect("a mined v5 transaction is on a branch that defines the Orchard pool");

    let bundle = fake_orchard_bundle_with_note(
        outputs_enabled_flags(bundle_version),
        ZatBalance::from_i64(0).expect("zero is a valid balance"),
        bundle_version,
        &v.cv_net,
        &v.rho,
        &v.cmx,
        v.ephemeral_key,
        v.c_enc,
        v.c_out,
    );

    tx.with_orchard_bundle(Some(bundle))
}

/// Test if shielded coinbase outputs are decryptable with an all-zero outgoing viewing key.
#[test]
fn coinbase_outputs_are_decryptable_for_fake_v5_blocks() {
    let _init_guard = zebra_test::init();

    for v in zebra_test::vectors::ORCHARD_NOTE_ENCRYPTION_ZERO_VECTOR.iter() {
        for net in Network::iter() {
            let tx = v5_transactions(net.block_iter())
                .find(|tx| tx.is_coinbase())
                .expect("coinbase V5 tx");

            let tx = with_note_encryption_vector(tx, v);
            assert!(
                tx.has_shielded_outputs(),
                "the rule only applies to transactions with shielded outputs",
            );

            assert_eq!(
                check::coinbase_outputs_are_decryptable(
                    &tx,
                    &net,
                    NetworkUpgrade::Nu5.activation_height(&net).unwrap(),
                ),
                Ok(())
            );
        }
    }
}

/// Test if random shielded outputs are NOT decryptable with an all-zero outgoing viewing key.
#[test]
fn shielded_outputs_are_not_decryptable_for_fake_v5_blocks() {
    let _init_guard = zebra_test::init();

    for v in zebra_test::vectors::ORCHARD_NOTE_ENCRYPTION_VECTOR.iter() {
        for net in Network::iter() {
            let tx = v5_transactions(net.block_iter())
                .find(|tx| tx.is_coinbase())
                .expect("V5 coinbase tx");

            let tx = with_note_encryption_vector(tx, v);

            assert_eq!(
                check::coinbase_outputs_are_decryptable(
                    &tx,
                    &net,
                    NetworkUpgrade::Nu5.activation_height(&net).unwrap(),
                ),
                Err(TransactionError::CoinbaseOutputsNotDecryptable)
            );
        }
    }
}

#[tokio::test]
async fn mempool_zip317_error() {
    let mut state: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let verifier = MempoolTxVerifier::new_for_tests(&Network::Mainnet, state.clone());

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

        // ZIP-317 policy is checked before expensive cryptographic verification, so the
        // verifier never reaches the anchor/nullifier check for this transaction.
    });

    let verifier_response = verifier
        .oneshot(MempoolRequest {
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
    let verifier = MempoolTxVerifier::new_for_tests(&Network::Mainnet, state.clone());

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
        .oneshot(MempoolRequest {
            transaction: std::sync::Arc::new(tx).into(),
            height,
        })
        .await;

    assert!(
        verifier_response.is_ok(),
        "expected successful verification, got: {verifier_response:?}"
    );
}

/// Test for CVE-2026-34377 https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-3vmh-33xr-9cqh
///
/// Ensure a block transaction with garbage Orchard proofs is rejected. Corrupting only the
/// authorizing data leaves the txid unchanged (ZIP-244), so a valid and a garbage version of the
/// same transaction share a mined id. The original bug let a mempool-cached result for that id
/// stand in for verification of the block transaction.
///
/// [`BlockTxVerifier`] has no mempool handle, so it cannot consult mempool state at all. That
/// makes the bypass structurally impossible rather than merely untaken, which is why this test
/// only needs to check that the garbage proofs are rejected on their own merits.
#[tokio::test(flavor = "multi_thread")]
async fn block_with_garbage_orchard_proofs_is_rejected() {
    let _init_guard = zebra_test::init();

    let state: MockService<_, _, _, _> = MockService::build().for_prop_tests();
    let verifier = BlockTxVerifier::new(&Network::Mainnet, state.clone());
    let verifier = Buffer::new(verifier, 1);

    let height = NetworkUpgrade::Nu6
        .activation_height(&Network::Mainnet)
        .expect("Nu6 activation height is specified");
    let fund_height = (height - 1).expect("too small");
    let (input, output, _known_utxos) = mock_transparent_transfer(
        fund_height,
        true,
        0,
        Amount::try_from(10001).expect("invalid value"),
    );

    let tx = Transaction::test_v5(
        NetworkUpgrade::Nu6,
        vec![input],
        vec![output],
        LockTime::min_lock_time_timestamp(),
        height,
    );
    let tx = insert_fake_orchard_shielded_data(tx);

    let tx_hash = tx.hash();

    // corrupt only auth data, txid stays the same (ZIP-244)
    let garbage_tx = with_garbage_orchard_authorization(tx.clone());
    assert_eq!(tx.hash(), garbage_tx.hash());

    // submit garbage version as block tx, must be rejected
    let resp = verifier
        .clone()
        .oneshot(BlockRequest {
            transaction_hash: tx_hash,
            transaction: Arc::new(garbage_tx),
            known_utxos: Arc::new(HashMap::new()),
            height,
            time: Utc::now(),
        })
        .await;

    assert!(resp.is_err(), "garbage proof must be rejected");
}

/// Regression test for the mempool-cache expiry bypass vulnerability.
///
/// A non-coinbase transaction with `nExpiryHeight = H+1` can be admitted to the mempool while
/// the best tip is still `H`, then presented inside a candidate block at height `H+2`, where it
/// has expired. Block transaction verification must reject it.
///
/// The original bug was a fast path that returned a mempool-cached verification result for a
/// transaction's mined id before re-running the expiry check, so the block passed semantic
/// verification with an expired transaction inside — a consensus split, since honest nodes
/// reject that block. The window existed because the mempool is active whenever Zebra is close
/// to the tip (not only exactly at it), and the download pipeline verifies blocks up to
/// `tip + full_verify_concurrency_limit` ahead of it.
///
/// That fast path no longer exists, and [`BlockTxVerifier`] holds no mempool handle, so the
/// bypass can no longer be reconstructed from a block request. What remains testable, and what
/// this test pins, is the rule the bypass evaded: block verification rejects a transaction whose
/// `nExpiryHeight` is below the block height.
#[tokio::test(flavor = "multi_thread")]
async fn mempool_cached_result_bypasses_expiry_check_for_block_at_next_height() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;

    // Heights used in the scenario:
    //   H   = canopy_height    (local best tip while the attack occurs)
    //   H+1 = mempool_height   (nExpiryHeight; tx is valid for mempool admission here)
    //   H+2 = expired_block_height (block height at which the tx has expired)
    let canopy_height = NetworkUpgrade::Canopy
        .activation_height(&network)
        .expect("Canopy activation height is specified");
    let mempool_height = (canopy_height + 1).expect("mempool height should be valid");
    let expired_block_height = (canopy_height + 2).expect("expired block height should be valid");
    let fund_height = (canopy_height - 1).expect("fund height should be valid");

    let (input, output, _known_utxos) = mock_transparent_transfer(
        fund_height,
        true,
        0,
        Amount::try_from(10001).expect("valid value"),
    );

    // V4 transaction with nExpiryHeight = mempool_height (H+1).
    // Valid in block H+1 (block_height == expiry_height) but expired in H+2
    // (block_height > expiry_height).  LockTime::unlocked() avoids a
    // BestChainNextMedianTimePast state query, keeping the test simpler.
    let tx = Transaction::test_v4(
        vec![input],
        vec![output],
        LockTime::unlocked(),
        mempool_height,
    );

    let tx_hash = tx.hash();

    let state: MockService<_, _, _, _> = MockService::build().for_unit_tests();
    let verifier = BlockTxVerifier::new(&network, state.clone());
    let verifier = Buffer::new(verifier, 1);

    // Submit the transaction as a block request at expired_block_height (H+2).
    //
    // The verifier must return Err(TransactionError::ExpiredTransaction)
    // because H+2 > nExpiryHeight. The expiry check runs before any state
    // query, so the mock state service is never called.
    let result = timeout(
        test_timeout(),
        verifier.clone().oneshot(BlockRequest {
            transaction_hash: tx_hash,
            transaction: Arc::new(tx.clone()),
            known_utxos: Arc::new(HashMap::new()),
            height: expired_block_height,
            time: Utc::now(),
        }),
    )
    .await
    .expect("block request should not time out");

    // Buffer boxes the service error, so downcast to check the specific variant.
    let err = result.expect_err(
        "expected block verification to fail for a transaction whose \
         nExpiryHeight is below the block height",
    );
    let tx_err = err
        .downcast::<TransactionError>()
        .expect("error should downcast to TransactionError");
    assert!(
        matches!(*tx_err, TransactionError::ExpiredTransaction { .. }),
        "expected ExpiredTransaction error for block at height {expired_block_height:?} \
         with nExpiryHeight {mempool_height:?}; \
         got: {tx_err:?}"
    );
}

/// Test the mempool standardness gate that runs before script verification:
/// a P2SH input whose redeem script exceeds `MAX_P2SH_SIGOPS` (15) must be rejected,
/// while a redeem script at the limit is accepted.
#[test]
fn mempool_standard_input_scripts_limits_p2sh_redeem_sigops() {
    let _init_guard = zebra_test::init();

    const OP_CHECKSIG: u8 = 0xac;

    // scriptPubKey: OP_HASH160 <20 bytes> OP_EQUAL
    let mut p2sh_lock_bytes = vec![0xa9, 0x14];
    p2sh_lock_bytes.extend_from_slice(&[0u8; 20]);
    p2sh_lock_bytes.push(0x87);

    let spent_output = transparent::Output {
        value: Amount::try_from(1_000_000).expect("valid amount"),
        lock_script: transparent::Script::new(&p2sh_lock_bytes),
    };

    // A scriptSig with a single direct push of `sigops` OP_CHECKSIGs: for a P2SH spend, the pushed
    // data is the (non-standard) redeem script, and each OP_CHECKSIG counts as one accurate sigop.
    let unlock_bytes_with_sigops = |sigops: usize| {
        let mut unlock_bytes = vec![u8::try_from(sigops).expect("small test count")];
        unlock_bytes.extend_from_slice(&vec![OP_CHECKSIG; sigops]);
        unlock_bytes
    };

    let tx_spending = |unlock_bytes: &[u8]| {
        Transaction::test_v5(
            NetworkUpgrade::Nu5,
            vec![transparent::Input::PrevOut {
                outpoint: transparent::OutPoint {
                    hash: Hash([0u8; 32]),
                    index: 0,
                },
                unlock_script: transparent::Script::new(unlock_bytes),
                sequence: u32::MAX,
            }],
            vec![spent_output.clone()],
            LockTime::unlocked(),
            Height(0),
        )
    };

    // A non-standard redeem script at the standardness limit is accepted.
    let tx = tx_spending(&unlock_bytes_with_sigops(15));
    assert_eq!(
        check::mempool_standard_input_scripts(&tx, std::slice::from_ref(&spent_output)),
        Ok(()),
        "P2SH redeem script with MAX_P2SH_SIGOPS sigops should be standard"
    );

    // One sigop above the limit is rejected before script verification.
    let tx = tx_spending(&unlock_bytes_with_sigops(16));
    assert_eq!(
        check::mempool_standard_input_scripts(&tx, std::slice::from_ref(&spent_output)),
        Err(TransactionError::NonStandardInputs),
        "P2SH redeem script above MAX_P2SH_SIGOPS should be rejected"
    );
}

/// Test the mempool standardness gate that runs before script verification:
/// spending a non-standard (high-sigop) scriptPubKey must be rejected via `AreInputsStandard`,
/// so the interpreter is never asked to run its signature operations; a genuinely standard
/// P2PKH input is accepted.
#[test]
fn mempool_standard_input_scripts_rejects_nonstandard_spent_output() {
    let _init_guard = zebra_test::init();

    let tx_spending = |unlock_bytes: &[u8], spent_output: &transparent::Output| {
        Transaction::test_v5(
            NetworkUpgrade::Nu5,
            vec![transparent::Input::PrevOut {
                outpoint: transparent::OutPoint {
                    hash: Hash([0u8; 32]),
                    index: 0,
                },
                unlock_script: transparent::Script::new(unlock_bytes),
                sequence: u32::MAX,
            }],
            vec![spent_output.clone()],
            LockTime::unlocked(),
            Height(0),
        )
    };

    // A non-standard spent scriptPubKey (50 x OP_CHECKSIG) is not a recognized standard type, so
    // the input is rejected before verification would run its 50 signature checks. This is the
    // non-P2SH counterpart of the P2SH redeem-script limit: without classifying the spent output,
    // a peer could plant such a UTXO and drive expensive verification with a cheap spend.
    let nonstandard_output = transparent::Output {
        value: Amount::try_from(1_000_000).expect("valid amount"),
        lock_script: transparent::Script::new(&[0xac; 50]),
    };
    let tx = tx_spending(&[0x01, 0xaa], &nonstandard_output);
    assert_eq!(
        check::mempool_standard_input_scripts(&tx, std::slice::from_ref(&nonstandard_output)),
        Err(TransactionError::NonStandardInputs),
        "spending a non-standard scriptPubKey should be rejected before script verification"
    );

    // A genuinely standard P2PKH input (2 scriptSig pushes: sig + pubkey) is accepted.
    let mut p2pkh_lock_bytes = vec![0x76, 0xa9, 0x14];
    p2pkh_lock_bytes.extend_from_slice(&[0u8; 20]);
    p2pkh_lock_bytes.extend_from_slice(&[0x88, 0xac]);
    let p2pkh_output = transparent::Output {
        value: Amount::try_from(1_000_000).expect("valid amount"),
        lock_script: transparent::Script::new(&p2pkh_lock_bytes),
    };
    let tx = tx_spending(&[0x01, 0xaa, 0x01, 0xbb], &p2pkh_output);
    assert_eq!(
        check::mempool_standard_input_scripts(&tx, std::slice::from_ref(&p2pkh_output)),
        Ok(()),
        "a standard P2PKH input with the correct stack depth should be accepted"
    );
}

/// Test the mempool standardness gate that runs before script verification:
/// non-push-only and oversized scriptSigs must be rejected, so signature operations
/// can't be hidden in a scriptSig that the interpreter would execute.
#[test]
fn mempool_standard_input_scripts_rejects_nonstandard_script_sigs() {
    let _init_guard = zebra_test::init();

    // scriptPubKey: OP_DUP OP_HASH160 <20 bytes> OP_EQUALVERIFY OP_CHECKSIG
    let mut p2pkh_lock_bytes = vec![0x76, 0xa9, 0x14];
    p2pkh_lock_bytes.extend_from_slice(&[0u8; 20]);
    p2pkh_lock_bytes.extend_from_slice(&[0x88, 0xac]);

    let spent_output = transparent::Output {
        value: Amount::try_from(1_000_000).expect("valid amount"),
        lock_script: transparent::Script::new(&p2pkh_lock_bytes),
    };

    let tx_spending = |unlock_bytes: &[u8]| {
        Transaction::test_v5(
            NetworkUpgrade::Nu5,
            vec![transparent::Input::PrevOut {
                outpoint: transparent::OutPoint {
                    hash: Hash([0u8; 32]),
                    index: 0,
                },
                unlock_script: transparent::Script::new(unlock_bytes),
                sequence: u32::MAX,
            }],
            vec![spent_output.clone()],
            LockTime::unlocked(),
            Height(0),
        )
    };

    // A scriptSig containing an operation (OP_CHECKSIG) instead of only pushes is rejected.
    let tx = tx_spending(&[0xac]);
    assert_eq!(
        check::mempool_standard_input_scripts(&tx, std::slice::from_ref(&spent_output)),
        Err(TransactionError::NonStandardScriptSigNotPushOnly { input_index: 0 }),
        "non-push-only scriptSig should be rejected"
    );

    // A push-only scriptSig above MAX_STANDARD_SCRIPTSIG_SIZE (1650) bytes is rejected:
    // OP_PUSHDATA2 <1648 little-endian> <1648 bytes> is 1651 bytes in total.
    let mut oversized_unlock_bytes = vec![0x4d, 0x70, 0x06];
    oversized_unlock_bytes.extend_from_slice(&[0u8; 1648]);
    let tx = tx_spending(&oversized_unlock_bytes);
    assert_eq!(
        check::mempool_standard_input_scripts(&tx, std::slice::from_ref(&spent_output)),
        Err(TransactionError::NonStandardScriptSigSize {
            input_index: 0,
            size: 1651,
        }),
        "oversized scriptSig should be rejected"
    );
}

// Unit tests for the private scriptSig-analysis helpers used by `check::are_inputs_standard`.

#[test]
fn count_script_push_ops_counts_pushes() {
    let _init_guard = zebra_test::init();
    // OP_0 <push 1 byte> <push 1 byte>
    assert_eq!(
        check::count_script_push_ops(&[0x00, 0x01, 0xaa, 0x01, 0xbb]),
        3
    );
}

#[test]
fn count_script_push_ops_empty_script() {
    let _init_guard = zebra_test::init();
    assert_eq!(check::count_script_push_ops(&[]), 0);
}

#[test]
fn count_script_push_ops_pushdata_variants() {
    let _init_guard = zebra_test::init();
    // OP_PUSHDATA1 <len=3> <3 bytes>
    assert_eq!(
        check::count_script_push_ops(&[0x4c, 0x03, 0xaa, 0xbb, 0xcc]),
        1
    );
    // OP_PUSHDATA2 <len=3 LE> <3 bytes>
    assert_eq!(
        check::count_script_push_ops(&[0x4d, 0x03, 0x00, 0xaa, 0xbb, 0xcc]),
        1
    );
    // OP_PUSHDATA4 <len=2 LE> <2 bytes>
    assert_eq!(
        check::count_script_push_ops(&[0x4e, 0x02, 0x00, 0x00, 0x00, 0xaa, 0xbb]),
        1
    );
}

#[test]
fn count_script_push_ops_truncated_script() {
    let _init_guard = zebra_test::init();
    // OP_PUSHBYTES_10 with only 3 bytes: the incomplete push errors and is filtered out.
    assert_eq!(check::count_script_push_ops(&[0x0a, 0xaa, 0xbb, 0xcc]), 0);
}

#[test]
fn extract_p2sh_redeemed_script_extracts_last_push() {
    let _init_guard = zebra_test::init();
    // <push "abc"> <push "de">: the redeemed script is the last push.
    let unlock_script = transparent::Script::new(&[0x03, 0x61, 0x62, 0x63, 0x02, 0x64, 0x65]);
    assert_eq!(
        check::extract_p2sh_redeemed_script(&unlock_script),
        Some(vec![0x64, 0x65])
    );
}

#[test]
fn extract_p2sh_redeemed_script_empty_script() {
    let _init_guard = zebra_test::init();
    assert!(check::extract_p2sh_redeemed_script(&transparent::Script::new(&[])).is_none());
}

#[test]
fn script_sig_args_expected_values() {
    use zcash_script::solver::ScriptKind;

    let _init_guard = zebra_test::init();

    // Build a P2PK lock script: <compressed_pubkey> OP_CHECKSIG
    fn p2pk_lock_script(pubkey: &[u8; 33]) -> transparent::Script {
        let mut s = Vec::with_capacity(1 + 33 + 1);
        s.push(0x21); // OP_PUSHBYTES_33
        s.extend_from_slice(pubkey);
        s.push(0xac); // OP_CHECKSIG
        transparent::Script::new(&s)
    }

    // Build a bare multisig lock script: OP_<required> <pubkeys...> OP_<total> OP_CHECKMULTISIG
    fn multisig_lock_script(required: u8, pubkeys: &[&[u8; 33]]) -> transparent::Script {
        let mut s = Vec::new();
        s.push(0x50 + required); // OP_1..=OP_16
        for pk in pubkeys {
            s.push(0x21); // OP_PUSHBYTES_33
            s.extend_from_slice(*pk);
        }
        s.push(0x50 + pubkeys.len() as u8); // OP_N total
        s.push(0xae); // OP_CHECKMULTISIG
        transparent::Script::new(&s)
    }

    // P2PKH: sig + pubkey.
    assert_eq!(
        check::script_sig_args_expected(&ScriptKind::PubKeyHash { hash: [0xaa; 20] }),
        Some(2)
    );
    // P2SH: the redeemed script push.
    assert_eq!(
        check::script_sig_args_expected(&ScriptKind::ScriptHash { hash: [0xbb; 20] }),
        Some(1)
    );
    // OP_RETURN: non-standard to spend.
    assert_eq!(
        check::script_sig_args_expected(&ScriptKind::NullData { data: vec![] }),
        None
    );

    // P2PK: sig only (classified via the solver).
    let p2pk_kind = check::standard_script_kind(&p2pk_lock_script(&[0x02; 33]))
        .expect("P2PK should be a standard script kind");
    assert_eq!(check::script_sig_args_expected(&p2pk_kind), Some(1));

    // 1-of-1 multisig: OP_0 + 1 sig.
    let ms_kind = multisig_lock_script(1, &[&[0x02; 33]]);
    let ms_kind = check::standard_script_kind(&ms_kind)
        .expect("1-of-1 multisig should be a standard script kind");
    assert_eq!(check::script_sig_args_expected(&ms_kind), Some(2));
}

// The shielded verification caches, exercised end to end through the transaction verifiers.
//
// The unit tests in `primitives::halo2::tests` and `primitives::sapling::tests` pin the cache's
// own behaviour against a stub inner service. These pin what the node actually gets from it: the
// proof and signature verification a mempool transaction paid for is reused by the block that
// mines it, and nothing else is.

/// The mock state service the cache tests verify against.
type CacheTestState = MockService<
    zebra_state::Request,
    zebra_state::Response,
    zebra_test::mock_service::PropTestAssertion,
    zebra_state::BoxError,
>;

/// Returns one real mainnet transaction that can be verified from the mempool and then in a block.
///
/// The predicates are what the two verifications need:
///
///   * an Orchard bundle, and no Sapling bundle, so the Halo2 verifier is the only shielded
///     verifier the transaction reaches;
///   * no transparent inputs, so neither verification queries the state for UTXOs, and the
///     sighash can be computed over an empty set of previous outputs;
///   * no time-based lock time, so the mempool verification makes no median-time-past query; and
///   * a fee at or above the ZIP 317 conventional fee, so the mempool verification is not
///     rejected as under-paying before it reaches the verifier.
fn cacheable_mainnet_orchard_transaction() -> Transaction {
    zebra_test::vectors::MAINNET_BLOCKS
        .values()
        .flat_map(|bytes| {
            let block: Block = bytes
                .zcash_deserialize_into()
                .expect("hard-coded test vector must deserialize");
            block.transactions.clone()
        })
        .find(|tx| {
            tx.has_orchard_shielded_data()
                && tx.inputs().is_empty()
                && !tx.has_sapling_shielded_data()
                && !tx.lock_time_is_time()
                && tx
                    .value_balance(&HashMap::new())
                    .ok()
                    .and_then(|balance| balance.remaining_transaction_value().ok())
                    .is_some_and(|fee| fee >= zip317::conventional_fee(tx))
        })
        .map(|tx| tx.as_ref().clone())
        .expect("the mainnet test blocks must contain a fee-paying Orchard-only transaction")
}

/// Returns the Orchard verification item the transaction verifier builds for `tx` at
/// `network_upgrade`.
///
/// Mirrors [`verify_v5_transaction`](super::verify_v5_transaction): the bundle and the sighash
/// come from one sighasher over an empty set of previous outputs, which is correct only because
/// [`cacheable_mainnet_orchard_transaction`] has no transparent inputs.
fn orchard_item(
    tx: &Transaction,
    network_upgrade: NetworkUpgrade,
) -> crate::primitives::halo2::Item {
    let sighasher = tx
        .sighasher(network_upgrade, Arc::new(Vec::new()))
        .expect("a mainnet Orchard transaction has a sighasher at its own network upgrade");
    let bundle = sighasher
        .orchard_bundle()
        .expect("the transaction was selected for having an Orchard bundle");

    crate::primitives::halo2::Item::new_with_wtx_id(
        bundle,
        sighasher.sighash(HashType::ALL, None),
        zebra_chain::transaction::WtxId {
            id: tx.hash(),
            auth_digest: tx
                .auth_digest()
                .expect("a v5 transaction has an authorizing-data digest"),
        },
    )
}

/// Answers the one state query a shielded-only mempool verification makes.
///
/// Block requests make none: this transaction has no transparent inputs to look up, and the
/// nullifier and anchor check is a mempool-only query.
fn respond_to_nullifier_and_anchor_check(state: &CacheTestState) {
    let mut state = state.clone();

    tokio::spawn(async move {
        state
            .expect_request_that(|req| {
                matches!(
                    req,
                    zebra_state::Request::CheckBestChainTipNullifiersAndAnchors(_)
                )
            })
            .await
            .expect("a mempool verification must check nullifiers and anchors")
            .respond(zebra_state::Response::ValidBestChainTipNullifiersAndAnchors);
    });
}

/// Returns the [`TransactionError`] behind a buffered block verifier's boxed error.
fn transaction_error(error: crate::BoxError) -> TransactionError {
    *error
        .downcast::<TransactionError>()
        .expect("the block verifier reports a typed transaction error")
}

/// Returns a block request that mines `tx` at `height`.
fn cache_test_block_request(tx: &Transaction, height: Height) -> BlockRequest {
    BlockRequest {
        transaction_hash: tx.hash(),
        transaction: Arc::new(tx.clone()),
        known_utxos: Arc::new(HashMap::new()),
        height,
        time: Utc::now(),
    }
}

/// The Halo2 proof cache, exercised end to end through the transaction verifiers.
///
/// This is one test rather than three because all three claims need the same transaction and a
/// cold cache for it. The Halo2 verifiers are process-wide `Lazy` statics, so only one test can
/// ever see that transaction's cache entry cold.
///
/// It runs on [`zebra_test::MULTI_THREADED_RUNTIME`] because it reaches a global verifier.
/// `tower-batch-control` spawns that verifier's batch worker on whichever runtime first touches
/// it, so a per-test runtime would leave the worker cancelled for every test that ran afterwards.
///
/// The three claims, in order:
///
///   1. a mempool verification records the proof, and the block that mines the same transaction
///      is answered from that record instead of verifying the proof again;
///   2. the record does not carry the transaction past the height-dependent checks: the block one
///      height past its expiry is still rejected. That is the mempool bypass Zebra removed as a
///      security fix in PR #10494, and the reason this cache holds a proof rather than a verdict
///      on a transaction;
///   3. a transaction whose authorizing data was replaced — the same txid, a different
///      authorizing-data digest, which is the shape of CVE-2026-34377 — never inherits the
///      record.
#[test]
fn the_halo2_cache_is_reused_only_for_the_transaction_that_earned_it() {
    let _init_guard = zebra_test::init();

    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        let state: CacheTestState = MockService::build().for_prop_tests();
        let mempool_verifier = MempoolTxVerifier::new_for_tests(&Network::Mainnet, state.clone());
        let block_verifier = Buffer::new(BlockTxVerifier::new(&Network::Mainnet, state.clone()), 1);

        let tx = cacheable_mainnet_orchard_transaction();
        let expiry_height = tx
            .expiry_height()
            .expect("a V5 transaction has an expiry height");
        let network_upgrade = NetworkUpgrade::current(&Network::Mainnet, expiry_height);
        let item = orchard_item(&tx, network_upgrade);
        assert_eq!(
            crate::primitives::halo2::inner_calls_for(network_upgrade, &item),
            0,
            "this transaction's bundle must not have been verified before this test"
        );

        // 1. The mempool verification is the only one that reaches the Halo2 verifier.
        respond_to_nullifier_and_anchor_check(&state);
        mempool_verifier
            .oneshot(MempoolRequest {
                transaction: Arc::new(tx.clone()).into(),
                height: expiry_height,
            })
            .await
            .expect("a real mainnet Orchard transaction must verify at its expiry height");

        assert_eq!(
            crate::primitives::halo2::inner_calls_for(network_upgrade, &item),
            1,
            "the mempool verification must reach the inner Halo2 verifier"
        );

        block_verifier
            .clone()
            .oneshot(cache_test_block_request(&tx, expiry_height))
            .await
            .expect("the same transaction must verify in a block");

        assert_eq!(
            crate::primitives::halo2::inner_calls_for(network_upgrade, &item),
            1,
            "the block verification must be answered from the cache"
        );

        // 2. The cached proof does not carry the transaction past the expiry check.
        let too_late =
            (expiry_height + 1).expect("a mainnet expiry height is far below the maximum");
        let error = block_verifier
            .clone()
            .oneshot(cache_test_block_request(&tx, too_late))
            .await
            .expect_err("a transaction mined past its expiry height must be rejected");

        assert_eq!(
            transaction_error(error),
            TransactionError::ExpiredTransaction {
                expiry_height,
                block_height: too_late,
                transaction_hash: tx.hash(),
            },
            "the rejection must be the expiry rule, not some other failure"
        );

        // 3. The authorizing-data twin does not inherit the cached result.
        let twin = with_garbage_orchard_authorization(tx.clone());
        assert_eq!(
            tx.hash(),
            twin.hash(),
            "replacing authorizing data must leave the txid unchanged, or this test proves nothing"
        );

        let twin_item = orchard_item(&twin, network_upgrade);
        // A collision here would already be the failure: the twin would inherit the valid
        // transaction's result instead of being verified.
        assert_eq!(
            crate::primitives::halo2::inner_calls_for(network_upgrade, &twin_item),
            0,
            "the authorizing-data twin must get a different cache key"
        );

        let error = block_verifier
            .clone()
            .oneshot(cache_test_block_request(&twin, expiry_height))
            .await
            .expect_err("a transaction with replaced authorizing data must be rejected");

        assert_eq!(
            transaction_error(error),
            TransactionError::Halo2VerificationFailed,
            "the twin must fail Orchard verification"
        );

        assert_eq!(
            crate::primitives::halo2::inner_calls_for(network_upgrade, &twin_item),
            1,
            "the twin must reach the inner Halo2 verifier"
        );
    });
}

/// Returns one real mainnet Sapling transaction that can be verified from the mempool and then in
/// a block, with the network upgrade it was mined under.
///
/// The predicates are the Sapling counterparts of
/// [`cacheable_mainnet_orchard_transaction`]'s: a Sapling bundle and no Orchard bundle, no
/// transparent inputs, no time-based lock time, and a fee at or above the ZIP 317 conventional
/// fee.
fn cacheable_mainnet_sapling_transaction() -> (NetworkUpgrade, Transaction) {
    zebra_test::vectors::MAINNET_BLOCKS
        .iter()
        .flat_map(|(height, bytes)| {
            let block: Block = bytes
                .zcash_deserialize_into()
                .expect("hard-coded test vector must deserialize");
            let nu = NetworkUpgrade::current(&Network::Mainnet, Height(*height));

            block
                .transactions
                .clone()
                .into_iter()
                .map(move |tx| (nu, tx))
        })
        .find(|(_nu, tx)| {
            tx.has_sapling_shielded_data()
                && !tx.has_orchard_shielded_data()
                && tx.inputs().is_empty()
                && !tx.lock_time_is_time()
                && tx
                    .value_balance(&HashMap::new())
                    .ok()
                    .and_then(|balance| balance.remaining_transaction_value().ok())
                    .is_some_and(|fee| fee >= zip317::conventional_fee(tx))
        })
        .map(|(nu, tx)| (nu, tx.as_ref().clone()))
        .expect("the mainnet test blocks must contain a fee-paying Sapling-only transaction")
}

/// Returns the Sapling verification item the transaction verifier builds for `tx` at
/// `network_upgrade`.
fn sapling_item(
    tx: &Transaction,
    network_upgrade: NetworkUpgrade,
) -> crate::primitives::sapling::Item {
    let sighasher = tx
        .sighasher(network_upgrade, Arc::new(Vec::new()))
        .expect("a mainnet Sapling transaction has a sighasher at its own network upgrade");
    let bundle = sighasher
        .sapling_bundle()
        .expect("the transaction was selected for having a Sapling bundle");

    crate::primitives::sapling::Item::new(
        bundle,
        sighasher.sighash(HashType::ALL, None),
        tx.unmined_id(),
    )
}

/// The Sapling bundle cache, exercised end to end through the transaction verifiers.
///
/// The Sapling counterpart of
/// [`the_halo2_cache_is_reused_only_for_the_transaction_that_earned_it`], and the same reasons
/// apply for it being one test on the shared runtime. Sapling bundles also appear in v4
/// transactions, whose legacy transaction ID is the hash of the whole serialization, so this also
/// covers the key form Orchard never sees.
///
/// The two claims, in order:
///
///   1. a mempool verification records the bundle, and the block that mines the same transaction
///      is answered from that record instead of verifying the proofs and signatures again;
///   2. the record does not carry the transaction past the height-dependent checks: the block one
///      height past its expiry is still rejected.
#[test]
fn the_sapling_cache_is_reused_only_for_the_transaction_that_earned_it() {
    let _init_guard = zebra_test::init();

    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        let state: CacheTestState = MockService::build().for_prop_tests();
        let mempool_verifier = MempoolTxVerifier::new_for_tests(&Network::Mainnet, state.clone());
        let block_verifier = Buffer::new(BlockTxVerifier::new(&Network::Mainnet, state.clone()), 1);

        let (network_upgrade, tx) = cacheable_mainnet_sapling_transaction();
        let expiry_height = tx
            .expiry_height()
            .expect("a V4 or V5 transaction has an expiry height");
        let item = sapling_item(&tx, network_upgrade);
        assert_eq!(
            crate::primitives::sapling::inner_calls_for(&item),
            0,
            "this transaction's bundle must not have been verified before this test"
        );

        // 1. The mempool verification is the only one that reaches the Sapling verifier.
        respond_to_nullifier_and_anchor_check(&state);
        mempool_verifier
            .oneshot(MempoolRequest {
                transaction: Arc::new(tx.clone()).into(),
                height: expiry_height,
            })
            .await
            .expect("a real mainnet Sapling transaction must verify at its expiry height");

        assert_eq!(
            crate::primitives::sapling::inner_calls_for(&item),
            1,
            "the mempool verification must reach the inner Sapling verifier"
        );

        block_verifier
            .clone()
            .oneshot(cache_test_block_request(&tx, expiry_height))
            .await
            .expect("the same transaction must verify in a block");

        assert_eq!(
            crate::primitives::sapling::inner_calls_for(&item),
            1,
            "the block verification must be answered from the cache"
        );

        // 2. The cached verification does not carry the transaction past the expiry check.
        let too_late =
            (expiry_height + 1).expect("a mainnet expiry height is far below the maximum");
        let error = block_verifier
            .clone()
            .oneshot(cache_test_block_request(&tx, too_late))
            .await
            .expect_err("a transaction mined past its expiry height must be rejected");

        assert_eq!(
            transaction_error(error),
            TransactionError::ExpiredTransaction {
                expiry_height,
                block_height: too_late,
                transaction_hash: tx.hash(),
            },
            "the rejection must be the expiry rule, not some other failure"
        );
    });
}
