//! Transaction Queue.
//!
//! All transactions that are sent from RPC methods should be added to this queue for retries.
//! Transactions can fail to be inserted to the mempool inmediatly to different reasons,
//! like having not mined utxos.
//!
//! The queue is a `HashMap` which can be shared by a `Listener` and a `Runner` component.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use chrono::Duration;
use tokio::{
    sync::mpsc::{channel, Receiver, Sender},
    time::Instant,
};

use tower::{Service, ServiceExt};

use zebra_chain::{
    chain_tip::ChainTip,
    parameters::NetworkUpgrade,
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

#[derive(Clone, Debug)]
/// The queue itself
pub struct Queue {
    transactions: HashMap<UnminedTxId, (Arc<Transaction>, Instant)>,
}

#[derive(Clone, Debug)]
/// The runner
pub struct Runner {
    queue: Arc<Mutex<Queue>>,
    sender: Sender<Option<UnminedTx>>,
}

/// The listener
pub struct Listener {
    queue: Arc<Mutex<Queue>>,
    receiver: Receiver<Option<UnminedTx>>,
}

impl Queue {
    /// Start a new queue
    pub fn start() -> (Listener, Runner) {
        let (sender, receiver) = channel(10);

        let queue = Arc::new(Mutex::new(Queue {
            transactions: HashMap::new(),
        }));

        let runner = Runner {
            queue: queue.clone(),
            sender,
        };

        let listener = Listener { queue, receiver };

        (listener, runner)
    }

    /// Get the transactions in the queue
    pub fn transactions(&self) -> HashMap<UnminedTxId, (Arc<Transaction>, Instant)> {
        self.transactions.clone()
    }

    /// Insert a transaction to the queue
    pub fn insert(&mut self, unmined_tx: UnminedTx) {
        self.transactions
            .insert(unmined_tx.id, (unmined_tx.transaction, Instant::now()));
    }

    /// Remove a transaction from the queue
    pub fn remove(&mut self, unmined_id: UnminedTxId) {
        self.transactions.remove(&unmined_id);
    }
}

impl Listener {
    /// Listen for transaction and insert them to the queue
    pub async fn listen(&mut self) {
        loop {
            if let Some(Some(tx)) = self.receiver.recv().await {
                self.queue
                    .lock()
                    .expect("queue mutex should be unpoisoned")
                    .insert(tx);
            }
        }
    }
}

impl Runner {
    /// Access the sender field of the runner.
    pub fn sender(&self) -> Sender<Option<UnminedTx>> {
        self.sender.clone()
    }

    /// Access the mutable queue.
    pub fn queue(&self) -> Arc<Mutex<Queue>> {
        self.queue.clone()
    }

    /// Get the queue transactions as a `HashSet` of unmined ids.
    fn transactions_as_hash_set(&self) -> HashSet<UnminedTxId> {
        let transactions = self
            .queue
            .lock()
            .expect("queue mutex should be unpoisoned")
            .transactions();
        transactions.iter().map(|t| *t.0).collect()
    }

    /// Get the queue transactions as a `Vec` of transactions.
    fn transactions_as_vec(&self) -> Vec<Arc<Transaction>> {
        let transactions = self
            .queue
            .lock()
            .expect("queue mutex should be unpoisoned")
            .transactions();
        transactions.iter().map(|t| t.1 .0.clone()).collect()
    }

    /// Retry sending to memempool if needed.
    pub async fn run<Mempool, State, Tip>(self, mempool: Mempool, state: State, _tip: Tip)
    where
        Mempool: Service<Request, Response = Response, Error = BoxError> + Clone + 'static,
        State: Service<ReadRequest, Response = ReadResponse, Error = zebra_state::BoxError>
            + Clone
            + Send
            + Sync
            + 'static,
        Tip: ChainTip + Clone + Send + Sync + 'static,
    {
        // TODO: this should be an argument
        let network = zebra_chain::parameters::Network::Mainnet;

        // TODO: Use tip.best_tip_height()
        let tip_height = zebra_chain::block::Height(1);

        // get spacing between blocks
        let spacing = NetworkUpgrade::target_spacing_for_height(network, tip_height);

        loop {
            // sleep until the next block
            tokio::time::sleep(spacing.to_std().unwrap()).await;

            // first, remove what is expired
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
    fn remove_expired(&self, spacing: Duration) {
        let duration_to_expire =
            Duration::seconds(NUMBER_OF_BLOCKS_TO_EXPIRE * spacing.num_seconds());
        let transactions = self
            .queue
            .lock()
            .expect("queue mutex should be unpoisoned")
            .transactions();
        let now = Instant::now();

        for tx in transactions.iter() {
            let tx_time =
                tx.1 .1
                    .checked_add(duration_to_expire.to_std().unwrap())
                    .unwrap();

            if now > tx_time {
                self.queue
                    .lock()
                    .expect("queue mutex should be unpoisoned")
                    .remove(*tx.0);
            }
        }
    }

    /// Remove transactions from the queue that had been inserted to the state or the mempool.
    fn remove_committed(&self, to_remove: HashSet<UnminedTxId>) {
        for r in to_remove {
            self.queue
                .lock()
                .expect("queue mutex should be unpoisoned")
                .remove(r);
        }
    }

    /// Check the mempool for given transactions.
    async fn check_mempool<Mempool>(
        mempool: Mempool,
        transactions: HashSet<UnminedTxId>,
    ) -> HashSet<UnminedTxId>
    where
        Mempool: Service<Request, Response = Response, Error = BoxError> + Clone + 'static,
    {
        let mut response = HashSet::new();
        let request = Request::TransactionsById(transactions);

        // TODO: ignore errors
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

        response
    }

    /// Check the state for given transactions.
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
    async fn retry<Mempool>(mempool: Mempool, transactions: Vec<Arc<Transaction>>)
    where
        Mempool: Service<Request, Response = Response, Error = BoxError> + Clone + 'static,
    {
        for tx in transactions {
            let transaction_parameter = Gossip::Tx(UnminedTx::from(tx.clone()));
            let request = Request::Queue(vec![transaction_parameter]);

            let _ = mempool
                .clone()
                .oneshot(request)
                .await
                .expect("Sending to memmpool should not panic");
        }
    }
}
