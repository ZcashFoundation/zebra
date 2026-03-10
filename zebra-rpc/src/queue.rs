//! Transaction Queue.
//!
//! All transactions that are sent from RPC methods should be added to this queue for retries.
//! Transactions can fail to be inserted to the mempool immediately by different reasons,
//! like having not mined utxos.
//!
//! The [`Queue`] is just an `IndexMap` of transactions with insertion date.
//! We use this data type because we want the transactions in the queue to be in order.
//! The [`Runner`] component will do the processing in it's [`Runner::run()`] method.

use std::{collections::HashSet, sync::Arc};

use chrono::Duration;
use indexmap::IndexMap;
use tokio::{
    sync::broadcast::{self, error::TryRecvError},
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

use zebra_state::{MinedTx, ReadRequest, ReadResponse};

#[cfg(test)]
mod tests;

/// The approximate target number of blocks a transaction can be in the queue.
const NUMBER_OF_BLOCKS_TO_EXPIRE: i64 = 5;

/// Size of the queue and channel.
pub const CHANNEL_AND_QUEUE_CAPACITY: usize = 20;

/// The height to use in spacing calculation if we don't have a chain tip.
const NO_CHAIN_TIP_HEIGHT: Height = Height(1);

#[derive(Clone, Debug)]
/// The queue is a container of transactions that are going to be
/// sent to the mempool again.
pub struct Queue {
    transactions: IndexMap<UnminedTxId, (Arc<Transaction>, Instant)>,
}

#[derive(Debug)]
/// The runner will make the processing of the transactions in the queue.
pub struct Runner {
    queue: Queue,
    receiver: broadcast::Receiver<UnminedTx>,
    tip_height: Height,
}

impl Queue {
    /// Start a new queue
    pub fn start() -> (Runner, broadcast::Sender<UnminedTx>) {
        let (sender, receiver) = broadcast::channel(CHANNEL_AND_QUEUE_CAPACITY);

        let queue = Queue {
            transactions: IndexMap::new(),
        };

        let runner = Runner {
            queue,
            receiver,
            tip_height: Height(0),
        };

        (runner, sender)
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
        self.transactions.swap_remove(&unmined_id);
    }

    /// Remove the oldest transaction from the queue.
    pub fn remove_first(&mut self) {
        self.transactions.shift_remove_index(0);
    }
}

impl Runner {
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

    /// Update the `tip_height` field with a new height.
    pub fn update_tip_height(&mut self, height: Height) {
        self.tip_height = height;
    }

    /// Retry sending to mempool if needed.
    ///
    /// Creates a loop that will run each time a new block is mined.
    /// In this loop, get the transactions that are in the queue and:
    /// - Check if they are now in the mempool and if so, delete the transaction from the queue.
    /// - Check if the transaction is now part of a block in the state and if so,
    ///   delete the transaction from the queue.
    /// - With the transactions left in the queue, retry sending them to the mempool ignoring
    ///   the result of this operation.
    ///
    /// Additionally, each iteration of the above loop, will receive and insert to the queue
    /// transactions that are pending in the channel.
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
        loop {
            // if we don't have a chain use `NO_CHAIN_TIP_HEIGHT` to get block spacing
            let tip_height = match tip.best_tip_height() {
                Some(height) => height,
                _ => NO_CHAIN_TIP_HEIGHT,
            };

            // get spacing between blocks
            let spacing = NetworkUpgrade::target_spacing_for_height(&network, tip_height);

            // sleep until the next block
            tokio::time::sleep(spacing.to_std().expect("should never be less than zero")).await;

            // get transactions from the channel
            loop {
                let tx = match self.receiver.try_recv() {
                    Ok(tx) => tx,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Lagged(skipped_count)) => {
                        tracing::info!("sendrawtransaction queue was full: skipped {skipped_count} transactions");
                        continue;
                    }
                    Err(TryRecvError::Closed) => {
                        tracing::info!(
                            "sendrawtransaction queue was closed: is Zebra shutting down?"
                        );
                        return;
                    }
                };

                self.queue.insert(tx.clone());
            }

            // skip some work if stored tip height is the same as the one arriving
            // TODO: check tip block hashes instead, so we always retry when there is a chain fork (these are rare)
            if tip_height != self.tip_height {
                // update the chain tip
                self.update_tip_height(tip_height);

                if !self.queue.transactions().is_empty() {
                    // remove what is expired
                    self.remove_expired(spacing);

                    // remove if any of the queued transactions is now in the mempool
                    let in_mempool =
                        Self::check_mempool(mempool.clone(), self.transactions_as_hash_set()).await;
                    self.remove_committed(in_mempool);

                    // remove if any of the queued transactions is now in the state
                    let in_state =
                        Self::check_state(state.clone(), self.transactions_as_hash_set()).await;
                    self.remove_committed(in_state);

                    // retry what is left in the queue
                    Self::retry(mempool.clone(), self.transactions_as_vec()).await;
                }
            }
        }
    }

    /// Remove transactions that are expired according to number of blocks and current spacing between blocks.
    fn remove_expired(&mut self, spacing: Duration) {
        // Have some extra time to make sure we re-submit each transaction `NUMBER_OF_BLOCKS_TO_EXPIRE`
        // times, as the main loop also takes some time to run.
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

            // ignore any error coming from the mempool
            let mempool_response = mempool.oneshot(request).await;
            if let Ok(Response::Transactions(txs)) = mempool_response {
                for tx in txs {
                    response.insert(tx.id);
                }
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

            // ignore any error coming from the state
            let state_response = state.clone().oneshot(request).await;
            if let Ok(ReadResponse::Transaction(Some(MinedTx { tx, .. }))) = state_response {
                response.insert(tx.unmined_id());
            }
        }

        response
    }

    /// Retry sending given transactions to mempool.
    ///
    /// Returns the transaction ids that were retried.
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

            // Send to mempool and ignore any error
            let _ = mempool.clone().oneshot(request).await;

            // return what we retried but don't delete from the queue,
            // we might retry again in a next call.
            retried.insert(unmined.id);
        }
        retried
    }
}
