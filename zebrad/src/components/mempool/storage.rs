use std::collections::{HashMap, HashSet, VecDeque};

use zebra_chain::transaction::{UnminedTx, UnminedTxId};

const MEMPOOL_SIZE: usize = 2;

#[derive(Debug)]
pub enum State {
    /// Rejected because verification failed.
    Invalid,
    /// An otherwise valid mempool transaction was mined into a block, therefore
    /// no longer belongs in the mempool.
    Confirmed,
    /// Stayed in mempool for too long without being mined.
    // TODO(2021-08-20): set expiration at 2 weeks? This is what Bitcoin does.
    Expired,
    /// Otherwise valid transaction removed from mempool, say because of LRU
    /// cache replacement.
    Evicted,
}

#[derive(Default)]
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
    pub fn insert(&mut self, tx: UnminedTx) -> Result<UnminedTxId, &State> {
        let tx_id = tx.id.clone();

        // First, check if we should reject this transaction.
        if self.rejected.contains_key(&tx.id) {
            return Err(self.rejected.get(&tx.id).unwrap());
        }

        // Then, we insert into the pool.
        self.verified.push_front(tx);

        // Once inserted, we evict transactions over the pool size limit in FIFO
        // order.
        for evicted_tx in self.verified.drain(MEMPOOL_SIZE..) {
            let _ = self.rejected.insert(evicted_tx.id, State::Evicted);
        }

        assert_eq!(self.verified.len(), MEMPOOL_SIZE);

        Ok(tx_id)
    }

    /// Returns `true` if a [`UnminedTx`] matching an [`UnminedTxId`] is in
    /// the mempool.
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
