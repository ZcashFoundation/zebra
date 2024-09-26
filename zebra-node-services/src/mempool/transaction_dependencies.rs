//! Representation of mempool transactions' dependencies on other transactions in the mempool.

use std::collections::{HashMap, HashSet};

use zebra_chain::{transaction, transparent};

/// Representation of mempool transactions' dependencies on other transactions in the mempool.
#[derive(Default, Debug)]
pub struct TransactionDependencies {
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
    pub fn add(
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
    /// that are tracked as being directly or indirectly dependent on that transaction from
    /// this [`TransactionDependencies`].
    ///
    /// Returns a list of transaction hashes that have been removed if they were previously
    /// in this [`TransactionDependencies`].
    pub fn remove(&mut self, &tx_hash: &transaction::Hash) -> HashSet<transaction::Hash> {
        let mut current_level_dependents: HashSet<_> = [tx_hash].into();
        let mut all_dependents = current_level_dependents.clone();

        while !current_level_dependents.is_empty() {
            current_level_dependents = current_level_dependents
                .iter()
                .flat_map(|dep| {
                    self.dependencies.remove(dep);
                    self.dependents.remove(dep).unwrap_or_default()
                })
                .collect();

            all_dependents.extend(&current_level_dependents);
        }

        all_dependents
    }

    /// Clear the maps of transaction dependencies.
    pub fn clear(&mut self) {
        self.dependencies.clear();
        self.dependents.clear();
    }

    /// Returns the map of transaction's dependencies
    pub fn dependencies(&self) -> &HashMap<transaction::Hash, HashSet<transaction::Hash>> {
        &self.dependencies
    }

    /// Returns the map of transaction's dependents
    pub fn dependents(&self) -> &HashMap<transaction::Hash, HashSet<transaction::Hash>> {
        &self.dependents
    }
}
