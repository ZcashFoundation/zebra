//! Randomised property tests for the finalized state.

use std::{cmp::Ordering, collections::HashMap, env, sync::Arc};

use proptest::prelude::*;

use zebra_chain::{
    block::{Block, Height},
    fmt::TypeNameToDebug,
    parameters::Network::*,
    primitives::Groth16Proof,
    serialization::ZcashDeserializeInto,
    sprout::JoinSplit,
    transaction::{JoinSplitData, LockTime, Transaction},
    transparent::{self, OutPoint},
};
use zebra_test::prelude::Result;

use crate::{
    config::Config,
    service::{
        arbitrary::PreparedChain,
        finalized_state::{FinalizedBlock, FinalizedState},
    },
    OrderedUtxo,
};

use Ordering::*;

const DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES: u32 = 32;

/// Make sure blocks with v5 transactions are correctly committed or rejected
/// from the finalized state.
#[test]
// TODO: move the double-spend checks to the non-finalized state / service,
//       and delete the nullifier checks in this test
#[ignore]
fn blocks_with_v5_transactions() -> Result<()> {
    zebra_test::init();
    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
        |((chain, count, network) in PreparedChain::default())| {
            let mut state = FinalizedState::new(&Config::ephemeral(), network);
            let mut height = Height(0);
            let mut old_height = None;

            let mut sprout_nullifiers = HashMap::new();
            let mut sapling_nullifiers = HashMap::new();
            let mut orchard_nullifiers = HashMap::new();

            // use `count` to minimize test failures, so they are easier to diagnose
            for block in chain.iter().take(count) {
                for nullifier in block.block.transactions.iter().map(|transaction| transaction.sprout_nullifiers()).flatten() {
                    *sprout_nullifiers.entry(nullifier).or_insert(0) += 1;
                }

                for nullifier in block.block.transactions.iter().map(|transaction| transaction.sapling_nullifiers()).flatten() {
                    *sapling_nullifiers.entry(nullifier).or_insert(0) += 1;
                }

                for nullifier in block.block.transactions.iter().map(|transaction| transaction.orchard_nullifiers()).flatten() {
                    *orchard_nullifiers.entry(nullifier).or_insert(0) += 1;
                }

                // TODO: detect duplicates in other column families, and make sure they are errors

                let commit_result = state.commit_finalized_direct(FinalizedBlock::from(block.clone()));

                // if there are any double-spends
                if sprout_nullifiers.values().any(|spends| *spends > 1) ||
                    sapling_nullifiers.values().any(|spends| *spends > 1) ||
                    orchard_nullifiers.values().any(|spends| *spends > 1) {
                        prop_assert_eq!(old_height, state.finalized_tip_height());
                        prop_assert!(commit_result.is_err());
                        // once we've had an error, no more blocks can be committed,
                        // so skip the remainder of the test
                        prop_assume!(false);
                    } else {
                        prop_assert_eq!(
                            Some(height),
                            state.finalized_tip_height(),
                            "{:?}",
                            commit_result,
                        );
                        prop_assert!(commit_result.is_ok());
                        prop_assert_eq!(commit_result.unwrap(), block.hash);
                    }

                old_height = Some(height);
                height = Height(height.0 + 1);
            }
    });

    Ok(())
}

// These tests use the `Arbitrary` trait to easily generate complex types,
// then modify those types to cause an error.
//
// We could use mainnet or testnet blocks in these tests,
// but the differences shouldn't matter,
// because we're only interested in spend validation,
// (and passing various other state checks)
proptest! {
    /// Make sure an arbitrary transparent output can be spent in the next block.
    ///
    /// This test makes sure there are no spurious rejections that might hide bugs in the other tests.
    /// (And that the test infrastructure generally works.)
    #[test]
    fn accept_transparent_spend_in_next_block(
        output in TypeNameToDebug::<transparent::Output>::arbitrary(),
        // if we passed a height here, we'd get coinbase inputs, which are not spends
        mut input in TypeNameToDebug::<transparent::Input>::arbitrary_with(None),
    ) {
        use transparent::Input::*;

        zebra_test::init();

        let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
            .zcash_deserialize_into::<Arc<Block>>()
            .expect("block should deserialize");
        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");
        let mut block2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        let output_transaction = Transaction::V4 {
            inputs: Vec::new(),
            outputs: vec![output.0],
            lock_time: LockTime::min_lock_time(),
            expiry_height: Height(0),
            joinsplit_data: None,
            sapling_shielded_data: None,
        };

        // create a valid spend
        if let PrevOut { ref mut outpoint, ..} = &mut input.0 {
            let unspent_outpoint = OutPoint {
                hash: output_transaction.hash(),
                index: 0,
            };
            *outpoint = unspent_outpoint;
        } else {
            unreachable!("height: None should produce PrevOut inputs");
        }

        let spend_transaction = Transaction::V4 {
            inputs: vec![input.0],
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
            expiry_height: Height(0),
            joinsplit_data: None,
            sapling_shielded_data: None,
        };

        block1
            .transactions
            .push(output_transaction.into());
        block2
            .transactions
            .push(spend_transaction.into());

        let mut state = FinalizedState::new(&Config::ephemeral(), Mainnet);

        prop_assert_eq!(None, state.finalized_tip_height());

        let commit_result = state.commit_finalized_direct(FinalizedBlock::from(genesis));

        prop_assert_eq!(Some(Height(0)), state.finalized_tip_height());
        prop_assert!(commit_result.is_ok());

        let commit_result =
            state.commit_finalized_direct(FinalizedBlock::from(Arc::new(block1)));

        prop_assert_eq!(Some(Height(1)), state.finalized_tip_height());
        prop_assert!(commit_result.is_ok());

        let commit_result =
            state.commit_finalized_direct(FinalizedBlock::from(Arc::new(block2)));

        prop_assert_eq!(Some(Height(2)), state.finalized_tip_height());
        prop_assert!(commit_result.is_ok());
    }

    /// Make sure duplicate transparent output spends are rejected by the finalized state
    /// if they come from different OutPoints in the same Transaction.
    #[test]
    fn reject_duplicate_transparent_spends_in_transaction(
        output in TypeNameToDebug::<transparent::Output>::arbitrary(),
        // if we passed a height here, we'd get coinbase inputs, which are not spends
        mut input1 in TypeNameToDebug::<transparent::Input>::arbitrary_with(None),
        mut input2 in TypeNameToDebug::<transparent::Input>::arbitrary_with(None),
    ) {
        use transparent::Input::*;

        zebra_test::init();

        let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
            .zcash_deserialize_into::<Arc<Block>>()
            .expect("block should deserialize");
        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");
        let mut block2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        let output_transaction = Transaction::V4 {
            inputs: Vec::new(),
            outputs: vec![output.0],
            lock_time: LockTime::min_lock_time(),
            expiry_height: Height(0),
            joinsplit_data: None,
            sapling_shielded_data: None,
        };

        // create a double-spend across two inputs
        if let (PrevOut { outpoint: ref mut outpoint2, ..}, PrevOut { outpoint: ref mut outpoint1, .. } ) = (&mut input2.0, &mut input1.0) {
            let unspent_outpoint = OutPoint {
                hash: output_transaction.hash(),
                index: 0,
            };
            *outpoint1 = unspent_outpoint;
            *outpoint2 = unspent_outpoint;
        } else {
            unreachable!("height: None should produce PrevOut inputs");
        }

        let spend_transaction = Transaction::V4 {
            inputs: vec![input1.0, input2.0],
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
            expiry_height: Height(0),
            joinsplit_data: None,
            sapling_shielded_data: None,
        };

        block1
            .transactions
            .push(output_transaction.into());
        block2
            .transactions
            .push(spend_transaction.into());

        let mut state = FinalizedState::new(&Config::ephemeral(), Mainnet);

        prop_assert_eq!(None, state.finalized_tip_height());

        let commit_result = state.commit_finalized_direct(FinalizedBlock::from(genesis));

        prop_assert_eq!(Some(Height(0)), state.finalized_tip_height());
        prop_assert!(commit_result.is_ok());

        let commit_result =
            state.commit_finalized_direct(FinalizedBlock::from(Arc::new(block1)));

        prop_assert_eq!(Some(Height(1)), state.finalized_tip_height());
        prop_assert!(commit_result.is_ok());

        let commit_result =
            state.commit_finalized_direct(FinalizedBlock::from(Arc::new(block2)));

        // block was rejected
        prop_assert_eq!(Some(Height(1)), state.finalized_tip_height());
        prop_assert!(commit_result.is_err());
    }

    /// Make sure duplicate transparent output spends are rejected by the finalized state
    /// if they come from different Transactions in the same Block.
    #[test]
    fn reject_duplicate_transparent_spends_in_block(
        output in TypeNameToDebug::<transparent::Output>::arbitrary(),
        // if we passed a height here, we'd get coinbase inputs, which are not spends
        mut input1 in TypeNameToDebug::<transparent::Input>::arbitrary_with(None),
        mut input2 in TypeNameToDebug::<transparent::Input>::arbitrary_with(None),
    ) {
        use transparent::Input::*;

        zebra_test::init();

        let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
            .zcash_deserialize_into::<Arc<Block>>()
            .expect("block should deserialize");
        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");
        let mut block2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        let output_transaction = Transaction::V4 {
            inputs: Vec::new(),
            outputs: vec![output.0],
            lock_time: LockTime::min_lock_time(),
            expiry_height: Height(0),
            joinsplit_data: None,
            sapling_shielded_data: None,
        };

        // create a double-spend across two inputs
        if let (PrevOut { outpoint: ref mut outpoint2, ..}, PrevOut { outpoint: ref mut outpoint1, .. } ) = (&mut input2.0, &mut input1.0) {
            let unspent_outpoint = OutPoint {
                hash: output_transaction.hash(),
                index: 0,
            };
            *outpoint1 = unspent_outpoint;
            *outpoint2 = unspent_outpoint;
        } else {
            unreachable!("height: None should produce PrevOut inputs");
        }

        let spend_transaction1 = Transaction::V4 {
            inputs: vec![input1.0],
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
            expiry_height: Height(0),
            joinsplit_data: None,
            sapling_shielded_data: None,
        };
        let spend_transaction2 = Transaction::V4 {
            inputs: vec![input2.0],
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
            expiry_height: Height(0),
            joinsplit_data: None,
            sapling_shielded_data: None,
        };

        block1
            .transactions
            .push(output_transaction.into());
        block2
            .transactions
            .push(spend_transaction1.into());
        block2
            .transactions
            .push(spend_transaction2.into());

        let mut state = FinalizedState::new(&Config::ephemeral(), Mainnet);

        prop_assert_eq!(None, state.finalized_tip_height());

        let commit_result = state.commit_finalized_direct(FinalizedBlock::from(genesis));

        prop_assert_eq!(Some(Height(0)), state.finalized_tip_height());
        prop_assert!(commit_result.is_ok());

        let commit_result =
            state.commit_finalized_direct(FinalizedBlock::from(Arc::new(block1)));

        prop_assert_eq!(Some(Height(1)), state.finalized_tip_height());
        prop_assert!(commit_result.is_ok());

        let commit_result =
            state.commit_finalized_direct(FinalizedBlock::from(Arc::new(block2)));

        // block was rejected
        prop_assert_eq!(Some(Height(1)), state.finalized_tip_height());
        prop_assert!(commit_result.is_err());
    }

    /// Make sure transparent output spends are rejected by the finalized state
    /// if the corresponding UTXO has already been spent from the state.
    #[test]
    fn reject_duplicate_transparent_spends_in_chain(
        output in TypeNameToDebug::<transparent::Output>::arbitrary(),
        // if we passed a height here, we'd get coinbase inputs, which are not spends
        mut input1 in TypeNameToDebug::<transparent::Input>::arbitrary_with(None),
        mut input2 in TypeNameToDebug::<transparent::Input>::arbitrary_with(None),
    ) {
        use transparent::Input::*;

        zebra_test::init();

        let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
            .zcash_deserialize_into::<Arc<Block>>()
            .expect("block should deserialize");
        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");
        let mut block2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");
        let mut block3 = zebra_test::vectors::BLOCK_MAINNET_3_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        let output_transaction = Transaction::V4 {
            inputs: Vec::new(),
            outputs: vec![output.0],
            lock_time: LockTime::min_lock_time(),
            expiry_height: Height(0),
            joinsplit_data: None,
            sapling_shielded_data: None,
        };

        // create a double-spend across two inputs
        if let (PrevOut { outpoint: ref mut outpoint2, ..}, PrevOut { outpoint: ref mut outpoint1, .. } ) = (&mut input2.0, &mut input1.0) {
            let unspent_outpoint = OutPoint {
                hash: output_transaction.hash(),
                index: 0,
            };
            *outpoint1 = unspent_outpoint;
            *outpoint2 = unspent_outpoint;
        } else {
            unreachable!("height: None should produce PrevOut inputs");
        }

        let spend_transaction1 = Transaction::V4 {
            inputs: vec![input1.0],
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
            expiry_height: Height(0),
            joinsplit_data: None,
            sapling_shielded_data: None,
        };
        let spend_transaction2 = Transaction::V4 {
            inputs: vec![input2.0],
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
            expiry_height: Height(0),
            joinsplit_data: None,
            sapling_shielded_data: None,
        };

        block1
            .transactions
            .push(output_transaction.into());
        block2
            .transactions
            .push(spend_transaction1.into());
        block3
            .transactions
            .push(spend_transaction2.into());

        let mut state = FinalizedState::new(&Config::ephemeral(), Mainnet);

        prop_assert_eq!(None, state.finalized_tip_height());

        let commit_result = state.commit_finalized_direct(FinalizedBlock::from(genesis));

        prop_assert_eq!(Some(Height(0)), state.finalized_tip_height());
        prop_assert!(commit_result.is_ok());

        let commit_result =
            state.commit_finalized_direct(FinalizedBlock::from(Arc::new(block1)));

        prop_assert_eq!(Some(Height(1)), state.finalized_tip_height());
        prop_assert!(commit_result.is_ok());

        let commit_result =
            state.commit_finalized_direct(FinalizedBlock::from(Arc::new(block2)));

        prop_assert_eq!(Some(Height(2)), state.finalized_tip_height());
        prop_assert!(commit_result.is_ok());

        let commit_result =
            state.commit_finalized_direct(FinalizedBlock::from(Arc::new(block3)));

        // block was rejected
        prop_assert_eq!(Some(Height(2)), state.finalized_tip_height());
        prop_assert!(commit_result.is_err());
    }

    /// Make sure transparent output spends are rejected by the finalized state
    /// if they spend an output in the same or later transaction in the block.
    ///
    /// And make sure earlier spends are accepted.
    #[test]
    fn transparent_spend_order_within_block(
        output in TypeNameToDebug::<transparent::Output>::arbitrary(),
        mut input in TypeNameToDebug::<transparent::Input>::arbitrary_with(None),
        spend_order in any::<Ordering>(),
    ) {
        use transparent::Input::*;

        zebra_test::init();

        let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
            .zcash_deserialize_into::<Arc<Block>>()
            .expect("block should deserialize");
        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        let mut output_transaction = Transaction::V4 {
            inputs: Vec::new(),
            outputs: vec![output.0.clone()],
            lock_time: LockTime::min_lock_time(),
            expiry_height: Height(0),
            joinsplit_data: None,
            sapling_shielded_data: None,
        };

        let mut unspent_outpoint = OutPoint {
            hash: output_transaction.hash(),
            index: 0,
        };

        // create a spend of the output
        if let PrevOut { ref mut outpoint, ..} = &mut input.0 {
            *outpoint = unspent_outpoint;
        } else {
            unreachable!("height: None should produce PrevOut inputs");
        }

        if spend_order == Equal {
            // Modifying the inputs changes the transaction ID,
            // so the input now points to an output that doesn't exist.
            //
            // We compensate for the ID change when we call the service.
            if let Transaction::V4 { ref mut inputs, ..} = &mut output_transaction {
                inputs.push(input.0);
            } else {
                unreachable!("should be Transaction::V4");
            }

            unspent_outpoint = OutPoint {
                hash: output_transaction.hash(),
                index: 0,
            };

            block1
                .transactions
                .push(output_transaction.into());
        } else {
            let spend_transaction = Transaction::V4 {
                inputs: vec![input.0],
                outputs: Vec::new(),
                lock_time: LockTime::min_lock_time(),
                expiry_height: Height(0),
                joinsplit_data: None,
                sapling_shielded_data: None,
            };

            if spend_order == Less {
                block1
                    .transactions
                    .push(spend_transaction.into());
                block1
                    .transactions
                    .push(output_transaction.into());
            } else {
                // this is the only valid order
                block1
                    .transactions
                    .push(output_transaction.into());
                block1
                    .transactions
                    .push(spend_transaction.into());
            }
        }

        let mut state = FinalizedState::new(&Config::ephemeral(), Mainnet);

        prop_assert_eq!(None, state.finalized_tip_height());

        let commit_result = state.commit_finalized_direct(FinalizedBlock::from(genesis));

        prop_assert_eq!(Some(Height(0)), state.finalized_tip_height());
        prop_assert!(commit_result.is_ok());

        let mut block1 = FinalizedBlock::from(Arc::new(block1));

        // Fake a new output with the modified transaction ID.
        // We can't just update the input, because that would change the transaction ID again.
        if spend_order == Equal {
            // miner reward, founders reward, and the one we added
            prop_assert_eq!(block1.new_outputs.len(), 3);
            // coinbase and the one we added
            prop_assert_eq!(block1.block.transactions.len(), 2);

            let new_utxo = OrderedUtxo::new(output.0, Height(1), false, 1);
            block1.new_outputs.insert(unspent_outpoint, new_utxo);
        }

        let commit_result =
            state.commit_finalized_direct(block1);

        if spend_order == Greater {
            prop_assert_eq!(
                Some(Height(1)),
                state.finalized_tip_height(),
                "{:?}",
                commit_result,
            );
            prop_assert!(commit_result.is_ok());
        } else {
            // block was rejected
            prop_assert_eq!(Some(Height(0)), state.finalized_tip_height());
            prop_assert!(commit_result.is_err());
        }
    }

    /// Make sure an arbitrary sprout nullifier is accepted by the finalized state.
    ///
    /// This test makes sure there are no spurious rejections that might hide bugs in the other tests.
    /// (And that the test infrastructure generally works.)
    #[test]
    fn accept_distinct_arbitrary_sprout_nullifiers(
        mut joinsplit in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        mut joinsplit_data in TypeNameToDebug::<JoinSplitData::<Groth16Proof>>::arbitrary(),
    ) {
        zebra_test::init();

        let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
            .zcash_deserialize_into::<Arc<Block>>()
            .expect("block should deserialize");
        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        // make sure the nullifiers are distinct
        if joinsplit.nullifiers[1] == joinsplit.nullifiers[0] {
            joinsplit.nullifiers[1].0[0] = 0x00;
            joinsplit.nullifiers[0].0[0] = 0x01;
        }

        // make sure there are no other duplicate nullifiers
        joinsplit_data.first = joinsplit.0;
        joinsplit_data.rest = Vec::new();

        let transaction = Transaction::V4 {
            inputs: Vec::new(),
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
            expiry_height: Height(0),
            joinsplit_data: Some(joinsplit_data.0),
            sapling_shielded_data: None,
        };

        block1
            .transactions
            .push(transaction.into());

        let mut state = FinalizedState::new(&Config::ephemeral(), Mainnet);

        prop_assert_eq!(None, state.finalized_tip_height());

        let commit_result = state.commit_finalized_direct(FinalizedBlock::from(genesis));

        prop_assert_eq!(Some(Height(0)), state.finalized_tip_height());
        prop_assert!(commit_result.is_ok());

        let commit_result =
            state.commit_finalized_direct(FinalizedBlock::from(Arc::new(block1)));

        // block was rejected
        prop_assert_eq!(Some(Height(1)), state.finalized_tip_height());
        prop_assert!(commit_result.is_ok());
    }

    /// Make sure duplicate sprout nullifiers are rejected by the finalized state
    /// if they come from the same JoinSplit.
    #[test]
    fn reject_duplicate_sprout_nullifiers_in_joinsplit(
        mut joinsplit in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        mut joinsplit_data in TypeNameToDebug::<JoinSplitData::<Groth16Proof>>::arbitrary(),
    ) {
        zebra_test::init();

        let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
            .zcash_deserialize_into::<Arc<Block>>()
            .expect("block should deserialize");
        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        // create a double-spend within the same joinsplit
        // this might not actually be valid under the consensus rules
        joinsplit.nullifiers[1] = joinsplit.nullifiers[0];

        joinsplit_data.rest.push(joinsplit.0);

        let transaction = Transaction::V4 {
            inputs: Vec::new(),
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
            expiry_height: Height(0),
            joinsplit_data: Some(joinsplit_data.0),
            sapling_shielded_data: None,
        };

        block1
            .transactions
            .push(transaction.into());

        let mut state = FinalizedState::new(&Config::ephemeral(), Mainnet);

        prop_assert_eq!(None, state.finalized_tip_height());

        let commit_result = state.commit_finalized_direct(FinalizedBlock::from(genesis));

        prop_assert_eq!(Some(Height(0)), state.finalized_tip_height());
        prop_assert!(commit_result.is_ok());

        let commit_result =
            state.commit_finalized_direct(FinalizedBlock::from(Arc::new(block1)));

        // block was rejected
        prop_assert_eq!(Some(Height(0)), state.finalized_tip_height());
        prop_assert!(commit_result.is_err());
    }

    /// Make sure duplicate sprout nullifiers are rejected by the finalized state
    /// if they come from different JoinSplits in the same JoinSplitData/Transaction.
    #[test]
    fn reject_duplicate_sprout_nullifiers_in_transaction(
        joinsplit1 in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        mut joinsplit2 in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        mut joinsplit_data in TypeNameToDebug::<JoinSplitData::<Groth16Proof>>::arbitrary(),
    ) {
        zebra_test::init();

        let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
            .zcash_deserialize_into::<Arc<Block>>()
            .expect("block should deserialize");
        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        // create a double-spend across two joinsplits
        joinsplit2.nullifiers[0] = joinsplit1.nullifiers[0];

        joinsplit_data.rest.push(joinsplit1.0);
        joinsplit_data.rest.push(joinsplit2.0);

        let transaction = Transaction::V4 {
            inputs: Vec::new(),
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
            expiry_height: Height(0),
            joinsplit_data: Some(joinsplit_data.0),
            sapling_shielded_data: None,
        };

        block1
            .transactions
            .push(transaction.into());

        let mut state = FinalizedState::new(&Config::ephemeral(), Mainnet);

        prop_assert_eq!(None, state.finalized_tip_height());

        let commit_result = state.commit_finalized_direct(FinalizedBlock::from(genesis));

        prop_assert_eq!(Some(Height(0)), state.finalized_tip_height());
        prop_assert!(commit_result.is_ok());

        let commit_result =
            state.commit_finalized_direct(FinalizedBlock::from(Arc::new(block1)));

        // block was rejected
        prop_assert_eq!(Some(Height(0)), state.finalized_tip_height());
        prop_assert!(commit_result.is_err());
    }

    /// Make sure duplicate sprout nullifiers are rejected by the finalized state
    /// if they come from different transactions in the same block.
    #[test]
    fn reject_duplicate_sprout_nullifiers_in_block(
        joinsplit1 in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        mut joinsplit2 in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        mut joinsplit_data1 in TypeNameToDebug::<JoinSplitData::<Groth16Proof>>::arbitrary(),
        mut joinsplit_data2 in TypeNameToDebug::<JoinSplitData::<Groth16Proof>>::arbitrary(),
    ) {
        zebra_test::init();

        let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
            .zcash_deserialize_into::<Arc<Block>>()
            .expect("block should deserialize");
        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        // create a double-spend across two transactions
        joinsplit2.nullifiers[0] = joinsplit1.nullifiers[0];

        joinsplit_data1.rest.push(joinsplit1.0);
        joinsplit_data2.rest.push(joinsplit2.0);

        let transaction1 = Transaction::V4 {
            inputs: Vec::new(),
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
            expiry_height: Height(0),
            joinsplit_data: Some(joinsplit_data1.0),
            sapling_shielded_data: None,
        };
        let transaction2 = Transaction::V4 {
            inputs: Vec::new(),
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
            expiry_height: Height(0),
            joinsplit_data: Some(joinsplit_data2.0),
            sapling_shielded_data: None,
        };

        block1
            .transactions
            .push(transaction1.into());
        block1
            .transactions
            .push(transaction2.into());

        let mut state = FinalizedState::new(&Config::ephemeral(), Mainnet);

        prop_assert_eq!(None, state.finalized_tip_height());

        let commit_result = state.commit_finalized_direct(FinalizedBlock::from(genesis));

        prop_assert_eq!(Some(Height(0)), state.finalized_tip_height());
        prop_assert!(commit_result.is_ok());

        let commit_result =
            state.commit_finalized_direct(FinalizedBlock::from(Arc::new(block1)));

        // block was rejected
        prop_assert_eq!(Some(Height(0)), state.finalized_tip_height());
        prop_assert!(commit_result.is_err());
    }

    /// Make sure duplicate sprout nullifiers are rejected by the finalized state
    /// if they come from different blocks in the same chain.
    #[test]
    fn reject_duplicate_sprout_nullifiers_in_chain(
        joinsplit1 in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        mut joinsplit2 in TypeNameToDebug::<JoinSplit::<Groth16Proof>>::arbitrary(),
        mut joinsplit_data1 in TypeNameToDebug::<JoinSplitData::<Groth16Proof>>::arbitrary(),
        mut joinsplit_data2 in TypeNameToDebug::<JoinSplitData::<Groth16Proof>>::arbitrary(),
    ) {
        zebra_test::init();

        let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
            .zcash_deserialize_into::<Arc<Block>>()
            .expect("block should deserialize");
        let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");
        let mut block2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        // create a double-spend across two blocks
        joinsplit2.nullifiers[0] = joinsplit1.nullifiers[0];

        joinsplit_data1.rest.push(joinsplit1.0);
        joinsplit_data2.rest.push(joinsplit2.0);

        let transaction1 = Transaction::V4 {
            inputs: Vec::new(),
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
            expiry_height: Height(0),
            joinsplit_data: Some(joinsplit_data1.0),
            sapling_shielded_data: None,
        };
        let transaction2 = Transaction::V4 {
            inputs: Vec::new(),
            outputs: Vec::new(),
            lock_time: LockTime::min_lock_time(),
            expiry_height: Height(0),
            joinsplit_data: Some(joinsplit_data2.0),
            sapling_shielded_data: None,
        };

        block1
            .transactions
            .push(transaction1.into());
        block2
            .transactions
            .push(transaction2.into());

        let mut state = FinalizedState::new(&Config::ephemeral(), Mainnet);

        prop_assert_eq!(None, state.finalized_tip_height());

        let commit_result = state.commit_finalized_direct(FinalizedBlock::from(genesis));

        prop_assert_eq!(Some(Height(0)), state.finalized_tip_height());
        prop_assert!(commit_result.is_ok());

        let commit_result =
            state.commit_finalized_direct(FinalizedBlock::from(Arc::new(block1)));

        prop_assert_eq!(Some(Height(1)), state.finalized_tip_height());
        prop_assert!(commit_result.is_ok());

        let commit_result =
            state.commit_finalized_direct(FinalizedBlock::from(Arc::new(block2)));

        // block was rejected
        prop_assert_eq!(Some(Height(1)), state.finalized_tip_height());
        prop_assert!(commit_result.is_err());
    }
}
