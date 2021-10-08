use std::{collections::HashSet, convert::TryFrom, fmt::Debug};

use proptest::prelude::*;
use proptest_derive::Arbitrary;

use zebra_chain::{
    at_least_one, orchard,
    primitives::{Groth16Proof, ZkSnarkProof},
    sapling,
    serialization::AtLeastOne,
    sprout,
    transaction::{self, JoinSplitData, Transaction, UnminedTx},
    transparent, LedgerState,
};

use crate::components::mempool::storage::SameEffectsTipRejectionError;

use super::super::{MempoolError, Storage};

proptest! {
    /// Test if a transaction that has a spend conflict with a transaction already in the mempool
    /// is rejected.
    ///
    /// A spend conflict in this case is when two transactions spend the same UTXO or reveal the
    /// same nullifier.
    #[test]
    fn conflicting_transactions_are_rejected(input in any::<SpendConflictTestInput>()) {
        let mut storage = Storage::default();

        let (first_transaction, second_transaction) = input.conflicting_transactions();
        let input_permutations = vec![
            (first_transaction.clone(), second_transaction.clone()),
            (second_transaction, first_transaction),
        ];

        for (transaction_to_accept, transaction_to_reject) in input_permutations {
            let id_to_accept = transaction_to_accept.id;
            let id_to_reject = transaction_to_reject.id;

            assert_eq!(storage.insert(transaction_to_accept), Ok(id_to_accept));

            assert_eq!(
                storage.insert(transaction_to_reject),
                Err(MempoolError::StorageEffectsTip(SameEffectsTipRejectionError::SpendConflict))
            );

            assert!(storage.contains_rejected(&id_to_reject));

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
        let mut storage = Storage::default();

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
            let first_id_to_accept = first_transaction_to_accept.id;
            let second_id_to_accept = second_transaction_to_accept.id;
            let id_to_reject = transaction_to_reject.id;

            assert_eq!(
                storage.insert(first_transaction_to_accept),
                Ok(first_id_to_accept)
            );

            assert_eq!(
                storage.insert(transaction_to_reject),
                Err(MempoolError::StorageEffectsTip(SameEffectsTipRejectionError::SpendConflict))
            );

            assert!(storage.contains_rejected(&id_to_reject));

            assert_eq!(
                storage.insert(second_transaction_to_accept),
                Ok(second_id_to_accept)
            );

            storage.clear();
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
        #[proptest(strategy = "Transaction::v4_strategy(LedgerState::default())")]
        first: Transaction,

        #[proptest(strategy = "Transaction::v4_strategy(LedgerState::default())")]
        second: Transaction,

        conflict: SpendConflictForTransactionV4,
    },

    /// Test V5 transactions to include Orchard nullifier conflicts.
    V5 {
        #[proptest(strategy = "Transaction::v5_strategy(LedgerState::default())")]
        first: Transaction,

        #[proptest(strategy = "Transaction::v5_strategy(LedgerState::default())")]
        second: Transaction,

        conflict: SpendConflictForTransactionV5,
    },
}

impl SpendConflictTestInput {
    /// Return two transactions that have a spend conflict.
    pub fn conflicting_transactions(self) -> (UnminedTx, UnminedTx) {
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

        (first.into(), second.into())
    }

    /// Return two transactions that have no spend conflicts.
    pub fn unconflicting_transactions(self) -> (UnminedTx, UnminedTx) {
        let (mut first, mut second) = match self {
            SpendConflictTestInput::V4 { first, second, .. } => (first, second),
            SpendConflictTestInput::V5 { first, second, .. } => (first, second),
        };

        Self::remove_transparent_conflicts(&mut first, &mut second);
        Self::remove_sprout_conflicts(&mut first, &mut second);
        Self::remove_sapling_conflicts(&mut first, &mut second);
        Self::remove_orchard_conflicts(&mut first, &mut second);

        (first.into(), second.into())
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
    new_input: transparent::Input,
}

/// A conflict caused by revealing the same Sprout nullifier.
#[derive(Arbitrary, Clone, Debug)]
struct SproutSpendConflict {
    new_joinsplit_data: transaction::JoinSplitData<Groth16Proof>,
}

/// A conflict caused by revealing the same Sapling nullifier.
#[derive(Clone, Debug)]
struct SaplingSpendConflict<A: sapling::AnchorVariant + Clone> {
    new_spend: sapling::Spend<A>,
    new_shared_anchor: A::Shared,
    fallback_shielded_data: sapling::ShieldedData<A>,
}

/// A conflict caused by revealing the same Orchard nullifier.
#[derive(Arbitrary, Clone, Debug)]
struct OrchardSpendConflict {
    new_shielded_data: orchard::ShieldedData,
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
        inputs.push(self.new_input);
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
            *joinsplit_data = Some(self.new_joinsplit_data);
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
                    new_spend,
                    new_shared_anchor,
                    fallback_shielded_data,
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

        let shielded_data = sapling_shielded_data.get_or_insert(self.fallback_shielded_data);

        match &mut shielded_data.transfers {
            SpendsAndMaybeOutputs { ref mut spends, .. } => spends.push(self.new_spend),
            JustOutputs { ref mut outputs } => {
                let new_outputs = outputs.clone();

                shielded_data.transfers = SpendsAndMaybeOutputs {
                    shared_anchor: self.new_shared_anchor,
                    spends: at_least_one![self.new_spend],
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
            *orchard_shielded_data = Some(self.new_shielded_data);
        }
    }
}
