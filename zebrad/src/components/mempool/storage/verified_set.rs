//! The set of verified transactions in the mempool.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    hash::Hash,
};

use zebra_chain::{
    orchard, sapling, sprout,
    transaction::{self, Transaction, UnminedTx, VerifiedUnminedTx},
    transparent,
};

use crate::components::mempool::pending_outputs::PendingOutputs;

use super::super::SameEffectsTipRejectionError;

// Imports for doc links
#[allow(unused_imports)]
use zebra_chain::transaction::MEMPOOL_TRANSACTION_COST_THRESHOLD;

#[derive(Default)]
struct TransactionDependencies {
    /// Lists of mempool transactions that create UTXOs spent by
    /// a mempool transaction. Used during block template construction
    /// to exclude transactions from block templates unless all of the
    /// transactions they depend on have been included.
    dependencies: HashMap<transaction::Hash, HashSet<transaction::Hash>>,

    /// Lists of transaction ids in the mempool that spend UTXOs created
    /// by a transaction in the mempool, e.g. tx1 -> set(tx2, tx3, tx4) where
    /// tx2, tx3, and tx4 spend outputs created by tx1.
    dependents: HashMap<transaction::Hash, HashSet<transaction::Hash>>,
}

impl TransactionDependencies {
    /// Adds a transaction that spends outputs created by other transactions in the mempool
    /// as a dependent of those transactions, and adds the transactions that created the outputs
    /// spent by the dependent transaction as dependencies of the dependent transaction.
    ///
    /// # Correctness
    ///
    /// It's the caller's responsibility to ensure that there are no cyclical dependencies.
    ///
    /// The transaction verifier will wait until the spent output of a transaction has been added to the verified set,
    /// so its `AwaitOutput` requests will timeout if there is a cyclical dependency.
    fn add(
        &mut self,
        dependent: transaction::Hash,
        spent_mempool_outpoints: Vec<transparent::OutPoint>,
    ) {
        for &spent_mempool_outpoint in &spent_mempool_outpoints {
            self.dependents
                .entry(spent_mempool_outpoint.hash)
                .or_default()
                .insert(dependent);
        }

        if !spent_mempool_outpoints.is_empty() {
            self.dependencies.entry(dependent).or_default().extend(
                spent_mempool_outpoints
                    .into_iter()
                    .map(|outpoint| outpoint.hash),
            );
        }
    }

    /// Removes the hash of a transaction in the mempool and the hashes of any transactions
    /// that are tracked as being directly dependent on that transaction from
    /// this [`TransactionDependencies`].
    ///
    /// Returns a list of transaction hashes that depend on the transaction being removed.
    fn remove(&mut self, tx_hash: &transaction::Hash) -> HashSet<transaction::Hash> {
        self.dependencies.remove(tx_hash);
        self.dependents.remove(tx_hash).unwrap_or_default()
    }
}

/// The set of verified transactions stored in the mempool.
///
/// This also caches the all the spent outputs from the transactions in the mempool. The spent
/// outputs include:
///
/// - the dependencies of transactions that spent the outputs of other transactions in the mempool
/// - the UTXOs of transactions in the mempool
/// - the transparent outpoints spent by transactions in the mempool
/// - the Sprout nullifiers revealed by transactions in the mempool
/// - the Sapling nullifiers revealed by transactions in the mempool
/// - the Orchard nullifiers revealed by transactions in the mempool
#[derive(Default)]
pub struct VerifiedSet {
    /// The set of verified transactions in the mempool.
    transactions: HashMap<transaction::Hash, VerifiedUnminedTx>,

    /// A map of dependencies between transactions in the mempool that
    /// spend or create outputs of other transactions in the mempool.
    transaction_dependencies: TransactionDependencies,

    /// The [`transparent::Utxo`]s created by verified transactions in the mempool
    ///
    /// Note that these UTXOs may not be unspent.
    /// Outputs can be spent by later transactions or blocks in the chain.
    created_outputs: HashMap<transparent::OutPoint, transparent::Output>,

    /// The total size of the transactions in the mempool if they were
    /// serialized.
    transactions_serialized_size: usize,

    /// The total cost of the verified transactions in the set.
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
    /// Returns a reference to the [`HashMap`] of [`VerifiedUnminedTx`]s in the set.
    pub fn transactions(&self) -> &HashMap<transaction::Hash, VerifiedUnminedTx> {
        &self.transactions
    }

    /// Returns a reference to the [`HashMap`] of transaction dependencies in the verified set.
    pub fn transaction_dependencies(
        &self,
    ) -> &HashMap<transaction::Hash, HashSet<transaction::Hash>> {
        &self.transaction_dependencies.dependencies
    }

    /// Returns a reference to the [`HashMap`] of transaction dependents in the verified set.
    pub fn transaction_dependents(
        &self,
    ) -> &HashMap<transaction::Hash, HashSet<transaction::Hash>> {
        &self.transaction_dependencies.dependents
    }

    /// Returns a [`transparent::Output`] created by a mempool transaction for the provided
    /// [`transparent::OutPoint`] if one exists, or None otherwise.
    pub fn created_output(&self, outpoint: &transparent::OutPoint) -> Option<transparent::Output> {
        self.created_outputs.get(outpoint).cloned()
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

    /// Returns the total serialized size of the verified transactions in the set.
    ///
    /// This can be less than the total cost, because the minimum transaction cost
    /// is based on the [`MEMPOOL_TRANSACTION_COST_THRESHOLD`].
    pub fn total_serialized_size(&self) -> usize {
        self.transactions_serialized_size
    }

    /// Returns `true` if the set of verified transactions contains the transaction with the
    /// specified [`transaction::Hash`].
    pub fn contains(&self, id: &transaction::Hash) -> bool {
        self.transactions.contains_key(id)
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
        spent_mempool_outpoints: Vec<transparent::OutPoint>,
        pending_outputs: &mut PendingOutputs,
    ) -> Result<(), SameEffectsTipRejectionError> {
        if self.has_spend_conflicts(&transaction.transaction) {
            return Err(SameEffectsTipRejectionError::SpendConflict);
        }

        // This likely only needs to check that the transaction hash of the outpoint is still in the mempool,
        // bu it's likely rare that a transaction spends multiple transparent outputs of
        // a single transaction in practice.
        for outpoint in &spent_mempool_outpoints {
            if !self.created_outputs.contains_key(outpoint) {
                return Err(SameEffectsTipRejectionError::MissingOutput);
            }
        }

        let tx_id = transaction.transaction.id.mined_id();
        self.transaction_dependencies
            .add(tx_id, spent_mempool_outpoints);

        self.cache_outputs_and_respond_to_pending_output_requests_from(
            tx_id,
            &transaction.transaction.transaction,
            pending_outputs,
        );

        self.transactions_serialized_size += transaction.transaction.size;
        self.total_cost += transaction.cost();
        self.transactions.insert(tx_id, transaction);

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
    #[allow(clippy::unwrap_in_result)]
    pub fn evict_one(&mut self) -> Vec<VerifiedUnminedTx> {
        if self.transactions.is_empty() {
            Vec::new()
        } else {
            use rand::distributions::{Distribution, WeightedIndex};
            use rand::prelude::thread_rng;

            let (keys, weights): (Vec<transaction::Hash>, Vec<u64>) = self
                .transactions
                .iter()
                .map(|(&tx_id, tx)| (tx_id, tx.eviction_weight()))
                .unzip();

            let dist = WeightedIndex::new(weights)
                .expect("there is at least one weight, all weights are non-negative, and the total is positive");

            let key_to_remove = keys
                .get(dist.sample(&mut thread_rng()))
                .expect("should have a key at every index in the distribution");

            self.remove(key_to_remove)
        }
    }

    /// Removes all transactions in the set that match the `predicate`.
    ///
    /// Returns the amount of transactions removed.
    pub fn remove_all_that(&mut self, predicate: impl Fn(&VerifiedUnminedTx) -> bool) -> usize {
        let keys_to_remove: Vec<_> = self
            .transactions
            .iter()
            .filter_map(|(&tx_id, tx)| predicate(tx).then_some(tx_id))
            .collect();

        let mut removed_count = 0;

        for key_to_remove in keys_to_remove {
            removed_count += self.remove(&key_to_remove).len();
        }

        removed_count
    }

    /// Removes a transaction from the set.
    ///
    /// Also removes its outputs from the internal caches.
    fn remove(&mut self, key_to_remove: &transaction::Hash) -> Vec<VerifiedUnminedTx> {
        let Some(removed_tx) = self.transactions.remove(key_to_remove) else {
            // Transaction key not found, it may have been removed as a dependent
            // of another transaction that was removed.
            return Vec::new();
        };

        self.transactions_serialized_size -= removed_tx.transaction.size;
        self.total_cost -= removed_tx.cost();
        self.remove_outputs(&removed_tx.transaction);

        self.update_metrics();

        let mut removed_txs = vec![removed_tx];
        let dependent_transactions = self.transaction_dependencies.remove(key_to_remove);

        for dependent_tx in dependent_transactions {
            removed_txs.extend(self.remove(&dependent_tx));
        }

        removed_txs
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

    /// Inserts the transaction's outputs into the internal caches and responds to pending output requests.
    fn cache_outputs_and_respond_to_pending_output_requests_from(
        &mut self,
        tx_hash: transaction::Hash,
        tx: &Transaction,
        pending_outputs: &mut PendingOutputs,
    ) {
        for (index, output) in tx.outputs().iter().cloned().enumerate() {
            let outpoint = transparent::OutPoint::from_usize(tx_hash, index);
            self.created_outputs.insert(outpoint, output.clone());
            pending_outputs.respond(&outpoint, output)
        }

        self.spent_outpoints.extend(tx.spent_outpoints());
        self.sprout_nullifiers.extend(tx.sprout_nullifiers());
        self.sapling_nullifiers.extend(tx.sapling_nullifiers());
        self.orchard_nullifiers.extend(tx.orchard_nullifiers());
    }

    /// Removes the tracked transaction outputs from the mempool.
    fn remove_outputs(&mut self, unmined_tx: &UnminedTx) {
        let tx = &unmined_tx.transaction;

        for index in 0..tx.outputs().len() {
            self.created_outputs
                .remove(&transparent::OutPoint::from_usize(
                    unmined_tx.id.mined_id(),
                    index,
                ));
        }

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
        // Track the sum of unpaid actions within each transaction (as they are subject to the
        // unpaid action limit). Transactions that have weight >= 1 have no unpaid actions by
        // definition.
        let mut unpaid_actions_with_weight_lt20pct = 0;
        let mut unpaid_actions_with_weight_lt40pct = 0;
        let mut unpaid_actions_with_weight_lt60pct = 0;
        let mut unpaid_actions_with_weight_lt80pct = 0;
        let mut unpaid_actions_with_weight_lt1 = 0;

        // Track the total number of paid actions across all transactions in the mempool. This
        // added to the bucketed unpaid actions above is equal to the total number of conventional
        // actions in the mempool.
        let mut paid_actions = 0;

        // Track the sum of transaction sizes (the metric by which they are mainly limited) across
        // several buckets.
        let mut size_with_weight_lt1 = 0;
        let mut size_with_weight_eq1 = 0;
        let mut size_with_weight_gt1 = 0;
        let mut size_with_weight_gt2 = 0;
        let mut size_with_weight_gt3 = 0;

        for entry in self.transactions().values() {
            paid_actions += entry.conventional_actions - entry.unpaid_actions;

            if entry.fee_weight_ratio > 3.0 {
                size_with_weight_gt3 += entry.transaction.size;
            } else if entry.fee_weight_ratio > 2.0 {
                size_with_weight_gt2 += entry.transaction.size;
            } else if entry.fee_weight_ratio > 1.0 {
                size_with_weight_gt1 += entry.transaction.size;
            } else if entry.fee_weight_ratio == 1.0 {
                size_with_weight_eq1 += entry.transaction.size;
            } else {
                size_with_weight_lt1 += entry.transaction.size;
                if entry.fee_weight_ratio < 0.2 {
                    unpaid_actions_with_weight_lt20pct += entry.unpaid_actions;
                } else if entry.fee_weight_ratio < 0.4 {
                    unpaid_actions_with_weight_lt40pct += entry.unpaid_actions;
                } else if entry.fee_weight_ratio < 0.6 {
                    unpaid_actions_with_weight_lt60pct += entry.unpaid_actions;
                } else if entry.fee_weight_ratio < 0.8 {
                    unpaid_actions_with_weight_lt80pct += entry.unpaid_actions;
                } else {
                    unpaid_actions_with_weight_lt1 += entry.unpaid_actions;
                }
            }
        }

        metrics::gauge!(
            "zcash.mempool.actions.unpaid",
            "bk" => "< 0.2",
        )
        .set(unpaid_actions_with_weight_lt20pct as f64);
        metrics::gauge!(
            "zcash.mempool.actions.unpaid",
            "bk" => "< 0.4",
        )
        .set(unpaid_actions_with_weight_lt40pct as f64);
        metrics::gauge!(
            "zcash.mempool.actions.unpaid",
            "bk" => "< 0.6",
        )
        .set(unpaid_actions_with_weight_lt60pct as f64);
        metrics::gauge!(
            "zcash.mempool.actions.unpaid",
            "bk" => "< 0.8",
        )
        .set(unpaid_actions_with_weight_lt80pct as f64);
        metrics::gauge!(
            "zcash.mempool.actions.unpaid",
            "bk" => "< 1",
        )
        .set(unpaid_actions_with_weight_lt1 as f64);
        metrics::gauge!("zcash.mempool.actions.paid").set(paid_actions as f64);
        metrics::gauge!("zcash.mempool.size.transactions",).set(self.transaction_count() as f64);
        metrics::gauge!(
            "zcash.mempool.size.weighted",
            "bk" => "< 1",
        )
        .set(size_with_weight_lt1 as f64);
        metrics::gauge!(
            "zcash.mempool.size.weighted",
            "bk" => "1",
        )
        .set(size_with_weight_eq1 as f64);
        metrics::gauge!(
            "zcash.mempool.size.weighted",
            "bk" => "> 1",
        )
        .set(size_with_weight_gt1 as f64);
        metrics::gauge!(
            "zcash.mempool.size.weighted",
            "bk" => "> 2",
        )
        .set(size_with_weight_gt2 as f64);
        metrics::gauge!(
            "zcash.mempool.size.weighted",
            "bk" => "> 3",
        )
        .set(size_with_weight_gt3 as f64);
        metrics::gauge!("zcash.mempool.size.bytes",).set(self.transactions_serialized_size as f64);
        metrics::gauge!("zcash.mempool.cost.bytes").set(self.total_cost as f64);
    }
}
