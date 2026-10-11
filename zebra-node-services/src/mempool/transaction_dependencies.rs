//! Representation of mempool transactions' dependencies on other transactions in the mempool.

use std::collections::{HashMap, HashSet};

use zebra_chain::{transaction, transparent};

/// Representation of mempool transactions' dependencies on other transactions in the mempool.
#[derive(Default, Debug, Clone)]
pub struct TransactionDependencies {
    /// Lists of mempool transaction ids that create UTXOs spent by
    /// a mempool transaction. Used during block template construction
    /// to exclude transactions from block templates unless all of the
    /// transactions they depend on have been included.
    ///
    /// # Note
    ///
    /// Dependencies that have been mined into blocks are not removed here until those blocks have
    /// been committed to the best chain. Dependencies that have been committed onto side chains, or
    /// which are in the verification pipeline but have not yet been committed to the best chain,
    /// are not removed here unless and until they arrive in the best chain, and the mempool is polled.
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

        // Only add entries to `dependencies` for transactions that spend unmined outputs so it
        // can be used to handle transactions with dependencies differently during block production.
        if !spent_mempool_outpoints.is_empty() {
            self.dependencies.insert(
                dependent,
                spent_mempool_outpoints
                    .into_iter()
                    .map(|outpoint| outpoint.hash)
                    .collect(),
            );
        }
    }

    /// Removes all dependents for a list of mined transaction ids and removes the mined transaction ids
    /// from the dependencies of their dependents.
    pub fn clear_mined_dependencies(&mut self, mined_ids: &HashSet<transaction::Hash>) {
        for mined_tx_id in mined_ids {
            for dependent_id in self.dependents.remove(mined_tx_id).unwrap_or_default() {
                let Some(dependencies) = self.dependencies.get_mut(&dependent_id) else {
                    // TODO: Move this struct to zebra-chain and log a warning here.
                    continue;
                };

                // TODO: Move this struct to zebra-chain and log a warning here if the dependency was not found.
                dependencies.remove(mined_tx_id);
                if dependencies.is_empty() {
                    self.dependencies.remove(&dependent_id);
                }
            }
        }
    }

    /// Removes the hash of a transaction in the mempool and the hashes of any transactions
    /// that are tracked as being directly or indirectly dependent on that transaction from
    /// this [`TransactionDependencies`].
    ///
    /// Returns a list of transaction hashes that were being tracked as dependents of the
    /// provided transaction hash.
    pub fn remove_all(&mut self, &tx_hash: &transaction::Hash) -> HashSet<transaction::Hash> {
        let mut all_dependents = HashSet::new();
        let mut current_level_dependents: HashSet<_> = [tx_hash].into();

        while !current_level_dependents.is_empty() {
            current_level_dependents = current_level_dependents
                .iter()
                .flat_map(|dependent| {
                    for dependency in self.dependencies.remove(dependent).unwrap_or_default() {
                        let Some(dependents_of_dependency) = self.dependents.get_mut(&dependency)
                        else {
                            continue;
                        };

                        dependents_of_dependency.remove(dependent);
                        if dependents_of_dependency.is_empty() {
                            self.dependents.remove(&dependency);
                        }
                    }

                    self.dependents.remove(dependent).unwrap_or_default()
                })
                .collect();

            all_dependents.extend(&current_level_dependents);
        }

        all_dependents
    }

    /// Returns a list of hashes of transactions that directly depend on the transaction for `tx_hash`.
    pub fn direct_dependents(&self, tx_hash: &transaction::Hash) -> HashSet<transaction::Hash> {
        self.dependents.get(tx_hash).cloned().unwrap_or_default()
    }

    /// Returns the set of hashes of transactions that directly or indirectly
    /// depend on the transaction for `tx_hash` — its full set of in-mempool
    /// descendants in the dependency DAG.
    ///
    /// Unlike [`TransactionDependencies::direct_dependents`], this traverses the
    /// entire `dependents` graph, so grandchildren and deeper descendants are
    /// included. The traversal keeps a `HashSet` of visited transactions and
    /// only pushes a transaction onto the work stack the first time it is
    /// reached, so each descendant is returned exactly once even in
    /// diamond-shaped dependency graphs (e.g. `A -> B`, `A -> C`, `B -> D`).
    ///
    /// This method is non-destructive: it does not modify `self`. It is the
    /// read-only counterpart of [`TransactionDependencies::remove_all`], which
    /// removes the same set of transactions from the maps.
    ///
    /// # Correctness
    ///
    /// Callers must ensure there are no cyclical dependencies, as documented on
    /// [`TransactionDependencies::add`]. The visited `HashSet` also bounds the
    /// traversal if a cycle is somehow present, so it cannot loop forever.
    pub fn all_dependents(&self, tx_hash: &transaction::Hash) -> HashSet<transaction::Hash> {
        let mut all_dependents = HashSet::new();
        let mut to_visit: Vec<transaction::Hash> = vec![*tx_hash];

        while let Some(current) = to_visit.pop() {
            if let Some(dependents) = self.dependents.get(&current) {
                for dependent in dependents {
                    if all_dependents.insert(*dependent) {
                        to_visit.push(*dependent);
                    }
                }
            }
        }

        all_dependents
    }

    /// Returns a list of hashes of transactions that are direct dependencies of the transaction for `tx_hash`.
    pub fn direct_dependencies(&self, tx_hash: &transaction::Hash) -> HashSet<transaction::Hash> {
        self.dependencies.get(tx_hash).cloned().unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use zebra_chain::transaction::Hash;

    use super::TransactionDependencies;

    /// Build a [`TransactionDependencies`] from `(parent, [children...])` edges,
    /// populating the `dependents` map directly.
    fn with_dependents(edges: &[(Hash, &[Hash])]) -> TransactionDependencies {
        let mut deps = TransactionDependencies::default();
        for (parent, children) in edges {
            deps.dependents
                .insert(*parent, children.iter().copied().collect());
        }
        deps
    }

    #[test]
    fn all_dependents_follows_transitive_chain() {
        // A -> B -> C
        let a = Hash([0x0a; 32]);
        let b = Hash([0x0b; 32]);
        let c = Hash([0x0c; 32]);
        let deps = with_dependents(&[(a, &[b]), (b, &[c])]);

        assert_eq!(deps.all_dependents(&a), HashSet::from([b, c]));
        assert_eq!(deps.all_dependents(&b), HashSet::from([c]));
        assert_eq!(deps.all_dependents(&c), HashSet::new());
    }

    #[test]
    fn all_dependents_handles_diamond() {
        // A -> B, A -> C, B -> D  (a fork / diamond-shaped dependency graph)
        let a = Hash([0x0a; 32]);
        let b = Hash([0x0b; 32]);
        let c = Hash([0x0c; 32]);
        let d = Hash([0x0d; 32]);
        let deps = with_dependents(&[(a, &[b, c]), (b, &[d])]);

        assert_eq!(deps.all_dependents(&a), HashSet::from([b, c, d]));
        assert_eq!(deps.all_dependents(&b), HashSet::from([d]));
        assert_eq!(deps.all_dependents(&c), HashSet::new());
        assert_eq!(deps.all_dependents(&d), HashSet::new());
    }

    #[test]
    fn all_dependents_matches_add_population() {
        // Build the same A -> B -> C DAG through the public `add` API, which
        // populates `dependents`, and check that `all_dependents` reads it.
        let a = Hash([0x0a; 32]);
        let b = Hash([0x0b; 32]);
        let c = Hash([0x0c; 32]);

        let mut deps = TransactionDependencies::default();
        // B spends an output created by A.
        deps.add(
            b,
            vec![zebra_chain::transparent::OutPoint { hash: a, index: 0 }],
        );
        // C spends an output created by B.
        deps.add(
            c,
            vec![zebra_chain::transparent::OutPoint { hash: b, index: 0 }],
        );

        assert_eq!(deps.all_dependents(&a), HashSet::from([b, c]));
        assert_eq!(deps.all_dependents(&b), HashSet::from([c]));
    }
}
