//! Transaction Queue.
//!
//! All transactions that are sent from RPC methods should be added to this queue for retries.
//! Transactions can fail to be inserted to the mempool inmediatly by different reasons,
//! like having not mined utxos.
//!
//! The [`Queue`] is just a `HashMap` of transactions with insertion date.
//! The [`Runner`] component will do the processing in it's [`Runner::run()`] method.

use std::{collections::HashSet, sync::Arc};

use chrono::Duration;
use indexmap::IndexMap;
use tokio::{
    sync::broadcast::{channel, Receiver, Sender},
    time::Instant,
};

use tower::{Service, ServiceExt};

use zebra_chain::{
    block::Height,
    chain_tip::ChainTip,
    parameters::{Network, NetworkUpgrade},
    transaction::{Transaction, UnminedTx, UnminedTxId},
};
use zebra_node_services::{
    mempool::{Gossip, Request, Response},
    BoxError,
};

use zebra_state::{ReadRequest, ReadResponse};

#[cfg(test)]
mod tests;

/// The number of blocks a transaction can be in the queue.
const NUMBER_OF_BLOCKS_TO_EXPIRE: i64 = 3;

/// Size of the queue and channel. Suggested valus is equal to
/// `mempool::downloads::MAX_INBOUND_CONCURRENCY`
const CHANNEL_AND_QUEUE_CAPACITY: usize = 10;

#[derive(Clone, Debug)]
/// The queue itself
pub struct Queue {
    transactions: IndexMap<UnminedTxId, (Arc<Transaction>, Instant)>,
}

#[derive(Debug)]
/// The runner
pub struct Runner {
    queue: Queue,
    sender: Sender<Option<UnminedTx>>,
}

impl Queue {
    /// Start a new queue
    pub fn start() -> Runner {
        let (sender, _receiver) = channel(CHANNEL_AND_QUEUE_CAPACITY);

        let queue = Queue {
            transactions: IndexMap::new(),
        };

        Runner { queue, sender }
    }

    /// Get the transactions in the queue.
    pub fn transactions(&self) -> IndexMap<UnminedTxId, (Arc<Transaction>, Instant)> {
        self.transactions.clone()
    }

    /// Insert a transaction to the queue.
    pub fn insert(&mut self, unmined_tx: UnminedTx) {
        self.transactions
            .insert(unmined_tx.id, (unmined_tx.transaction, Instant::now()));

        // remove if queue is over capacity
        if self.transactions.len() > CHANNEL_AND_QUEUE_CAPACITY {
            self.remove_first();
        }
    }

    /// Remove a transaction from the queue.
    pub fn remove(&mut self, unmined_id: UnminedTxId) {
        self.transactions.remove(&unmined_id);
    }

    /// Remove the oldest transaction from the queue.
    pub fn remove_first(&mut self) {
        self.transactions.shift_remove_index(0);
    }
}

impl Runner {
    /// Access the sender field of the runner.
    pub fn sender(&self) -> Sender<Option<UnminedTx>> {
        self.sender.clone()
    }

    /// Create a new receiver.
    pub fn receiver(&self) -> Receiver<Option<UnminedTx>> {
        self.sender.subscribe()
    }

    /// Access the queue.
    pub fn queue(&self) -> Queue {
        self.queue.clone()
    }

    /// Get the queue transactions as a `HashSet` of unmined ids.
    fn transactions_as_hash_set(&self) -> HashSet<UnminedTxId> {
        let transactions = self.queue.transactions();
        transactions.iter().map(|t| *t.0).collect()
    }

    /// Get the queue transactions as a `Vec` of transactions.
    fn transactions_as_vec(&self) -> Vec<Arc<Transaction>> {
        let transactions = self.queue.transactions();
        transactions.iter().map(|t| t.1 .0.clone()).collect()
    }

    /// Retry sending to memempool if needed.
    pub async fn run<Mempool, State, Tip>(
        mut self,
        mempool: Mempool,
        state: State,
        tip: Tip,
        network: Network,
    ) where
        Mempool: Service<Request, Response = Response, Error = BoxError> + Clone + 'static,
        State: Service<ReadRequest, Response = ReadResponse, Error = zebra_state::BoxError>
            + Clone
            + Send
            + Sync
            + 'static,
        Tip: ChainTip + Clone + Send + Sync + 'static,
    {
        // If we don't have a chain use height 1 to get block spacing.
        let tip_height = match tip.best_tip_height() {
            Some(height) => height,
            _ => Height(1),
        };

        // get spacing between blocks
        let spacing = NetworkUpgrade::target_spacing_for_height(network, tip_height);

        let mut receiver = self.sender.subscribe();

        loop {
            // sleep until the next block
            tokio::time::sleep(spacing.to_std().expect("should never be less than zero")).await;

            // get transactions from the channel
            while let Ok(Some(tx)) = receiver.try_recv() {
                let _ = &self.queue.insert(tx.clone());
            }

            // remove what is expired
            self.remove_expired(spacing);

            // remove if any of the queued transactions is now in the mempool
            let in_mempool =
                Self::check_mempool(mempool.clone(), self.transactions_as_hash_set()).await;
            self.remove_committed(in_mempool);

            // remove if any of the queued transactions is now in the state
            let in_state = Self::check_state(state.clone(), self.transactions_as_hash_set()).await;
            self.remove_committed(in_state);

            // retry what is left in the queue
            let _retried = Self::retry(mempool.clone(), self.transactions_as_vec()).await;
        }
    }

    /// Remove transactions that are expired according to number of blocks and current spacing between blocks.
    fn remove_expired(&mut self, spacing: Duration) {
        // To make sure we re-submit each transaction `NUMBER_OF_BLOCKS_TO_EXPIRE` times,
        // as the main loop also takes some time to run.
        let extra_time = Duration::seconds(5);

        let duration_to_expire =
            Duration::seconds(NUMBER_OF_BLOCKS_TO_EXPIRE * spacing.num_seconds()) + extra_time;
        let transactions = self.queue.transactions();
        let now = Instant::now();

        for tx in transactions.iter() {
            let tx_time =
                tx.1 .1
                    .checked_add(
                        duration_to_expire
                            .to_std()
                            .expect("should never be less than zero"),
                    )
                    .expect("this is low numbers, should always be inside bounds");

            if now > tx_time {
                self.queue.remove(*tx.0);
            }
        }
    }

    /// Remove transactions from the queue that had been inserted to the state or the mempool.
    fn remove_committed(&mut self, to_remove: HashSet<UnminedTxId>) {
        for r in to_remove {
            self.queue.remove(r);
        }
    }

    /// Check the mempool for given transactions.
    ///
    /// Returns transactions that are in the mempool.
    async fn check_mempool<Mempool>(
        mempool: Mempool,
        transactions: HashSet<UnminedTxId>,
    ) -> HashSet<UnminedTxId>
    where
        Mempool: Service<Request, Response = Response, Error = BoxError> + Clone + 'static,
    {
        let mut response = HashSet::new();

        if !transactions.is_empty() {
            let request = Request::TransactionsById(transactions);

            let mempool_response = mempool
                .oneshot(request)
                .await
                .expect("Requesting transactions should not panic");

            match mempool_response {
                Response::Transactions(txs) => {
                    for tx in txs {
                        response.insert(tx.id);
                    }
                }
                _ => unreachable!("TransactionsById always respond with at least an empty vector"),
            }
        }

        response
    }

    /// Check the state for given transactions.
    ///
    /// Returns transactions that are in the state.
    async fn check_state<State>(
        state: State,
        transactions: HashSet<UnminedTxId>,
    ) -> HashSet<UnminedTxId>
    where
        State: Service<ReadRequest, Response = ReadResponse, Error = zebra_state::BoxError>
            + Clone
            + Send
            + Sync
            + 'static,
    {
        let mut response = HashSet::new();

        for t in transactions {
            let request = ReadRequest::Transaction(t.mined_id());

            let state_response = state
                .clone()
                .oneshot(request)
                .await
                .expect("Requesting transactions should not panic");

            match state_response {
                ReadResponse::Transaction(Some(tx)) => {
                    response.insert(tx.0.unmined_id());
                }
                ReadResponse::Transaction(None) => {}
                _ => unreachable!("ReadResponse::Transaction is always some or none"),
            }
        }

        response
    }

    /// Retry sending given transactions to mempool.
    ///
    /// Returns the transaction ids what were retried.
    async fn retry<Mempool>(
        mempool: Mempool,
        transactions: Vec<Arc<Transaction>>,
    ) -> HashSet<UnminedTxId>
    where
        Mempool: Service<Request, Response = Response, Error = BoxError> + Clone + 'static,
    {
        let mut retried = HashSet::new();

        for tx in transactions {
            let unmined = UnminedTx::from(tx);
            let gossip = Gossip::Tx(unmined.clone());
            let request = Request::Queue(vec![gossip]);

            // Send to memmpool and ignore any error
            let _ = mempool.clone().oneshot(request).await;

            // retrurn what we retried but don't delete from the queue,
            // we might retry again in a next call.
            retried.insert(unmined.id);
        }
        retried
    }
}
