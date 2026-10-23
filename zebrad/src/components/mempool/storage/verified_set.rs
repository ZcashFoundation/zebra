//! The set of verified transactions in the mempool.

use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
};

use zebra_chain::{
    block::Height,
    ironwood, orchard, sapling, sprout,
    transaction::{self, UnminedTx, UnminedTxId, VerifiedUnminedTx},
    transparent,
};
use zebra_node_services::mempool::TransactionDependencies;

use crate::components::mempool::pending_outputs::PendingOutputs;

use super::super::SameEffectsTipRejectionError;

// Imports for doc links
#[allow(unused_imports)]
use zebra_chain::transaction::MEMPOOL_TRANSACTION_COST_THRESHOLD;

#[cfg(test)]
mod tests;

/// The set of verified transactions stored in the mempool.
///
/// This also caches the all the spent outputs from the transactions in the mempool. The spent
/// outputs include:
///
/// - the dependencies of transactions that spent the outputs of other transactions in the mempool
/// - the outputs of transactions in the mempool
/// - the transparent outpoints spent by transactions in the mempool
/// - the Sprout nullifiers revealed by transactions in the mempool
/// - the Sapling nullifiers revealed by transactions in the mempool
/// - the Orchard nullifiers revealed by transactions in the mempool
/// - the Ironwood nullifiers revealed by transactions in the mempool
#[derive(Default)]
pub struct VerifiedSet {
    /// The set of verified transactions in the mempool.
    transactions: HashMap<transaction::Hash, VerifiedUnminedTx>,

    /// A map of dependencies between transactions in the mempool that
    /// spend or create outputs of other transactions in the mempool.
    transaction_dependencies: TransactionDependencies,

    /// The [`transparent::Output`]s created by verified transactions in the mempool.
    ///
    /// These outputs may be spent by other transactions in the mempool.
    created_outputs: HashMap<transparent::OutPoint, transparent::Output>,

    /// The total size of the transactions in the mempool if they were
    /// serialized.
    transactions_serialized_size: usize,

    /// The total cost of the verified transactions in the set.
    total_cost: u64,

    /// Incrementally maintained action and fee-weighted size totals.
    metrics: MempoolMetrics,

    /// The set of spent out points by the verified transactions.
    spent_outpoints: HashSet<transparent::OutPoint>,

    /// The set of revealed Sprout nullifiers.
    sprout_nullifiers: HashSet<sprout::Nullifier>,

    /// The set of revealed Sapling nullifiers.
    sapling_nullifiers: HashSet<sapling::Nullifier>,

    /// The set of revealed Orchard nullifiers.
    orchard_nullifiers: HashSet<orchard::Nullifier>,

    /// The set of revealed Ironwood nullifiers.
    ironwood_nullifiers: HashSet<ironwood::Nullifier>,
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

    /// Returns a reference to the [`TransactionDependencies`] in the set.
    pub fn transaction_dependencies(&self) -> &TransactionDependencies {
        &self.transaction_dependencies
    }

    /// Returns a [`transparent::Output`] created by a mempool transaction for the provided
    /// [`transparent::OutPoint`] if one exists, or None otherwise.
    pub fn created_output(&self, outpoint: &transparent::OutPoint) -> Option<transparent::Output> {
        self.created_outputs.get(outpoint).cloned()
    }

    /// Returns true if a tx in the set has spent the output at the provided outpoint.
    pub fn has_spent_outpoint(&self, outpoint: &transparent::OutPoint) -> bool {
        self.spent_outpoints.contains(outpoint)
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
        self.transaction_dependencies.clear();
        self.spent_outpoints.clear();
        self.sprout_nullifiers.clear();
        self.sapling_nullifiers.clear();
        self.orchard_nullifiers.clear();
        self.ironwood_nullifiers.clear();
        self.created_outputs.clear();
        self.transactions_serialized_size = 0;
        self.total_cost = 0;
        self.metrics = MempoolMetrics::default();
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
        mut transaction: VerifiedUnminedTx,
        spent_mempool_outpoints: Vec<transparent::OutPoint>,
        pending_outputs: &mut PendingOutputs,
        height: Option<Height>,
    ) -> Result<(), SameEffectsTipRejectionError> {
        if self.has_spend_conflicts(&transaction.transaction) {
            return Err(SameEffectsTipRejectionError::SpendConflict);
        }

        // This likely only needs to check that the transaction hash of the outpoint is still in the mempool,
        // but it's likely rare that a transaction spends multiple transparent outputs of
        // a single transaction in practice.
        for outpoint in &spent_mempool_outpoints {
            if !self.created_outputs.contains_key(outpoint) {
                return Err(SameEffectsTipRejectionError::MissingOutput);
            }
        }

        let tx_id = transaction.transaction.id.mined_id();
        self.transaction_dependencies
            .add(tx_id, spent_mempool_outpoints);

        // Inserts the transaction's outputs into the internal caches and responds to pending output requests.
        let tx = &transaction.transaction.transaction;
        for (index, output) in tx.outputs().iter().cloned().enumerate() {
            let outpoint = transparent::OutPoint::from_usize(tx_id, index);
            self.created_outputs.insert(outpoint, output.clone());
            pending_outputs.respond(&outpoint, output)
        }

        self.spent_outpoints.extend(tx.spent_outpoints());
        self.sprout_nullifiers.extend(tx.sprout_nullifiers());
        self.sapling_nullifiers.extend(tx.sapling_nullifiers());
        self.orchard_nullifiers.extend(tx.orchard_nullifiers());
        self.ironwood_nullifiers.extend(tx.ironwood_nullifiers());

        self.transactions_serialized_size += transaction.transaction.size;
        self.total_cost += transaction.cost();
        self.metrics.update(&transaction, true);
        transaction.time = Some(chrono::Utc::now());
        transaction.height = height;
        self.transactions.insert(tx_id, transaction);

        self.update_metrics();

        Ok(())
    }

    /// Evict one transaction and any transactions that directly or indirectly depend on
    /// its outputs from the set, returns the victim transaction and any dependent transactions.
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
    /// This function is equivalent to `EvictTransaction` in [ZIP-401].
    ///
    /// [ZIP-401]: https://zips.z.cash/zip-0401
    #[allow(clippy::unwrap_in_result)]
    pub fn evict_one(&mut self) -> Option<VerifiedUnminedTx> {
        use rand::distr::{weighted::WeightedIndex, Distribution};

        let (keys, weights): (Vec<transaction::Hash>, Vec<u64>) = self
            .transactions
            .iter()
            .map(|(&tx_id, tx)| (tx_id, tx.eviction_weight()))
            .unzip();

        let dist = WeightedIndex::new(weights).expect(
            "there is at least one weight, all weights are non-negative, and the total is positive",
        );

        let key_to_remove = keys
            .get(dist.sample(&mut rand::rng()))
            .expect("should have a key at every index in the distribution");

        // Removes the randomly selected transaction and all of its dependents from the set,
        // then returns just the randomly selected transaction
        self.remove(key_to_remove).pop()
    }

    /// Clears a list of mined transaction ids from the lists of dependencies for
    /// any other transactions in the mempool and removes their dependents.
    pub fn clear_mined_dependencies(&mut self, mined_ids: &HashSet<transaction::Hash>) {
        self.transaction_dependencies
            .clear_mined_dependencies(mined_ids);
    }

    /// Removes all transactions in the set that match the `predicate`.
    ///
    /// Returns the amount of transactions removed.
    pub fn remove_all_that(
        &mut self,
        predicate: impl Fn(&VerifiedUnminedTx) -> bool,
    ) -> HashSet<UnminedTxId> {
        let keys_to_remove: Vec<_> = self
            .transactions
            .iter()
            .filter_map(|(&tx_id, tx)| predicate(tx).then_some(tx_id))
            .collect();

        let mut removed_transactions = HashSet::new();

        for key_to_remove in keys_to_remove {
            if !self.transactions.contains_key(&key_to_remove) {
                // Skip any keys that may have already been removed as their dependencies were removed.
                continue;
            }

            removed_transactions.extend(
                self.remove(&key_to_remove)
                    .into_iter()
                    .map(|tx| tx.transaction.id),
            );
        }

        removed_transactions
    }

    /// Accepts a transaction id for a transaction to remove from the verified set.
    ///
    /// Removes the transaction and any transactions that directly or indirectly
    /// depend on it from the set.
    ///
    /// Returns a list of transactions that have been removed with the target transaction
    /// as the last item.
    ///
    /// Also removes the outputs of any removed transactions from the internal caches.
    fn remove(&mut self, key_to_remove: &transaction::Hash) -> Vec<VerifiedUnminedTx> {
        let removed_transactions: Vec<_> = self
            .transaction_dependencies
            .remove_all(key_to_remove)
            .iter()
            .chain(std::iter::once(key_to_remove))
            .filter_map(|key_to_remove| {
                let Some(removed_tx) = self.transactions.remove(key_to_remove) else {
                    tracing::warn!(?key_to_remove, "invalid transaction key");
                    return None;
                };

                self.transactions_serialized_size -= removed_tx.transaction.size;
                self.total_cost -= removed_tx.cost();
                self.metrics.update(&removed_tx, false);
                self.remove_outputs(&removed_tx.transaction);

                Some(removed_tx)
            })
            .collect();

        self.update_metrics();
        removed_transactions
    }

    /// Returns `true` if the given `transaction` has any spend conflicts with transactions in the
    /// mempool.
    ///
    /// Two transactions have a spend conflict if they spend the same UTXO or if they reveal the
    /// same nullifier.
    fn has_spend_conflicts(&self, unmined_tx: &UnminedTx) -> bool {
        let tx = &unmined_tx.transaction;

        Self::has_conflicts(&self.spent_outpoints, tx.spent_outpoints())
            || Self::has_conflicts(&self.sprout_nullifiers, tx.sprout_nullifiers())
            || Self::has_conflicts(&self.sapling_nullifiers, tx.sapling_nullifiers())
            || Self::has_conflicts(&self.orchard_nullifiers, tx.orchard_nullifiers())
            || Self::has_conflicts(&self.ironwood_nullifiers, tx.ironwood_nullifiers())
    }

    /// Removes the tracked transaction outputs from the mempool.
    fn remove_outputs(&mut self, unmined_tx: &UnminedTx) {
        let tx = &unmined_tx.transaction;

        let output_count = tx
            .transparent_bundle()
            .map_or(0, |bundle| bundle.vout.len());
        for index in 0..output_count {
            self.created_outputs
                .remove(&transparent::OutPoint::from_usize(
                    unmined_tx.id.mined_id(),
                    index,
                ));
        }

        Self::remove_from_set(&mut self.spent_outpoints, tx.spent_outpoints());
        Self::remove_from_set(&mut self.sprout_nullifiers, tx.sprout_nullifiers());
        Self::remove_from_set(&mut self.sapling_nullifiers, tx.sapling_nullifiers());
        Self::remove_from_set(&mut self.orchard_nullifiers, tx.orchard_nullifiers());
        Self::remove_from_set(&mut self.ironwood_nullifiers, tx.ironwood_nullifiers());
    }

    /// Returns `true` if the two sets have common items.
    fn has_conflicts<T>(set: &HashSet<T>, mut list: impl Iterator<Item = T>) -> bool
    where
        T: Eq + Hash,
    {
        list.any(|item| set.contains(&item))
    }

    /// Removes some items from a [`HashSet`].
    fn remove_from_set<T>(set: &mut HashSet<T>, items: impl IntoIterator<Item = T>)
    where
        T: Eq + Hash,
    {
        for item in items {
            set.remove(&item);
        }
    }

    /// Publish cached totals without scanning the transactions.
    fn update_metrics(&self) {
        // Integer-to-f64 casts below can round large totals, which is fine for display-only gauges.

        metrics::gauge!(
            "zcash.mempool.actions.unpaid",
            "bk" => "< 0.2",
        )
        .set(f64::from(self.metrics.unpaid_actions[0]));
        metrics::gauge!(
            "zcash.mempool.actions.unpaid",
            "bk" => "< 0.4",
        )
        .set(f64::from(self.metrics.unpaid_actions[1]));
        metrics::gauge!(
            "zcash.mempool.actions.unpaid",
            "bk" => "< 0.6",
        )
        .set(f64::from(self.metrics.unpaid_actions[2]));
        metrics::gauge!(
            "zcash.mempool.actions.unpaid",
            "bk" => "< 0.8",
        )
        .set(f64::from(self.metrics.unpaid_actions[3]));
        metrics::gauge!(
            "zcash.mempool.actions.unpaid",
            "bk" => "< 1",
        )
        .set(f64::from(self.metrics.unpaid_actions[4]));
        metrics::gauge!("zcash.mempool.actions.paid").set(f64::from(self.metrics.paid_actions));
        metrics::gauge!("zcash.mempool.size.transactions",).set(self.transaction_count() as f64);
        metrics::gauge!(
            "zcash.mempool.size.weighted",
            "bk" => "< 1",
        )
        .set(self.metrics.size_by_weight[0] as f64);
        metrics::gauge!(
            "zcash.mempool.size.weighted",
            "bk" => "1",
        )
        .set(self.metrics.size_by_weight[1] as f64);
        metrics::gauge!(
            "zcash.mempool.size.weighted",
            "bk" => "> 1",
        )
        .set(self.metrics.size_by_weight[2] as f64);
        metrics::gauge!(
            "zcash.mempool.size.weighted",
            "bk" => "> 2",
        )
        .set(self.metrics.size_by_weight[3] as f64);
        metrics::gauge!(
            "zcash.mempool.size.weighted",
            "bk" => "> 3",
        )
        .set(self.metrics.size_by_weight[4] as f64);
        metrics::gauge!("zcash.mempool.size.bytes",).set(self.transactions_serialized_size as f64);
        metrics::gauge!("zcash.mempool.cost.bytes").set(self.total_cost as f64);
    }
}

/// Non-cumulative metric buckets, updated once for each inserted or removed transaction.
#[derive(Default)]
struct MempoolMetrics {
    /// Unpaid actions in fee-weight intervals [0, .2), [.2, .4), [.4, .6), [.6, .8), [.8, 1).
    unpaid_actions: [u32; 5],
    /// Paid actions across all fee weights.
    paid_actions: u32,
    /// Serialized sizes for fee weights < 1, = 1, (1, 2], (2, 3], and > 3.
    size_by_weight: [usize; 5],
}

impl MempoolMetrics {
    /// Add a transaction's contribution when `insert` is true, otherwise subtract it.
    fn update(&mut self, transaction: &VerifiedUnminedTx, insert: bool) {
        let weight = transaction.fee_weight_ratio;
        let size_bucket = if weight > 3.0 {
            4
        } else if weight > 2.0 {
            3
        } else if weight > 1.0 {
            2
        } else if weight == 1.0 {
            1
        } else {
            0
        };
        let paid_actions = transaction.conventional_actions - transaction.unpaid_actions;
        if insert {
            self.paid_actions += paid_actions;
            self.size_by_weight[size_bucket] += transaction.transaction.size;
        } else {
            self.paid_actions -= paid_actions;
            self.size_by_weight[size_bucket] -= transaction.transaction.size;
        }

        // Transactions with weight >= 1 have no unpaid actions.
        if size_bucket == 0 {
            let unpaid_bucket = if weight < 0.2 {
                0
            } else if weight < 0.4 {
                1
            } else if weight < 0.6 {
                2
            } else if weight < 0.8 {
                3
            } else {
                4
            };
            if insert {
                self.unpaid_actions[unpaid_bucket] += transaction.unpaid_actions;
            } else {
                self.unpaid_actions[unpaid_bucket] -= transaction.unpaid_actions;
            }
        }
    }
}
