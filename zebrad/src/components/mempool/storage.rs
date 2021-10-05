use std::{
    collections::{HashMap, HashSet, VecDeque},
    hash::Hash,
};

use zebra_chain::transaction::{self, Transaction, UnminedTx, UnminedTxId};
use zebra_consensus::error::TransactionError;

use super::MempoolError;

#[cfg(test)]
pub mod tests;

const MEMPOOL_SIZE: usize = 2;

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum State {
    /// Rejected because verification failed.
    Invalid(TransactionError),
    /// Rejected because it has a spend conflict with another transaction already in the mempool.
    SpendConflict,
    /// Stayed in mempool for too long without being mined.
    Expired,
    /// Otherwise valid transaction removed from mempool, say because of FIFO
    /// (first in, first out) policy.
    Excess,
}

#[derive(Default)]
pub struct Storage {
    /// The set of verified transactions in the mempool. This is a
    /// cache of size [`MEMPOOL_SIZE`].
    verified: VecDeque<UnminedTx>,
    /// The set of rejected transactions by id, and their rejection reasons.
    rejected: HashMap<UnminedTxId, State>,
}

impl Storage {
    /// Insert a [`UnminedTx`] into the mempool.
    ///
    /// If its insertion results in evicting other transactions, they will be tracked
    /// as [`State::Excess`].
    pub fn insert(&mut self, tx: UnminedTx) -> Result<UnminedTxId, MempoolError> {
        let tx_id = tx.id;

        // First, check if we should reject this transaction.
        if self.rejected.contains_key(&tx.id) {
            return Err(match self.rejected.get(&tx.id).unwrap() {
                State::Invalid(e) => MempoolError::Invalid(e.clone()),
                State::Expired => MempoolError::Expired,
                State::Excess => MempoolError::Excess,
                State::SpendConflict => MempoolError::SpendConflict,
            });
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
            self.rejected.insert(tx.id, State::SpendConflict);
            return Err(MempoolError::Rejected);
        }

        // Then, we insert into the pool.
        self.verified.push_front(tx);

        // Once inserted, we evict transactions over the pool size limit in FIFO
        // order.
        if self.verified.len() > MEMPOOL_SIZE {
            for evicted_tx in self.verified.drain(MEMPOOL_SIZE..) {
                let _ = self.rejected.insert(evicted_tx.id, State::Excess);
            }

            assert_eq!(self.verified.len(), MEMPOOL_SIZE);
        }

        Ok(tx_id)
    }

    /// Returns `true` if a [`UnminedTx`] matching an [`UnminedTxId`] is in
    /// the mempool.
    pub fn contains(&self, txid: &UnminedTxId) -> bool {
        self.verified.iter().any(|tx| &tx.id == txid)
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
    pub fn tx_ids(&self) -> Vec<UnminedTxId> {
        self.verified.iter().map(|tx| tx.id).collect()
    }

    /// Returns the set of [`Transaction`]s matching `tx_ids` in the mempool.
    pub fn transactions(&self, tx_ids: HashSet<UnminedTxId>) -> Vec<UnminedTx> {
        self.verified
            .iter()
            .filter(|tx| tx_ids.contains(&tx.id))
            .cloned()
            .collect()
    }

    /// Returns the set of [`Transaction`]s in the mempool.
    pub fn transactions_all(&self) -> Vec<UnminedTx> {
        self.verified.iter().cloned().collect()
    }

    /// Returns `true` if a [`UnminedTx`] matching an [`UnminedTxId`] is in
    /// the mempool rejected list.
    pub fn contains_rejected(&self, txid: &UnminedTxId) -> bool {
        self.rejected.contains_key(txid)
    }

    /// Returns the set of [`UnminedTxId`]s matching ids in the rejected list.
    pub fn rejected_transactions(&self, tx_ids: HashSet<UnminedTxId>) -> Vec<UnminedTxId> {
        tx_ids
            .into_iter()
            .filter(|tx| self.rejected.contains_key(tx))
            .collect()
    }

    /// Clears the whole mempool storage.
    pub fn clear(&mut self) {
        self.verified.clear();
        self.rejected.clear();
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
