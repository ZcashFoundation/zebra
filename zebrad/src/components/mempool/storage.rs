use std::{
    collections::{HashMap, HashSet, VecDeque},
    hash::Hash,
};

use thiserror::Error;

use zebra_chain::transaction::{self, Transaction, UnminedTx, UnminedTxId};

use super::MempoolError;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

#[cfg(test)]
pub mod tests;

const MEMPOOL_SIZE: usize = 2;

/// The size limit for mempool transaction rejection lists.
///
/// > The size of RecentlyEvicted SHOULD never exceed `eviction_memory_entries` entries,
/// > which is the constant 40000.
///
/// https://zips.z.cash/zip-0401#specification
///
/// We use the specified value for all lists for consistency.
const MAX_EVICTION_MEMORY_ENTRIES: usize = 40_000;

/// Transactions rejected based on transaction authorizing data (scripts, proofs, signatures),
/// These rejections are only valid for the current tip.
///
/// Each committed block clears these rejections, because new blocks can supply missing inputs.
#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
#[allow(dead_code)]
pub enum ExactTipRejectionError {
    #[error("transaction did not pass consensus validation")]
    FailedVerification(#[from] zebra_consensus::error::TransactionError),
}

/// Transactions rejected based only on their effects (spends, outputs, transaction header).
/// These rejections are only valid for the current tip.
///
/// Each committed block clears these rejections, because new blocks can evict other transactions.
#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
#[allow(dead_code)]
pub enum SameEffectsTipRejectionError {
    #[error(
        "transaction rejected because another transaction in the mempool has already spent some of \
        its inputs"
    )]
    SpendConflict,
}

/// Transactions rejected based only on their effects (spends, outputs, transaction header).
/// These rejections are valid while the current chain continues to grow.
///
/// Rollbacks and network upgrades clear these rejections, because they can lower the tip height,
/// or change the consensus rules.
#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
#[allow(dead_code)]
pub enum SameEffectsChainRejectionError {
    #[error("best chain tip has reached transaction expiry height")]
    Expired,

    /// Otherwise valid transaction removed from mempool due to ZIP-401 random eviction.
    ///
    /// Consensus rule:
    /// > The txid (rather than the wtxid ...) is used even for version 5 transactions
    ///
    /// https://zips.z.cash/zip-0401#specification
    #[error("transaction evicted from the mempool due to ZIP-401 denial of service limits")]
    RandomlyEvicted,
}

#[derive(Default)]
pub struct Storage {
    /// The set of verified transactions in the mempool. This is a
    /// cache of size [`MEMPOOL_SIZE`].
    verified: VecDeque<UnminedTx>,

    /// The set of transactions rejected due to bad authorizations, or for other reasons,
    /// and their rejection reasons. These rejections only apply to the current tip.
    ///
    /// Only transactions with the exact `UnminedTxId` are invalid.
    tip_rejected_exact: HashMap<UnminedTxId, ExactTipRejectionError>,

    /// A set of transactions rejected for their effects, and their rejection reasons.
    /// These rejections only apply to the current tip.
    ///
    /// Any transaction with the same `transaction::Hash` is invalid.
    tip_rejected_same_effects: HashMap<transaction::Hash, SameEffectsTipRejectionError>,

    /// The set of transactions rejected for their effects, and their rejection reasons.
    /// These rejections apply until a rollback or network upgrade.
    ///
    /// Any transaction with the same `transaction::Hash` is invalid.
    chain_rejected_same_effects: HashMap<transaction::Hash, SameEffectsChainRejectionError>,
}

impl Storage {
    /// Insert a [`UnminedTx`] into the mempool.
    ///
    /// If its insertion results in evicting other transactions, they will be tracked
    /// as [`StorageRejectionError::RandomlyEvicted`].
    pub fn insert(&mut self, tx: UnminedTx) -> Result<UnminedTxId, MempoolError> {
        let tx_id = tx.id;

        // First, check if we have a cached rejection for this transaction.
        if let Some(error) = self.rejection_error(&tx.id) {
            return Err(error);
        }

        // If `tx` is already in the mempool, we don't change anything.
        //
        // Security: transactions must not get refreshed by new queries,
        // because that allows malicious peers to keep transactions live forever.
        if self.verified.contains(&tx) {
            return Err(MempoolError::InMempool);
        }

        // If `tx` spends an UTXO already spent by another transaction in the mempool or reveals a
        // nullifier already revealed by another transaction in the mempool, reject that
        // transaction.
        if self.check_spend_conflicts(&tx) {
            let error = SameEffectsTipRejectionError::SpendConflict;
            self.tip_rejected_same_effects
                .insert(tx.id.mined_id(), error.clone());
            return Err(error.into());
        }

        // Then, we insert into the pool.
        self.verified.push_front(tx);

        // Security: stop the transaction or rejection lists using too much memory

        // Once inserted, we evict transactions over the pool size limit in FIFO
        // order.
        //
        // TODO: use random weighted eviction as specified in ZIP-401 (#2780)
        if self.verified.len() > MEMPOOL_SIZE {
            for evicted_tx in self.verified.drain(MEMPOOL_SIZE..) {
                let _ = self.chain_rejected_same_effects.insert(
                    evicted_tx.id.mined_id(),
                    SameEffectsChainRejectionError::RandomlyEvicted,
                );
            }

            assert_eq!(self.verified.len(), MEMPOOL_SIZE);
        }

        self.limit_rejection_list_memory();

        Ok(tx_id)
    }

    /// Remove [`UnminedTx`]es from the mempool via exact [`UnminedTxId`].
    ///
    /// For v5 transactions, transactions are matched by WTXID, using both the:
    /// - non-malleable transaction ID, and
    /// - authorizing data hash.
    ///
    /// This matches the exact transaction, with identical blockchain effects, signatures, and proofs.
    ///
    /// Returns the number of transactions which were removed.
    ///
    /// Removes from the 'verified' set, if present.
    /// Maintains the order in which the other unmined transactions have been inserted into the mempool.
    ///
    /// Does not add or remove from the 'rejected' tracking set.
    #[allow(dead_code)]
    pub fn remove_exact(&mut self, exact_wtxids: &HashSet<UnminedTxId>) -> usize {
        let original_size = self.verified.len();

        self.verified.retain(|tx| !exact_wtxids.contains(&tx.id));

        original_size - self.verified.len()
    }

    /// Remove [`UnminedTx`]es from the mempool via non-malleable [`transaction::Hash`].
    ///
    /// For v5 transactions, transactions are matched by TXID,
    /// using only the non-malleable transaction ID.
    /// This matches any transaction with the same effect on the blockchain state,
    /// even if its signatures and proofs are different.
    ///
    /// Returns the number of transactions which were removed.
    ///
    /// Removes from the 'verified' set, if present.
    /// Maintains the order in which the other unmined transactions have been inserted into the mempool.
    ///
    /// Does not add or remove from the 'rejected' tracking set.
    pub fn remove_same_effects(&mut self, mined_ids: &HashSet<transaction::Hash>) -> usize {
        let original_size = self.verified.len();

        self.verified
            .retain(|tx| !mined_ids.contains(&tx.id.mined_id()));

        original_size - self.verified.len()
    }

    /// Clears the whole mempool storage.
    pub fn clear(&mut self) {
        self.verified.clear();
        self.tip_rejected_exact.clear();
        self.tip_rejected_same_effects.clear();
        self.chain_rejected_same_effects.clear();
    }

    /// Clears rejections that only apply to the current tip.
    pub fn clear_tip_rejections(&mut self) {
        self.tip_rejected_exact.clear();
        self.tip_rejected_same_effects.clear();
    }

    /// Clears rejections that only apply to the current tip.
    ///
    /// # Security
    ///
    /// This method must be called at the end of every method that adds rejections.
    /// Otherwise, peers could make our reject lists use a lot of RAM.
    fn limit_rejection_list_memory(&mut self) {
        // These lists are an optimisation - it's ok to totally clear them as needed.
        if self.tip_rejected_exact.len() > MAX_EVICTION_MEMORY_ENTRIES {
            self.tip_rejected_exact.clear();
        }
        if self.tip_rejected_same_effects.len() > MAX_EVICTION_MEMORY_ENTRIES {
            self.tip_rejected_same_effects.clear();
        }
        if self.chain_rejected_same_effects.len() > MAX_EVICTION_MEMORY_ENTRIES {
            self.chain_rejected_same_effects.clear();
        }
    }

    /// Returns the set of [`UnminedTxId`]s in the mempool.
    pub fn tx_ids(&self) -> impl Iterator<Item = UnminedTxId> + '_ {
        self.verified.iter().map(|tx| tx.id)
    }

    /// Returns the set of [`Transaction`]s in the mempool.
    pub fn transactions(&self) -> impl Iterator<Item = &UnminedTx> {
        self.verified.iter()
    }

    /// Returns the number of transactions in the mempool.
    #[allow(dead_code)]
    pub fn transaction_count(&self) -> usize {
        self.verified.len()
    }

    /// Returns the set of [`Transaction`]s with exactly matching `tx_ids` in the mempool.
    ///
    /// This matches the exact transaction, with identical blockchain effects, signatures, and proofs.
    pub fn transactions_exact(
        &self,
        tx_ids: HashSet<UnminedTxId>,
    ) -> impl Iterator<Item = &UnminedTx> {
        self.verified
            .iter()
            .filter(move |tx| tx_ids.contains(&tx.id))
    }

    /// Returns `true` if a [`UnminedTx`] exactly matching an [`UnminedTxId`] is in
    /// the mempool.
    ///
    /// This matches the exact transaction, with identical blockchain effects, signatures, and proofs.
    pub fn contains_transaction_exact(&self, txid: &UnminedTxId) -> bool {
        self.verified.iter().any(|tx| &tx.id == txid)
    }

    /// Returns the number of rejected [`UnminedTxId`]s or [`transaction::Hash`]es.
    #[allow(dead_code)]
    pub fn rejected_transaction_count(&self) -> usize {
        self.tip_rejected_exact.len()
            + self.tip_rejected_same_effects.len()
            + self.chain_rejected_same_effects.len()
    }

    /// Returns `true` if a [`UnminedTx`] matching an [`UnminedTxId`] is in
    /// any mempool rejected list.
    ///
    /// This matches transactions based on each rejection list's matching rule.
    pub fn rejection_error(&self, txid: &UnminedTxId) -> Option<MempoolError> {
        if let Some(error) = self.tip_rejected_exact.get(txid) {
            return Some(error.clone().into());
        }

        if let Some(error) = self.tip_rejected_same_effects.get(&txid.mined_id()) {
            return Some(error.clone().into());
        }

        if let Some(error) = self.chain_rejected_same_effects.get(&txid.mined_id()) {
            return Some(error.clone().into());
        }

        None
    }

    /// Returns the set of [`UnminedTxId`]s matching `tx_ids` in the rejected list.
    ///
    /// This matches transactions based on each rejection list's matching rule.
    pub fn rejected_transactions(
        &self,
        tx_ids: HashSet<UnminedTxId>,
    ) -> impl Iterator<Item = UnminedTxId> + '_ {
        tx_ids
            .into_iter()
            .filter(move |txid| self.contains_rejected(txid))
    }

    /// Returns `true` if a [`UnminedTx`] matching the supplied [`UnminedTxId`] is in
    /// the mempool rejected list.
    ///
    /// This matches transactions based on each rejection list's matching rule.
    pub fn contains_rejected(&self, txid: &UnminedTxId) -> bool {
        self.tip_rejected_exact.contains_key(txid)
            || self
                .tip_rejected_same_effects
                .contains_key(&txid.mined_id())
            || self
                .chain_rejected_same_effects
                .contains_key(&txid.mined_id())
    }

    /// Checks if the `tx` transaction has spend conflicts with another transaction in the mempool.
    ///
    /// Two transactions have a spend conflict if they spent the same UTXO or if they reveal the
    /// same nullifier.
    fn check_spend_conflicts(&self, tx: &UnminedTx) -> bool {
        self.has_spend_conflicts(tx, Transaction::spent_outpoints)
            || self.has_spend_conflicts(tx, Transaction::sprout_nullifiers)
            || self.has_spend_conflicts(tx, Transaction::sapling_nullifiers)
            || self.has_spend_conflicts(tx, Transaction::orchard_nullifiers)
    }

    /// Checks if the `tx` transaction has any spend conflicts with the transactions in the mempool
    /// for the provided output type obtained through the `extractor`.
    fn has_spend_conflicts<'slf, 'tx, Extractor, Outputs>(
        &'slf self,
        tx: &'tx UnminedTx,
        extractor: Extractor,
    ) -> bool
    where
        'slf: 'tx,
        Extractor: Fn(&'tx Transaction) -> Outputs,
        Outputs: IntoIterator,
        Outputs::Item: Eq + Hash + 'tx,
    {
        // TODO: This algorithm should be improved to avoid a performance impact when the mempool
        // size is increased (#2784).
        let new_outputs: HashSet<_> = extractor(&tx.transaction).into_iter().collect();

        self.verified
            .iter()
            .flat_map(|tx| extractor(&tx.transaction))
            .any(|output| new_outputs.contains(&output))
    }
}
