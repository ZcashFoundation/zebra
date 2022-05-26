use std::{
    borrow::Cow,
    collections::{HashSet, VecDeque},
    hash::Hash,
};

use zebra_chain::{
    orchard, sapling, sprout,
    transaction::{Transaction, UnminedTx, UnminedTxId, VerifiedUnminedTx},
    transparent,
};

use super::super::SameEffectsTipRejectionError;

/// The set of verified transactions stored in the mempool.
///
/// This also caches the all the spent outputs from the transactions in the mempool. The spent
/// outputs include:
///
/// - the transparent outpoints spent by transactions in the mempool
/// - the Sprout nullifiers revealed by transactions in the mempool
/// - the Sapling nullifiers revealed by transactions in the mempool
/// - the Orchard nullifiers revealed by transactions in the mempool
#[derive(Default)]
pub struct VerifiedSet {
    /// The set of verified transactions in the mempool.
    transactions: VecDeque<VerifiedUnminedTx>,

    /// The total size of the transactions in the mempool if they were
    /// serialized.
    transactions_serialized_size: usize,

    /// The total cost of the verified transactons in the set.
    total_cost: u64,

    /// The set of spent out points by the verified transactions.
    spent_outpoints: HashSet<transparent::OutPoint>,

    /// The set of revealed Sprout nullifiers.
    sprout_nullifiers: HashSet<sprout::Nullifier>,

    /// The set of revealed Sapling nullifiers.
    sapling_nullifiers: HashSet<sapling::Nullifier>,

    /// The set of revealed Orchard nullifiers.
    orchard_nullifiers: HashSet<orchard::Nullifier>,
}

impl Drop for VerifiedSet {
    fn drop(&mut self) {
        // zero the metrics on drop
        self.clear()
    }
}

impl VerifiedSet {
    /// Returns an iterator over the transactions in the set.
    pub fn transactions(&self) -> impl Iterator<Item = &UnminedTx> + '_ {
        self.transactions.iter().map(|tx| &tx.transaction)
    }

    /// Returns the number of verified transactions in the set.
    pub fn transaction_count(&self) -> usize {
        self.transactions.len()
    }

    /// Returns the total cost of the verified transactions in the set.
    ///
    /// [ZIP-401]: https://zips.z.cash/zip-0401
    pub fn total_cost(&self) -> u64 {
        self.total_cost
    }

    /// Returns `true` if the set of verified transactions contains the transaction with the
    /// specified `id.
    pub fn contains(&self, id: &UnminedTxId) -> bool {
        self.transactions.iter().any(|tx| &tx.transaction.id == id)
    }

    /// Clear the set of verified transactions.
    ///
    /// Also clears all internal caches.
    pub fn clear(&mut self) {
        self.transactions.clear();
        self.spent_outpoints.clear();
        self.sprout_nullifiers.clear();
        self.sapling_nullifiers.clear();
        self.orchard_nullifiers.clear();
        self.transactions_serialized_size = 0;
        self.total_cost = 0;
        self.update_metrics();
    }

    /// Insert a `transaction` into the set.
    ///
    /// Returns an error if the `transaction` has spend conflicts with any other transaction
    /// already in the set.
    ///
    /// Two transactions have a spend conflict if they spend the same UTXO or if they reveal the
    /// same nullifier.
    pub fn insert(
        &mut self,
        transaction: VerifiedUnminedTx,
    ) -> Result<(), SameEffectsTipRejectionError> {
        if self.has_spend_conflicts(&transaction.transaction) {
            return Err(SameEffectsTipRejectionError::SpendConflict);
        }

        self.cache_outputs_from(&transaction.transaction.transaction);
        self.transactions_serialized_size += transaction.transaction.size;
        self.total_cost += transaction.cost();
        self.transactions.push_front(transaction);

        self.update_metrics();

        Ok(())
    }

    /// Evict one transaction from the set, returns the victim transaction.
    ///
    /// Removes a transaction with probability in direct proportion to the
    /// eviction weight, as per [ZIP-401].
    ///
    /// Consensus rule:
    ///
    /// > Each transaction also has an eviction weight, which is cost +
    /// > low_fee_penalty, where low_fee_penalty is 16000 if the transaction pays
    /// > a fee less than the conventional fee, otherwise 0. The conventional fee
    /// > is currently defined as 1000 zatoshis
    ///
    /// # Note
    ///
    /// Collecting and calculating weights is O(n). But in practice n is limited
    /// to 20,000 (mempooltxcostlimit/min(cost)), so the actual cost shouldn't
    /// be too bad.
    ///
    /// [ZIP-401]: https://zips.z.cash/zip-0401
    pub fn evict_one(&mut self) -> Option<VerifiedUnminedTx> {
        if self.transactions.is_empty() {
            None
        } else {
            use rand::distributions::{Distribution, WeightedIndex};
            use rand::prelude::thread_rng;

            let weights: Vec<u64> = self
                .transactions
                .iter()
                .map(|tx| tx.clone().eviction_weight())
                .collect();

            let dist = WeightedIndex::new(weights).unwrap();

            Some(self.remove(dist.sample(&mut thread_rng())))
        }
    }

    /// Removes all transactions in the set that match the `predicate`.
    ///
    /// Returns the amount of transactions removed.
    pub fn remove_all_that(&mut self, predicate: impl Fn(&VerifiedUnminedTx) -> bool) -> usize {
        // Clippy suggests to remove the `collect` and the `into_iter` further down. However, it is
        // unable to detect that when that is done, there is a borrow conflict. What happens is the
        // iterator borrows `self.transactions` immutably, but it also need to be borrowed mutably
        // in order to remove the transactions while traversing the iterator.
        #[allow(clippy::needless_collect)]
        let indices_to_remove: Vec<_> = self
            .transactions
            .iter()
            .enumerate()
            .filter(|(_, tx)| predicate(tx))
            .map(|(index, _)| index)
            .collect();

        let removed_count = indices_to_remove.len();

        // Correctness: remove indexes in reverse order,
        // so earlier indexes still correspond to the same transactions
        for index_to_remove in indices_to_remove.into_iter().rev() {
            self.remove(index_to_remove);
        }

        removed_count
    }

    /// Removes a transaction from the set.
    ///
    /// Also removes its outputs from the internal caches.
    fn remove(&mut self, transaction_index: usize) -> VerifiedUnminedTx {
        let removed_tx = self
            .transactions
            .remove(transaction_index)
            .expect("invalid transaction index");

        self.transactions_serialized_size -= removed_tx.transaction.size;
        self.total_cost -= removed_tx.cost();
        self.remove_outputs(&removed_tx.transaction);

        self.update_metrics();

        removed_tx
    }

    /// Returns `true` if the given `transaction` has any spend conflicts with transactions in the
    /// mempool.
    ///
    /// Two transactions have a spend conflict if they spend the same UTXO or if they reveal the
    /// same nullifier.
    fn has_spend_conflicts(&self, unmined_tx: &UnminedTx) -> bool {
        let tx = &unmined_tx.transaction;

        Self::has_conflicts(&self.spent_outpoints, tx.spent_outpoints())
            || Self::has_conflicts(&self.sprout_nullifiers, tx.sprout_nullifiers().copied())
            || Self::has_conflicts(&self.sapling_nullifiers, tx.sapling_nullifiers().copied())
            || Self::has_conflicts(&self.orchard_nullifiers, tx.orchard_nullifiers().copied())
    }

    /// Inserts the transaction's outputs into the internal caches.
    fn cache_outputs_from(&mut self, tx: &Transaction) {
        self.spent_outpoints.extend(tx.spent_outpoints());
        self.sprout_nullifiers.extend(tx.sprout_nullifiers());
        self.sapling_nullifiers.extend(tx.sapling_nullifiers());
        self.orchard_nullifiers.extend(tx.orchard_nullifiers());
    }

    /// Removes the tracked transaction outputs from the mempool.
    fn remove_outputs(&mut self, unmined_tx: &UnminedTx) {
        let tx = &unmined_tx.transaction;

        let spent_outpoints = tx.spent_outpoints().map(Cow::Owned);
        let sprout_nullifiers = tx.sprout_nullifiers().map(Cow::Borrowed);
        let sapling_nullifiers = tx.sapling_nullifiers().map(Cow::Borrowed);
        let orchard_nullifiers = tx.orchard_nullifiers().map(Cow::Borrowed);

        Self::remove_from_set(&mut self.spent_outpoints, spent_outpoints);
        Self::remove_from_set(&mut self.sprout_nullifiers, sprout_nullifiers);
        Self::remove_from_set(&mut self.sapling_nullifiers, sapling_nullifiers);
        Self::remove_from_set(&mut self.orchard_nullifiers, orchard_nullifiers);
    }

    /// Returns `true` if the two sets have common items.
    fn has_conflicts<T>(set: &HashSet<T>, mut list: impl Iterator<Item = T>) -> bool
    where
        T: Eq + Hash,
    {
        list.any(|item| set.contains(&item))
    }

    /// Removes some items from a [`HashSet`].
    ///
    /// Each item in the list of `items` should be wrapped in a [`Cow`]. This allows this generic
    /// method to support both borrowed and owned items.
    fn remove_from_set<'t, T>(set: &mut HashSet<T>, items: impl IntoIterator<Item = Cow<'t, T>>)
    where
        T: Clone + Eq + Hash + 't,
    {
        for item in items {
            set.remove(&item);
        }
    }

    fn update_metrics(&mut self) {
        metrics::gauge!(
            "zcash.mempool.size.transactions",
            self.transaction_count() as _
        );
        metrics::gauge!(
            "zcash.mempool.size.bytes",
            self.transactions_serialized_size as _
        );
        metrics::gauge!("zcash.mempool.cost.bytes", self.total_cost as _);
    }
}
