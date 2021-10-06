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

/// Transactions rejected based on transaction authorizing data (scripts, proofs, signatures),
/// or for other reasons.
#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
#[allow(dead_code)]
pub enum ExactRejectionError {
    #[error("transaction did not pass consensus validation")]
    FailedVerification(#[from] zebra_consensus::error::TransactionError),
}

/// Transactions rejected based only on their effects (spends, outputs, transaction header).
#[derive(Error, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
#[allow(dead_code)]
pub enum SameEffectsRejectionError {
    #[error(
        "transaction rejected because another transaction in the mempool has already spent some of \
        its inputs"
    )]
    SpendConflict,

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
    /// and their rejection reasons.
    ///
    /// Only transactions with the exact `UnminedTxId` are invalid.
    rejected_exact: HashMap<UnminedTxId, ExactRejectionError>,

    /// The set of transactions rejected for their effects, and their rejection reasons.
    ///
    /// Any transaction with the same `transaction::Hash` is invalid.
    rejected_same_effects: HashMap<transaction::Hash, SameEffectsRejectionError>,
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
            self.rejected_same_effects
                .insert(tx.id.mined_id(), SameEffectsRejectionError::SpendConflict);
            return Err(SameEffectsRejectionError::SpendConflict.into());
        }

        // Then, we insert into the pool.
        self.verified.push_front(tx);

        // Once inserted, we evict transactions over the pool size limit in FIFO
        // order.
        //
        // TODO: use random weighted eviction as specified in ZIP-401 (#2780)
        if self.verified.len() > MEMPOOL_SIZE {
            for evicted_tx in self.verified.drain(MEMPOOL_SIZE..) {
                let _ = self.rejected_same_effects.insert(
                    evicted_tx.id.mined_id(),
                    SameEffectsRejectionError::RandomlyEvicted,
                );
            }

            assert_eq!(self.verified.len(), MEMPOOL_SIZE);
        }

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

    /// Returns `true` if a [`UnminedTx`] matching the supplied [`UnminedTxId`] is in
    /// the mempool rejected list.
    pub fn contains_rejected(&self, txid: &UnminedTxId) -> bool {
        self.rejected_exact.contains_key(txid)
            || self.rejected_same_effects.contains_key(&txid.mined_id())
    }

    /// Returns `true` if a [`UnminedTx`] matching an [`UnminedTxId`] is in
    /// the mempool rejected list.
    pub fn rejection_error(&self, txid: &UnminedTxId) -> Option<MempoolError> {
        if let Some(exact_error) = self.rejected_exact.get(txid) {
            return Some(exact_error.clone().into());
        }

        if let Some(effects_error) = self.rejected_same_effects.get(&txid.mined_id()) {
            return Some(effects_error.clone().into());
        }

        None
    }

    /// Returns the set of [`UnminedTxId`]s matching ids in the rejected list.
    pub fn rejected_transactions(&self, tx_ids: HashSet<UnminedTxId>) -> Vec<UnminedTxId> {
        tx_ids
            .into_iter()
            .filter(|txid| self.contains_rejected(txid))
            .collect()
    }

    /// Returns the number of rejected [`UnminedTxId`]s or [`transaction::Hash`]es.
    #[allow(dead_code)]
    pub fn rejected_transaction_count(&self) -> usize {
        self.rejected_exact.len() + self.rejected_same_effects.len()
    }

    /// Clears the whole mempool storage.
    pub fn clear(&mut self) {
        self.verified.clear();
        self.rejected_exact.clear();
        self.rejected_same_effects.clear();
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
