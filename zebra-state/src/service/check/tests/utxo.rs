//! Randomised property tests for UTXO contextual validation

use std::{convert::TryInto, env, sync::Arc};

use proptest::prelude::*;

use zebra_chain::{
    block::{Block, Height},
    fmt::TypeNameToDebug,
    serialization::ZcashDeserializeInto,
    transaction::{LockTime, Transaction},
    transparent,
};

use crate::{
    arbitrary::Prepare,
    service::StateService,
    tests::setup::{new_state_with_mainnet_genesis, transaction_v4_from_coinbase},
    FinalizedBlock,
    ValidateContextError::{
        DuplicateTransparentSpend, EarlyTransparentSpend, MissingTransparentOutput,
    },
};

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
        zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        // create an output
        let output_transaction = transaction_v4_with_transparent_data([], [output.0]);

        // create a spend
        let expected_outpoint = transparent::OutPoint {
            hash: output_transaction.hash(),
            index: 0,
        };
        prevout_input.set_outpoint(expected_outpoint);
        let spend_transaction = transaction_v4_with_transparent_data([prevout_input.0], []);

        // convert the coinbase transaction to a version that the non-finalized state will accept
        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1
            .transactions
            .extend([output_transaction.into(), spend_transaction.into()]);

        let (mut state, _genesis) = new_state_with_mainnet_genesis();
        let previous_mem = state.mem.clone();

        // randomly choose to commit the block to the finalized or non-finalized state
        if use_finalized_state {
            let block1 = FinalizedBlock::from(Arc::new(block1));
            let commit_result = state.disk.commit_finalized_direct(block1.clone(), "test");

            // the block was committed
            prop_assert_eq!(Some((Height(1), block1.hash)), state.best_tip());
            prop_assert!(commit_result.is_ok());

            // the non-finalized state didn't change
            prop_assert!(state.mem.eq_internal_state(&previous_mem));

            // the finalized state added then spent the UTXO
            prop_assert!(state.disk.utxo(&expected_outpoint).is_none());
            // the non-finalized state does not have the UTXO
            prop_assert!(state.mem.any_utxo(&expected_outpoint).is_none());
        } else {
            let block1 = Arc::new(block1).prepare();
            let commit_result = state.validate_and_commit(block1.clone());

            // the block was committed
            prop_assert_eq!(commit_result, Ok(()));
            prop_assert_eq!(Some((Height(1), block1.hash)), state.best_tip());

            // the block data is in the non-finalized state
            prop_assert!(!state.mem.eq_internal_state(&previous_mem));

            // the non-finalized state has created and spent the UTXO
            prop_assert_eq!(state.mem.chain_set.len(), 1);
            let chain = state
                .mem
                .chain_set
                .iter()
                .next()
                .unwrap();
            prop_assert!(!chain.unspent_utxos().contains_key(&expected_outpoint));
            prop_assert!(chain.created_utxos.contains_key(&expected_outpoint));
            prop_assert!(chain.spent_utxos.contains(&expected_outpoint));

            // the finalized state does not have the UTXO
            prop_assert!(state.disk.utxo(&expected_outpoint).is_none());
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
        zebra_test::init();

        // if we use the non-finalized state for the first block,
        // we have to use it for the second as well
        if !use_finalized_state_output {
            use_finalized_state_spend = false;
        }

        let mut block2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        let TestState {
            mut state, block1, ..
        } = new_state_with_mainnet_transparent_data([], [output.0], use_finalized_state_output);
        let previous_mem = state.mem.clone();

        let expected_outpoint = transparent::OutPoint {
            hash: block1.transactions[1].hash(),
            index: 0,
        };
        prevout_input.set_outpoint(expected_outpoint);

        let spend_transaction = transaction_v4_with_transparent_data([prevout_input.0], []);

        // convert the coinbase transaction to a version that the non-finalized state will accept
        block2.transactions[0] = transaction_v4_from_coinbase(&block2.transactions[0]).into();

        block2.transactions.push(spend_transaction.into());

        if use_finalized_state_spend {
            let block2 = FinalizedBlock::from(Arc::new(block2));
            let commit_result = state.disk.commit_finalized_direct(block2.clone(), "test");

            // the block was committed
            prop_assert_eq!(Some((Height(2), block2.hash)), state.best_tip());
            prop_assert!(commit_result.is_ok());

            // the non-finalized state didn't change
            prop_assert!(state.mem.eq_internal_state(&previous_mem));

            // the finalized state has spent the UTXO
            prop_assert!(state.disk.utxo(&expected_outpoint).is_none());
        } else {
            let block2 = Arc::new(block2).prepare();
            let commit_result = state.validate_and_commit(block2.clone());

            // the block was committed
            prop_assert_eq!(commit_result, Ok(()));
            prop_assert_eq!(Some((Height(2), block2.hash)), state.best_tip());

            // the block data is in the non-finalized state
            prop_assert!(!state.mem.eq_internal_state(&previous_mem));

            // the UTXO is spent
            prop_assert_eq!(state.mem.chain_set.len(), 1);
            let chain = state
                .mem
                .chain_set
                .iter()
                .next()
                .unwrap();
            prop_assert!(!chain.unspent_utxos().contains_key(&expected_outpoint));

            if use_finalized_state_output {
                // the chain has spent the UTXO from the finalized state
                prop_assert!(!chain.created_utxos.contains_key(&expected_outpoint));
                prop_assert!(chain.spent_utxos.contains(&expected_outpoint));
                // the finalized state has the UTXO, but it will get deleted on commit
                prop_assert!(state.disk.utxo(&expected_outpoint).is_some());
            } else {
                // the chain has spent its own UTXO
                prop_assert!(!chain.unspent_utxos().contains_key(&expected_outpoint));
                prop_assert!(chain.created_utxos.contains_key(&expected_outpoint));
                prop_assert!(chain.spent_utxos.contains(&expected_outpoint));
                // the finalized state does not have the UTXO
                prop_assert!(state.disk.utxo(&expected_outpoint).is_none());
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
        zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        let output_transaction = transaction_v4_with_transparent_data([], [output.0]);

        let expected_outpoint = transparent::OutPoint {
            hash: output_transaction.hash(),
            index: 0,
        };
        prevout_input1.set_outpoint(expected_outpoint);
        prevout_input2.set_outpoint(expected_outpoint);

        let spend_transaction =
            transaction_v4_with_transparent_data([prevout_input1.0, prevout_input2.0], []);

        // convert the coinbase transaction to a version that the non-finalized state will accept
        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1
            .transactions
            .extend([output_transaction.into(), spend_transaction.into()]);

        let (mut state, genesis) = new_state_with_mainnet_genesis();
        let previous_mem = state.mem.clone();

        let block1 = Arc::new(block1).prepare();
        let commit_result = state.validate_and_commit(block1);

        // the block was rejected
        prop_assert_eq!(
            commit_result,
            Err(DuplicateTransparentSpend {
                outpoint: expected_outpoint,
                location: "the same block",
            }
            .into())
        );
        prop_assert_eq!(Some((Height(0), genesis.hash)), state.best_tip());

        // the non-finalized state did not change
        prop_assert!(state.mem.eq_internal_state(&previous_mem));

        // the finalized state does not have the UTXO
        prop_assert!(state.disk.utxo(&expected_outpoint).is_none());
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
        zebra_test::init();

        let mut block2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        let TestState {
            mut state, block1, ..
        } = new_state_with_mainnet_transparent_data([], [output.0], use_finalized_state_output);
        let previous_mem = state.mem.clone();

        let expected_outpoint = transparent::OutPoint {
            hash: block1.transactions[1].hash(),
            index: 0,
        };
        prevout_input1.set_outpoint(expected_outpoint);
        prevout_input2.set_outpoint(expected_outpoint);

        let spend_transaction =
            transaction_v4_with_transparent_data([prevout_input1.0, prevout_input2.0], []);

        // convert the coinbase transaction to a version that the non-finalized state will accept
        block2.transactions[0] = transaction_v4_from_coinbase(&block2.transactions[0]).into();

        block2.transactions.push(spend_transaction.into());

        let block2 = Arc::new(block2).prepare();
        let commit_result = state.validate_and_commit(block2);

        // the block was rejected
        prop_assert_eq!(
            commit_result,
            Err(DuplicateTransparentSpend {
                outpoint: expected_outpoint,
                location: "the same block",
            }
            .into())
        );
        prop_assert_eq!(Some((Height(1), block1.hash())), state.best_tip());

        // the non-finalized state did not change
        prop_assert!(state.mem.eq_internal_state(&previous_mem));

        if use_finalized_state_output {
            // the finalized state has the UTXO
            prop_assert!(state.disk.utxo(&expected_outpoint).is_some());
            // the non-finalized state has no chains (so it can't have the UTXO)
            prop_assert!(state.mem.chain_set.iter().next().is_none());
        } else {
            let chain = state
                .mem
                .chain_set
                .iter()
                .next()
                .unwrap();
            // the non-finalized state has the UTXO
            prop_assert!(chain.unspent_utxos().contains_key(&expected_outpoint));
            // the finalized state does not have the UTXO
            prop_assert!(state.disk.utxo(&expected_outpoint).is_none());
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
        zebra_test::init();

        let mut block2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        let TestState {
            mut state, block1, ..
        } = new_state_with_mainnet_transparent_data([], [output.0], use_finalized_state_output);
        let previous_mem = state.mem.clone();

        let expected_outpoint = transparent::OutPoint {
            hash: block1.transactions[1].hash(),
            index: 0,
        };
        prevout_input1.set_outpoint(expected_outpoint);
        prevout_input2.set_outpoint(expected_outpoint);

        let spend_transaction1 = transaction_v4_with_transparent_data([prevout_input1.0], []);
        let spend_transaction2 = transaction_v4_with_transparent_data([prevout_input2.0], []);

        // convert the coinbase transaction to a version that the non-finalized state will accept
        block2.transactions[0] = transaction_v4_from_coinbase(&block2.transactions[0]).into();

        block2
            .transactions
            .extend([spend_transaction1.into(), spend_transaction2.into()]);

        let block2 = Arc::new(block2).prepare();
        let commit_result = state.validate_and_commit(block2);

        // the block was rejected
        prop_assert_eq!(
            commit_result,
            Err(DuplicateTransparentSpend {
                outpoint: expected_outpoint,
                location: "the same block",
            }
            .into())
        );
        prop_assert_eq!(Some((Height(1), block1.hash())), state.best_tip());

        // the non-finalized state did not change
        prop_assert!(state.mem.eq_internal_state(&previous_mem));

        if use_finalized_state_output {
            // the finalized state has the UTXO
            prop_assert!(state.disk.utxo(&expected_outpoint).is_some());
            // the non-finalized state has no chains (so it can't have the UTXO)
            prop_assert!(state.mem.chain_set.iter().next().is_none());
        } else {
            let chain = state
                .mem
                .chain_set
                .iter()
                .next()
                .unwrap();
            // the non-finalized state has the UTXO
            prop_assert!(chain.unspent_utxos().contains_key(&expected_outpoint));
            // the finalized state does not have the UTXO
            prop_assert!(state.disk.utxo(&expected_outpoint).is_none());
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
        zebra_test::init();

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
            mut state, block1, ..
        } = new_state_with_mainnet_transparent_data([], [output.0], use_finalized_state_output);
        let mut previous_mem = state.mem.clone();

        let expected_outpoint = transparent::OutPoint {
            hash: block1.transactions[1].hash(),
            index: 0,
        };
        prevout_input1.set_outpoint(expected_outpoint);
        prevout_input2.set_outpoint(expected_outpoint);

        let spend_transaction1 = transaction_v4_with_transparent_data([prevout_input1.0], []);
        let spend_transaction2 = transaction_v4_with_transparent_data([prevout_input2.0], []);

        // convert the coinbase transactions to a version that the non-finalized state will accept
        block2.transactions[0] = transaction_v4_from_coinbase(&block2.transactions[0]).into();
        block3.transactions[0] = transaction_v4_from_coinbase(&block3.transactions[0]).into();

        block2.transactions.push(spend_transaction1.into());
        block3.transactions.push(spend_transaction2.into());

        let block2 = Arc::new(block2);

        if use_finalized_state_spend {
            let block2 = FinalizedBlock::from(block2.clone());
            let commit_result = state.disk.commit_finalized_direct(block2.clone(), "test");

            // the block was committed
            prop_assert_eq!(Some((Height(2), block2.hash)), state.best_tip());
            prop_assert!(commit_result.is_ok());

            // the non-finalized state didn't change
            prop_assert!(state.mem.eq_internal_state(&previous_mem));

            // the finalized state has spent the UTXO
            prop_assert!(state.disk.utxo(&expected_outpoint).is_none());
            // the non-finalized state does not have the UTXO
            prop_assert!(state.mem.any_utxo(&expected_outpoint).is_none());
        } else {
            let block2 = block2.clone().prepare();
            let commit_result = state.validate_and_commit(block2.clone());

            // the block was committed
            prop_assert_eq!(commit_result, Ok(()));
            prop_assert_eq!(Some((Height(2), block2.hash)), state.best_tip());

            // the block data is in the non-finalized state
            prop_assert!(!state.mem.eq_internal_state(&previous_mem));

            prop_assert_eq!(state.mem.chain_set.len(), 1);
            let chain = state
                .mem
                .chain_set
                .iter()
                .next()
                .unwrap();

            if use_finalized_state_output {
                // the finalized state has the unspent UTXO
                prop_assert!(state.disk.utxo(&expected_outpoint).is_some());
                // the non-finalized state has spent the UTXO
                prop_assert!(chain.spent_utxos.contains(&expected_outpoint));
            } else {
                // the non-finalized state has created and spent the UTXO
                prop_assert!(!chain.unspent_utxos().contains_key(&expected_outpoint));
                prop_assert!(chain.created_utxos.contains_key(&expected_outpoint));
                prop_assert!(chain.spent_utxos.contains(&expected_outpoint));
                // the finalized state does not have the UTXO
                prop_assert!(state.disk.utxo(&expected_outpoint).is_none());
            }

            previous_mem = state.mem.clone();
        }

        let block3 = Arc::new(block3).prepare();
        let commit_result = state.validate_and_commit(block3);

        // the block was rejected
        if use_finalized_state_spend {
            prop_assert_eq!(
                commit_result,
                Err(MissingTransparentOutput {
                    outpoint: expected_outpoint,
                    location: "the non-finalized and finalized chain",
                }
                .into())
            );
        } else {
            prop_assert_eq!(
                commit_result,
                Err(DuplicateTransparentSpend {
                    outpoint: expected_outpoint,
                    location: "the non-finalized chain",
                }
                .into())
            );
        }
        prop_assert_eq!(Some((Height(2), block2.hash())), state.best_tip());

        // the non-finalized state did not change
        prop_assert!(state.mem.eq_internal_state(&previous_mem));

        // Since the non-finalized state has not changed, we don't need to check it again
        if use_finalized_state_spend {
            // the finalized state has spent the UTXO
            prop_assert!(state.disk.utxo(&expected_outpoint).is_none());
        } else if use_finalized_state_output {
            // the finalized state has the unspent UTXO
            // but the non-finalized state has spent it
            prop_assert!(state.disk.utxo(&expected_outpoint).is_some());
        } else {
            // the non-finalized state has created and spent the UTXO
            // and the finalized state does not have the UTXO
            prop_assert!(state.disk.utxo(&expected_outpoint).is_none());
        }
    }

    /// Make sure a transparent spend with a missing UTXO
    /// is rejected by state contextual validation.
    #[test]
    fn reject_missing_transparent_spend(
        prevout_input in TypeNameToDebug::<transparent::Input>::arbitrary_with(None),
    ) {
        zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        let expected_outpoint = prevout_input.outpoint().unwrap();
        let spend_transaction = transaction_v4_with_transparent_data([prevout_input.0], []);

        // convert the coinbase transaction to a version that the non-finalized state will accept
        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1.transactions.push(spend_transaction.into());

        let (mut state, genesis) = new_state_with_mainnet_genesis();
        let previous_mem = state.mem.clone();

        let block1 = Arc::new(block1).prepare();
        let commit_result = state.validate_and_commit(block1);

        // the block was rejected
        prop_assert_eq!(
            commit_result,
            Err(MissingTransparentOutput {
                outpoint: expected_outpoint,
                location: "the non-finalized and finalized chain",
            }
            .into())
        );
        prop_assert_eq!(Some((Height(0), genesis.hash)), state.best_tip());

        // the non-finalized state did not change
        prop_assert!(state.mem.eq_internal_state(&previous_mem));

        // the finalized state does not have the UTXO
        prop_assert!(state.disk.utxo(&expected_outpoint).is_none());
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
        zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        // create an output
        let output_transaction = transaction_v4_with_transparent_data([], [output.0]);

        // create a spend
        let expected_outpoint = transparent::OutPoint {
            hash: output_transaction.hash(),
            index: 0,
        };
        prevout_input.set_outpoint(expected_outpoint);
        let spend_transaction = transaction_v4_with_transparent_data([prevout_input.0], []);

        // convert the coinbase transaction to a version that the non-finalized state will accept
        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        // put the spend transaction before the output transaction in the block
        block1
            .transactions
            .extend([spend_transaction.into(), output_transaction.into()]);

        let (mut state, genesis) = new_state_with_mainnet_genesis();
        let previous_mem = state.mem.clone();

        let block1 = Arc::new(block1).prepare();
        let commit_result = state.validate_and_commit(block1);

        // the block was rejected
        prop_assert_eq!(
            commit_result,
            Err(EarlyTransparentSpend {
                outpoint: expected_outpoint,
            }
            .into())
        );
        prop_assert_eq!(Some((Height(0), genesis.hash)), state.best_tip());

        // the non-finalized state did not change
        prop_assert!(state.mem.eq_internal_state(&previous_mem));

        // the finalized state does not have the UTXO
        prop_assert!(state.disk.utxo(&expected_outpoint).is_none());
    }
}

/// State associated with transparent UTXO tests.
struct TestState {
    /// The pre-populated state service.
    state: StateService,

    /// The genesis block that has already been committed to the `state` service's
    /// finalized state.
    #[allow(dead_code)]
    genesis: FinalizedBlock,

    /// A block at height 1, that has already been committed to the `state` service.
    block1: Arc<Block>,
}

/// Return a new `StateService` containing the mainnet genesis block.
/// Also returns the finalized genesis block itself.
fn new_state_with_mainnet_transparent_data(
    inputs: impl IntoIterator<Item = transparent::Input>,
    outputs: impl IntoIterator<Item = transparent::Output>,
    use_finalized_state: bool,
) -> TestState {
    let (mut state, genesis) = new_state_with_mainnet_genesis();
    let previous_mem = state.mem.clone();

    let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block should deserialize");

    let outputs: Vec<_> = outputs.into_iter().collect();
    let outputs_len: u32 = outputs
        .len()
        .try_into()
        .expect("unexpectedly large output iterator");

    let transaction = transaction_v4_with_transparent_data(inputs, outputs);
    let transaction_hash = transaction.hash();

    let expected_outpoints = (0..outputs_len).map(|index| transparent::OutPoint {
        hash: transaction_hash,
        index,
    });

    block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();
    block1.transactions.push(transaction.into());

    let block1 = Arc::new(block1);

    if use_finalized_state {
        let block1 = FinalizedBlock::from(block1.clone());
        let commit_result = state.disk.commit_finalized_direct(block1.clone(), "test");

        // the block was committed
        assert_eq!(Some((Height(1), block1.hash)), state.best_tip());
        assert!(commit_result.is_ok());

        // the non-finalized state didn't change
        assert!(state.mem.eq_internal_state(&previous_mem));

        for expected_outpoint in expected_outpoints {
            // the finalized state has the UTXOs
            assert!(state.disk.utxo(&expected_outpoint).is_some());
            // the non-finalized state does not have the UTXOs
            assert!(state.mem.any_utxo(&expected_outpoint).is_none());
        }
    } else {
        let block1 = block1.clone().prepare();
        let commit_result = state.validate_and_commit(block1.clone());

        // the block was committed
        assert_eq!(commit_result, Ok(()));
        assert_eq!(Some((Height(1), block1.hash)), state.best_tip());

        // the block data is in the non-finalized state
        assert!(!state.mem.eq_internal_state(&previous_mem));

        assert_eq!(state.mem.chain_set.len(), 1);

        for expected_outpoint in expected_outpoints {
            // the non-finalized state has the unspent UTXOs
            assert!(state
                .mem
                .chain_set
                .iter()
                .next()
                .unwrap()
                .unspent_utxos()
                .contains_key(&expected_outpoint));
            // the finalized state does not have the UTXOs
            assert!(state.disk.utxo(&expected_outpoint).is_none());
        }
    }

    TestState {
        state,
        genesis,
        block1,
    }
}

/// Return a `Transaction::V4`, using transparent `inputs` and `outputs`,
///
/// Other fields have empty or default values.
fn transaction_v4_with_transparent_data(
    inputs: impl IntoIterator<Item = transparent::Input>,
    outputs: impl IntoIterator<Item = transparent::Output>,
) -> Transaction {
    let inputs: Vec<_> = inputs.into_iter().collect();
    let outputs: Vec<_> = outputs.into_iter().collect();

    // do any fixups here, if required

    Transaction::V4 {
        inputs,
        outputs,
        lock_time: LockTime::min_lock_time(),
        expiry_height: Height(0),
        joinsplit_data: None,
        sapling_shielded_data: None,
    }
}
