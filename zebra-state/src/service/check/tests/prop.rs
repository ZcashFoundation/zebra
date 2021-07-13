//! Randomised property tests for state contextual validation nullifier: (), in_finalized_state: ()  nullifier: (), in_finalized_state: ()  checks.

use std::{convert::TryInto, sync::Arc};

use itertools::Itertools;
use proptest::prelude::*;

use zebra_chain::{
    block::{Block, Height},
    fmt::TypeNameToDebug,
    parameters::Network::*,
    primitives::Groth16Proof,
    serialization::ZcashDeserializeInto,
    sprout::{self, JoinSplit},
    transaction::{JoinSplitData, LockTime, Transaction},
};

use crate::{
    config::Config, service::StateService, tests::Prepare, FinalizedBlock,
    ValidateContextError::DuplicateSproutNullifier,
};

// These tests use the `Arbitrary` trait to easily generate complex types,
// then modify those types to cause an error (or to ensure success).
//
// We could use mainnet or testnet blocks in these tests,
// but the differences shouldn't matter,
// because we're only interested in spend validation,
// (and passing various other state checks).
proptest! {
    /// Make sure an arbitrary sprout nullifier is accepted by state contextual validation.
    ///
    /// This test makes sure there are no spurious rejections that might hide bugs in the other tests.
    /// (And that the test infrastructure generally works.)
    #[test]
    fn accept_distinct_arbitrary_sprout_nullifiers(
        mut joinsplit in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        mut joinsplit_data in TypeNameToDebug::<JoinSplitData::<Groth16Proof>>::arbitrary(),
    ) {
        zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        make_distinct_nullifiers(&mut joinsplit.nullifiers);

        // make sure there are no other nullifiers
        joinsplit_data.first = joinsplit.0;
        joinsplit_data.rest = Vec::new();

        let transaction = transaction_v4_with_joinsplit_data(joinsplit_data.0);

        // convert the coinbase transaction to a version that the non-finalized state will accept
        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1
            .transactions
            .push(transaction.into());

        let (mut state, _genesis) = new_state_with_mainnet_genesis();

        let block1 = Arc::new(block1).prepare();
        let commit_result =
            state.validate_and_commit(block1.clone());

        // block was accepted
        prop_assert_eq!(commit_result, Ok(()));
        prop_assert_eq!(Some((Height(1), block1.hash)), state.best_tip());
    }

    /// Make sure duplicate sprout nullifiers are rejected by state contextual validation,
    /// if they come from the same JoinSplit.
    #[test]
    fn reject_duplicate_sprout_nullifiers_in_joinsplit(
        mut joinsplit in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        mut joinsplit_data in TypeNameToDebug::<JoinSplitData::<Groth16Proof>>::arbitrary(),
    ) {
        zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        // create a double-spend within the same joinsplit
        // this might not actually be valid under the nullifier generation consensus rules
        let duplicate_nullifier = joinsplit.nullifiers[0];
        joinsplit.nullifiers[1] = duplicate_nullifier;

        joinsplit_data.first = joinsplit.0;
        joinsplit_data.rest = Vec::new();

        let transaction = transaction_v4_with_joinsplit_data(joinsplit_data.0);

        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1
            .transactions
            .push(transaction.into());

        let (mut state, genesis) = new_state_with_mainnet_genesis();

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
    }

    /// Make sure duplicate sprout nullifiers are rejected by state contextual validation,
    /// if they come from different JoinSplits in the same JoinSplitData/Transaction.
    #[test]
    fn reject_duplicate_sprout_nullifiers_in_transaction(
        mut joinsplit1 in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        mut joinsplit2 in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        mut joinsplit_data in TypeNameToDebug::<JoinSplitData::<Groth16Proof>>::arbitrary(),
    ) {
        zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        make_distinct_nullifiers(&mut joinsplit1.nullifiers.iter_mut().chain(joinsplit2.nullifiers.iter_mut()));

        // create a double-spend across two joinsplits
        let duplicate_nullifier = joinsplit1.nullifiers[0];
        joinsplit2.nullifiers[0] = duplicate_nullifier;

        // make sure there are no other nullifiers
        joinsplit_data.first = joinsplit1.0;
        joinsplit_data.rest = vec![joinsplit2.0];

        let transaction = transaction_v4_with_joinsplit_data(joinsplit_data.0);

        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1
            .transactions
            .push(transaction.into());

        let (mut state, genesis) = new_state_with_mainnet_genesis();

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
    }

    /// Make sure duplicate sprout nullifiers are rejected by state contextual validation,
    /// if they come from different transactions in the same block.
    #[test]
    fn reject_duplicate_sprout_nullifiers_in_block(
        mut joinsplit1 in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        mut joinsplit2 in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        mut joinsplit_data1 in TypeNameToDebug::<JoinSplitData::<Groth16Proof>>::arbitrary(),
        mut joinsplit_data2 in TypeNameToDebug::<JoinSplitData::<Groth16Proof>>::arbitrary(),
    ) {
        zebra_test::init();

        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        make_distinct_nullifiers(&mut joinsplit1.nullifiers.iter_mut().chain(joinsplit2.nullifiers.iter_mut()));

        // create a double-spend across two transactions
        let duplicate_nullifier = joinsplit1.nullifiers[0];
        joinsplit2.nullifiers[0] = duplicate_nullifier;

        // make sure there are no other nullifiers
        joinsplit_data1.first = joinsplit1.0;
        joinsplit_data1.rest = Vec::new();

        joinsplit_data2.first = joinsplit2.0;
        joinsplit_data2.rest = Vec::new();

        let transaction1 = transaction_v4_with_joinsplit_data(joinsplit_data1.0);
        let transaction2 = transaction_v4_with_joinsplit_data(joinsplit_data2.0);

        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

        block1
            .transactions
            .push(transaction1.into());
        block1
            .transactions
            .push(transaction2.into());

        let (mut state, genesis) = new_state_with_mainnet_genesis();

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
    }

    /// Make sure duplicate sprout nullifiers are rejected by state contextual validation,
    /// if they come from different blocks in the same chain.
    #[test]
    fn reject_duplicate_sprout_nullifiers_in_chain(
        mut joinsplit1 in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        mut joinsplit2 in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        mut joinsplit_data1 in TypeNameToDebug::<JoinSplitData::<Groth16Proof>>::arbitrary(),
        mut joinsplit_data2 in TypeNameToDebug::<JoinSplitData::<Groth16Proof>>::arbitrary(),
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

        // make sure there are no other nullifiers
        joinsplit_data1.first = joinsplit1.0;
        joinsplit_data1.rest = Vec::new();

        joinsplit_data2.first = joinsplit2.0;
        joinsplit_data2.rest = Vec::new();

        let transaction1 = transaction_v4_with_joinsplit_data(joinsplit_data1.0);
        let transaction2 = transaction_v4_with_joinsplit_data(joinsplit_data2.0);

        block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();
        block2.transactions[0] = transaction_v4_from_coinbase(&block2.transactions[0]).into();

        block1
            .transactions
            .push(transaction1.into());
        block2
            .transactions
            .push(transaction2.into());

        let (mut state, _genesis) = new_state_with_mainnet_genesis();

        let block1_hash;
        // randomly choose to commit the next block to the finalized or non-finalized state
        if duplicate_in_finalized_state {
            let block1 = FinalizedBlock::from(Arc::new(block1));
            let commit_result = state.disk.commit_finalized_direct(block1.clone(), "test");

            prop_assert_eq!(Some((Height(1), block1.hash)), state.best_tip());
            prop_assert!(commit_result.is_ok());

            block1_hash = block1.hash;
        } else {
            let block1 = Arc::new(block1).prepare();
            let commit_result =
                state.validate_and_commit(block1.clone());

            prop_assert_eq!(commit_result, Ok(()));
            prop_assert_eq!(Some((Height(1), block1.hash)), state.best_tip());

            block1_hash = block1.hash;
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
    let commit_result = state.disk.commit_finalized_direct(genesis.clone(), "test");

    assert_eq!(Some((Height(0), genesis.hash)), state.best_tip());
    assert!(commit_result.is_ok());

    (state, genesis)
}

/// Make sure the supplied nullifiers are distinct, modifying them if necessary.
fn make_distinct_nullifiers<'joinsplit>(
    nullifiers: impl IntoIterator<Item = &'joinsplit mut sprout::Nullifier>,
) {
    let nullifiers: Vec<_> = nullifiers.into_iter().collect();

    if nullifiers.iter().unique().count() < nullifiers.len() {
        let mut tweak: u8 = 0x00;
        for nullifier in nullifiers {
            nullifier.0[0] = tweak;
            tweak = tweak
                .checked_add(0x01)
                .expect("unexpectedly large nullifier list");
        }
    }
}

/// Return a `Transaction::V4` containing `joinsplit_data`.
///
/// Other fields have empty or default values.
fn transaction_v4_with_joinsplit_data(
    joinsplit_data: impl Into<Option<JoinSplitData<Groth16Proof>>>,
) -> Transaction {
    let mut joinsplit_data = joinsplit_data.into();

    // set value balance to 0 to pass the chain value pool checks
    if let Some(ref mut joinsplit_data) = joinsplit_data {
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
