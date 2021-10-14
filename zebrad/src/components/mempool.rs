//! Zebra mempool.

use std::{
    collections::HashSet,
    future::Future,
    iter,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{future::FutureExt, stream::Stream};
use tokio::sync::{mpsc, watch};
use tower::{buffer::Buffer, timeout::Timeout, util::BoxService, Service};

use zebra_chain::{
    block::Height,
    chain_tip::ChainTip,
    transaction::{UnminedTx, UnminedTxId},
};
use zebra_consensus::{error::TransactionError, transaction};
use zebra_network as zn;
use zebra_state as zs;
use zebra_state::{ChainTipChange, TipAction};
use zs::ChainTipBlock;

use crate::components::sync::SyncStatus;

mod config;
mod crawler;
pub mod downloads;
mod error;
pub mod gossip;
mod storage;

#[cfg(test)]
mod tests;

pub use crate::BoxError;

pub use config::Config;
pub use crawler::Crawler;
pub use error::MempoolError;
pub use gossip::gossip_mempool_transaction_id;
pub use storage::{
    ExactTipRejectionError, SameEffectsChainRejectionError, SameEffectsTipRejectionError,
};

#[cfg(test)]
pub use storage::tests::unmined_transactions_in_blocks;

use downloads::{
    Downloads as TxDownloads, Gossip, TRANSACTION_DOWNLOAD_TIMEOUT, TRANSACTION_VERIFY_TIMEOUT,
};

type Outbound = Buffer<BoxService<zn::Request, zn::Response, zn::BoxError>, zn::Request>;
type State = Buffer<BoxService<zs::Request, zs::Response, zs::BoxError>, zs::Request>;
type TxVerifier = Buffer<
    BoxService<transaction::Request, transaction::Response, TransactionError>,
    transaction::Request,
>;
type InboundTxDownloads = TxDownloads<Timeout<Outbound>, Timeout<TxVerifier>, State>;

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub enum Request {
    TransactionIds,
    TransactionsById(HashSet<UnminedTxId>),
    RejectedTransactionIds(HashSet<UnminedTxId>),
    Queue(Vec<Gossip>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Response {
    Transactions(Vec<UnminedTx>),
    TransactionIds(Vec<UnminedTxId>),
    RejectedTransactionIds(Vec<UnminedTxId>),
    Queued(Vec<Result<(), MempoolError>>),
}

/// The state of the mempool.
///
/// Indicates wether it is enabled or disabled and, if enabled, contains
/// the necessary data to run it.
#[allow(clippy::large_enum_variant)]
enum ActiveState {
    /// The Mempool is disabled.
    Disabled,
    /// The Mempool is enabled.
    Enabled {
        /// The join handle for the crawler task.
        ///
        /// Used to receive panics and errors from the task,
        /// and abort the task when the mempool is disabled.
        crawler_task_handle: tokio::task::JoinHandle<Result<(), BoxError>>,

        /// Used to receive crawled transaction IDs from the crawler task.
        crawled_transaction_receiver: mpsc::Receiver<HashSet<UnminedTxId>>,

        /// The transaction download and verify stream.
        tx_downloads: Pin<Box<InboundTxDownloads>>,

        /// The Mempool storage itself.
        ///
        /// ##: Correctness: only components internal to the [`Mempool`] struct are allowed to
        /// inject transactions into `storage`, as transactions must be verified beforehand.
        storage: storage::Storage,
    },
}

impl Default for ActiveState {
    fn default() -> Self {
        ActiveState::Disabled
    }
}

impl ActiveState {
    /// Return whether the mempool is enabled or not.
    pub fn is_enabled(&self) -> bool {
        match self {
            ActiveState::Disabled => false,
            ActiveState::Enabled { .. } => true,
        }
    }

    /// Returns the number of in-flight downloads.
    #[allow(dead_code)]
    pub fn downloads_in_flight(&self) -> usize {
        if let ActiveState::Enabled { tx_downloads, .. } = self {
            tx_downloads.in_flight()
        } else {
            0
        }
    }

    /// Returns the current state, leaving a [`Disabled`] in its place.
    fn take(&mut self) -> Self {
        std::mem::take(self)
    }

    /// Clean up completed download and verify tasks, and add to mempool if successful.
    ///
    /// Returns inserted transaction IDs, so they can be gossiped to peers.
    fn insert_verified_transactions(&mut self, cx: &mut Context<'_>) -> HashSet<UnminedTxId> {
        // Collect inserted transaction ids.
        let mut inserted_ids = HashSet::<_>::new();

        if let ActiveState::Enabled {
            crawler_task_handle: _,
            crawled_transaction_receiver: _,
            tx_downloads,
            storage,
        } = self
        {
            while let Poll::Ready(Some(r)) = tx_downloads.as_mut().poll_next(cx) {
                match r {
                    Ok(tx) => {
                        if let Ok(inserted_id) = storage.insert(tx.clone()) {
                            // Save transaction ids that we will send to peers
                            inserted_ids.insert(inserted_id);
                        }
                    }
                    Err((txid, e)) => {
                        storage.reject_if_needed(txid, e);
                        // TODO: should we also log the result?
                    }
                };
            }
        }

        inserted_ids
    }

    /// Remove mined and expired transactions,
    /// in response to `block` being added to the best chain tip.
    ///
    /// Returns the set of inserted transactions,
    /// after removing mined and expired transactions.
    #[must_use = "the remaining transaction IDs should not be discarded"]
    fn remove_rejected_transactions(
        &mut self,
        block: ChainTipBlock,
        mut inserted_ids: HashSet<UnminedTxId>,
    ) -> HashSet<UnminedTxId> {
        if let ActiveState::Enabled {
            crawler_task_handle: _,
            crawled_transaction_receiver: _,
            tx_downloads,
            storage,
        } = self
        {
            // Remove mined transaction from the mempool.
            let mined_ids = block.transaction_hashes.iter().cloned().collect();
            tx_downloads.cancel(&mined_ids);
            storage.remove_same_effects(&mined_ids);
            storage.clear_tip_rejections();
            inserted_ids = remove_mined_from_peer_list(&inserted_ids, &mined_ids);

            // Remove expired transactions from the mempool.
            // These transactions are added to the reject list,
            // which removes them from gossiped
            let expired_ids = storage.remove_expired_transactions(block.height);
            inserted_ids = remove_expired_from_peer_list(&inserted_ids, &expired_ids);
        }

        inserted_ids
    }

    /// Queue `transaction` for download, if needed.
    /// Returns the corresponding error if the transaction should not be downloaded.
    ///
    /// Checks the storage and existing queue to see if each transaction should be downloaded.
    //
    // Note: This is implemented as an associated method to simplify the calling code,
    //       which partly borrows the ActiveState's fields.
    fn queue_for_download(
        tx_downloads: &mut Pin<Box<InboundTxDownloads>>,
        storage: &mut storage::Storage,
        transaction: Gossip,
    ) -> Result<(), MempoolError> {
        storage.should_download_or_verify(transaction.id())?;
        tx_downloads.download_if_needed_and_verify(transaction)?;

        Ok(())
    }

    /// Queues all the crawled transaction IDs in the channel.
    ///
    /// If the channel is closed, returns an error.
    fn queue_crawled_transaction_ids(&mut self) -> Result<(), mpsc::error::TryRecvError> {
        if let ActiveState::Enabled {
            crawled_transaction_receiver,
            tx_downloads,
            storage,
            ..
        } = self
        {
            let mut unique_ids = HashSet::new();

            loop {
                match crawled_transaction_receiver.try_recv() {
                    Ok(ids) => unique_ids.extend(ids),
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    closed @ Err(mpsc::error::TryRecvError::Closed) => {
                        closed?;
                    }
                }
            }

            for tx_id in unique_ids {
                // It's ok for crawled transactions to be rejected by the mempool
                let _ = ActiveState::queue_for_download(tx_downloads, storage, tx_id.into());
            }
        }

        Ok(())
    }

    /// Checks if the mempool crawler task has panicked or exited.
    ///
    /// If the task is still running, or the mempool is disabled, returns this `ActiveState`.
    /// Otherwise, returns an error.
    fn crawler_task_result(&mut self) -> Result<(), BoxError> {
        if let ActiveState::Enabled {
            crawler_task_handle,
            ..
        } = self
        {
            // the crawler should never return, even with a success value
            if let Some(panic_result) = crawler_task_handle.now_or_never() {
                let task_result =
                    panic_result.expect("unexpected panic or cancellation in mempool crawler task");
                task_result?;

                return Err("mempool crawler task unexpectedly exited, with no errors".into());
            }
        }

        Ok(())
    }
}

impl Drop for ActiveState {
    fn drop(&mut self) {
        if let ActiveState::Enabled { tx_downloads, .. } = self {
            tx_downloads.cancel_all();
        }

        self.crawler_task_result()
            .expect("unexpected panic or return from crawler task");

        if let ActiveState::Enabled {
            crawler_task_handle,
            ..
        } = self
        {
            crawler_task_handle.abort();

            // We could check for errors during abort,
            // but we'd need to filter out `JoinError::Cancelled`,
            // because it is returned when the task aborts.
        }
    }
}

/// Mempool async management and query service.
///
/// The mempool is the set of all verified transactions that this node is aware
/// of that have yet to be confirmed by the Zcash network. A transaction is
/// confirmed when it has been included in a block ('mined').
pub struct Mempool {
    /// The state of the mempool.
    active_state: ActiveState,

    /// Allows checking if we are near the tip to enable/disable the mempool.
    sync_status: SyncStatus,

    /// If the state's best chain tip has reached this height, always enable the mempool.
    debug_enable_at_height: Option<Height>,

    /// Allow efficient access to the best tip of the blockchain.
    latest_chain_tip: zs::LatestChainTip,

    /// Allows the detection of newly added chain tip blocks,
    /// and chain tip resets.
    chain_tip_change: ChainTipChange,

    /// Handle to the outbound service.
    /// Used to construct the transaction downloader.
    outbound: Outbound,

    /// Handle to the state service.
    /// Used to construct the transaction downloader.
    state: State,

    /// Handle to the transaction verifier service.
    /// Used to construct the transaction downloader.
    tx_verifier: TxVerifier,

    /// Used to broadcast transaction ids to peers via the gossip task.
    gossiped_transaction_sender: watch::Sender<HashSet<UnminedTxId>>,
}

impl Mempool {
    pub(crate) fn new(
        config: &Config,
        outbound: Outbound,
        state: State,
        tx_verifier: TxVerifier,
        sync_status: SyncStatus,
        latest_chain_tip: zs::LatestChainTip,
        chain_tip_change: ChainTipChange,
    ) -> (Mempool, watch::Receiver<HashSet<UnminedTxId>>) {
        let (gossiped_transaction_sender, gossiped_transaction_receiver) =
            tokio::sync::watch::channel(HashSet::new());

        let mut service = Mempool {
            active_state: ActiveState::Disabled,
            sync_status,
            debug_enable_at_height: config.debug_enable_at_height.map(Height),
            latest_chain_tip,
            chain_tip_change,
            outbound,
            state,
            tx_verifier,
            gossiped_transaction_sender,
        };

        // Make sure `is_enabled` is accurate.
        // Otherwise, it is only updated in `poll_ready`, right before each service call.
        service
            .update_state()
            .expect("unexpected error in newly initialized crawler task");

        (service, gossiped_transaction_receiver)
    }

    /// Is the mempool enabled by a debug config option?
    fn is_enabled_by_debug(&self) -> bool {
        let mut is_debug_enabled = false;

        // optimise non-debug performance
        if self.debug_enable_at_height.is_none() {
            return is_debug_enabled;
        }

        let enable_at_height = self
            .debug_enable_at_height
            .expect("unexpected debug_enable_at_height: just checked for None");

        if let Some(best_tip_height) = self.latest_chain_tip.best_tip_height() {
            is_debug_enabled = best_tip_height >= enable_at_height;

            if is_debug_enabled && !self.is_enabled() {
                info!(
                    ?best_tip_height,
                    ?enable_at_height,
                    "enabling mempool for debugging"
                );
            }
        }

        is_debug_enabled
    }

    /// Update the mempool state (enabled / disabled) depending on how close to
    /// the tip is the synchronization, including side effects to state changes.
    fn update_state(&mut self) -> Result<(), BoxError> {
        // check for errors before possibly aborting the task
        self.active_state.crawler_task_result()?;

        let is_close_to_tip = self.sync_status.is_close_to_tip() || self.is_enabled_by_debug();

        if self.is_enabled() == is_close_to_tip {
            // the active state is up to date
            return Ok(());
        }

        // Update enabled / disabled state
        if is_close_to_tip {
            info!("activating mempool: Zebra is close to the tip");

            let tx_downloads = Box::pin(TxDownloads::new(
                Timeout::new(self.outbound.clone(), TRANSACTION_DOWNLOAD_TIMEOUT),
                Timeout::new(self.tx_verifier.clone(), TRANSACTION_VERIFY_TIMEOUT),
                self.state.clone(),
            ));

            let (crawler_task_handle, crawled_transaction_receiver) =
                Crawler::spawn(self.outbound.clone());

            self.active_state = ActiveState::Enabled {
                crawler_task_handle,
                crawled_transaction_receiver,
                tx_downloads,
                storage: Default::default(),
            };
        } else {
            info!("deactivating mempool: Zebra is syncing lots of blocks");

            // The drop implementation cancels the crawler and download tasks
            self.active_state = ActiveState::Disabled;
        }

        Ok(())
    }

    /// Return whether the mempool is enabled or not.
    pub fn is_enabled(&self) -> bool {
        self.active_state.is_enabled()
    }
}

impl Service<Request> for Mempool {
    type Response = Response;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.update_state()?;

        if self.active_state.is_enabled() {
            if let Some(tip_action) = self.chain_tip_change.last_tip_change() {
                match tip_action {
                    // Clear the mempool and cancel downloads if there has been a chain tip reset.
                    TipAction::Reset { .. } => {
                        // use the same code for disable and reset, for consistency

                        // drop the state, cancelling tasks if it is active
                        std::mem::drop(self.active_state.take());

                        // create a new active state, if we are currently active
                        self.update_state()?;
                    }
                    // Cancel downloads/verifications/storage of transactions
                    // with the same mined IDs as recently mined transactions.
                    TipAction::Grow { block } => {
                        let send_to_peers_ids = self.active_state.insert_verified_transactions(cx);
                        let send_to_peers_ids = self
                            .active_state
                            .remove_rejected_transactions(block, send_to_peers_ids);

                        // Send transactions that were not rejected nor expired to peers
                        if !send_to_peers_ids.is_empty() {
                            let _ = self.gossiped_transaction_sender.send(send_to_peers_ids)?;
                        }
                    }
                }
            }

            // After any reset, queue newly crawled transaction IDs for download and verification
            self.active_state.queue_crawled_transaction_ids()?;
        } else {
            // When the mempool is disabled we still return that the service is ready.
            // Otherwise, callers could block waiting for the mempool to be enabled,
            // which may not be the desired behavior.
        }

        Poll::Ready(Ok(()))
    }

    /// Call the mempool service.
    ///
    /// Errors indicate that the peer has done something wrong or unexpected,
    /// and will cause callers to disconnect from the remote peer.
    #[instrument(name = "mempool", skip(self, req))]
    fn call(&mut self, req: Request) -> Self::Future {
        match &mut self.active_state {
            ActiveState::Enabled {
                crawler_task_handle: _,
                crawled_transaction_receiver: _,
                tx_downloads,
                storage,
            } => match req {
                Request::TransactionIds => {
                    let res = storage.tx_ids().collect();
                    async move { Ok(Response::TransactionIds(res)) }.boxed()
                }
                Request::TransactionsById(ids) => {
                    let res = storage.transactions_exact(ids).cloned().collect();
                    async move { Ok(Response::Transactions(res)) }.boxed()
                }
                Request::RejectedTransactionIds(ids) => {
                    let res = storage.rejected_transactions(ids).collect();
                    async move { Ok(Response::RejectedTransactionIds(res)) }.boxed()
                }
                Request::Queue(gossiped_txs) => {
                    let rsp: Vec<Result<(), MempoolError>> = gossiped_txs
                        .into_iter()
                        .map(|gossiped_tx| {
                            ActiveState::queue_for_download(tx_downloads, storage, gossiped_tx)
                                .map_err(Into::into)
                        })
                        .collect();
                    async move { Ok(Response::Queued(rsp)) }.boxed()
                }
            },
            ActiveState::Disabled => {
                // We can't return an error since that will cause a disconnection
                // by the peer connection handler. Therefore, return successful
                // empty responses.
                let resp = match req {
                    Request::TransactionIds => Response::TransactionIds(Default::default()),
                    Request::TransactionsById(_) => Response::Transactions(Default::default()),
                    Request::RejectedTransactionIds(_) => {
                        Response::RejectedTransactionIds(Default::default())
                    }
                    // Special case; we can signal the error inside the response.
                    Request::Queue(gossiped_txs) => Response::Queued(
                        iter::repeat(Err(MempoolError::Disabled))
                            .take(gossiped_txs.len())
                            .collect(),
                    ),
                };
                async move { Ok(resp) }.boxed()
            }
        }
    }
}

/// Returns inserted IDs after removing mined transaction IDs.
#[must_use = "the difference between the sets should not be discarded"]
fn remove_mined_from_peer_list(
    send_to_peers_ids: &HashSet<UnminedTxId>,
    mined_ids: &HashSet<zebra_chain::transaction::Hash>,
) -> HashSet<UnminedTxId> {
    let mut unmined_ids = send_to_peers_ids.clone();

    unmined_ids.retain(|unmined_id| !mined_ids.contains(&unmined_id.mined_id()));

    unmined_ids
}

/// Returns inserted IDs after removing expired transaction IDs.
#[must_use = "the difference between the sets should not be discarded"]
fn remove_expired_from_peer_list(
    send_to_peers_ids: &HashSet<UnminedTxId>,
    expired_ids: &HashSet<UnminedTxId>,
) -> HashSet<UnminedTxId> {
    send_to_peers_ids.difference(expired_ids).copied().collect()
}
