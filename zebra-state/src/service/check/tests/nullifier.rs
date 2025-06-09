//! Randomised property tests for nullifier contextual validation

use std::{env, sync::Arc};

use itertools::Itertools;
use proptest::prelude::*;

use zebra_chain::{
    block::{Block, Height},
    fmt::TypeNameToDebug,
    orchard,
    parameters::NetworkUpgrade::Nu5,
    primitives::Groth16Proof,
    sapling::{self, FieldNotPresent, PerSpendAnchor, TransferData::*},
    serialization::ZcashDeserializeInto,
    sprout::JoinSplit,
    transaction::{JoinSplitData, LockTime, Transaction},
};

use crate::{
    arbitrary::Prepare,
    service::{
        check::nullifier::tx_no_duplicates_in_chain, read, write::validate_and_commit_non_finalized,
    },
    tests::setup::{new_state_with_mainnet_genesis, transaction_v4_from_coinbase},
    CheckpointVerifiedBlock,
    ValidateContextError::{
        DuplicateOrchardNullifier, DuplicateSaplingNullifier, DuplicateSproutNullifier,
    },
};

// These tests use the `Arbitrary` trait to easily generate complex types,
// then modify those types to cause an error (or to ensure success).
//
// We could use mainnet or testnet blocks in these tests,
// but the differences shouldn't matter,
// because we're only interested in spend validation,
// (and passing various other state checks).

const DEFAULT_NULLIFIER_PROPTEST_CASES: u32 = 2;

proptest! {
    #![proptest_config(
        proptest::test_runner::Config::with_cases(env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_NULLIFIER_PROPTEST_CASES))
    )]

    // sprout

    /// Make sure an arbitrary sprout nullifier is accepted by state contextual validation.
    ///
    /// This test makes sure there are no spurious rejections that might hide bugs in the other tests.
    /// (And that the test infrastructure generally works.)
    #[test]
    fn accept_distinct_arbitrary_sprout_nullifiers_in_one_block(
        mut joinsplit in TypeNameToDebug::<JoinSplit<Groth16Proof>>::arbitrary(),
        joinsplit_data in TypeNameToDebug::<JoinSplitData<Groth16Proof>>::arbitrary(),
        use_finalized_state in any::<bool>(),
    ) {
        let _init_guard = zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        make_distinct_nullifiers(&mut joinsplit.nullifiers);
        let expected_nullifiers = joinsplit.nullifiers;

        let transaction = transaction_v4_with_joinsplit_data(joinsplit_data.0, [joinsplit.0]);

        // convert the coinbase transaction to a version that the non-finalized state will accept
        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1.transactions.push(transaction.into());

        let (mut finalized_state, mut non_finalized_state, _genesis) = new_state_with_mainnet_genesis();

        // Allows anchor checks to pass
        finalized_state.populate_with_anchors(&block1);

        let previous_mem = non_finalized_state.clone();

        // randomly choose to commit the block to the finalized or non-finalized state
        if use_finalized_state {
            let block1 = CheckpointVerifiedBlock::from(Arc::new(block1));
            let commit_result = finalized_state.commit_finalized_direct(block1.clone().into(), None, "test");

            // the block was committed
            prop_assert_eq!(Some((Height(1), block1.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
            prop_assert!(commit_result.is_ok());

            // the non-finalized state didn't change
            prop_assert!(non_finalized_state.eq_internal_state(&previous_mem));

            // the finalized state has the nullifiers
            prop_assert!(finalized_state
                .contains_sprout_nullifier(&expected_nullifiers[0]));
            prop_assert!(finalized_state
                .contains_sprout_nullifier(&expected_nullifiers[1]));
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
            prop_assert!(!non_finalized_state.eq_internal_state(&previous_mem));

            // the non-finalized state has the nullifiers
            prop_assert_eq!(non_finalized_state.chain_count(), 1);
            prop_assert!(non_finalized_state
                .best_contains_sprout_nullifier(&expected_nullifiers[0]));
            prop_assert!(non_finalized_state
                .best_contains_sprout_nullifier(&expected_nullifiers[1]));
        }
    }

    /// Make sure duplicate sprout nullifiers are rejected by state contextual validation,
    /// if they come from the same JoinSplit.
    #[test]
    fn reject_duplicate_sprout_nullifiers_in_joinsplit(
        mut joinsplit in TypeNameToDebug::<JoinSplit<Groth16Proof>>::arbitrary(),
        joinsplit_data in TypeNameToDebug::<JoinSplitData<Groth16Proof>>::arbitrary(),
    ) {
        let _init_guard = zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        // create a double-spend within the same joinsplit
        // this might not actually be valid under the nullifier generation consensus rules
        let duplicate_nullifier = joinsplit.nullifiers[0];
        joinsplit.nullifiers[1] = duplicate_nullifier;

        let transaction = transaction_v4_with_joinsplit_data(joinsplit_data.0, [joinsplit.0]);

        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1.transactions.push(transaction.into());

            let (finalized_state, mut non_finalized_state, genesis) = new_state_with_mainnet_genesis();

        // Allows anchor checks to pass
        finalized_state.populate_with_anchors(&block1);

        let previous_mem = non_finalized_state.clone();

        let block1 = Arc::new(block1).prepare();
        let commit_result = validate_and_commit_non_finalized(
            &finalized_state.db,
            &mut non_finalized_state,
            block1
        );

        // if the random proptest data produces other errors,
        // we might need to just check `is_err()` here
        prop_assert_eq!(
            commit_result,
            Err(DuplicateSproutNullifier {
                nullifier: duplicate_nullifier,
                in_finalized_state: false,
            }
            .into())
        );
        // block was rejected
        prop_assert_eq!(Some((Height(0), genesis.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
        prop_assert!(non_finalized_state.eq_internal_state(&previous_mem));
    }

    /// Make sure duplicate sprout nullifiers are rejected by state contextual validation,
    /// if they come from different JoinSplits in the same JoinSplitData/Transaction.
    #[test]
    fn reject_duplicate_sprout_nullifiers_in_transaction(
        mut joinsplit1 in TypeNameToDebug::<JoinSplit<Groth16Proof>>::arbitrary(),
        mut joinsplit2 in TypeNameToDebug::<JoinSplit<Groth16Proof>>::arbitrary(),
        joinsplit_data in TypeNameToDebug::<JoinSplitData<Groth16Proof>>::arbitrary(),
    ) {
        let _init_guard = zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        make_distinct_nullifiers(
            joinsplit1
                .nullifiers
                .iter_mut()
                .chain(joinsplit2.nullifiers.iter_mut()),
        );

        // create a double-spend across two joinsplits
        let duplicate_nullifier = joinsplit1.nullifiers[0];
        joinsplit2.nullifiers[0] = duplicate_nullifier;

        let transaction =
            transaction_v4_with_joinsplit_data(joinsplit_data.0, [joinsplit1.0, joinsplit2.0]);

        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1.transactions.push(transaction.into());

            let (finalized_state, mut non_finalized_state, genesis) = new_state_with_mainnet_genesis();

        // Allows anchor checks to pass
        finalized_state.populate_with_anchors(&block1);

        let previous_mem = non_finalized_state.clone();

        let block1 = Arc::new(block1).prepare();
        let commit_result = validate_and_commit_non_finalized(
            &finalized_state.db,
            &mut non_finalized_state,
            block1
        );

        prop_assert_eq!(
            commit_result,
            Err(DuplicateSproutNullifier {
                nullifier: duplicate_nullifier,
                in_finalized_state: false,
            }
            .into())
        );
        prop_assert_eq!(Some((Height(0), genesis.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
        prop_assert!(non_finalized_state.eq_internal_state(&previous_mem));
    }

    /// Make sure duplicate sprout nullifiers are rejected by state contextual validation,
    /// if they come from different transactions in the same block.
    #[test]
    fn reject_duplicate_sprout_nullifiers_in_block(
        mut joinsplit1 in TypeNameToDebug::<JoinSplit<Groth16Proof>>::arbitrary(),
        mut joinsplit2 in TypeNameToDebug::<JoinSplit<Groth16Proof>>::arbitrary(),
        joinsplit_data1 in TypeNameToDebug::<JoinSplitData<Groth16Proof>>::arbitrary(),
        joinsplit_data2 in TypeNameToDebug::<JoinSplitData<Groth16Proof>>::arbitrary(),
    ) {
        let _init_guard = zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        make_distinct_nullifiers(
            joinsplit1
                .nullifiers
                .iter_mut()
                .chain(joinsplit2.nullifiers.iter_mut()),
        );

        // create a double-spend across two transactions
        let duplicate_nullifier = joinsplit1.nullifiers[0];
        joinsplit2.nullifiers[0] = duplicate_nullifier;

        let transaction1 = transaction_v4_with_joinsplit_data(joinsplit_data1.0, [joinsplit1.0]);
        let transaction2 = transaction_v4_with_joinsplit_data(joinsplit_data2.0, [joinsplit2.0]);

        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1
            .transactions
            .extend([transaction1.into(), transaction2.into()]);

            let (finalized_state, mut non_finalized_state, genesis) = new_state_with_mainnet_genesis();

        // Allows anchor checks to pass
        finalized_state.populate_with_anchors(&block1);

        let previous_mem = non_finalized_state.clone();

        let block1 = Arc::new(block1).prepare();
        let commit_result = validate_and_commit_non_finalized(
            &finalized_state.db,
            &mut non_finalized_state,
            block1
        );

        prop_assert_eq!(
            commit_result,
            Err(DuplicateSproutNullifier {
                nullifier: duplicate_nullifier,
                in_finalized_state: false,
            }
            .into())
        );
        prop_assert_eq!(Some((Height(0), genesis.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
        prop_assert!(non_finalized_state.eq_internal_state(&previous_mem));
    }

    /// Make sure duplicate sprout nullifiers are rejected by state contextual validation,
    /// if they come from different blocks in the same chain.
    #[test]
    fn reject_duplicate_sprout_nullifiers_in_chain(
        mut joinsplit1 in TypeNameToDebug::<JoinSplit<Groth16Proof>>::arbitrary(),
        mut joinsplit2 in TypeNameToDebug::<JoinSplit<Groth16Proof>>::arbitrary(),
        joinsplit_data1 in TypeNameToDebug::<JoinSplitData<Groth16Proof>>::arbitrary(),
        joinsplit_data2 in TypeNameToDebug::<JoinSplitData<Groth16Proof>>::arbitrary(),
        duplicate_in_finalized_state in any::<bool>(),
    ) {
        let _init_guard = zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");
        let mut block2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        make_distinct_nullifiers(
            joinsplit1
                .nullifiers
                .iter_mut()
                .chain(joinsplit2.nullifiers.iter_mut()),
        );
        let expected_nullifiers = joinsplit1.nullifiers;

        // create a double-spend across two blocks
        let duplicate_nullifier = joinsplit1.nullifiers[0];
        joinsplit2.nullifiers[0] = duplicate_nullifier;

        let transaction1 = Arc::new(transaction_v4_with_joinsplit_data(joinsplit_data1.0, [joinsplit1.0]));
        let transaction2 = transaction_v4_with_joinsplit_data(joinsplit_data2.0, [joinsplit2.0]);

        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();
        block2.transactions[0] = transaction_v4_from_coinbase(&block2.transactions[0]).into();

        block1.transactions.push(transaction1.clone());
        block2.transactions.push(transaction2.into());

        let (mut finalized_state, mut non_finalized_state, _genesis) = new_state_with_mainnet_genesis();

        // Allows anchor checks to pass
        finalized_state.populate_with_anchors(&block1);
        finalized_state.populate_with_anchors(&block2);

        let mut previous_mem = non_finalized_state.clone();

        // makes sure there are no spurious rejections that might hide bugs in `tx_no_duplicates_in_chain`
        let check_tx_no_duplicates_in_chain =
            tx_no_duplicates_in_chain(&finalized_state.db, non_finalized_state.best_chain(), &transaction1);
        prop_assert!(check_tx_no_duplicates_in_chain.is_ok());

        let block1_hash;
        // randomly choose to commit the next block to the finalized or non-finalized state
        if duplicate_in_finalized_state {
            let block1 = CheckpointVerifiedBlock::from(Arc::new(block1));
            let commit_result = finalized_state.commit_finalized_direct(block1.clone().into(), None, "test");

            prop_assert_eq!(Some((Height(1), block1.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
            prop_assert!(commit_result.is_ok());
            prop_assert!(non_finalized_state.eq_internal_state(&previous_mem));
            prop_assert!(finalized_state
                .contains_sprout_nullifier(&expected_nullifiers[0]));
            prop_assert!(finalized_state
                .contains_sprout_nullifier(&expected_nullifiers[1]));

            block1_hash = block1.hash;
        } else {
            let block1 = Arc::new(block1).prepare();
            let commit_result = validate_and_commit_non_finalized(
                &finalized_state.db,
                &mut non_finalized_state,
                block1.clone()
            );

            prop_assert_eq!(commit_result, Ok(()));
            prop_assert_eq!(Some((Height(1), block1.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
            prop_assert!(!non_finalized_state.eq_internal_state(&previous_mem));
            prop_assert!(non_finalized_state
                .best_contains_sprout_nullifier(&expected_nullifiers[0]));
            prop_assert!(non_finalized_state
                .best_contains_sprout_nullifier(&expected_nullifiers[1]));

            block1_hash = block1.hash;
            previous_mem = non_finalized_state.clone();
        }

        let block2 = Arc::new(block2).prepare();
        let commit_result = validate_and_commit_non_finalized(
            &finalized_state.db,
            &mut non_finalized_state,
            block2
        );

        prop_assert_eq!(
            commit_result,
            Err(DuplicateSproutNullifier {
                nullifier: duplicate_nullifier,
                in_finalized_state: duplicate_in_finalized_state,
            }
            .into())
        );

        let check_tx_no_duplicates_in_chain =
            tx_no_duplicates_in_chain(&finalized_state.db, non_finalized_state.best_chain(), &transaction1);

        prop_assert_eq!(
            check_tx_no_duplicates_in_chain,
            Err(DuplicateSproutNullifier {
                nullifier: duplicate_nullifier,
                in_finalized_state: duplicate_in_finalized_state,
            })
        );

        prop_assert_eq!(Some((Height(1), block1_hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
        prop_assert!(non_finalized_state.eq_internal_state(&previous_mem));
    }

    // sapling

    /// Make sure an arbitrary sapling nullifier is accepted by state contextual validation.
    ///
    /// This test makes sure there are no spurious rejections that might hide bugs in the other tests.
    /// (And that the test infrastructure generally works.)
    #[test]
    fn accept_distinct_arbitrary_sapling_nullifiers_in_one_block(
        spend in TypeNameToDebug::<sapling::Spend<PerSpendAnchor>>::arbitrary(),
        sapling_shielded_data in TypeNameToDebug::<sapling::ShieldedData<PerSpendAnchor>>::arbitrary(),
        use_finalized_state in any::<bool>(),
    ) {
        let _init_guard = zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        let expected_nullifier = spend.nullifier;

        let transaction =
            transaction_v4_with_sapling_shielded_data(sapling_shielded_data.0, [spend.0]);

        // convert the coinbase transaction to a version that the non-finalized state will accept
        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1.transactions.push(transaction.into());

        let (mut finalized_state, mut non_finalized_state, _genesis) = new_state_with_mainnet_genesis();

        // Allows anchor checks to pass
        finalized_state.populate_with_anchors(&block1);

        let previous_mem = non_finalized_state.clone();

        // randomly choose to commit the block to the finalized or non-finalized state
        if use_finalized_state {
            let block1 = CheckpointVerifiedBlock::from(Arc::new(block1));
            let commit_result = finalized_state.commit_finalized_direct(block1.clone().into(),None,  "test");

            prop_assert_eq!(Some((Height(1), block1.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
            prop_assert!(commit_result.is_ok());
            prop_assert!(non_finalized_state.eq_internal_state(&previous_mem));
            prop_assert!(finalized_state.contains_sapling_nullifier(&expected_nullifier));
        } else {
            let block1 = Arc::new(block1).prepare();
            let commit_result = validate_and_commit_non_finalized(
                &finalized_state.db,
                &mut non_finalized_state,
                block1.clone()
            );

            prop_assert_eq!(commit_result, Ok(()));
            prop_assert_eq!(Some((Height(1), block1.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
            prop_assert!(!non_finalized_state.eq_internal_state(&previous_mem));
            prop_assert!(non_finalized_state
                .best_contains_sapling_nullifier(&expected_nullifier));
        }
    }

    /// Make sure duplicate sapling nullifiers are rejected by state contextual validation,
    /// if they come from different Spends in the same sapling::ShieldedData/Transaction.
    #[test]
    fn reject_duplicate_sapling_nullifiers_in_transaction(
        spend1 in TypeNameToDebug::<sapling::Spend<PerSpendAnchor>>::arbitrary(),
        mut spend2 in TypeNameToDebug::<sapling::Spend<PerSpendAnchor>>::arbitrary(),
        sapling_shielded_data in TypeNameToDebug::<sapling::ShieldedData<PerSpendAnchor>>::arbitrary(),
    ) {
        let _init_guard = zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        // create a double-spend across two spends
        let duplicate_nullifier = spend1.nullifier;
        spend2.nullifier = duplicate_nullifier;

        let transaction = transaction_v4_with_sapling_shielded_data(
            sapling_shielded_data.0,
            [spend1.0, spend2.0],
        );

        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1.transactions.push(transaction.into());

            let (finalized_state, mut non_finalized_state, genesis) = new_state_with_mainnet_genesis();

        // Allows anchor checks to pass
        finalized_state.populate_with_anchors(&block1);

        let previous_mem = non_finalized_state.clone();

        let block1 = Arc::new(block1).prepare();
        let commit_result = validate_and_commit_non_finalized(
            &finalized_state.db,
            &mut non_finalized_state,
            block1
        );

        prop_assert_eq!(
            commit_result,
            Err(DuplicateSaplingNullifier {
                nullifier: duplicate_nullifier,
                in_finalized_state: false,
            }
            .into())
        );
        prop_assert_eq!(Some((Height(0), genesis.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
        prop_assert!(non_finalized_state.eq_internal_state(&previous_mem));
    }

    /// Make sure duplicate sapling nullifiers are rejected by state contextual validation,
    /// if they come from different transactions in the same block.
    #[test]
    fn reject_duplicate_sapling_nullifiers_in_block(
        spend1 in TypeNameToDebug::<sapling::Spend<PerSpendAnchor>>::arbitrary(),
        mut spend2 in TypeNameToDebug::<sapling::Spend<PerSpendAnchor>>::arbitrary(),
        sapling_shielded_data1 in TypeNameToDebug::<sapling::ShieldedData<PerSpendAnchor>>::arbitrary(),
        sapling_shielded_data2 in TypeNameToDebug::<sapling::ShieldedData<PerSpendAnchor>>::arbitrary(),
    ) {
        let _init_guard = zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        // create a double-spend across two transactions
        let duplicate_nullifier = spend1.nullifier;
        spend2.nullifier = duplicate_nullifier;

        let transaction1 =
            transaction_v4_with_sapling_shielded_data(sapling_shielded_data1.0, [spend1.0]);
        let transaction2 =
            transaction_v4_with_sapling_shielded_data(sapling_shielded_data2.0, [spend2.0]);

        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1
            .transactions
            .extend([transaction1.into(), transaction2.into()]);

        let (finalized_state, mut non_finalized_state, genesis) = new_state_with_mainnet_genesis();

        // Allows anchor checks to pass
        finalized_state.populate_with_anchors(&block1);

        let previous_mem = non_finalized_state.clone();

        let block1 = Arc::new(block1).prepare();
        let commit_result = validate_and_commit_non_finalized(
            &finalized_state.db,
            &mut non_finalized_state,
            block1
        );

        prop_assert_eq!(
            commit_result,
            Err(DuplicateSaplingNullifier {
                nullifier: duplicate_nullifier,
                in_finalized_state: false,
            }
            .into())
        );
        prop_assert_eq!(Some((Height(0), genesis.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
        prop_assert!(non_finalized_state.eq_internal_state(&previous_mem));
    }

    /// Make sure duplicate sapling nullifiers are rejected by state contextual validation,
    /// if they come from different blocks in the same chain.
    #[test]
    fn reject_duplicate_sapling_nullifiers_in_chain(
        spend1 in TypeNameToDebug::<sapling::Spend<PerSpendAnchor>>::arbitrary(),
        mut spend2 in TypeNameToDebug::<sapling::Spend<PerSpendAnchor>>::arbitrary(),
        sapling_shielded_data1 in TypeNameToDebug::<sapling::ShieldedData<PerSpendAnchor>>::arbitrary(),
        sapling_shielded_data2 in TypeNameToDebug::<sapling::ShieldedData<PerSpendAnchor>>::arbitrary(),
        duplicate_in_finalized_state in any::<bool>(),
    ) {
        let _init_guard = zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");
        let mut block2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        // create a double-spend across two blocks
        let duplicate_nullifier = spend1.nullifier;
        spend2.nullifier = duplicate_nullifier;

        let transaction1 =
            Arc::new(transaction_v4_with_sapling_shielded_data(sapling_shielded_data1.0, [spend1.0]));
        let transaction2 =
            transaction_v4_with_sapling_shielded_data(sapling_shielded_data2.0, [spend2.0]);

        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();
        block2.transactions[0] = transaction_v4_from_coinbase(&block2.transactions[0]).into();

        block1.transactions.push(transaction1.clone());
        block2.transactions.push(transaction2.into());

        let (mut finalized_state, mut non_finalized_state, _genesis) = new_state_with_mainnet_genesis();

        // Allows anchor checks to pass
        finalized_state.populate_with_anchors(&block1);
        finalized_state.populate_with_anchors(&block2);

        let mut previous_mem = non_finalized_state.clone();

        // makes sure there are no spurious rejections that might hide bugs in `tx_no_duplicates_in_chain`
        let check_tx_no_duplicates_in_chain =
            tx_no_duplicates_in_chain(&finalized_state.db, non_finalized_state.best_chain(), &transaction1);
        prop_assert!(check_tx_no_duplicates_in_chain.is_ok());

        let block1_hash;
        // randomly choose to commit the next block to the finalized or non-finalized state
        if duplicate_in_finalized_state {
            let block1 = CheckpointVerifiedBlock::from(Arc::new(block1));
            let commit_result = finalized_state.commit_finalized_direct(block1.clone().into(),None,  "test");

            prop_assert_eq!(Some((Height(1), block1.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
            prop_assert!(commit_result.is_ok());
            prop_assert!(non_finalized_state.eq_internal_state(&previous_mem));
            prop_assert!(finalized_state.contains_sapling_nullifier(&duplicate_nullifier));

            block1_hash = block1.hash;
        } else {
            let block1 = Arc::new(block1).prepare();
            let commit_result = validate_and_commit_non_finalized(
                &finalized_state.db,
                &mut non_finalized_state,
                block1.clone()
            );

            prop_assert_eq!(commit_result, Ok(()));
            prop_assert_eq!(Some((Height(1), block1.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
            prop_assert!(!non_finalized_state.eq_internal_state(&previous_mem));
            prop_assert!(non_finalized_state

                .best_contains_sapling_nullifier(&duplicate_nullifier));

            block1_hash = block1.hash;
            previous_mem = non_finalized_state.clone();
        }

        let block2 = Arc::new(block2).prepare();
        let commit_result = validate_and_commit_non_finalized(
            &finalized_state.db,
            &mut non_finalized_state,
            block2
        );

        prop_assert_eq!(
            commit_result,
            Err(DuplicateSaplingNullifier {
                nullifier: duplicate_nullifier,
                in_finalized_state: duplicate_in_finalized_state,
            }
            .into())
        );

        let check_tx_no_duplicates_in_chain =
            tx_no_duplicates_in_chain(&finalized_state.db, non_finalized_state.best_chain(), &transaction1);

        prop_assert_eq!(
            check_tx_no_duplicates_in_chain,
            Err(DuplicateSaplingNullifier {
                nullifier: duplicate_nullifier,
                in_finalized_state: duplicate_in_finalized_state,
            })
        );

        prop_assert_eq!(Some((Height(1), block1_hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
        prop_assert!(non_finalized_state.eq_internal_state(&previous_mem));
    }

    // orchard

    /// Make sure an arbitrary orchard nullifier is accepted by state contextual validation.
    ///
    /// This test makes sure there are no spurious rejections that might hide bugs in the other tests.
    /// (And that the test infrastructure generally works.)
    #[test]
    fn accept_distinct_arbitrary_orchard_nullifiers_in_one_block(
        authorized_action in TypeNameToDebug::<orchard::AuthorizedAction<orchard::OrchardVanilla>>::arbitrary(),
        orchard_shielded_data in TypeNameToDebug::<orchard::ShieldedData<orchard::OrchardVanilla>>::arbitrary(),
        use_finalized_state in any::<bool>(),
    ) {
        let _init_guard = zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        let expected_nullifier = authorized_action.action.nullifier;

        let transaction = transaction_v5_with_orchard_shielded_data(
            orchard_shielded_data.0,
            [authorized_action.0],
        );

        // convert the coinbase transaction to a version that the non-finalized state will accept
        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1.transactions.push(transaction.into());

    let (mut finalized_state, mut non_finalized_state, _genesis) = new_state_with_mainnet_genesis();

        // Allows anchor checks to pass
        finalized_state.populate_with_anchors(&block1);

        let previous_mem = non_finalized_state.clone();

        // randomly choose to commit the block to the finalized or non-finalized state
        if use_finalized_state {
            let block1 = CheckpointVerifiedBlock::from(Arc::new(block1));
            let commit_result = finalized_state.commit_finalized_direct(block1.clone().into(), None, "test");

            prop_assert_eq!(Some((Height(1), block1.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
            prop_assert!(commit_result.is_ok());
            prop_assert!(non_finalized_state.eq_internal_state(&previous_mem));
            prop_assert!(finalized_state.contains_orchard_nullifier(&expected_nullifier));
        } else {
            let block1 = Arc::new(block1).prepare();
            let commit_result = validate_and_commit_non_finalized(
                &finalized_state.db,
                &mut non_finalized_state,
                block1.clone()
            );

            prop_assert_eq!(commit_result, Ok(()));
            prop_assert_eq!(Some((Height(1), block1.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
            prop_assert!(!non_finalized_state.eq_internal_state(&previous_mem));
            prop_assert!(non_finalized_state

                .best_contains_orchard_nullifier(&expected_nullifier));
        }
    }

    /// Make sure duplicate orchard nullifiers are rejected by state contextual validation,
    /// if they come from different AuthorizedActions in the same orchard::ShieldedData/Transaction.
    #[test]
    fn reject_duplicate_orchard_nullifiers_in_transaction(
        authorized_action1 in TypeNameToDebug::<orchard::AuthorizedAction<orchard::OrchardVanilla>>::arbitrary(),
        mut authorized_action2 in TypeNameToDebug::<orchard::AuthorizedAction<orchard::OrchardVanilla>>::arbitrary(),
        orchard_shielded_data in TypeNameToDebug::<orchard::ShieldedData<orchard::OrchardVanilla>>::arbitrary(),
    ) {
        let _init_guard = zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        // create a double-spend across two authorized_actions
        let duplicate_nullifier = authorized_action1.action.nullifier;
        authorized_action2.action.nullifier = duplicate_nullifier;

        let transaction = transaction_v5_with_orchard_shielded_data(
            orchard_shielded_data.0,
            [authorized_action1.0, authorized_action2.0],
        );

        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1.transactions.push(transaction.into());

            let (finalized_state, mut non_finalized_state, genesis) = new_state_with_mainnet_genesis();

        // Allows anchor checks to pass
        finalized_state.populate_with_anchors(&block1);

        let previous_mem = non_finalized_state.clone();

        let block1 = Arc::new(block1).prepare();
        let commit_result = validate_and_commit_non_finalized(
            &finalized_state.db,
            &mut non_finalized_state,
            block1
        );

        prop_assert_eq!(
            commit_result,
            Err(DuplicateOrchardNullifier {
                nullifier: duplicate_nullifier,
                in_finalized_state: false,
            }
            .into())
        );
        prop_assert_eq!(Some((Height(0), genesis.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
        prop_assert!(non_finalized_state.eq_internal_state(&previous_mem));
    }

    /// Make sure duplicate orchard nullifiers are rejected by state contextual validation,
    /// if they come from different transactions in the same block.
    #[test]
    fn reject_duplicate_orchard_nullifiers_in_block(
        authorized_action1 in TypeNameToDebug::<orchard::AuthorizedAction<orchard::OrchardVanilla>>::arbitrary(),
        mut authorized_action2 in TypeNameToDebug::<orchard::AuthorizedAction<orchard::OrchardVanilla>>::arbitrary(),
        orchard_shielded_data1 in TypeNameToDebug::<orchard::ShieldedData<orchard::OrchardVanilla>>::arbitrary(),
        orchard_shielded_data2 in TypeNameToDebug::<orchard::ShieldedData<orchard::OrchardVanilla>>::arbitrary(),
    ) {
        let _init_guard = zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        // create a double-spend across two transactions
        let duplicate_nullifier = authorized_action1.action.nullifier;
        authorized_action2.action.nullifier = duplicate_nullifier;

        let transaction1 = transaction_v5_with_orchard_shielded_data(
            orchard_shielded_data1.0,
            [authorized_action1.0],
        );
        let transaction2 = transaction_v5_with_orchard_shielded_data(
            orchard_shielded_data2.0,
            [authorized_action2.0],
        );

        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1
            .transactions
            .extend([transaction1.into(), transaction2.into()]);

            let (finalized_state, mut non_finalized_state, genesis) = new_state_with_mainnet_genesis();

        // Allows anchor checks to pass
        finalized_state.populate_with_anchors(&block1);

        let previous_mem = non_finalized_state.clone();

        let block1 = Arc::new(block1).prepare();
        let commit_result = validate_and_commit_non_finalized(
            &finalized_state.db,
            &mut non_finalized_state,
            block1
        );

        prop_assert_eq!(
            commit_result,
            Err(DuplicateOrchardNullifier {
                nullifier: duplicate_nullifier,
                in_finalized_state: false,
            }
            .into())
        );
        prop_assert_eq!(Some((Height(0), genesis.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
        prop_assert!(non_finalized_state.eq_internal_state(&previous_mem));
    }

    /// Make sure duplicate orchard nullifiers are rejected by state contextual validation,
    /// if they come from different blocks in the same chain.
    #[test]
    fn reject_duplicate_orchard_nullifiers_in_chain(
        authorized_action1 in TypeNameToDebug::<orchard::AuthorizedAction<orchard::OrchardVanilla>>::arbitrary(),
        mut authorized_action2 in TypeNameToDebug::<orchard::AuthorizedAction<orchard::OrchardVanilla>>::arbitrary(),
        orchard_shielded_data1 in TypeNameToDebug::<orchard::ShieldedData<orchard::OrchardVanilla>>::arbitrary(),
        orchard_shielded_data2 in TypeNameToDebug::<orchard::ShieldedData<orchard::OrchardVanilla>>::arbitrary(),
        duplicate_in_finalized_state in any::<bool>(),
    ) {
        let _init_guard = zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");
        let mut block2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        // create a double-spend across two blocks
        let duplicate_nullifier = authorized_action1.action.nullifier;
        authorized_action2.action.nullifier = duplicate_nullifier;

        let transaction1 = Arc::new(transaction_v5_with_orchard_shielded_data(
            orchard_shielded_data1.0,
            [authorized_action1.0],
        ));
        let transaction2 = transaction_v5_with_orchard_shielded_data(
            orchard_shielded_data2.0,
            [authorized_action2.0],
        );

        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();
        block2.transactions[0] = transaction_v4_from_coinbase(&block2.transactions[0]).into();

        block1.transactions.push(transaction1.clone());
        block2.transactions.push(transaction2.into());

    let (mut finalized_state, mut non_finalized_state, _genesis) = new_state_with_mainnet_genesis();

        // Allows anchor checks to pass
        finalized_state.populate_with_anchors(&block1);
        finalized_state.populate_with_anchors(&block2);

        let mut previous_mem = non_finalized_state.clone();

        // makes sure there are no spurious rejections that might hide bugs in `tx_no_duplicates_in_chain`
        let check_tx_no_duplicates_in_chain =
            tx_no_duplicates_in_chain(&finalized_state.db, non_finalized_state.best_chain(), &transaction1);
        prop_assert!(check_tx_no_duplicates_in_chain.is_ok());

        let block1_hash;
        // randomly choose to commit the next block to the finalized or non-finalized state
        if duplicate_in_finalized_state {
            let block1 = CheckpointVerifiedBlock::from(Arc::new(block1));
            let commit_result = finalized_state.commit_finalized_direct(block1.clone().into(), None, "test");

            prop_assert_eq!(Some((Height(1), block1.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
            prop_assert!(commit_result.is_ok());
            prop_assert!(non_finalized_state.eq_internal_state(&previous_mem));
            prop_assert!(finalized_state.contains_orchard_nullifier(&duplicate_nullifier));

            block1_hash = block1.hash;
        } else {
            let block1 = Arc::new(block1).prepare();
            let commit_result = validate_and_commit_non_finalized(
                &finalized_state.db,
                &mut non_finalized_state,
                block1.clone()
            );

            prop_assert_eq!(commit_result, Ok(()));
            prop_assert_eq!(Some((Height(1), block1.hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
            prop_assert!(!non_finalized_state.eq_internal_state(&previous_mem));
            prop_assert!(non_finalized_state
                .best_contains_orchard_nullifier(&duplicate_nullifier));

            block1_hash = block1.hash;
            previous_mem = non_finalized_state.clone();
        }

        let block2 = Arc::new(block2).prepare();
        let commit_result = validate_and_commit_non_finalized(
            &finalized_state.db,
            &mut non_finalized_state,
            block2
        );

        prop_assert_eq!(
            commit_result,
            Err(DuplicateOrchardNullifier {
                nullifier: duplicate_nullifier,
                in_finalized_state: duplicate_in_finalized_state,
            }
            .into())
        );

        let check_tx_no_duplicates_in_chain =
            tx_no_duplicates_in_chain(&finalized_state.db, non_finalized_state.best_chain(), &transaction1);

        prop_assert_eq!(
            check_tx_no_duplicates_in_chain,
            Err(DuplicateOrchardNullifier {
                nullifier: duplicate_nullifier,
                in_finalized_state: duplicate_in_finalized_state,
            })
        );

        prop_assert_eq!(Some((Height(1), block1_hash)), read::best_tip(&non_finalized_state, &finalized_state.db));
        prop_assert!(non_finalized_state.eq_internal_state(&previous_mem));
    }
}

/// Make sure the supplied nullifiers are distinct, modifying them if necessary.
fn make_distinct_nullifiers<'until_modified, NullifierT>(
    nullifiers: impl IntoIterator<Item = &'until_modified mut NullifierT>,
) where
    NullifierT: Into<[u8; 32]> + Clone + Eq + std::hash::Hash + 'until_modified,
    [u8; 32]: Into<NullifierT>,
{
    let nullifiers: Vec<_> = nullifiers.into_iter().collect();

    if nullifiers.iter().unique().count() < nullifiers.len() {
        let mut tweak: u8 = 0x00;
        for nullifier in nullifiers {
            let mut nullifier_bytes: [u8; 32] = nullifier.clone().into();
            nullifier_bytes[0] = tweak;
            *nullifier = nullifier_bytes.into();

            tweak = tweak
                .checked_add(0x01)
                .expect("unexpectedly large nullifier list");
        }
    }
}

/// Return a `Transaction::V4` containing `joinsplit_data`,
/// with its `JoinSplit`s replaced by `joinsplits`.
///
/// Other fields have empty or default values.
///
/// # Panics
///
/// If there are no `JoinSplit`s in `joinsplits`.
fn transaction_v4_with_joinsplit_data(
    joinsplit_data: impl Into<Option<JoinSplitData<Groth16Proof>>>,
    joinsplits: impl IntoIterator<Item = JoinSplit<Groth16Proof>>,
) -> Transaction {
    let mut joinsplit_data = joinsplit_data.into();
    let joinsplits: Vec<_> = joinsplits.into_iter().collect();

    if let Some(ref mut joinsplit_data) = joinsplit_data {
        // make sure there are no other nullifiers, by replacing all the joinsplits
        let (first, rest) = joinsplits
            .split_first()
            .expect("unexpected empty joinsplits");
        joinsplit_data.first = first.clone();
        joinsplit_data.rest = rest.to_vec();

        // set value balance to 0 to pass the chain value pool checks
        let zero_amount = 0.try_into().expect("unexpected invalid zero amount");

        joinsplit_data.first.vpub_old = zero_amount;
        joinsplit_data.first.vpub_new = zero_amount;

        for joinsplit in &mut joinsplit_data.rest {
            joinsplit.vpub_old = zero_amount;
            joinsplit.vpub_new = zero_amount;
        }
    }

    Transaction::V4 {
        inputs: Vec::new(),
        outputs: Vec::new(),
        lock_time: LockTime::min_lock_time_timestamp(),
        expiry_height: Height(0),
        joinsplit_data,
        sapling_shielded_data: None,
    }
}

/// Return a `Transaction::V4` containing `sapling_shielded_data`.
/// with its `Spend`s replaced by `spends`.
///
/// Other fields have empty or default values.
///
/// Note: since sapling nullifiers in V5 transactions are identical to V4 transactions,
/// we just use V4 transactions in the tests.
///
/// # Panics
///
/// If there are no `Spend`s in `spends`, and no `Output`s in `sapling_shielded_data`.
fn transaction_v4_with_sapling_shielded_data(
    sapling_shielded_data: impl Into<Option<sapling::ShieldedData<PerSpendAnchor>>>,
    spends: impl IntoIterator<Item = sapling::Spend<PerSpendAnchor>>,
) -> Transaction {
    let mut sapling_shielded_data = sapling_shielded_data.into();
    let spends: Vec<_> = spends.into_iter().collect();

    if let Some(ref mut sapling_shielded_data) = sapling_shielded_data {
        // make sure there are no other nullifiers, by replacing all the spends
        sapling_shielded_data.transfers = match (
            sapling_shielded_data.transfers.clone(),
            spends.try_into().ok(),
        ) {
            // old and new spends: replace spends
            (
                SpendsAndMaybeOutputs {
                    shared_anchor,
                    maybe_outputs,
                    ..
                },
                Some(spends),
            ) => SpendsAndMaybeOutputs {
                shared_anchor,
                spends,
                maybe_outputs,
            },
            // old spends, but no new spends: delete spends, panic if no outputs
            (SpendsAndMaybeOutputs { maybe_outputs, .. }, None) => JustOutputs {
                outputs: maybe_outputs.try_into().expect(
                    "unexpected invalid TransferData: must have at least one spend or one output",
                ),
            },
            // no old spends, but new spends: add spends
            (JustOutputs { outputs, .. }, Some(spends)) => SpendsAndMaybeOutputs {
                shared_anchor: FieldNotPresent,
                spends,
                maybe_outputs: outputs.into(),
            },
            // no old and no new spends: do nothing
            (just_outputs @ JustOutputs { .. }, None) => just_outputs,
        };

        // set value balance to 0 to pass the chain value pool checks
        let zero_amount = 0.try_into().expect("unexpected invalid zero amount");
        sapling_shielded_data.value_balance = zero_amount;
    }

    Transaction::V4 {
        inputs: Vec::new(),
        outputs: Vec::new(),
        lock_time: LockTime::min_lock_time_timestamp(),
        expiry_height: Height(0),
        joinsplit_data: None,
        sapling_shielded_data,
    }
}

/// Return a `Transaction::V5` containing `orchard_shielded_data`.
/// with its `AuthorizedAction`s replaced by `authorized_actions`.
///
/// Other fields have empty or default values.
///
/// # Panics
///
/// If there are no `AuthorizedAction`s in `authorized_actions`.
fn transaction_v5_with_orchard_shielded_data(
    orchard_shielded_data: impl Into<Option<orchard::ShieldedData<orchard::OrchardVanilla>>>,
    authorized_actions: impl IntoIterator<Item = orchard::AuthorizedAction<orchard::OrchardVanilla>>,
) -> Transaction {
    let mut orchard_shielded_data = orchard_shielded_data.into();
    let authorized_actions: Vec<_> = authorized_actions.into_iter().collect();

    if let Some(ref mut orchard_shielded_data) = orchard_shielded_data {
        // make sure there are no other nullifiers, by replacing all the authorized_actions
        orchard_shielded_data.actions = authorized_actions.try_into().expect(
            "unexpected invalid orchard::ShieldedData: must have at least one AuthorizedAction",
        );

        // set value balance to 0 to pass the chain value pool checks
        let zero_amount = 0.try_into().expect("unexpected invalid zero amount");
        orchard_shielded_data.value_balance = zero_amount;
    }

    Transaction::V5 {
        network_upgrade: Nu5,
        inputs: Vec::new(),
        outputs: Vec::new(),
        lock_time: LockTime::min_lock_time_timestamp(),
        expiry_height: Height(0),
        sapling_shielded_data: None,
        orchard_shielded_data,
    }
}
