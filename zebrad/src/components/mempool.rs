//! Zebra mempool.

use std::{
    collections::HashSet,
    future::Future,
    iter,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{future::FutureExt, stream::Stream};
use tokio::sync::watch;
use tower::{buffer::Buffer, service_fn, timeout::Timeout, util::BoxService, Service};

use zebra_chain::{
    block::Height,
    chain_tip::ChainTip,
    transaction::{UnminedTx, UnminedTxId},
};
use zebra_consensus::{error::TransactionError, transaction};
use zebra_network as zn;
use zebra_state as zs;
use zebra_state::{ChainTipChange, TipAction};

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
    Downloads as TxDownloads, Gossip, TransactionDownloadVerifyError, TRANSACTION_DOWNLOAD_TIMEOUT,
    TRANSACTION_VERIFY_TIMEOUT,
};

type Outbound = Buffer<BoxService<zn::Request, zn::Response, zn::BoxError>, zn::Request>;
type State = Buffer<BoxService<zs::Request, zs::Response, zs::BoxError>, zs::Request>;
type TxVerifier = Buffer<
    BoxService<transaction::Request, transaction::Response, TransactionError>,
    transaction::Request,
>;
type InboundTxDownloads = TxDownloads<Timeout<Outbound>, Timeout<TxVerifier>, State>;

#[derive(Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub enum Request {
    TransactionIds,
    TransactionsById(HashSet<UnminedTxId>),
    RejectedTransactionIds(HashSet<UnminedTxId>),
    Queue(Vec<Gossip>),
}

#[derive(Debug)]
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
        mempool_crawler_task_handle: tokio::task::JoinHandle<Result<(), BoxError>>,

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

    /// Returns the current state, leaving a [`Disabled`] in its place.
    fn take(&mut self) -> Self {
        std::mem::take(self)
    }

    /// Checks if the mempool crawler task has panicked or exited.
    ///
    /// If the task is still running, or the mempool is disabled, returns this `ActiveState`.
    /// Otherwise, returns an error.
    fn mempool_crawler_task_result(&mut self) -> Result<(), BoxError> {
        if let ActiveState::Enabled {
            mempool_crawler_task_handle,
            ..
        } = self
        {
            // the crawler should never return, even with a success value
            if let Some(panic_result) = mempool_crawler_task_handle.now_or_never() {
                let task_result =
                    panic_result.expect("unexpected panic or cancellation in mempool crawler task");
                task_result?;

                return Err("mempool crawler task unexpectedly exited, with no errors".into());
            }
        }

        Ok(())
    }

    /// Aborts the mempool crawler task, returning any previous errors from that task.
    fn abort_mempool_crawler_task(mut self) -> Result<(), BoxError> {
        self.mempool_crawler_task_result()?;

        if let ActiveState::Enabled {
            mempool_crawler_task_handle,
            ..
        } = self
        {
            mempool_crawler_task_handle.abort();

            // We could check for errors during abort,
            // but we'd need to filter out `JoinError::Cancelled`,
            // because it is returned when the task aborts.
        }

        Ok(())
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

    /// Sender part of a gossip transactions channel.
    /// Used to broadcast transaction ids to peers.
    transaction_sender: watch::Sender<HashSet<UnminedTxId>>,
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
    ) -> (Self, watch::Receiver<HashSet<UnminedTxId>>) {
        let (transaction_sender, transaction_receiver) =
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
            transaction_sender,
        };

        // Make sure `is_enabled` is accurate.
        // Otherwise, it is only updated in `poll_ready`, right before each service call.
        service
            .update_state()
            .expect("unexpected error in newly initialized crawler task");

        (service, transaction_receiver)
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
        self.active_state.mempool_crawler_task_result()?;

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

            let mempool_crawler_task_handle = Crawler::spawn(
                &Config::default(),
                self.outbound.clone(),
                service_fn(|_| async {
                    todo!("pass a queue sender and check the receiver in poll ready")
                }),
                self.sync_status.clone(),
                self.chain_tip_change.clone(),
            );

            self.active_state = ActiveState::Enabled {
                mempool_crawler_task_handle,
                tx_downloads,
                storage: Default::default(),
            };
        } else {
            info!("deactivating mempool: Zebra is syncing lots of blocks");

            self.active_state.take().abort_mempool_crawler_task()?;
        }

        Ok(())
    }

    /// Return whether the mempool is enabled or not.
    pub fn is_enabled(&self) -> bool {
        self.active_state.is_enabled()
    }

    /// Check if transaction should be downloaded and/or verified.
    ///
    /// If it is already in the mempool (or in its rejected list)
    /// then it shouldn't be downloaded/verified.
    fn should_download_or_verify(
        storage: &mut storage::Storage,
        txid: UnminedTxId,
    ) -> Result<(), MempoolError> {
        // Check if the transaction is already in the mempool.
        if storage.contains_transaction_exact(&txid) {
            return Err(MempoolError::InMempool);
        }
        if let Some(error) = storage.rejection_error(&txid) {
            return Err(error);
        }
        Ok(())
    }
}

impl Service<Request> for Mempool {
    type Response = Response;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.update_state()?;

        match &mut self.active_state {
            ActiveState::Enabled {
                mempool_crawler_task_handle: _,
                tx_downloads,
                storage,
            } => {
                if let Some(tip_action) = self.chain_tip_change.last_tip_change() {
                    match tip_action {
                        // Clear the mempool and cancel downloads if there has been a chain tip reset.
                        TipAction::Reset { .. } => {
                            storage.clear();
                            tx_downloads.cancel_all();
                        }
                        // Cancel downloads/verifications/storage of transactions
                        // with the same mined IDs as recently mined transactions.
                        TipAction::Grow { block } => {
                            let mined_ids = block.transaction_hashes.iter().cloned().collect();
                            tx_downloads.cancel(&mined_ids);
                            storage.remove_same_effects(&mined_ids);
                            storage.clear_tip_rejections();
                        }
                    }
                }

                // Collect inserted transaction ids.
                let mut send_to_peers_ids = HashSet::<_>::new();

                // Clean up completed download tasks and add to mempool if successful.
                while let Poll::Ready(Some(r)) = tx_downloads.as_mut().poll_next(cx) {
                    match r {
                        Ok(tx) => {
                            if let Ok(inserted_id) = storage.insert(tx.clone()) {
                                // Save transaction ids that we will send to peers
                                send_to_peers_ids.insert(inserted_id);
                            }
                        }
                        Err((txid, e)) => {
                            reject_if_needed(storage, txid, e);
                            // TODO: should we also log the result?
                        }
                    };
                }

                // Remove expired transactions from the mempool.
                if let Some(tip_height) = self.latest_chain_tip.best_tip_height() {
                    let expired_transactions = remove_expired_transactions(storage, tip_height);
                    // Remove transactions that are expired from the peers list
                    send_to_peers_ids =
                        remove_expired_from_peer_list(&send_to_peers_ids, &expired_transactions);
                }

                // Send transactions that were not rejected nor expired to peers
                if !send_to_peers_ids.is_empty() {
                    let _ = self.transaction_sender.send(send_to_peers_ids)?;
                }
            }
            ActiveState::Disabled => {
                // When the mempool is disabled we still return that the service is ready.
                // Otherwise, callers could block waiting for the mempool to be enabled,
                // which may not be the desired behavior.
            }
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
                mempool_crawler_task_handle: _,
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
                            Self::should_download_or_verify(storage, gossiped_tx.id())?;
                            tx_downloads.download_if_needed_and_verify(gossiped_tx)?;
                            Ok(())
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

/// Remove transactions from the mempool if they have not been mined after a specified height.
///
/// https://zips.z.cash/zip-0203#specification
fn remove_expired_transactions(
    storage: &mut storage::Storage,
    tip_height: zebra_chain::block::Height,
) -> HashSet<UnminedTxId> {
    let mut txid_set = HashSet::new();
    // we need a separate set, since reject() takes the original unmined ID,
    // then extracts the mined ID out of it
    let mut unmined_id_set = HashSet::new();

    for t in storage.transactions() {
        if let Some(expiry_height) = t.transaction.expiry_height() {
            if tip_height >= expiry_height {
                txid_set.insert(t.id.mined_id());
                unmined_id_set.insert(t.id);
            }
        }
    }

    // expiry height is effecting data, so we match by non-malleable TXID
    storage.remove_same_effects(&txid_set);

    // also reject it
    for id in unmined_id_set.iter() {
        storage.reject(*id, SameEffectsChainRejectionError::Expired.into());
    }

    unmined_id_set
}

/// Remove expired transaction ids from a given list of inserted ones.
fn remove_expired_from_peer_list(
    send_to_peers_ids: &HashSet<UnminedTxId>,
    expired_transactions: &HashSet<UnminedTxId>,
) -> HashSet<UnminedTxId> {
    send_to_peers_ids
        .difference(expired_transactions)
        .copied()
        .collect()
}

/// Add a transaction that failed download and verification to the rejected list
/// if needed, depending on the reason for the failure.
fn reject_if_needed(
    storage: &mut storage::Storage,
    txid: UnminedTxId,
    e: TransactionDownloadVerifyError,
) {
    match e {
        // Rejecting a transaction already in state would speed up further
        // download attempts without checking the state. However it would
        // make the reject list grow forever.
        // TODO: revisit after reviewing the rejected list cleanup criteria?
        // TODO: if we decide to reject it, then we need to pass the block hash
        // to State::Confirmed. This would require the zs::Response::Transaction
        // to include the hash, which would need to be implemented.
        TransactionDownloadVerifyError::InState |
        // An unknown error in the state service, better do nothing
        TransactionDownloadVerifyError::StateError(_) |
        // Sync has just started. Mempool shouldn't even be enabled, so will not
        // happen in practice.
        TransactionDownloadVerifyError::NoTip |
        // If download failed, do nothing; the crawler will end up trying to
        // download it again.
        TransactionDownloadVerifyError::DownloadFailed(_) |
        // If it was cancelled then a block was mined, or there was a network
        // upgrade, etc. No reason to reject it.
        TransactionDownloadVerifyError::Cancelled => {}

        // Consensus verification failed. Reject transaction to avoid
        // having to download and verify it again just for it to fail again.
        TransactionDownloadVerifyError::Invalid(e) => {
            storage.reject(txid, ExactTipRejectionError::FailedVerification(e).into())
        }
    }
}
