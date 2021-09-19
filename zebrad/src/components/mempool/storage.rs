use std::collections::{HashMap, HashSet, VecDeque};

use zebra_chain::{
    block,
    transaction::{UnminedTx, UnminedTxId},
};
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
    /// An otherwise valid mempool transaction was mined into a block, therefore
    /// no longer belongs in the mempool.
    Confirmed(block::Hash),
    /// Stayed in mempool for too long without being mined.
    // TODO(2021-08-20): set expiration at 2 weeks? This is what Bitcoin does.
    Expired,
    /// Transaction fee is too low for the current mempool state.
    LowFee,
    /// Otherwise valid transaction removed from mempool, say because of FIFO
    /// (first in, first out) policy.
    Excess,
}

#[derive(Clone, Default)]
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
    #[allow(dead_code)]
    pub fn insert(&mut self, tx: UnminedTx) -> Result<UnminedTxId, MempoolError> {
        let tx_id = tx.id;

        // First, check if we should reject this transaction.
        if self.rejected.contains_key(&tx.id) {
            return Err(match self.rejected.get(&tx.id).unwrap() {
                State::Invalid(e) => MempoolError::Invalid(e.clone()),
                State::Expired => MempoolError::Expired,
                State::Confirmed(block_hash) => MempoolError::InBlock(*block_hash),
                State::Excess => MempoolError::Excess,
                State::LowFee => MempoolError::LowFee,
            });
        }

        // If `tx` is already in the mempool, we don't change anything.
        //
        // Security: transactions must not get refreshed by new queries,
        // because that allows malicious peers to keep transactions live forever.
        if self.verified.contains(&tx) {
            return Err(MempoolError::InMempool);
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
    #[allow(dead_code)]
    pub fn contains(&self, txid: &UnminedTxId) -> bool {
        self.verified.iter().any(|tx| &tx.id == txid)
    }

    /// Remove a [`UnminedTx`] from the mempool via [`UnminedTxId`].  Returns
    /// whether the transaction was present.
    ///
    /// Removes from the 'verified' set, does not remove from the 'rejected'
    /// tracking set, if present. Maintains the order in which the other unmined
    /// transactions have been inserted into the mempool.
    #[allow(dead_code)]
    pub fn remove(&mut self, txid: &UnminedTxId) -> Option<UnminedTx> {
        // If the txid exists in the verified set and is then deleted,
        // `retain()` removes it and returns `Some(UnminedTx)`. If it's not
        // present and nothing changes, returns `None`.

        match self.verified.clone().iter().find(|tx| &tx.id == txid) {
            Some(tx) => {
                self.verified.retain(|tx| &tx.id != txid);
                Some(tx.clone())
            }
            None => None,
        }
    }

    /// Returns the set of [`UnminedTxId`]s in the mempool.
    pub fn tx_ids(&self) -> Vec<UnminedTxId> {
        self.verified.iter().map(|tx| tx.id).collect()
    }

    /// Returns the set of [`Transaction`]s matching ids in the mempool.
    pub fn transactions(&self, tx_ids: HashSet<UnminedTxId>) -> Vec<UnminedTx> {
        self.verified
            .iter()
            .filter(|tx| tx_ids.contains(&tx.id))
            .cloned()
            .collect()
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
}
