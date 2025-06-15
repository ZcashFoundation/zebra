//! Randomised property tests for mempool storage.

use std::{collections::HashSet, env, fmt::Debug, thread, time::Duration};

use proptest::{collection::vec, prelude::*};
use proptest_derive::Arbitrary;

use zebra_chain::{
    amount::Amount,
    at_least_one,
    fmt::{DisplayToDebug, SummaryDebug},
    orchard,
    primitives::{Groth16Proof, ZkSnarkProof},
    sapling,
    serialization::AtLeastOne,
    sprout,
    transaction::{self, JoinSplitData, Transaction, UnminedTxId, VerifiedUnminedTx},
    transparent, LedgerState,
};

use crate::components::mempool::{
    config::Config,
    storage::{
        eviction_list::EvictionList, MempoolError, RejectionError, SameEffectsTipRejectionError,
        Storage, MAX_EVICTION_MEMORY_ENTRIES,
    },
    SameEffectsChainRejectionError,
};

use MultipleTransactionRemovalTestInput::*;

/// The mempool list limit tests can run for a long time.
///
/// We reduce the number of cases for those tests,
/// so individual tests take less than 10 seconds on most machines.
const DEFAULT_MEMPOOL_LIST_PROPTEST_CASES: u32 = 64;

/// Eviction memory time used for tests. Most tests won't care about this
/// so we use a large enough value that will never be reached in the tests.
const EVICTION_MEMORY_TIME: Duration = Duration::from_secs(60 * 60);

/// Transaction count used in some tests to derive the mempool test size.
const MEMPOOL_TX_COUNT: usize = 4;

proptest! {
    #![proptest_config(
        proptest::test_runner::Config::with_cases(env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_MEMPOOL_LIST_PROPTEST_CASES))
    )]

    /// Test that the reject list length limits are applied when inserting conflicting transactions.
    #[test]
    fn reject_lists_are_limited_insert_conflict(
        input in any::<SpendConflictTestInput>(),
        mut rejection_template in any::<UnminedTxId>()
    ) {
        let mut storage = Storage::new(
            &Config {
                tx_cost_limit: 160_000_000,
                eviction_memory_time: EVICTION_MEMORY_TIME,
                ..Default::default()
            });

        let (first_transaction, second_transaction) = input.conflicting_transactions();
        let input_permutations = vec![
            (first_transaction.clone(), second_transaction.clone()),
            (second_transaction, first_transaction),
        ];

        for (transaction_to_accept, transaction_to_reject) in input_permutations {
            let id_to_accept = transaction_to_accept.transaction.id;

            prop_assert_eq!(storage.insert(transaction_to_accept, Vec::new(), None, None), Ok(id_to_accept));

            // Make unique IDs by converting the index to bytes, and writing it to each ID
            let unique_ids = (0..MAX_EVICTION_MEMORY_ENTRIES as u32).map(move |index| {
                let index = index.to_le_bytes();
                rejection_template.mined_id_mut().0[0..4].copy_from_slice(&index);
                if let Some(auth_digest) = rejection_template.auth_digest_mut() {
                    auth_digest.0[0..4].copy_from_slice(&index);
                }

                rejection_template
            });

            for rejection in unique_ids {
                storage.reject(rejection, SameEffectsTipRejectionError::SpendConflict.into());
            }

            // Make sure there were no duplicates
            prop_assert_eq!(storage.rejected_transaction_count(), MAX_EVICTION_MEMORY_ENTRIES);

            // The transaction_to_reject will conflict with at least one of:
            // - transaction_to_accept, or
            // - a rejection from rejections
            prop_assert_eq!(
                storage.insert(transaction_to_reject, Vec::new(), None, None),
                Err(MempoolError::StorageEffectsTip(SameEffectsTipRejectionError::SpendConflict))
            );

            // Since we inserted more than MAX_EVICTION_MEMORY_ENTRIES,
            // the storage should have cleared the reject list
            prop_assert_eq!(storage.rejected_transaction_count(), 0);

            storage.clear();
        }
    }

    /// Test that the reject list length limits are applied when evicting transactions.
    #[test]
    fn reject_lists_are_limited_insert_eviction(
        transactions in vec(any::<VerifiedUnminedTx>(), MEMPOOL_TX_COUNT + 1).prop_map(SummaryDebug),
        mut rejection_template in any::<UnminedTxId>()
    ) {
        // Use as cost limit the costs of all transactions except one
        let cost_limit = transactions.iter().take(MEMPOOL_TX_COUNT).map(|tx| tx.cost()).sum();

        let mut storage: Storage = Storage::new(&Config {
            tx_cost_limit: cost_limit,
            eviction_memory_time: EVICTION_MEMORY_TIME,
            ..Default::default()
        });

        // Make unique IDs by converting the index to bytes, and writing it to each ID
        let unique_ids = (0..MAX_EVICTION_MEMORY_ENTRIES as u32).map(move |index| {
            let index = index.to_le_bytes();
            rejection_template.mined_id_mut().0[0..4].copy_from_slice(&index);
            if let Some(auth_digest) = rejection_template.auth_digest_mut() {
                auth_digest.0[0..4].copy_from_slice(&index);
            }

            rejection_template
        });

        for rejection in unique_ids {
            storage.reject(rejection, SameEffectsChainRejectionError::RandomlyEvicted.into());
        }

        // Make sure there were no duplicates
        prop_assert_eq!(storage.rejected_transaction_count(), MAX_EVICTION_MEMORY_ENTRIES);

        for (i, transaction) in transactions.iter().enumerate() {
            let tx_id = transaction.transaction.id;

            if i < transactions.len() - 1 {
                // The initial transactions should be successful
                prop_assert_eq!(
                    storage.insert(transaction.clone(), Vec::new(), None, None),
                    Ok(tx_id)
                );
            } else {
                // The final transaction will cause a random eviction,
                // which might return an error if this transaction is chosen
                let result = storage.insert(transaction.clone(), Vec::new(), None, None);

                if result.is_ok() {
                    prop_assert_eq!(
                        result,
                        Ok(tx_id)
                    );
                } else {
                    prop_assert_eq!(
                        result,
                        Err(MempoolError::StorageEffectsChain(SameEffectsChainRejectionError::RandomlyEvicted))
                    );
                }
            }
        }

        // Check if at least one transaction was evicted.
        // (More than one an be evicted to meet the limit.)
        prop_assert!(storage.transaction_count() <= MEMPOOL_TX_COUNT);

        // Since we inserted more than MAX_EVICTION_MEMORY_ENTRIES,
        // the storage should have removed the older entries and kept its size
        prop_assert_eq!(storage.rejected_transaction_count(), MAX_EVICTION_MEMORY_ENTRIES);
    }

    /// Test that the reject list length limits are applied when directly rejecting transactions.
    #[test]
    fn reject_lists_are_limited_reject(
        rejection_error in any::<RejectionError>(),
        mut rejection_template in any::<UnminedTxId>()
    ) {
        let mut storage: Storage = Storage::new(&Config {
            tx_cost_limit: 160_000_000,
            eviction_memory_time: EVICTION_MEMORY_TIME,
            ..Default::default()
        });

        // Make unique IDs by converting the index to bytes, and writing it to each ID
        let unique_ids = (0..(MAX_EVICTION_MEMORY_ENTRIES + 1) as u32).map(move |index| {
            let index = index.to_le_bytes();
            rejection_template.mined_id_mut().0[0..4].copy_from_slice(&index);
            if let Some(auth_digest) = rejection_template.auth_digest_mut() {
                auth_digest.0[0..4].copy_from_slice(&index);
            }

            rejection_template
        });

        for (index, rejection) in unique_ids.enumerate() {
            storage.reject(rejection, rejection_error.clone());

            if index == MAX_EVICTION_MEMORY_ENTRIES - 1 {
                // Make sure there were no duplicates
                prop_assert_eq!(storage.rejected_transaction_count(), MAX_EVICTION_MEMORY_ENTRIES);
            } else if index == MAX_EVICTION_MEMORY_ENTRIES {
                // Since we inserted more than MAX_EVICTION_MEMORY_ENTRIES,
                // all with the same error,
                // the storage should have either cleared the reject list
                // or removed the oldest ones, depending on the structure
                // used.
                match rejection_error {
                    RejectionError::ExactTip(_) |
                    RejectionError::SameEffectsTip(_) => {
                        prop_assert_eq!(storage.rejected_transaction_count(), 0);
                    },
                    RejectionError::SameEffectsChain(_) => {
                        prop_assert_eq!(storage.rejected_transaction_count(), MAX_EVICTION_MEMORY_ENTRIES);
                    },
                }
            }
        }
    }

    /// Test that the reject list length time limits are applied
    /// when directly rejecting transactions.
    #[test]
    fn reject_lists_are_time_pruned(
        mut rejection_template in any::<UnminedTxId>()
    ) {
        let mut storage = Storage::new(&Config {
            tx_cost_limit: 160_000_000,
            eviction_memory_time: Duration::from_millis(10),
            ..Default::default()
        });

        // Make unique IDs by converting the index to bytes, and writing it to each ID
        let unique_ids: Vec<UnminedTxId> = (0..2_u32).map(move |index| {
            let index = index.to_le_bytes();
            rejection_template.mined_id_mut().0[0..4].copy_from_slice(&index);
            if let Some(auth_digest) = rejection_template.auth_digest_mut() {
                auth_digest.0[0..4].copy_from_slice(&index);
            }

            rejection_template
        }).collect();

        storage.reject(unique_ids[0], SameEffectsChainRejectionError::RandomlyEvicted.into());
        thread::sleep(Duration::from_millis(11));
        storage.reject(unique_ids[1], SameEffectsChainRejectionError::RandomlyEvicted.into());

        prop_assert_eq!(storage.rejected_transaction_count(), 1);
    }
}

proptest! {
    /// Test if a transaction that has a spend conflict with a transaction already in the mempool
    /// is rejected.
    ///
    /// A spend conflict in this case is when two transactions spend the same UTXO or reveal the
    /// same nullifier.
    #[test]
    fn conflicting_transactions_are_rejected(input in any::<SpendConflictTestInput>()) {
        let mut storage: Storage = Storage::new(&Config {
            tx_cost_limit: 160_000_000,
            eviction_memory_time: EVICTION_MEMORY_TIME,
            ..Default::default()
        });

        let (first_transaction, second_transaction) = input.conflicting_transactions();
        let input_permutations = vec![
            (first_transaction.clone(), second_transaction.clone()),
            (second_transaction, first_transaction),
        ];

        for (transaction_to_accept, transaction_to_reject) in input_permutations {
            let id_to_accept = transaction_to_accept.transaction.id;
            let id_to_reject = transaction_to_reject.transaction.id;

            prop_assert_eq!(storage.insert(transaction_to_accept, Vec::new(), None, None), Ok(id_to_accept));

            prop_assert_eq!(
                storage.insert(transaction_to_reject, Vec::new(), None, None),
                Err(MempoolError::StorageEffectsTip(SameEffectsTipRejectionError::SpendConflict))
            );

            prop_assert!(storage.contains_rejected(&id_to_reject));

            storage.clear();
        }
    }

    /// Test if a rejected transaction is properly rolled back.
    ///
    /// When a transaction is rejected, it should not leave anything in the cache that could lead
    /// to false detection of spend conflicts.
    #[test]
    fn rejected_transactions_are_properly_rolled_back(input in any::<SpendConflictTestInput>())
    {
        let mut storage: Storage = Storage::new(&Config {
            tx_cost_limit: 160_000_000,
            eviction_memory_time: EVICTION_MEMORY_TIME,
            ..Default::default()
        });

        let (first_unconflicting_transaction, second_unconflicting_transaction) =
            input.clone().unconflicting_transactions();
        let (first_transaction, second_transaction) = input.conflicting_transactions();

        let input_permutations = vec![
            (
                first_transaction.clone(),
                second_transaction.clone(),
                second_unconflicting_transaction,
            ),
            (
                second_transaction,
                first_transaction,
                first_unconflicting_transaction,
            ),
        ];

        for (first_transaction_to_accept, transaction_to_reject, second_transaction_to_accept) in
            input_permutations
        {
            let first_id_to_accept = first_transaction_to_accept.transaction.id;
            let second_id_to_accept = second_transaction_to_accept.transaction.id;
            let id_to_reject = transaction_to_reject.transaction.id;

            prop_assert_eq!(
                storage.insert(first_transaction_to_accept, Vec::new(), None, None),
                Ok(first_id_to_accept)
            );

            prop_assert_eq!(
                storage.insert(transaction_to_reject, Vec::new(), None, None),
                Err(MempoolError::StorageEffectsTip(SameEffectsTipRejectionError::SpendConflict))
            );

            prop_assert!(storage.contains_rejected(&id_to_reject));

            prop_assert_eq!(
                storage.insert(second_transaction_to_accept, Vec::new(), None, None),
                Ok(second_id_to_accept)
            );

            storage.clear();
        }
    }

    /// Test if multiple transactions are properly removed.
    ///
    /// Attempting to remove multiple transactions must remove all of them and leave all of the
    /// others.
    #[test]
    fn removal_of_multiple_transactions(input in any::<MultipleTransactionRemovalTestInput>()) {
        let mut storage: Storage = Storage::new(&Config {
            tx_cost_limit: 160_000_000,
            eviction_memory_time: EVICTION_MEMORY_TIME,
            ..Default::default()
        });

        // Insert all input transactions, and keep track of the IDs of the one that were actually
        // inserted.
        let inserted_transactions: HashSet<_> = input
            .transactions()
            .filter_map(|transaction| {
                let id = transaction.transaction.id;

                storage.insert(transaction.clone(), Vec::new(), None, None).ok().map(|_| id)
            })
            .collect();

        // Check that the inserted transactions are still there.
        for transaction_id in &inserted_transactions {
            prop_assert!(storage.contains_transaction_exact(&transaction_id.mined_id()));
        }

        // Remove some transactions.
        match &input {
            RemoveExact { wtx_ids_to_remove, .. } => storage.remove_exact(wtx_ids_to_remove),
            RejectAndRemoveSameEffects { mined_ids_to_remove, .. } => {
                let num_removals = storage.reject_and_remove_same_effects(mined_ids_to_remove, vec![]);
                    for &removed_transaction_id in mined_ids_to_remove.iter() {
                        prop_assert_eq!(
                            storage.rejection_error(&UnminedTxId::Legacy(removed_transaction_id)),
                            Some(SameEffectsChainRejectionError::Mined.into())
                        );
                    }
                num_removals
            },
        };

        // Check that the removed transactions are not in the storage.
        let removed_transactions = input.removed_transaction_ids();

        for removed_transaction_id in &removed_transactions {
            prop_assert!(!storage.contains_transaction_exact(&removed_transaction_id.mined_id()));
        }

        // Check that the remaining transactions are still in the storage.
        let remaining_transactions = inserted_transactions.difference(&removed_transactions);

        for remaining_transaction_id in remaining_transactions {
            prop_assert!(storage.contains_transaction_exact(&remaining_transaction_id.mined_id()));
        }
    }
}

/// Test input consisting of two transactions and a conflict to be applied to them.
///
/// When the conflict is applied, both transactions will have a shared spend (either a UTXO used as
/// an input, or a nullifier revealed by both transactions).
#[derive(Arbitrary, Clone, Debug)]
enum SpendConflictTestInput {
    /// Test V4 transactions to include Sprout nullifier conflicts.
    V4 {
        #[proptest(
            strategy = "Transaction::v4_strategy(LedgerState::default()).prop_map(DisplayToDebug)"
        )]
        first: DisplayToDebug<Transaction>,

        #[proptest(
            strategy = "Transaction::v4_strategy(LedgerState::default()).prop_map(DisplayToDebug)"
        )]
        second: DisplayToDebug<Transaction>,

        conflict: SpendConflictForTransactionV4,
    },

    /// Test V5 transactions to include Orchard nullifier conflicts.
    V5 {
        #[proptest(
            strategy = "Transaction::v5_strategy(LedgerState::default()).prop_map(DisplayToDebug)"
        )]
        first: DisplayToDebug<Transaction>,

        #[proptest(
            strategy = "Transaction::v5_strategy(LedgerState::default()).prop_map(DisplayToDebug)"
        )]
        second: DisplayToDebug<Transaction>,

        conflict: SpendConflictForTransactionV5,
    },
}

impl SpendConflictTestInput {
    /// Return two transactions that have a spend conflict.
    pub fn conflicting_transactions(self) -> (VerifiedUnminedTx, VerifiedUnminedTx) {
        let (first, second) = match self {
            SpendConflictTestInput::V4 {
                mut first,
                mut second,
                conflict,
            } => {
                conflict.clone().apply_to(&mut first);
                conflict.apply_to(&mut second);

                (first, second)
            }
            SpendConflictTestInput::V5 {
                mut first,
                mut second,
                conflict,
            } => {
                conflict.clone().apply_to(&mut first);
                conflict.apply_to(&mut second);

                (first, second)
            }
        };

        (
            VerifiedUnminedTx::new(
                first.0.into(),
                // make sure miner fee is big enough for all cases
                Amount::try_from(1_000_000).expect("valid amount"),
                0,
            )
            .expect("verification should pass"),
            VerifiedUnminedTx::new(
                second.0.into(),
                // make sure miner fee is big enough for all cases
                Amount::try_from(1_000_000).expect("valid amount"),
                0,
            )
            .expect("verification should pass"),
        )
    }

    /// Return two transactions that have no spend conflicts.
    pub fn unconflicting_transactions(self) -> (VerifiedUnminedTx, VerifiedUnminedTx) {
        let (mut first, mut second) = match self {
            SpendConflictTestInput::V4 { first, second, .. } => (first, second),
            SpendConflictTestInput::V5 { first, second, .. } => (first, second),
        };

        Self::remove_transparent_conflicts(&mut first, &mut second);
        Self::remove_sprout_conflicts(&mut first, &mut second);
        Self::remove_sapling_conflicts(&mut first, &mut second);
        Self::remove_orchard_conflicts(&mut first, &mut second);

        (
            VerifiedUnminedTx::new(
                first.0.into(),
                // make sure miner fee is big enough for all cases
                Amount::try_from(1_000_000).expect("valid amount"),
                0,
            )
            .expect("verification should pass"),
            VerifiedUnminedTx::new(
                second.0.into(),
                // make sure miner fee is big enough for all cases
                Amount::try_from(1_000_000).expect("valid amount"),
                0,
            )
            .expect("verification should pass"),
        )
    }

    /// Find transparent outpoint spends shared by two transactions, then remove them from the
    /// transactions.
    fn remove_transparent_conflicts(first: &mut Transaction, second: &mut Transaction) {
        let first_spent_outpoints: HashSet<_> = first.spent_outpoints().collect();
        let second_spent_outpoints: HashSet<_> = second.spent_outpoints().collect();

        let conflicts: HashSet<_> = first_spent_outpoints
            .intersection(&second_spent_outpoints)
            .collect();

        for transaction in [first, second] {
            transaction.inputs_mut().retain(|input| {
                input
                    .outpoint()
                    .as_ref()
                    .map(|outpoint| !conflicts.contains(outpoint))
                    .unwrap_or(true)
            });
        }
    }

    /// Find identical Sprout nullifiers revealed by both transactions, then remove the joinsplits
    /// that contain them from both transactions.
    fn remove_sprout_conflicts(first: &mut Transaction, second: &mut Transaction) {
        let first_nullifiers: HashSet<_> = first.sprout_nullifiers().copied().collect();
        let second_nullifiers: HashSet<_> = second.sprout_nullifiers().copied().collect();

        let conflicts: HashSet<_> = first_nullifiers
            .intersection(&second_nullifiers)
            .copied()
            .collect();

        for transaction in [first, second] {
            match transaction {
                // JoinSplits with Bctv14 Proofs
                Transaction::V2 { joinsplit_data, .. } | Transaction::V3 { joinsplit_data, .. } => {
                    Self::remove_joinsplits_with_conflicts(joinsplit_data, &conflicts)
                }

                // JoinSplits with Groth Proofs
                Transaction::V4 { joinsplit_data, .. } => {
                    Self::remove_joinsplits_with_conflicts(joinsplit_data, &conflicts)
                }

                // No JoinSplits
                Transaction::V1 { .. } | Transaction::V5 { .. } => {}
                #[cfg(feature = "tx_v6")]
                Transaction::V6 { .. } => {}
            }
        }
    }

    /// Remove from a transaction's [`JoinSplitData`] the joinsplits that contain nullifiers
    /// present in the `conflicts` set.
    ///
    /// This may clear the entire Sprout joinsplit data.
    fn remove_joinsplits_with_conflicts<P: ZkSnarkProof>(
        maybe_joinsplit_data: &mut Option<JoinSplitData<P>>,
        conflicts: &HashSet<sprout::Nullifier>,
    ) {
        let mut should_clear_joinsplit_data = false;

        if let Some(joinsplit_data) = maybe_joinsplit_data.as_mut() {
            joinsplit_data.rest.retain(|joinsplit| {
                !joinsplit
                    .nullifiers
                    .iter()
                    .any(|nullifier| conflicts.contains(nullifier))
            });

            let first_joinsplit_should_be_removed = joinsplit_data
                .first
                .nullifiers
                .iter()
                .any(|nullifier| conflicts.contains(nullifier));

            if first_joinsplit_should_be_removed {
                if joinsplit_data.rest.is_empty() {
                    should_clear_joinsplit_data = true;
                } else {
                    joinsplit_data.first = joinsplit_data.rest.remove(0);
                }
            }
        }

        if should_clear_joinsplit_data {
            *maybe_joinsplit_data = None;
        }
    }

    /// Find identical Sapling nullifiers revealed by both transactions, then remove the spends
    /// that contain them from both transactions.
    fn remove_sapling_conflicts(first: &mut Transaction, second: &mut Transaction) {
        let first_nullifiers: HashSet<_> = first.sapling_nullifiers().copied().collect();
        let second_nullifiers: HashSet<_> = second.sapling_nullifiers().copied().collect();

        let conflicts: HashSet<_> = first_nullifiers
            .intersection(&second_nullifiers)
            .copied()
            .collect();

        for transaction in [first, second] {
            match transaction {
                // Spends with Groth Proofs
                Transaction::V4 {
                    sapling_shielded_data,
                    ..
                } => {
                    Self::remove_sapling_transfers_with_conflicts(sapling_shielded_data, &conflicts)
                }

                Transaction::V5 {
                    sapling_shielded_data,
                    ..
                } => {
                    Self::remove_sapling_transfers_with_conflicts(sapling_shielded_data, &conflicts)
                }

                #[cfg(feature = "tx_v6")]
                Transaction::V6 {
                    sapling_shielded_data,
                    ..
                } => {
                    Self::remove_sapling_transfers_with_conflicts(sapling_shielded_data, &conflicts)
                }

                // No Spends
                Transaction::V1 { .. } | Transaction::V2 { .. } | Transaction::V3 { .. } => {}
            }
        }
    }

    /// Remove from a transaction's [`sapling::ShieldedData`] the spends that contain nullifiers
    /// present in the `conflicts` set.
    ///
    /// This may clear the entire shielded data.
    fn remove_sapling_transfers_with_conflicts<A>(
        maybe_shielded_data: &mut Option<sapling::ShieldedData<A>>,
        conflicts: &HashSet<sapling::Nullifier>,
    ) where
        A: sapling::AnchorVariant + Clone,
    {
        if let Some(shielded_data) = maybe_shielded_data.take() {
            match shielded_data.transfers {
                sapling::TransferData::JustOutputs { .. } => {
                    *maybe_shielded_data = Some(shielded_data)
                }

                sapling::TransferData::SpendsAndMaybeOutputs {
                    shared_anchor,
                    spends,
                    maybe_outputs,
                } => {
                    let updated_spends: Vec<_> = spends
                        .into_vec()
                        .into_iter()
                        .filter(|spend| !conflicts.contains(&spend.nullifier))
                        .collect();

                    if let Ok(spends) = AtLeastOne::try_from(updated_spends) {
                        *maybe_shielded_data = Some(sapling::ShieldedData {
                            transfers: sapling::TransferData::SpendsAndMaybeOutputs {
                                shared_anchor,
                                spends,
                                maybe_outputs,
                            },
                            ..shielded_data
                        });
                    } else if let Ok(outputs) = AtLeastOne::try_from(maybe_outputs) {
                        *maybe_shielded_data = Some(sapling::ShieldedData {
                            transfers: sapling::TransferData::JustOutputs { outputs },
                            ..shielded_data
                        });
                    }
                }
            }
        }
    }

    /// Find identical Orchard nullifiers revealed by both transactions, then remove the actions
    /// that contain them from both transactions.
    fn remove_orchard_conflicts(first: &mut Transaction, second: &mut Transaction) {
        let first_nullifiers: HashSet<_> = first.orchard_nullifiers().copied().collect();
        let second_nullifiers: HashSet<_> = second.orchard_nullifiers().copied().collect();

        let conflicts: HashSet<_> = first_nullifiers
            .intersection(&second_nullifiers)
            .copied()
            .collect();

        for transaction in [first, second] {
            match transaction {
                Transaction::V5 {
                    orchard_shielded_data,
                    ..
                } => Self::remove_orchard_actions_with_conflicts(orchard_shielded_data, &conflicts),

                #[cfg(feature = "tx_v6")]
                Transaction::V6 {
                    orchard_shielded_data,
                    ..
                } => Self::remove_orchard_actions_with_conflicts(orchard_shielded_data, &conflicts),

                // No Spends
                Transaction::V1 { .. }
                | Transaction::V2 { .. }
                | Transaction::V3 { .. }
                | Transaction::V4 { .. } => {}
            }
        }
    }

    /// Remove from a transaction's [`orchard::ShieldedData`] the actions that contain nullifiers
    /// present in the `conflicts` set.
    ///
    /// This may clear the entire shielded data.
    fn remove_orchard_actions_with_conflicts(
        maybe_shielded_data: &mut Option<orchard::ShieldedData>,
        conflicts: &HashSet<orchard::Nullifier>,
    ) {
        if let Some(shielded_data) = maybe_shielded_data.take() {
            let updated_actions: Vec<_> = shielded_data
                .actions
                .into_vec()
                .into_iter()
                .filter(|action| !conflicts.contains(&action.action.nullifier))
                .collect();

            if let Ok(actions) = AtLeastOne::try_from(updated_actions) {
                *maybe_shielded_data = Some(orchard::ShieldedData {
                    actions,
                    ..shielded_data
                });
            }
        }
    }
}

/// A spend conflict valid for V4 transactions.
#[derive(Arbitrary, Clone, Debug)]
enum SpendConflictForTransactionV4 {
    Transparent(Box<TransparentSpendConflict>),
    Sprout(Box<SproutSpendConflict>),
    Sapling(Box<SaplingSpendConflict<sapling::PerSpendAnchor>>),
}

/// A spend conflict valid for V5 transactions.
#[derive(Arbitrary, Clone, Debug)]
enum SpendConflictForTransactionV5 {
    Transparent(Box<TransparentSpendConflict>),
    Sapling(Box<SaplingSpendConflict<sapling::SharedAnchor>>),
    Orchard(Box<OrchardSpendConflict>),
}

/// A conflict caused by spending the same UTXO.
#[derive(Arbitrary, Clone, Debug)]
struct TransparentSpendConflict {
    new_input: DisplayToDebug<transparent::Input>,
}

/// A conflict caused by revealing the same Sprout nullifier.
#[derive(Arbitrary, Clone, Debug)]
struct SproutSpendConflict {
    new_joinsplit_data: DisplayToDebug<transaction::JoinSplitData<Groth16Proof>>,
}

/// A conflict caused by revealing the same Sapling nullifier.
#[derive(Clone, Debug)]
struct SaplingSpendConflict<A: sapling::AnchorVariant + Clone> {
    new_spend: DisplayToDebug<sapling::Spend<A>>,
    new_shared_anchor: A::Shared,
    fallback_shielded_data: DisplayToDebug<sapling::ShieldedData<A>>,
}

/// A conflict caused by revealing the same Orchard nullifier.
#[derive(Arbitrary, Clone, Debug)]
struct OrchardSpendConflict {
    new_shielded_data: DisplayToDebug<orchard::ShieldedData>,
}

impl SpendConflictForTransactionV4 {
    /// Apply a spend conflict to a V4 transaction.
    ///
    /// Changes the `transaction_v4` to include the spend that will result in a conflict.
    pub fn apply_to(self, transaction_v4: &mut Transaction) {
        let (inputs, joinsplit_data, sapling_shielded_data) = match transaction_v4 {
            Transaction::V4 {
                inputs,
                joinsplit_data,
                sapling_shielded_data,
                ..
            } => (inputs, joinsplit_data, sapling_shielded_data),
            _ => unreachable!("incorrect transaction version generated for test"),
        };

        use SpendConflictForTransactionV4::*;
        match self {
            Transparent(transparent_conflict) => transparent_conflict.apply_to(inputs),
            Sprout(sprout_conflict) => sprout_conflict.apply_to(joinsplit_data),
            Sapling(sapling_conflict) => sapling_conflict.apply_to(sapling_shielded_data),
        }
    }
}

impl SpendConflictForTransactionV5 {
    /// Apply a spend conflict to a V5 transaction.
    ///
    /// Changes the `transaction_v5` to include the spend that will result in a conflict.
    pub fn apply_to(self, transaction_v5: &mut Transaction) {
        let (inputs, sapling_shielded_data, orchard_shielded_data) = match transaction_v5 {
            Transaction::V5 {
                inputs,
                sapling_shielded_data,
                orchard_shielded_data,
                ..
            } => (inputs, sapling_shielded_data, orchard_shielded_data),
            _ => unreachable!("incorrect transaction version generated for test"),
        };

        use SpendConflictForTransactionV5::*;
        match self {
            Transparent(transparent_conflict) => transparent_conflict.apply_to(inputs),
            Sapling(sapling_conflict) => sapling_conflict.apply_to(sapling_shielded_data),
            Orchard(orchard_conflict) => orchard_conflict.apply_to(orchard_shielded_data),
        }
    }
}

impl TransparentSpendConflict {
    /// Apply a transparent spend conflict.
    ///
    /// Adds a new input to a transaction's list of transparent `inputs`. The transaction will then
    /// conflict with any other transaction that also has that same new input.
    pub fn apply_to(self, inputs: &mut Vec<transparent::Input>) {
        inputs.push(self.new_input.0);
    }
}

impl SproutSpendConflict {
    /// Apply a Sprout spend conflict.
    ///
    /// Ensures that a transaction's `joinsplit_data` has a nullifier used to represent a conflict.
    /// If the transaction already has Sprout joinsplits, the first nullifier is replaced with the
    /// new nullifier. Otherwise, a joinsplit is inserted with that new nullifier in the
    /// transaction.
    ///
    /// The transaction will then conflict with any other transaction with the same new nullifier.
    pub fn apply_to(self, joinsplit_data: &mut Option<transaction::JoinSplitData<Groth16Proof>>) {
        if let Some(existing_joinsplit_data) = joinsplit_data.as_mut() {
            existing_joinsplit_data.first.nullifiers[0] =
                self.new_joinsplit_data.first.nullifiers[0];
        } else {
            *joinsplit_data = Some(self.new_joinsplit_data.0);
        }
    }
}

/// Generate arbitrary [`SaplingSpendConflict`]s.
///
/// This had to be implemented manually because of the constraints required as a consequence of the
/// generic type parameter.
impl<A> Arbitrary for SaplingSpendConflict<A>
where
    A: sapling::AnchorVariant + Clone + Debug + 'static,
    A::Shared: Arbitrary,
    sapling::Spend<A>: Arbitrary,
    sapling::TransferData<A>: Arbitrary,
{
    type Parameters = ();

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<(sapling::Spend<A>, A::Shared, sapling::ShieldedData<A>)>()
            .prop_map(|(new_spend, new_shared_anchor, fallback_shielded_data)| {
                SaplingSpendConflict {
                    new_spend: new_spend.into(),
                    new_shared_anchor,
                    fallback_shielded_data: fallback_shielded_data.into(),
                }
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl<A: sapling::AnchorVariant + Clone> SaplingSpendConflict<A> {
    /// Apply a Sapling spend conflict.
    ///
    /// Ensures that a transaction's `sapling_shielded_data` has a nullifier used to represent a
    /// conflict. If the transaction already has Sapling shielded data, a new spend is added with
    /// the new nullifier. Otherwise, a fallback instance of Sapling shielded data is inserted in
    /// the transaction, and then the spend is added.
    ///
    /// The transaction will then conflict with any other transaction with the same new nullifier.
    pub fn apply_to(self, sapling_shielded_data: &mut Option<sapling::ShieldedData<A>>) {
        use sapling::TransferData::*;

        let shielded_data = sapling_shielded_data.get_or_insert(self.fallback_shielded_data.0);

        match &mut shielded_data.transfers {
            SpendsAndMaybeOutputs { ref mut spends, .. } => spends.push(self.new_spend.0),
            JustOutputs { ref mut outputs } => {
                let new_outputs = outputs.clone();

                shielded_data.transfers = SpendsAndMaybeOutputs {
                    shared_anchor: self.new_shared_anchor,
                    spends: at_least_one![self.new_spend.0],
                    maybe_outputs: new_outputs.into_vec(),
                };
            }
        }
    }
}

impl OrchardSpendConflict {
    /// Apply a Orchard spend conflict.
    ///
    /// Ensures that a transaction's `orchard_shielded_data` has a nullifier used to represent a
    /// conflict. If the transaction already has Orchard shielded data, a new action is added with
    /// the new nullifier. Otherwise, a fallback instance of Orchard shielded data that contains
    /// the new action is inserted in the transaction.
    ///
    /// The transaction will then conflict with any other transaction with the same new nullifier.
    pub fn apply_to(self, orchard_shielded_data: &mut Option<orchard::ShieldedData>) {
        if let Some(shielded_data) = orchard_shielded_data.as_mut() {
            shielded_data.actions.first_mut().action.nullifier =
                self.new_shielded_data.actions.first().action.nullifier;
        } else {
            *orchard_shielded_data = Some(self.new_shielded_data.0);
        }
    }
}

/// A series of transactions and a sub-set of them to remove.
///
/// The set of transactions to remove can either be exact WTX IDs to remove exact transactions or
/// mined IDs to remove transactions that have the same effects specified by the ID.
#[derive(Clone, Debug)]
pub enum MultipleTransactionRemovalTestInput {
    RemoveExact {
        transactions: SummaryDebug<Vec<VerifiedUnminedTx>>,
        wtx_ids_to_remove: SummaryDebug<HashSet<UnminedTxId>>,
    },

    RejectAndRemoveSameEffects {
        transactions: SummaryDebug<Vec<VerifiedUnminedTx>>,
        mined_ids_to_remove: SummaryDebug<HashSet<transaction::Hash>>,
    },
}

impl Arbitrary for MultipleTransactionRemovalTestInput {
    type Parameters = ();

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        vec(any::<VerifiedUnminedTx>(), 1..MEMPOOL_TX_COUNT)
            .prop_flat_map(|transactions| {
                let indices_to_remove =
                    vec(any::<bool>(), 1..=transactions.len()).prop_map(|removal_markers| {
                        removal_markers
                            .into_iter()
                            .enumerate()
                            .filter(|(_, should_remove)| *should_remove)
                            .map(|(index, _)| index)
                            .collect::<HashSet<usize>>()
                    });

                (Just(transactions), indices_to_remove)
            })
            .prop_flat_map(|(transactions, indices_to_remove)| {
                let wtx_ids_to_remove: HashSet<_> = indices_to_remove
                    .iter()
                    .map(|&index| transactions[index].transaction.id)
                    .collect();

                let mined_ids_to_remove: HashSet<transaction::Hash> = wtx_ids_to_remove
                    .iter()
                    .map(|unmined_id| unmined_id.mined_id())
                    .collect();

                prop_oneof![
                    Just(RejectAndRemoveSameEffects {
                        transactions: transactions.clone().into(),
                        mined_ids_to_remove: mined_ids_to_remove.into(),
                    }),
                    Just(RemoveExact {
                        transactions: transactions.into(),
                        wtx_ids_to_remove: wtx_ids_to_remove.into(),
                    }),
                ]
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl MultipleTransactionRemovalTestInput {
    /// Iterate over all transactions generated as input.
    pub fn transactions(&self) -> impl Iterator<Item = &VerifiedUnminedTx> + '_ {
        match self {
            RemoveExact { transactions, .. } | RejectAndRemoveSameEffects { transactions, .. } => {
                transactions.iter()
            }
        }
    }

    /// Return a [`HashSet`] of [`UnminedTxId`]s of the transactions to be removed.
    pub fn removed_transaction_ids(&self) -> HashSet<UnminedTxId> {
        match self {
            RemoveExact {
                wtx_ids_to_remove, ..
            } => wtx_ids_to_remove.0.clone(),

            RejectAndRemoveSameEffects {
                transactions,
                mined_ids_to_remove,
            } => transactions
                .iter()
                .map(|transaction| transaction.transaction.id)
                .filter(|id| mined_ids_to_remove.contains(&id.mined_id()))
                .collect(),
        }
    }
}

proptest! {

    // Some tests need to sleep which makes tests slow, so use a single
    // case. We also don't need multiple cases since proptest is just used
    // to generate random TXIDs.
    #![proptest_config(
        proptest::test_runner::Config::with_cases(1)
    )]

    /// Check if EvictionList limits the number of entries.
    #[test]
    fn eviction_list_size(
        mut rejection_template in any::<UnminedTxId>()
    ) {
        // Make unique IDs by converting the index to bytes, and writing it to each ID
        let txids: Vec<UnminedTxId> = (0..4_u32).map(move |index| {
            let index = index.to_le_bytes();
            rejection_template.mined_id_mut().0[0..4].copy_from_slice(&index);
            if let Some(auth_digest) = rejection_template.auth_digest_mut() {
                auth_digest.0[0..4].copy_from_slice(&index);
            }

            rejection_template
        }).collect();

        let mut e = EvictionList::new(2, EVICTION_MEMORY_TIME);
        for txid in txids.iter() {
            e.insert(txid.mined_id());
        }
        prop_assert!(!e.contains_key(&txids[0].mined_id()));
        prop_assert!(!e.contains_key(&txids[1].mined_id()));
        prop_assert!(e.contains_key(&txids[2].mined_id()));
        prop_assert!(e.contains_key(&txids[3].mined_id()));
        prop_assert_eq!(e.len(), 2);
    }

    /// Check if EvictionList removes old entries.
    #[test]
    fn eviction_list_time(
        mut rejection_template in any::<UnminedTxId>()
    ) {
        // Make unique IDs by converting the index to bytes, and writing it to each ID
        let txids: Vec<UnminedTxId> = (0..2_u32).map(move |index| {
            let index = index.to_le_bytes();
            rejection_template.mined_id_mut().0[0..4].copy_from_slice(&index);
            if let Some(auth_digest) = rejection_template.auth_digest_mut() {
                auth_digest.0[0..4].copy_from_slice(&index);
            }

            rejection_template
        }).collect();

        let mut e = EvictionList::new(2, Duration::from_millis(10));
        e.insert(txids[0].mined_id());
        thread::sleep(Duration::from_millis(11));

        // First txid has expired, but list wasn't pruned yet.
        // Make sure len() and contains_key() take that into account.
        prop_assert!(!e.contains_key(&txids[0].mined_id()));
        prop_assert_eq!(e.len(), 0);

        e.insert(txids[1].mined_id());
        prop_assert!(!e.contains_key(&txids[0].mined_id()));
        prop_assert!(e.contains_key(&txids[1].mined_id()));
        prop_assert_eq!(e.len(), 1);
    }

    /// Check if EvictionList removes old entries and computes length correctly
    /// when the list becomes mixed with expired and non-expired entries.
    #[test]
    fn eviction_list_time_mixed(
        mut rejection_template in any::<UnminedTxId>()
    ) {
        // Eviction time (in ms) used in this test. If the value is too low it may cause
        // this test to fail in slower systems.
        const EVICTION_TIME: u64 = 500;
        // Time to wait (in ms) before adding transactions that should not expire
        // after EVICTION_TIME.
        // If should be a bit smaller than EVICTION_TIME, but with enough time to
        // add 10 transactions to the list and still have time to spare before EVICTION_TIME.
        const BEFORE_EVICTION_TIME: u64 = 250;

        // Make unique IDs by converting the index to bytes, and writing it to each ID
        let txids: Vec<UnminedTxId> = (0..20_u32).map(move |index| {
            let index = index.to_le_bytes();
            rejection_template.mined_id_mut().0[0..4].copy_from_slice(&index);
            if let Some(auth_digest) = rejection_template.auth_digest_mut() {
                auth_digest.0[0..4].copy_from_slice(&index);
            }

            rejection_template
        }).collect();

        let mut e = EvictionList::new(20, Duration::from_millis(EVICTION_TIME));
        for txid in txids.iter().take(10) {
            e.insert(txid.mined_id());
        }
        thread::sleep(Duration::from_millis(BEFORE_EVICTION_TIME));
        // Add the next 10 before the eviction time to avoid a prune from
        // happening.
        for txid in txids.iter().skip(10) {
            e.insert(txid.mined_id());
        }
        thread::sleep(Duration::from_millis(EVICTION_TIME - BEFORE_EVICTION_TIME + 1));

        // At this point, the first 10 entries should be expired
        // and the next 10 should not, and the list hasn't been pruned yet.
        // Make sure len() and contains_key() take that into account.
        // Note: if one of these fails, you may need to adjust EVICTION_TIME and/or
        // BEFORE_EVICTION_TIME, see above.
        for txid in txids.iter().take(10) {
            prop_assert!(!e.contains_key(&txid.mined_id()));
        }
        for txid in txids.iter().skip(10) {
            prop_assert!(e.contains_key(&txid.mined_id()));
        }
        prop_assert_eq!(e.len(), 10);

        // Make sure all of them are expired
        thread::sleep(Duration::from_millis(EVICTION_TIME + 1));

        for txid in txids.iter() {
            prop_assert!(!e.contains_key(&txid.mined_id()));
        }
        prop_assert_eq!(e.len(), 0);
    }

    /// Check if EvictionList panics if entries are added multiple times.
    #[test]
    #[should_panic]
    fn eviction_list_refresh(
        txid in any::<UnminedTxId>()
    ) {
        let mut e = EvictionList::new(2, EVICTION_MEMORY_TIME);
        e.insert(txid.mined_id());
        e.insert(txid.mined_id());
    }
}
