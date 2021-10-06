use std::fmt::Debug;

use proptest::prelude::*;
use proptest_derive::Arbitrary;

use zebra_chain::{
    at_least_one, orchard,
    primitives::Groth16Proof,
    sapling,
    transaction::{self, Transaction, UnminedTx},
    transparent, LedgerState,
};

use crate::components::mempool::storage::SameEffectsRejectionError;

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

            assert_eq!(
                storage.insert(transaction_to_accept),
                Ok(id_to_accept)
            );

            assert_eq!(
                storage.insert(transaction_to_reject),
                Err(MempoolError::StorageEffects(SameEffectsRejectionError::SpendConflict))
            );

            assert!(storage.contains_rejected_exact(&id_to_reject));

            storage.clear();
        }
    }
}

/// Test input consisting of two transactions and a conflict to be applied to them.
///
/// When the conflict is applied, both transactions will have a shared spend (either a UTXO used as
/// an input, or a nullifier revealed by both transactions).
#[derive(Arbitrary, Debug)]
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
