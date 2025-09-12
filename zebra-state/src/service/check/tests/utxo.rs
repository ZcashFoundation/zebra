//! Test vectors and randomised property tests for UTXO contextual validation

use std::{env, sync::Arc};

use proptest::prelude::*;

use zebra_chain::{
    amount::Amount,
    block::{Block, Height},
    fmt::TypeNameToDebug,
    serialization::ZcashDeserializeInto,
    transaction::{self, LockTime, Transaction},
    transparent,
};

use crate::{
    arbitrary::Prepare,
    constants::MIN_TRANSPARENT_COINBASE_MATURITY,
    service::{
        check, finalized_state::FinalizedState, non_finalized_state::NonFinalizedState, read,
        write::validate_and_commit_non_finalized,
    },
    tests::setup::{new_state_with_mainnet_genesis, transaction_v4_from_coinbase},
    CheckpointVerifiedBlock,
    ValidateContextError::{
        DuplicateTransparentSpend, EarlyTransparentSpend, ImmatureTransparentCoinbaseSpend,
        MissingTransparentOutput, UnshieldedTransparentCoinbaseSpend,
    },
};

/// Check that shielded, mature spends of coinbase transparent outputs succeed.
///
/// This test makes sure there are no spurious rejections that might hide bugs in the other tests.
/// (And that the test infrastructure generally works.)
#[test]
fn accept_shielded_mature_coinbase_utxo_spend() {
    let _init_guard = zebra_test::init();

    let created_height = Height(1);
    let outpoint = transparent::OutPoint {
        hash: transaction::Hash([0u8; 32]),
        index: 0,
    };
    let output = transparent::Output {
        value: Amount::zero(),
        lock_script: transparent::Script::new(&[]),
    };
    let ordered_utxo = transparent::OrderedUtxo::new(output, created_height, 0);

    let min_spend_height = Height(created_height.0 + MIN_TRANSPARENT_COINBASE_MATURITY);
    let spend_restriction = transparent::CoinbaseSpendRestriction::CheckCoinbaseMaturity {
        spend_height: min_spend_height,
    };

    let result =
        check::utxo::transparent_coinbase_spend(outpoint, spend_restriction, ordered_utxo.as_ref());

    assert_eq!(
        result,
        Ok(()),
        "mature transparent coinbase spend check should return Ok(())"
    );
}

/// Check that non-shielded spends of coinbase transparent outputs fail.
#[test]
fn reject_unshielded_coinbase_utxo_spend() {
    let _init_guard = zebra_test::init();

    let created_height = Height(1);
    let outpoint = transparent::OutPoint {
        hash: transaction::Hash([0u8; 32]),
        index: 0,
    };
    let output = transparent::Output {
        value: Amount::zero(),
        lock_script: transparent::Script::new(&[]),
    };
    let ordered_utxo = transparent::OrderedUtxo::new(output, created_height, 0);

    let spend_restriction = transparent::CoinbaseSpendRestriction::DisallowCoinbaseSpend;

    let result =
        check::utxo::transparent_coinbase_spend(outpoint, spend_restriction, ordered_utxo.as_ref());
    assert_eq!(result, Err(UnshieldedTransparentCoinbaseSpend { outpoint }));
}

/// Check that early spends of coinbase transparent outputs fail.
#[test]
fn reject_immature_coinbase_utxo_spend() {
    let _init_guard = zebra_test::init();

    let created_height = Height(1);
    let outpoint = transparent::OutPoint {
        hash: transaction::Hash([0u8; 32]),
        index: 0,
    };
    let output = transparent::Output {
        value: Amount::zero(),
        lock_script: transparent::Script::new(&[]),
    };
    let ordered_utxo = transparent::OrderedUtxo::new(output, created_height, 0);

    let min_spend_height = Height(created_height.0 + MIN_TRANSPARENT_COINBASE_MATURITY);
    let spend_height = Height(min_spend_height.0 - 1);
    let spend_restriction =
        transparent::CoinbaseSpendRestriction::CheckCoinbaseMaturity { spend_height };

    let result =
        check::utxo::transparent_coinbase_spend(outpoint, spend_restriction, ordered_utxo.as_ref());
    assert_eq!(
        result,
        Err(ImmatureTransparentCoinbaseSpend {
            outpoint,
            spend_height,
            min_spend_height,
            created_height
        })
    );
}

// These tests use the `Arbitrary` trait to easily generate complex types,
// then modify those types to cause an error (or to ensure success).
//
// We could use mainnet or testnet blocks in these tests,
// but the differences shouldn't matter,
// because we're only interested in spend validation,
// (and passing various other state checks).

const DEFAULT_UTXO_PROPTEST_CASES: u32 = 16;

proptest! {
    #![proptest_config(
        proptest::test_runner::Config::with_cases(env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_UTXO_PROPTEST_CASES))
    )]

    /// Make sure an arbitrary transparent spend from a previous transaction in this block
    /// is accepted by state contextual validation.
    ///
    /// This test makes sure there are no spurious rejections that might hide bugs in the other tests.
    /// (And that the test infrastructure generally works.)
    ///
    /// It also covers a potential edge case where later transactions can spend outputs
    /// of previous transactions in a block, but earlier transactions can not spend later outputs.
    #[test]
    fn accept_later_transparent_spend_from_this_block(
        output in TypeNameToDebug::<transparent::Output>::arbitrary(),
        mut prevout_input in TypeNameToDebug::<transparent::Input>::arbitrary_with(None),
        use_finalized_state in any::<bool>(),
    ) {
        let _init_guard = zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        // create an output
        let output_transaction = transaction_v4_with_transparent_data([], [], [output.0.clone()]);

        // create a spend
        let expected_outpoint = transparent::OutPoint {
            hash: output_transaction.hash(),
            index: 0,
        };
        prevout_input.set_outpoint(expected_outpoint);
        let spend_transaction = transaction_v4_with_transparent_data(
            [prevout_input.0],
            [(expected_outpoint, output.0)],
            []
        );

        // convert the coinbase transaction to a version that the non-finalized state will accept
        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1
            .transactions
            .extend([output_transaction.into(), spend_transaction.into()]);

        let (mut finalized_state, mut non_finalized_state, _genesis) = new_state_with_mainnet_genesis();
        let previous_non_finalized_state = non_finalized_state.clone();

        // randomly choose to commit the block to the finalized or non-finalized state
        if use_finalized_state {
            let block1 = CheckpointVerifiedBlock::from(Arc::new(block1));
            let commit_result = finalized_state.commit_finalized_direct(block1.clone().into(), None, "test");

            // the block was committed
            prop_assert_eq!(Some((Height(1), block1.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
            prop_assert!(commit_result.is_ok());

            // the non-finalized state didn't change
            prop_assert!(non_finalized_state.eq_internal_state(&previous_non_finalized_state));

            // the finalized state added then spent the UTXO
            prop_assert!(finalized_state.utxo(&expected_outpoint).is_none());
            // the non-finalized state does not have the UTXO
            prop_assert!(non_finalized_state.any_utxo(&expected_outpoint).is_none());
        } else {
            let block1 = Arc::new(block1).prepare();
            let commit_result = validate_and_commit_non_finalized(
                &finalized_state.db,
                &mut non_finalized_state,
                block1.clone()
            );

            // the block was committed
            prop_assert_eq!(commit_result, Ok(()));
            prop_assert_eq!(Some((Height(1), block1.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));

            // the block data is in the non-finalized state
            prop_assert!(!non_finalized_state.eq_internal_state(&previous_non_finalized_state));

            // the non-finalized state has created and spent the UTXO
            prop_assert_eq!(non_finalized_state.chain_count(), 1);
            let chain = non_finalized_state
                .chain_iter()
                .next()
                .unwrap();
            prop_assert!(!chain.unspent_utxos().contains_key(&expected_outpoint));
            prop_assert!(chain.created_utxos.contains_key(&expected_outpoint));
            prop_assert!(chain.spent_utxos.contains_key(&expected_outpoint));

            // the finalized state does not have the UTXO
            prop_assert!(finalized_state.utxo(&expected_outpoint).is_none());
        }
    }

    /// Make sure an arbitrary transparent spend from a previous block
    /// is accepted by state contextual validation.
    #[test]
    fn accept_arbitrary_transparent_spend_from_previous_block(
        output in TypeNameToDebug::<transparent::Output>::arbitrary(),
        mut prevout_input in TypeNameToDebug::<transparent::Input>::arbitrary_with(None),
        use_finalized_state_output in any::<bool>(),
        mut use_finalized_state_spend in any::<bool>(),
    ) {
        let _init_guard = zebra_test::init();

        // if we use the non-finalized state for the first block,
        // we have to use it for the second as well
        if !use_finalized_state_output {
            use_finalized_state_spend = false;
        }

        let mut block2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        let TestState {
            mut finalized_state, mut non_finalized_state, block1, ..
        } = new_state_with_mainnet_transparent_data([], [], [output.0.clone()], use_finalized_state_output);
        let previous_non_finalized_state = non_finalized_state.clone();

        let expected_outpoint = transparent::OutPoint {
            hash: block1.transactions[1].hash(),
            index: 0,
        };
        prevout_input.set_outpoint(expected_outpoint);

        let spend_transaction = transaction_v4_with_transparent_data(
            [prevout_input.0],
            [(expected_outpoint, output.0)],
            []
        );

        // convert the coinbase transaction to a version that the non-finalized state will accept
        block2.transactions[0] = transaction_v4_from_coinbase(&block2.transactions[0]).into();

        block2.transactions.push(spend_transaction.into());

        if use_finalized_state_spend {
            let block2 = CheckpointVerifiedBlock::from(Arc::new(block2));
            let commit_result = finalized_state.commit_finalized_direct(block2.clone().into(),None,  "test");

            // the block was committed
            prop_assert_eq!(Some((Height(2), block2.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
            prop_assert!(commit_result.is_ok());

            // the non-finalized state didn't change
            prop_assert!(non_finalized_state.eq_internal_state(&previous_non_finalized_state));

            // the finalized state has spent the UTXO
            prop_assert!(finalized_state.utxo(&expected_outpoint).is_none());
        } else {
            let block2 = Arc::new(block2).prepare();
            let commit_result = validate_and_commit_non_finalized(
                &finalized_state.db,
                &mut non_finalized_state,
                block2.clone()
            );

            // the block was committed
            prop_assert_eq!(commit_result, Ok(()));
            prop_assert_eq!(Some((Height(2), block2.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));

            // the block data is in the non-finalized state
            prop_assert!(!non_finalized_state.eq_internal_state(&previous_non_finalized_state));

            // the UTXO is spent
            prop_assert_eq!(non_finalized_state.chain_count(), 1);
            let chain = non_finalized_state
                .chain_iter()
                .next()
                .unwrap();
            prop_assert!(!chain.unspent_utxos().contains_key(&expected_outpoint));

            if use_finalized_state_output {
                // the chain has spent the UTXO from the finalized state
                prop_assert!(!chain.created_utxos.contains_key(&expected_outpoint));
                prop_assert!(chain.spent_utxos.contains_key(&expected_outpoint));
                // the finalized state has the UTXO, but it will get deleted on commit
                prop_assert!(finalized_state.utxo(&expected_outpoint).is_some());
            } else {
                // the chain has spent its own UTXO
                prop_assert!(!chain.unspent_utxos().contains_key(&expected_outpoint));
                prop_assert!(chain.created_utxos.contains_key(&expected_outpoint));
                prop_assert!(chain.spent_utxos.contains_key(&expected_outpoint));
                // the finalized state does not have the UTXO
                prop_assert!(finalized_state.utxo(&expected_outpoint).is_none());
            }
        }
    }

    /// Make sure a duplicate transparent spend, by two inputs in the same transaction,
    /// using an output from a previous transaction in this block,
    /// is rejected by state contextual validation.
    #[test]
    fn reject_duplicate_transparent_spend_in_same_transaction_from_same_block(
        output in TypeNameToDebug::<transparent::Output>::arbitrary(),
        mut prevout_input1 in TypeNameToDebug::<transparent::Input>::arbitrary_with(None),
        mut prevout_input2 in TypeNameToDebug::<transparent::Input>::arbitrary_with(None),
    ) {
        let _init_guard = zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        let output_transaction = transaction_v4_with_transparent_data([], [], [output.0.clone()]);

        let expected_outpoint = transparent::OutPoint {
            hash: output_transaction.hash(),
            index: 0,
        };
        prevout_input1.set_outpoint(expected_outpoint);
        prevout_input2.set_outpoint(expected_outpoint);

        let spend_transaction = transaction_v4_with_transparent_data(
            [prevout_input1.0, prevout_input2.0],
            [(expected_outpoint, output.0)],
            []
        );

        // convert the coinbase transaction to a version that the non-finalized state will accept
        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1
            .transactions
            .extend([output_transaction.into(), spend_transaction.into()]);

            let (finalized_state, mut non_finalized_state, genesis) = new_state_with_mainnet_genesis();
            let previous_non_finalized_state = non_finalized_state.clone();

        let block1 = Arc::new(block1).prepare();
        let commit_result = validate_and_commit_non_finalized(
            &finalized_state.db,
            &mut non_finalized_state,
            block1
        );

        // the block was rejected
        prop_assert_eq!(
            commit_result,
            Err(Box::new(DuplicateTransparentSpend {
                outpoint: expected_outpoint,
                location: "the same block",
            })
            .into())
        );
        prop_assert_eq!(Some((Height(0), genesis.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));

        // the non-finalized state did not change
        prop_assert!(non_finalized_state.eq_internal_state(&previous_non_finalized_state));

        // the finalized state does not have the UTXO
        prop_assert!(finalized_state.utxo(&expected_outpoint).is_none());
    }

    /// Make sure a duplicate transparent spend, by two inputs in the same transaction,
    /// using an output from a previous block in this chain,
    /// is rejected by state contextual validation.
    #[test]
    fn reject_duplicate_transparent_spend_in_same_transaction_from_previous_block(
        output in TypeNameToDebug::<transparent::Output>::arbitrary(),
        mut prevout_input1 in TypeNameToDebug::<transparent::Input>::arbitrary_with(None),
        mut prevout_input2 in TypeNameToDebug::<transparent::Input>::arbitrary_with(None),
        use_finalized_state_output in any::<bool>(),
    ) {
        let _init_guard = zebra_test::init();

        let mut block2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        let TestState {
            finalized_state, mut non_finalized_state, block1, ..
        } = new_state_with_mainnet_transparent_data([], [], [output.0.clone()], use_finalized_state_output);
        let previous_non_finalized_state = non_finalized_state.clone();

        let expected_outpoint = transparent::OutPoint {
            hash: block1.transactions[1].hash(),
            index: 0,
        };
        prevout_input1.set_outpoint(expected_outpoint);
        prevout_input2.set_outpoint(expected_outpoint);

        let spend_transaction = transaction_v4_with_transparent_data(
            [prevout_input1.0, prevout_input2.0],
            [(expected_outpoint, output.0)],
            []
        );

        // convert the coinbase transaction to a version that the non-finalized state will accept
        block2.transactions[0] = transaction_v4_from_coinbase(&block2.transactions[0]).into();

        block2.transactions.push(spend_transaction.into());

        let block2 = Arc::new(block2).prepare();
        let commit_result = validate_and_commit_non_finalized(
            &finalized_state.db,
            &mut non_finalized_state,
            block2
        );

        // the block was rejected
        prop_assert_eq!(
            commit_result,
            Err(Box::new(DuplicateTransparentSpend {
                outpoint: expected_outpoint,
                location: "the same block",
            })
            .into())
        );
        prop_assert_eq!(Some((Height(1), block1.hash())), read::best_tip(&non_finalized_state, &finalized_state.db));

        // the non-finalized state did not change
        prop_assert!(non_finalized_state.eq_internal_state(&previous_non_finalized_state));

        if use_finalized_state_output {
            // the finalized state has the UTXO
            prop_assert!(finalized_state.utxo(&expected_outpoint).is_some());
            // the non-finalized state has no chains (so it can't have the UTXO)
            prop_assert!(non_finalized_state.chain_iter().next().is_none());
        } else {
            let chain = non_finalized_state
                .chain_iter()
                .next()
                .unwrap();
            // the non-finalized state has the UTXO
            prop_assert!(chain.unspent_utxos().contains_key(&expected_outpoint));
            // the finalized state does not have the UTXO
            prop_assert!(finalized_state.utxo(&expected_outpoint).is_none());
        }
    }

    /// Make sure a duplicate transparent spend,
    /// by two inputs in different transactions in the same block,
    /// using an output from a previous block in this chain,
    /// is rejected by state contextual validation.
    #[test]
    fn reject_duplicate_transparent_spend_in_same_block_from_previous_block(
        output in TypeNameToDebug::<transparent::Output>::arbitrary(),
        mut prevout_input1 in TypeNameToDebug::<transparent::Input>::arbitrary_with(None),
        mut prevout_input2 in TypeNameToDebug::<transparent::Input>::arbitrary_with(None),
        use_finalized_state_output in any::<bool>(),
    ) {
        let _init_guard = zebra_test::init();

        let mut block2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        let TestState {
            finalized_state, mut non_finalized_state, block1, ..
        } = new_state_with_mainnet_transparent_data([], [], [output.0.clone()], use_finalized_state_output);
        let previous_non_finalized_state = non_finalized_state.clone();

        let expected_outpoint = transparent::OutPoint {
            hash: block1.transactions[1].hash(),
            index: 0,
        };
        prevout_input1.set_outpoint(expected_outpoint);
        prevout_input2.set_outpoint(expected_outpoint);

        let spend_transaction1 = transaction_v4_with_transparent_data(
            [prevout_input1.0],
            [(expected_outpoint, output.0.clone())],
            []
        );
        let spend_transaction2 = transaction_v4_with_transparent_data(
            [prevout_input2.0],
            [(expected_outpoint, output.0)],
            []
        );

        // convert the coinbase transaction to a version that the non-finalized state will accept
        block2.transactions[0] = transaction_v4_from_coinbase(&block2.transactions[0]).into();

        block2
            .transactions
            .extend([spend_transaction1.into(), spend_transaction2.into()]);

        let block2 = Arc::new(block2).prepare();
        let commit_result = validate_and_commit_non_finalized(
            &finalized_state.db,
            &mut non_finalized_state,
            block2
        );

        // the block was rejected
        prop_assert_eq!(
            commit_result,
            Err(Box::new(DuplicateTransparentSpend {
                outpoint: expected_outpoint,
                location: "the same block",
            })
            .into())
        );
        prop_assert_eq!(Some((Height(1), block1.hash())), read::best_tip(&non_finalized_state, &finalized_state.db));

        // the non-finalized state did not change
        prop_assert!(non_finalized_state.eq_internal_state(&previous_non_finalized_state));

        if use_finalized_state_output {
            // the finalized state has the UTXO
            prop_assert!(finalized_state.utxo(&expected_outpoint).is_some());
            // the non-finalized state has no chains (so it can't have the UTXO)
            prop_assert!(non_finalized_state.chain_iter().next().is_none());
        } else {
            let chain = non_finalized_state
                .chain_iter()
                .next()
                .unwrap();
            // the non-finalized state has the UTXO
            prop_assert!(chain.unspent_utxos().contains_key(&expected_outpoint));
            // the finalized state does not have the UTXO
            prop_assert!(finalized_state.utxo(&expected_outpoint).is_none());
        }
    }

    /// Make sure a duplicate transparent spend,
    /// by two inputs in different blocks in the same chain,
    /// using an output from a previous block in this chain,
    /// is rejected by state contextual validation.
    #[test]
    fn reject_duplicate_transparent_spend_in_same_chain_from_previous_block(
        output in TypeNameToDebug::<transparent::Output>::arbitrary(),
        mut prevout_input1 in TypeNameToDebug::<transparent::Input>::arbitrary_with(None),
        mut prevout_input2 in TypeNameToDebug::<transparent::Input>::arbitrary_with(None),
        use_finalized_state_output in any::<bool>(),
        mut use_finalized_state_spend in any::<bool>(),
    ) {
        let _init_guard = zebra_test::init();

        // if we use the non-finalized state for the first block,
        // we have to use it for the second as well
        if !use_finalized_state_output {
            use_finalized_state_spend = false;
        }

        let mut block2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");
        let mut block3 = zebra_test::vectors::BLOCK_MAINNET_3_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        let TestState {
            mut finalized_state, mut non_finalized_state, block1, ..
        } = new_state_with_mainnet_transparent_data([], [], [output.0.clone()], use_finalized_state_output);
        let mut previous_non_finalized_state = non_finalized_state.clone();

        let expected_outpoint = transparent::OutPoint {
            hash: block1.transactions[1].hash(),
            index: 0,
        };
        prevout_input1.set_outpoint(expected_outpoint);
        prevout_input2.set_outpoint(expected_outpoint);

        let spend_transaction1 = transaction_v4_with_transparent_data(
            [prevout_input1.0],
            [(expected_outpoint, output.0.clone())],
            []
        );
        let spend_transaction2 = transaction_v4_with_transparent_data(
            [prevout_input2.0],
            [(expected_outpoint, output.0)],
            []
        );

        // convert the coinbase transactions to a version that the non-finalized state will accept
        block2.transactions[0] = transaction_v4_from_coinbase(&block2.transactions[0]).into();
        block3.transactions[0] = transaction_v4_from_coinbase(&block3.transactions[0]).into();

        block2.transactions.push(spend_transaction1.into());
        block3.transactions.push(spend_transaction2.into());

        let block2 = Arc::new(block2);

        if use_finalized_state_spend {
            let block2 = CheckpointVerifiedBlock::from(block2.clone());
            let commit_result = finalized_state.commit_finalized_direct(block2.clone().into(), None, "test");

            // the block was committed
            prop_assert_eq!(Some((Height(2), block2.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
            prop_assert!(commit_result.is_ok());

            // the non-finalized state didn't change
            prop_assert!(non_finalized_state.eq_internal_state(&previous_non_finalized_state));

            // the finalized state has spent the UTXO
            prop_assert!(finalized_state.utxo(&expected_outpoint).is_none());
            // the non-finalized state does not have the UTXO
            prop_assert!(non_finalized_state.any_utxo(&expected_outpoint).is_none());
        } else {
            let block2 = block2.clone().prepare();
            let commit_result = validate_and_commit_non_finalized(
                &finalized_state.db,
                &mut non_finalized_state,
                block2.clone()
            );

            // the block was committed
            prop_assert_eq!(commit_result, Ok(()));
            prop_assert_eq!(Some((Height(2), block2.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));

            // the block data is in the non-finalized state
            prop_assert!(!non_finalized_state.eq_internal_state(&previous_non_finalized_state));

            prop_assert_eq!(non_finalized_state.chain_count(), 1);
            let chain = non_finalized_state
                .chain_iter()
                .next()
                .unwrap();

            if use_finalized_state_output {
                // the finalized state has the unspent UTXO
                prop_assert!(finalized_state.utxo(&expected_outpoint).is_some());
                // the non-finalized state has spent the UTXO
                prop_assert!(chain.spent_utxos.contains_key(&expected_outpoint));
            } else {
                // the non-finalized state has created and spent the UTXO
                prop_assert!(!chain.unspent_utxos().contains_key(&expected_outpoint));
                prop_assert!(chain.created_utxos.contains_key(&expected_outpoint));
                prop_assert!(chain.spent_utxos.contains_key(&expected_outpoint));
                // the finalized state does not have the UTXO
                prop_assert!(finalized_state.utxo(&expected_outpoint).is_none());
            }

            previous_non_finalized_state = non_finalized_state.clone();
        }

        let block3 = Arc::new(block3).prepare();
        let commit_result = validate_and_commit_non_finalized(
            &finalized_state.db,
            &mut non_finalized_state,
            block3
        );

        // the block was rejected
        if use_finalized_state_spend {
            prop_assert_eq!(
                commit_result,
                Err(Box::new(MissingTransparentOutput {
                    outpoint: expected_outpoint,
                    location: "the non-finalized and finalized chain",
                })
                .into())
            );
        } else {
            prop_assert_eq!(
                commit_result,
                Err(Box::new(DuplicateTransparentSpend {
                    outpoint: expected_outpoint,
                    location: "the non-finalized chain",
                })
                .into())
            );
        }
        prop_assert_eq!(Some((Height(2), block2.hash())), read::best_tip(&non_finalized_state, &finalized_state.db));

        // the non-finalized state did not change
        prop_assert!(non_finalized_state.eq_internal_state(&previous_non_finalized_state));

        // Since the non-finalized state has not changed, we don't need to check it again
        if use_finalized_state_spend {
            // the finalized state has spent the UTXO
            prop_assert!(finalized_state.utxo(&expected_outpoint).is_none());
        } else if use_finalized_state_output {
            // the finalized state has the unspent UTXO
            // but the non-finalized state has spent it
            prop_assert!(finalized_state.utxo(&expected_outpoint).is_some());
        } else {
            // the non-finalized state has created and spent the UTXO
            // and the finalized state does not have the UTXO
            prop_assert!(finalized_state.utxo(&expected_outpoint).is_none());
        }
    }

    /// Make sure a transparent spend with a missing UTXO
    /// is rejected by state contextual validation.
    #[test]
    fn reject_missing_transparent_spend(
        unused_output in TypeNameToDebug::<transparent::Output>::arbitrary(),
        prevout_input in TypeNameToDebug::<transparent::Input>::arbitrary_with(None),
    ) {
        let _init_guard = zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        let expected_outpoint = prevout_input.outpoint().unwrap();
        let spend_transaction = transaction_v4_with_transparent_data(
            [prevout_input.0],
            // provide an fake spent output for value fixups
            [(expected_outpoint, unused_output.0)],
            []
        );

        // convert the coinbase transaction to a version that the non-finalized state will accept
        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1.transactions.push(spend_transaction.into());

        let (finalized_state, mut non_finalized_state, genesis) = new_state_with_mainnet_genesis();
        let previous_non_finalized_state = non_finalized_state.clone();

        let block1 = Arc::new(block1).prepare();
        let commit_result = validate_and_commit_non_finalized(
            &finalized_state.db,
            &mut non_finalized_state,
            block1
        );

        // the block was rejected
        prop_assert_eq!(
            commit_result,
            Err(Box::new(MissingTransparentOutput {
                outpoint: expected_outpoint,
                location: "the non-finalized and finalized chain",
            })
            .into())
        );
        prop_assert_eq!(Some((Height(0), genesis.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));

        // the non-finalized state did not change
        prop_assert!(non_finalized_state.eq_internal_state(&previous_non_finalized_state));

        // the finalized state does not have the UTXO
        prop_assert!(finalized_state.utxo(&expected_outpoint).is_none());
    }

    /// Make sure transparent output spends are rejected by state contextual validation,
    /// if they spend an output in the same or later transaction in the block.
    ///
    /// This test covers a potential edge case where later transactions can spend outputs
    /// of previous transactions in a block, but earlier transactions can not spend later outputs.
    #[test]
    fn reject_earlier_transparent_spend_from_this_block(
        output in TypeNameToDebug::<transparent::Output>::arbitrary(),
        mut prevout_input in TypeNameToDebug::<transparent::Input>::arbitrary_with(None),
    ) {
        let _init_guard = zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        // create an output
        let output_transaction = transaction_v4_with_transparent_data([], [], [output.0.clone()]);

        // create a spend
        let expected_outpoint = transparent::OutPoint {
            hash: output_transaction.hash(),
            index: 0,
        };
        prevout_input.set_outpoint(expected_outpoint);
        let spend_transaction = transaction_v4_with_transparent_data(
            [prevout_input.0],
            [(expected_outpoint, output.0)],
            []
        );

        // convert the coinbase transaction to a version that the non-finalized state will accept
        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        // put the spend transaction before the output transaction in the block
        block1
            .transactions
            .extend([spend_transaction.into(), output_transaction.into()]);

            let (finalized_state, mut non_finalized_state, genesis) = new_state_with_mainnet_genesis();
            let previous_non_finalized_state = non_finalized_state.clone();

        let block1 = Arc::new(block1).prepare();
        let commit_result = validate_and_commit_non_finalized(
            &finalized_state.db,
            &mut non_finalized_state,
            block1
        );

        // the block was rejected
        prop_assert_eq!(
            commit_result,
            Err(Box::new(EarlyTransparentSpend {
                outpoint: expected_outpoint,
            })
            .into())
        );
        prop_assert_eq!(Some((Height(0), genesis.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));

        // the non-finalized state did not change
        prop_assert!(non_finalized_state.eq_internal_state(&previous_non_finalized_state));

        // the finalized state does not have the UTXO
        prop_assert!(finalized_state.utxo(&expected_outpoint).is_none());
    }
}

/// State associated with transparent UTXO tests.
struct TestState {
    /// The pre-populated finalized state.
    finalized_state: FinalizedState,

    /// The pre-populated non-finalized state.
    non_finalized_state: NonFinalizedState,

    /// The genesis block that has already been committed to the `state` service's
    /// finalized state.
    #[allow(dead_code)]
    genesis: CheckpointVerifiedBlock,

    /// A block at height 1, that has already been committed to the `state` service.
    block1: Arc<Block>,
}

/// Return a new `StateService` containing the mainnet genesis block.
/// Also returns the finalized genesis block itself.
fn new_state_with_mainnet_transparent_data(
    inputs: impl IntoIterator<Item = transparent::Input>,
    spent_outputs: impl IntoIterator<Item = (transparent::OutPoint, transparent::Output)>,
    outputs: impl IntoIterator<Item = transparent::Output>,
    use_finalized_state: bool,
) -> TestState {
    let (mut finalized_state, mut non_finalized_state, genesis) = new_state_with_mainnet_genesis();
    let previous_non_finalized_state = non_finalized_state.clone();

    let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block should deserialize");

    let outputs: Vec<_> = outputs.into_iter().collect();
    let outputs_len: u32 = outputs
        .len()
        .try_into()
        .expect("unexpectedly large output iterator");

    let transaction = transaction_v4_with_transparent_data(inputs, spent_outputs, outputs);
    let transaction_hash = transaction.hash();

    let expected_outpoints = (0..outputs_len).map(|index| transparent::OutPoint {
        hash: transaction_hash,
        index,
    });

    block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();
    block1.transactions.push(transaction.into());

    let block1 = Arc::new(block1);

    if use_finalized_state {
        let block1 = CheckpointVerifiedBlock::from(block1.clone());
        let commit_result =
            finalized_state.commit_finalized_direct(block1.clone().into(), None, "test");

        // the block was committed
        assert_eq!(
            Some((Height(1), block1.hash)),
            read::best_tip(&non_finalized_state, &finalized_state.db)
        );
        assert!(commit_result.is_ok());

        // the non-finalized state didn't change
        assert!(non_finalized_state.eq_internal_state(&previous_non_finalized_state));

        for expected_outpoint in expected_outpoints {
            // the finalized state has the UTXOs
            assert!(finalized_state.utxo(&expected_outpoint).is_some());
            // the non-finalized state does not have the UTXOs
            assert!(non_finalized_state.any_utxo(&expected_outpoint).is_none());
        }
    } else {
        let block1 = block1.clone().prepare();
        let commit_result = validate_and_commit_non_finalized(
            &finalized_state.db,
            &mut non_finalized_state,
            block1.clone(),
        );

        // the block was committed
        assert_eq!(
            commit_result,
            Ok(()),
            "unexpected invalid block 1, modified with generated transactions: \n\
             converted coinbase: {:?} \n\
             generated non-coinbase: {:?}",
            block1.block.transactions[0],
            block1.block.transactions[1],
        );
        assert_eq!(
            Some((Height(1), block1.hash)),
            read::best_tip(&non_finalized_state, &finalized_state.db)
        );

        // the block data is in the non-finalized state
        assert!(!non_finalized_state.eq_internal_state(&previous_non_finalized_state));

        assert_eq!(non_finalized_state.chain_count(), 1);

        for expected_outpoint in expected_outpoints {
            // the non-finalized state has the unspent UTXOs
            assert!(non_finalized_state
                .chain_iter()
                .next()
                .unwrap()
                .unspent_utxos()
                .contains_key(&expected_outpoint));
            // the finalized state does not have the UTXOs
            assert!(finalized_state.utxo(&expected_outpoint).is_none());
        }
    }

    TestState {
        finalized_state,
        non_finalized_state,
        genesis,
        block1,
    }
}

/// Return a `Transaction::V4`, using transparent `inputs` and their `spent_outputs`,
/// and newly created `outputs`.
///
/// Other fields have empty or default values.
fn transaction_v4_with_transparent_data(
    inputs: impl IntoIterator<Item = transparent::Input>,
    spent_outputs: impl IntoIterator<Item = (transparent::OutPoint, transparent::Output)>,
    outputs: impl IntoIterator<Item = transparent::Output>,
) -> Transaction {
    let inputs: Vec<_> = inputs.into_iter().collect();
    let outputs: Vec<_> = outputs.into_iter().collect();

    let mut transaction = Transaction::V4 {
        inputs,
        outputs,
        lock_time: LockTime::min_lock_time_timestamp(),
        expiry_height: Height(0),
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    // do required fixups, but ignore any errors,
    // because we're not checking all the consensus rules here
    let _ = transaction.fix_remaining_value(&spent_outputs.into_iter().collect());

    transaction
}
