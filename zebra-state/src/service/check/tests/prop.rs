//! Randomised property tests for state contextual validation nullifier: (), in_finalized_state: ()  nullifier: (), in_finalized_state: ()  checks.

use std::{convert::TryInto, sync::Arc};

use itertools::Itertools;
use proptest::prelude::*;

use zebra_chain::{
    block::{Block, Height},
    fmt::TypeNameToDebug,
    parameters::Network::*,
    primitives::Groth16Proof,
    sapling::{self, FieldNotPresent, PerSpendAnchor, TransferData::*},
    serialization::ZcashDeserializeInto,
    sprout::JoinSplit,
    transaction::{JoinSplitData, LockTime, Transaction},
};

use crate::{
    config::Config,
    service::StateService,
    tests::Prepare,
    FinalizedBlock,
    ValidateContextError::{DuplicateSaplingNullifier, DuplicateSproutNullifier},
};

// These tests use the `Arbitrary` trait to easily generate complex types,
// then modify those types to cause an error (or to ensure success).
//
// We could use mainnet or testnet blocks in these tests,
// but the differences shouldn't matter,
// because we're only interested in spend validation,
// (and passing various other state checks).

// sprout

proptest! {
    /// Make sure an arbitrary sprout nullifier is accepted by state contextual validation.
    ///
    /// This test makes sure there are no spurious rejections that might hide bugs in the other tests.
    /// (And that the test infrastructure generally works.)
    #[test]
    fn accept_distinct_arbitrary_sprout_nullifiers(
        mut joinsplit in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        joinsplit_data in TypeNameToDebug::<JoinSplitData::<Groth16Proof>>::arbitrary(),
        use_finalized_state in any::<bool>(),
    ) {
        zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        make_distinct_nullifiers(&mut joinsplit.nullifiers);

        let transaction = transaction_v4_with_joinsplit_data(joinsplit_data.0, &[joinsplit.0]);

        // convert the coinbase transaction to a version that the non-finalized state will accept
        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1
            .transactions
            .push(transaction.into());

        let (mut state, _genesis) = new_state_with_mainnet_genesis();
        let previous_mem = state.mem.clone();

        // randomly choose to commit the block to the finalized or non-finalized state
        if use_finalized_state {
            let block1 = FinalizedBlock::from(Arc::new(block1));
            let commit_result = state.disk.commit_finalized_direct(block1.clone(), "test");

            prop_assert_eq!(Some((Height(1), block1.hash)), state.best_tip());
            prop_assert!(commit_result.is_ok());
            prop_assert!(state.mem.eq_internal_state(&previous_mem));
        } else {
            let block1 = Arc::new(block1).prepare();
            let commit_result =
                state.validate_and_commit(block1.clone());

            prop_assert_eq!(commit_result, Ok(()));
            prop_assert_eq!(Some((Height(1), block1.hash)), state.best_tip());
            prop_assert!(!state.mem.eq_internal_state(&previous_mem));
        }
    }

    /// Make sure duplicate sprout nullifiers are rejected by state contextual validation,
    /// if they come from the same JoinSplit.
    #[test]
    fn reject_duplicate_sprout_nullifiers_in_joinsplit(
        mut joinsplit in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        joinsplit_data in TypeNameToDebug::<JoinSplitData::<Groth16Proof>>::arbitrary(),
    ) {
        zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        // create a double-spend within the same joinsplit
        // this might not actually be valid under the nullifier generation consensus rules
        let duplicate_nullifier = joinsplit.nullifiers[0];
        joinsplit.nullifiers[1] = duplicate_nullifier;

        let transaction = transaction_v4_with_joinsplit_data(joinsplit_data.0, &[joinsplit.0]);

        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1
            .transactions
            .push(transaction.into());

        let (mut state, genesis) = new_state_with_mainnet_genesis();
        let previous_mem = state.mem.clone();

        let block1 = Arc::new(block1).prepare();
        let commit_result =
            state.validate_and_commit(block1);

        // if the random proptest data produces other errors,
        // we might need to just check `is_err()` here
        prop_assert_eq!(
            commit_result,
            Err(
                DuplicateSproutNullifier {
                    nullifier: duplicate_nullifier,
                    in_finalized_state: false,
                }.into()
            )
        );
        // block was rejected
        prop_assert_eq!(Some((Height(0), genesis.hash)), state.best_tip());
        prop_assert!(state.mem.eq_internal_state(&previous_mem));
    }

    /// Make sure duplicate sprout nullifiers are rejected by state contextual validation,
    /// if they come from different JoinSplits in the same JoinSplitData/Transaction.
    #[test]
    fn reject_duplicate_sprout_nullifiers_in_transaction(
        mut joinsplit1 in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        mut joinsplit2 in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        joinsplit_data in TypeNameToDebug::<JoinSplitData::<Groth16Proof>>::arbitrary(),
    ) {
        zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        make_distinct_nullifiers(&mut joinsplit1.nullifiers.iter_mut().chain(joinsplit2.nullifiers.iter_mut()));

        // create a double-spend across two joinsplits
        let duplicate_nullifier = joinsplit1.nullifiers[0];
        joinsplit2.nullifiers[0] = duplicate_nullifier;

        let transaction = transaction_v4_with_joinsplit_data(
            joinsplit_data.0,
            &[joinsplit1.0, joinsplit2.0]
        );

        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1
            .transactions
            .push(transaction.into());

        let (mut state, genesis) = new_state_with_mainnet_genesis();
        let previous_mem = state.mem.clone();

        let block1 = Arc::new(block1).prepare();
        let commit_result =
            state.validate_and_commit(block1);

        prop_assert_eq!(
            commit_result,
            Err(
                DuplicateSproutNullifier {
                    nullifier: duplicate_nullifier,
                    in_finalized_state: false,
                }.into()
            )
        );
        prop_assert_eq!(Some((Height(0), genesis.hash)), state.best_tip());
        prop_assert!(state.mem.eq_internal_state(&previous_mem));
    }

    /// Make sure duplicate sprout nullifiers are rejected by state contextual validation,
    /// if they come from different transactions in the same block.
    #[test]
    fn reject_duplicate_sprout_nullifiers_in_block(
        mut joinsplit1 in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        mut joinsplit2 in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        joinsplit_data1 in TypeNameToDebug::<JoinSplitData::<Groth16Proof>>::arbitrary(),
        joinsplit_data2 in TypeNameToDebug::<JoinSplitData::<Groth16Proof>>::arbitrary(),
    ) {
        zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        make_distinct_nullifiers(&mut joinsplit1.nullifiers.iter_mut().chain(joinsplit2.nullifiers.iter_mut()));

        // create a double-spend across two transactions
        let duplicate_nullifier = joinsplit1.nullifiers[0];
        joinsplit2.nullifiers[0] = duplicate_nullifier;

        let transaction1 = transaction_v4_with_joinsplit_data(joinsplit_data1.0, &[joinsplit1.0]);
        let transaction2 = transaction_v4_with_joinsplit_data(joinsplit_data2.0, &[joinsplit2.0]);

        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1
            .transactions
            .push(transaction1.into());
        block1
            .transactions
            .push(transaction2.into());

        let (mut state, genesis) = new_state_with_mainnet_genesis();
        let previous_mem = state.mem.clone();

        let block1 = Arc::new(block1).prepare();
        let commit_result =
            state.validate_and_commit(block1);

        prop_assert_eq!(
            commit_result,
            Err(
                DuplicateSproutNullifier {
                    nullifier: duplicate_nullifier,
                    in_finalized_state: false,
                }.into()
            )
        );
        prop_assert_eq!(Some((Height(0), genesis.hash)), state.best_tip());
        prop_assert!(state.mem.eq_internal_state(&previous_mem));
    }

    /// Make sure duplicate sprout nullifiers are rejected by state contextual validation,
    /// if they come from different blocks in the same chain.
    #[test]
    fn reject_duplicate_sprout_nullifiers_in_chain(
        mut joinsplit1 in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        mut joinsplit2 in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        joinsplit_data1 in TypeNameToDebug::<JoinSplitData::<Groth16Proof>>::arbitrary(),
        joinsplit_data2 in TypeNameToDebug::<JoinSplitData::<Groth16Proof>>::arbitrary(),
        duplicate_in_finalized_state in any::<bool>(),
    ) {
        zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");
        let mut block2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        make_distinct_nullifiers(&mut joinsplit1.nullifiers.iter_mut().chain(joinsplit2.nullifiers.iter_mut()));

        // create a double-spend across two blocks
        let duplicate_nullifier = joinsplit1.nullifiers[0];
        joinsplit2.nullifiers[0] = duplicate_nullifier;

        let transaction1 = transaction_v4_with_joinsplit_data(joinsplit_data1.0, &[joinsplit1.0]);
        let transaction2 = transaction_v4_with_joinsplit_data(joinsplit_data2.0, &[joinsplit2.0]);

        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();
        block2.transactions[0] = transaction_v4_from_coinbase(&block2.transactions[0]).into();

        block1
            .transactions
            .push(transaction1.into());
        block2
            .transactions
            .push(transaction2.into());

        let (mut state, _genesis) = new_state_with_mainnet_genesis();
        let mut previous_mem = state.mem.clone();

        let block1_hash;
        // randomly choose to commit the next block to the finalized or non-finalized state
        if duplicate_in_finalized_state {
            let block1 = FinalizedBlock::from(Arc::new(block1));
            let commit_result = state.disk.commit_finalized_direct(block1.clone(), "test");

            prop_assert_eq!(Some((Height(1), block1.hash)), state.best_tip());
            prop_assert!(commit_result.is_ok());
            prop_assert!(state.mem.eq_internal_state(&previous_mem));

            block1_hash = block1.hash;
        } else {
            let block1 = Arc::new(block1).prepare();
            let commit_result =
                state.validate_and_commit(block1.clone());

            prop_assert_eq!(commit_result, Ok(()));
            prop_assert_eq!(Some((Height(1), block1.hash)), state.best_tip());
            prop_assert!(!state.mem.eq_internal_state(&previous_mem));

            block1_hash = block1.hash;
            previous_mem = state.mem.clone();
        }

        let block2 = Arc::new(block2).prepare();
        let commit_result =
            state.validate_and_commit(block2);

        prop_assert_eq!(
            commit_result,
            Err(
                DuplicateSproutNullifier {
                    nullifier: duplicate_nullifier,
                    in_finalized_state: duplicate_in_finalized_state,
                }.into()
            )
        );
        prop_assert_eq!(Some((Height(1), block1_hash)), state.best_tip());
        prop_assert!(state.mem.eq_internal_state(&previous_mem));
    }
}

// sapling

proptest! {
    /// Make sure an arbitrary sapling nullifier is accepted by state contextual validation.
    ///
    /// This test makes sure there are no spurious rejections that might hide bugs in the other tests.
    /// (And that the test infrastructure generally works.)
    #[test]
    fn accept_distinct_arbitrary_sapling_nullifiers(
        spend in TypeNameToDebug::<sapling::Spend::<PerSpendAnchor>>::arbitrary(),
        sapling_shielded_data in TypeNameToDebug::<sapling::ShieldedData::<PerSpendAnchor>>::arbitrary(),
        use_finalized_state in any::<bool>(),
    ) {
        zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        let transaction = transaction_v4_with_sapling_shielded_data(
            sapling_shielded_data.0,
            &[spend.0]
        );

        // convert the coinbase transaction to a version that the non-finalized state will accept
        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1
            .transactions
            .push(transaction.into());

        let (mut state, _genesis) = new_state_with_mainnet_genesis();
        let previous_mem = state.mem.clone();

        // randomly choose to commit the block to the finalized or non-finalized state
        if use_finalized_state {
            let block1 = FinalizedBlock::from(Arc::new(block1));
            let commit_result = state.disk.commit_finalized_direct(block1.clone(), "test");

            prop_assert_eq!(Some((Height(1), block1.hash)), state.best_tip());
            prop_assert!(commit_result.is_ok());
            prop_assert!(state.mem.eq_internal_state(&previous_mem));
        } else {
            let block1 = Arc::new(block1).prepare();
            let commit_result =
                state.validate_and_commit(block1.clone());

            prop_assert_eq!(commit_result, Ok(()));
            prop_assert_eq!(Some((Height(1), block1.hash)), state.best_tip());
            prop_assert!(!state.mem.eq_internal_state(&previous_mem));
        }
    }

    /// Make sure duplicate sapling nullifiers are rejected by state contextual validation,
    /// if they come from different Spends in the same sapling::ShieldedData/Transaction.
    #[test]
    fn reject_duplicate_sapling_nullifiers_in_transaction(
        spend1 in TypeNameToDebug::<sapling::Spend::<PerSpendAnchor>>::arbitrary(),
        mut spend2 in TypeNameToDebug::<sapling::Spend::<PerSpendAnchor>>::arbitrary(),
        sapling_shielded_data in TypeNameToDebug::<sapling::ShieldedData::<PerSpendAnchor>>::arbitrary(),
    ) {
        zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        make_distinct_nullifiers(&mut [spend1.nullifier, spend2.nullifier]);

        // create a double-spend across two spends
        let duplicate_nullifier = spend1.nullifier;
        spend2.nullifier = duplicate_nullifier;

        let transaction = transaction_v4_with_sapling_shielded_data(
            sapling_shielded_data.0,
            &[spend1.0, spend2.0],
        );

        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1
            .transactions
            .push(transaction.into());

        let (mut state, genesis) = new_state_with_mainnet_genesis();
        let previous_mem = state.mem.clone();

        let block1 = Arc::new(block1).prepare();
        let commit_result =
            state.validate_and_commit(block1);

        prop_assert_eq!(
            commit_result,
            Err(
                DuplicateSaplingNullifier {
                    nullifier: duplicate_nullifier,
                    in_finalized_state: false,
                }.into()
            )
        );
        prop_assert_eq!(Some((Height(0), genesis.hash)), state.best_tip());
        prop_assert!(state.mem.eq_internal_state(&previous_mem));
    }

    /// Make sure duplicate sapling nullifiers are rejected by state contextual validation,
    /// if they come from different transactions in the same block.
    #[test]
    fn reject_duplicate_sapling_nullifiers_in_block(
        spend1 in TypeNameToDebug::<sapling::Spend::<PerSpendAnchor>>::arbitrary(),
        mut spend2 in TypeNameToDebug::<sapling::Spend::<PerSpendAnchor>>::arbitrary(),
        sapling_shielded_data1 in TypeNameToDebug::<sapling::ShieldedData::<PerSpendAnchor>>::arbitrary(),
        sapling_shielded_data2 in TypeNameToDebug::<sapling::ShieldedData::<PerSpendAnchor>>::arbitrary(),
    ) {
        zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        make_distinct_nullifiers(&mut [spend1.nullifier, spend2.nullifier]);

        // create a double-spend across two transactions
        let duplicate_nullifier = spend1.nullifier;
        spend2.nullifier = duplicate_nullifier;

        let transaction1 = transaction_v4_with_sapling_shielded_data(
            sapling_shielded_data1.0,
            &[spend1.0]
        );
        let transaction2 = transaction_v4_with_sapling_shielded_data(
            sapling_shielded_data2.0,
            &[spend2.0]
        );

        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1
            .transactions
            .push(transaction1.into());
        block1
            .transactions
            .push(transaction2.into());

        let (mut state, genesis) = new_state_with_mainnet_genesis();
        let previous_mem = state.mem.clone();

        let block1 = Arc::new(block1).prepare();
        let commit_result =
            state.validate_and_commit(block1);

        prop_assert_eq!(
            commit_result,
            Err(
                DuplicateSaplingNullifier {
                    nullifier: duplicate_nullifier,
                    in_finalized_state: false,
                }.into()
            )
        );
        prop_assert_eq!(Some((Height(0), genesis.hash)), state.best_tip());
        prop_assert!(state.mem.eq_internal_state(&previous_mem));
    }

    /// Make sure duplicate sapling nullifiers are rejected by state contextual validation,
    /// if they come from different blocks in the same chain.
    #[test]
    fn reject_duplicate_sapling_nullifiers_in_chain(
        spend1 in TypeNameToDebug::<sapling::Spend::<PerSpendAnchor>>::arbitrary(),
        mut spend2 in TypeNameToDebug::<sapling::Spend::<PerSpendAnchor>>::arbitrary(),
        sapling_shielded_data1 in TypeNameToDebug::<sapling::ShieldedData::<PerSpendAnchor>>::arbitrary(),
        sapling_shielded_data2 in TypeNameToDebug::<sapling::ShieldedData::<PerSpendAnchor>>::arbitrary(),
        duplicate_in_finalized_state in any::<bool>(),
    ) {
        zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");
        let mut block2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        make_distinct_nullifiers(&mut [spend1.nullifier, spend2.nullifier]);

        // create a double-spend across two blocks
        let duplicate_nullifier = spend1.nullifier;
        spend2.nullifier = duplicate_nullifier;

        let transaction1 = transaction_v4_with_sapling_shielded_data(
            sapling_shielded_data1.0,
            &[spend1.0]
        );
        let transaction2 = transaction_v4_with_sapling_shielded_data(
            sapling_shielded_data2.0,
            &[spend2.0]
        );

        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();
        block2.transactions[0] = transaction_v4_from_coinbase(&block2.transactions[0]).into();

        block1
            .transactions
            .push(transaction1.into());
        block2
            .transactions
            .push(transaction2.into());

        let (mut state, _genesis) = new_state_with_mainnet_genesis();
        let mut previous_mem = state.mem.clone();

        let block1_hash;
        // randomly choose to commit the next block to the finalized or non-finalized state
        if duplicate_in_finalized_state {
            let block1 = FinalizedBlock::from(Arc::new(block1));
            let commit_result = state.disk.commit_finalized_direct(block1.clone(), "test");

            prop_assert_eq!(Some((Height(1), block1.hash)), state.best_tip());
            prop_assert!(commit_result.is_ok());
            prop_assert!(state.mem.eq_internal_state(&previous_mem));

            block1_hash = block1.hash;
        } else {
            let block1 = Arc::new(block1).prepare();
            let commit_result =
                state.validate_and_commit(block1.clone());

            prop_assert_eq!(commit_result, Ok(()));
            prop_assert_eq!(Some((Height(1), block1.hash)), state.best_tip());
            prop_assert!(!state.mem.eq_internal_state(&previous_mem));

            block1_hash = block1.hash;
            previous_mem = state.mem.clone();
        }

        let block2 = Arc::new(block2).prepare();
        let commit_result =
            state.validate_and_commit(block2);

        prop_assert_eq!(
            commit_result,
            Err(
                DuplicateSaplingNullifier {
                    nullifier: duplicate_nullifier,
                    in_finalized_state: duplicate_in_finalized_state,
                }.into()
            )
        );
        prop_assert_eq!(Some((Height(1), block1_hash)), state.best_tip());
        prop_assert!(state.mem.eq_internal_state(&previous_mem));
    }
}

/// Return a new `StateService` containing the mainnet genesis block.
/// Also returns the finalized genesis block itself.
fn new_state_with_mainnet_genesis() -> (StateService, FinalizedBlock) {
    let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .expect("block should deserialize");

    let mut state = StateService::new(Config::ephemeral(), Mainnet);

    assert_eq!(None, state.best_tip());

    let genesis = FinalizedBlock::from(genesis);
    state
        .disk
        .commit_finalized_direct(genesis.clone(), "test")
        .expect("unexpected invalid genesis block test vector");

    assert_eq!(Some((Height(0), genesis.hash)), state.best_tip());

    (state, genesis)
}

/// Make sure the supplied nullifiers are distinct, modifying them if necessary.
fn make_distinct_nullifiers<'shielded_data, NullifierT>(
    nullifiers: impl IntoIterator<Item = &'shielded_data mut NullifierT>,
) where
    NullifierT: Into<[u8; 32]> + Clone + Eq + std::hash::Hash + 'shielded_data,
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
fn transaction_v4_with_joinsplit_data<'js>(
    joinsplit_data: impl Into<Option<JoinSplitData<Groth16Proof>>>,
    joinsplits: impl IntoIterator<Item = &'js JoinSplit<Groth16Proof>>,
) -> Transaction {
    let mut joinsplit_data = joinsplit_data.into();
    let joinsplits: Vec<_> = joinsplits.into_iter().cloned().collect();

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

        for mut joinsplit in &mut joinsplit_data.rest {
            joinsplit.vpub_old = zero_amount;
            joinsplit.vpub_new = zero_amount;
        }
    }

    Transaction::V4 {
        inputs: Vec::new(),
        outputs: Vec::new(),
        lock_time: LockTime::min_lock_time(),
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
fn transaction_v4_with_sapling_shielded_data<'s>(
    sapling_shielded_data: impl Into<Option<sapling::ShieldedData<PerSpendAnchor>>>,
    spends: impl IntoIterator<Item = &'s sapling::Spend<PerSpendAnchor>>,
) -> Transaction {
    let mut sapling_shielded_data = sapling_shielded_data.into();
    let spends: Vec<_> = spends.into_iter().cloned().collect();

    if let Some(ref mut sapling_shielded_data) = sapling_shielded_data {
        // make sure there are no other nullifiers, by replacing all the spends
        sapling_shielded_data.transfers = match (
            sapling_shielded_data.transfers.clone(),
            spends.is_empty(),
        ) {
            // old and new spends: replace spends
            (
                SpendsAndMaybeOutputs {
                    shared_anchor,
                    maybe_outputs,
                    ..
                },
                false,
            ) => SpendsAndMaybeOutputs {
                shared_anchor,
                spends: spends.try_into().expect("just checked at least one spend"),
                maybe_outputs,
            },
            // old spends, but no new spends: delete spends, panic if no outputs
            (SpendsAndMaybeOutputs { maybe_outputs, .. }, true) => JustOutputs {
                outputs: maybe_outputs.try_into().expect(
                    "unexpected invalid TransferData: must have at least one spend or one output",
                ),
            },
            // no old spends, but new spends: add spends
            (JustOutputs { outputs, .. }, false) => SpendsAndMaybeOutputs {
                shared_anchor: FieldNotPresent,
                spends: spends.try_into().expect("just checked at least one spend"),
                maybe_outputs: outputs.into(),
            },
            // no old and no new spends: do nothing
            (just_outputs @ JustOutputs { .. }, true) => just_outputs,
        };

        // set value balance to 0 to pass the chain value pool checks
        let zero_amount = 0.try_into().expect("unexpected invalid zero amount");
        sapling_shielded_data.value_balance = zero_amount;
    }

    Transaction::V4 {
        inputs: Vec::new(),
        outputs: Vec::new(),
        lock_time: LockTime::min_lock_time(),
        expiry_height: Height(0),
        joinsplit_data: None,
        sapling_shielded_data,
    }
}

/// Return a `Transaction::V4` with the coinbase data from `coinbase`.
///
/// Used to convert a coinbase transaction to a version that the non-finalized state will accept.
fn transaction_v4_from_coinbase(coinbase: &Transaction) -> Transaction {
    assert!(
        !coinbase.has_sapling_shielded_data(),
        "conversion assumes sapling shielded data is None"
    );

    Transaction::V4 {
        inputs: coinbase.inputs().to_vec(),
        outputs: coinbase.outputs().to_vec(),
        lock_time: coinbase.lock_time(),
        // `Height(0)` means that the expiry height is ignored
        expiry_height: coinbase.expiry_height().unwrap_or(Height(0)),
        // invalid for coinbase transactions
        joinsplit_data: None,
        sapling_shielded_data: None,
    }
}
