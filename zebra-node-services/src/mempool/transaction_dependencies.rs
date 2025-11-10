//! Representation of mempool transactions' dependencies on other transactions in the mempool.

use std::collections::{HashMap, HashSet, VecDeque};

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
                let _ = dependencies.remove(mined_tx_id);
            }
        }
    }

    /// Removes the hash of a transaction in the mempool and the hashes of any transactions
    /// that are tracked as being directly or indirectly dependent on that transaction from
    /// this [`TransactionDependencies`].
    ///
    /// Returns a list of transaction hashes that were being tracked as dependents of the
    /// provided transaction hash.
    ///
    /// # Invariant
    ///
    /// Every hash in the returned set was present in [`Self::dependents`] at the time of removal.
    /// Removing a child transaction also removes it from each parent's dependents list, so callers
    /// can safely assume the IDs are still present in their own transaction maps.
    pub fn remove_all(&mut self, &tx_hash: &transaction::Hash) -> HashSet<transaction::Hash> {
        let mut all_dependents = HashSet::new();
        let mut queue: VecDeque<_> = VecDeque::from([tx_hash]);

        while let Some(removed) = queue.pop_front() {
            if let Some(parents) = self.dependencies.remove(&removed) {
                for parent in parents {
                    if let Some(children) = self.dependents.get_mut(&parent) {
                        children.remove(&removed);

                        if children.is_empty() {
                            self.dependents.remove(&parent);
                        }
                    }
                }
            }

            let dependents = self.dependents.remove(&removed).unwrap_or_default();
            for dependent in dependents {
                if all_dependents.insert(dependent) {
                    queue.push_back(dependent);
                }
            }
        }

        all_dependents
    }

    /// Returns a list of hashes of transactions that directly depend on the transaction for `tx_hash`.
    pub fn direct_dependents(&self, tx_hash: &transaction::Hash) -> HashSet<transaction::Hash> {
        self.dependents.get(tx_hash).cloned().unwrap_or_default()
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
    use super::*;

    fn hash(byte: u8) -> transaction::Hash {
        let mut bytes = [0u8; 32];
        bytes[0] = byte;
        transaction::Hash::from(bytes)
    }

    fn outpoint(hash: transaction::Hash) -> transparent::OutPoint {
        transparent::OutPoint::from_usize(hash, 0)
    }

    #[test]
    fn remove_all_drops_stale_dependents() {
        let mut deps = TransactionDependencies::default();
        let parent = hash(1);
        let child = hash(2);

        deps.add(child, vec![outpoint(parent)]);
        assert!(deps.direct_dependents(&parent).contains(&child));

        let removed = deps.remove_all(&child);
        assert!(removed.is_empty());
        assert!(deps.direct_dependents(&parent).is_empty());

        let removed_parent = deps.remove_all(&parent);
        assert!(removed_parent.is_empty());
        assert!(deps.direct_dependents(&parent).is_empty());
    }

    #[test]
    fn remove_all_returns_transitive_dependents() {
        let mut deps = TransactionDependencies::default();
        let parent = hash(10);
        let child = hash(11);
        let grandchild = hash(12);

        deps.add(child, vec![outpoint(parent)]);
        deps.add(grandchild, vec![outpoint(child)]);

        let removed = deps.remove_all(&parent);
        let expected: HashSet<_> = [child, grandchild].into_iter().collect();
        assert_eq!(removed, expected);
        assert!(deps.direct_dependents(&parent).is_empty());
        assert!(deps.direct_dependents(&child).is_empty());
        assert!(deps.direct_dependencies(&grandchild).is_empty());
    }

    #[test]
    fn remove_parent_with_multiple_children() {
        let mut deps = TransactionDependencies::default();
        let parent = hash(30);
        let children = [hash(31), hash(32), hash(33)];

        for child in children {
            deps.add(child, vec![outpoint(parent)]);
        }

        let removed = deps.remove_all(&parent);
        let expected: HashSet<_> = children.into_iter().collect();
        assert_eq!(removed, expected);
        assert!(deps.direct_dependents(&parent).is_empty());
    }

    #[test]
    fn remove_child_with_multiple_parents() {
        let mut deps = TransactionDependencies::default();
        let parents = [hash(40), hash(41)];
        let child = hash(42);

        deps.add(
            child,
            parents.iter().copied().map(outpoint).collect::<Vec<_>>(),
        );

        let removed = deps.remove_all(&child);
        assert!(removed.is_empty());
        for parent in parents {
            assert!(deps.direct_dependents(&parent).is_empty());
        }
    }

    #[test]
    fn remove_from_complex_graph() {
        let mut deps = TransactionDependencies::default();
        let a = hash(50);
        let b = hash(51);
        let c = hash(52);
        let d = hash(53);

        deps.add(b, vec![outpoint(a)]);
        deps.add(c, vec![outpoint(a)]);
        deps.add(d, vec![outpoint(b), outpoint(c)]);

        let removed = deps.remove_all(&a);
        let expected: HashSet<_> = [b, c, d].into_iter().collect();
        assert_eq!(removed, expected);
        assert!(deps.dependents().is_empty());
        assert!(deps.dependencies().is_empty());
    }

    #[test]
    fn clear_mined_dependencies_removes_correct_hash() {
        let mut deps = TransactionDependencies::default();
        let parent = hash(20);
        let child = hash(21);

        deps.add(child, vec![outpoint(parent)]);
        assert!(deps.direct_dependencies(&child).contains(&parent));

        let mined_ids: HashSet<_> = [parent].into_iter().collect();
        deps.clear_mined_dependencies(&mined_ids);

        assert!(deps.direct_dependents(&parent).is_empty());
        assert!(deps.direct_dependencies(&child).is_empty());
        assert!(deps.dependents().get(&parent).is_none());
    }
}
