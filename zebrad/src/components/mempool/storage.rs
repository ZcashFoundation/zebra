use std::collections::{HashMap, HashSet, VecDeque};

use zebra_chain::{
    block,
    transaction::{UnminedTx, UnminedTxId},
};
use zebra_consensus::error::TransactionError;

use super::MempoolError;

#[allow(dead_code)]
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
    Victim,
}

#[derive(Clone, Default)]
pub struct Storage {
    /// The set of verified transactions in the mempool.  Currently this is a
    /// cache of size 2.
    verified: VecDeque<UnminedTx>,
    /// The set of rejected transactions by id, and their rejection reasons.
    rejected: HashMap<UnminedTxId, State>,
}

impl Storage {
    /// Insert a [`UnminedTx`] into the mempool.
    ///
    /// If its insertion results in evicting other transactions, they will be tracked
    /// as [`State::Evicted`].
    #[allow(dead_code)]
    pub fn insert(&mut self, tx: UnminedTx) -> Result<UnminedTxId, MempoolError> {
        let tx_id = tx.id.clone();

        // First, check if we should reject this transaction.
        if self.rejected.contains_key(&tx.id) {
            return Err(match self.rejected.get(&tx.id).unwrap() {
                State::Invalid(e) => MempoolError::Invalid(e.clone()),
                State::Expired => MempoolError::Expired,
                State::Confirmed(block_hash) => MempoolError::InBlock(*block_hash),
                State::Victim => MempoolError::Victim,
                State::LowFee => MempoolError::LowFee,
            });
        }

        // If `tx` is already in the mempool, we don't change anything.
        //
        // TODO: Should it get bumped up to the top?
        if self.verified.contains(&tx) {
            return Err(MempoolError::InMempool);
        }

        // Then, we insert into the pool.
        self.verified.push_front(tx);

        // Once inserted, we evict transactions over the pool size limit in FIFO
        // order.
        for evicted_tx in self.verified.drain(MEMPOOL_SIZE..) {
            let _ = self.rejected.insert(evicted_tx.id, State::Victim);
        }

        assert_eq!(self.verified.len(), MEMPOOL_SIZE);

        Ok(tx_id)
    }

    /// Returns `true` if a [`UnminedTx`] matching an [`UnminedTxId`] is in
    /// the mempool.
    #[allow(dead_code)]
    pub fn contains(self, txid: &UnminedTxId) -> bool {
        match self.verified.iter().find(|tx| &tx.id == txid) {
            Some(_) => true,
            None => false,
        }
    }

    /// Returns the set of [`UnminedTxId`]s in the mempool.
    pub fn tx_ids(self) -> Vec<UnminedTxId> {
        self.verified.iter().map(|tx| tx.id).collect()
    }

    /// Returns the set of [`Transaction`]s matching ids in the mempool.
    pub fn transactions(self, tx_ids: HashSet<UnminedTxId>) -> Vec<UnminedTx> {
        self.verified
            .into_iter()
            .filter(|tx| tx_ids.contains(&tx.id))
            .collect()
    }
}
