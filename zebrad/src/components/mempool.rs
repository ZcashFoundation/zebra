//! Zebra mempool.
//!
//! A zebrad application component that manages the active collection, reception,
//! gossip, verification, in-memory storage, eviction, and rejection of unmined Zcash
//! transactions (those that have not been confirmed in a mined block on the
//! blockchain).
//!
//! Major parts of the mempool include:
//!  * [Mempool Service][`Mempool`]
//!    * activates when the syncer is near the chain tip
//!    * spawns [download and verify tasks][`downloads::Downloads`] for each crawled or gossiped transaction
//!    * handles in-memory [storage][`storage::Storage`] of unmined transactions
//!  * [Crawler][`crawler::Crawler`]
//!    * runs in the background to periodically poll peers for fresh unmined transactions
//!  * [Queue Checker][`queue_checker::QueueChecker`]
//!    * runs in the background, polling the mempool to store newly verified transactions
//!  * [Transaction Gossip Task][`gossip::gossip_mempool_transaction_id`]
//!    * runs in the background and gossips newly added mempool transactions
//!      to peers

use std::{
    collections::HashSet,
    future::Future,
    iter,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{future::FutureExt, stream::Stream};
use tokio::sync::watch;
use tower::{buffer::Buffer, timeout::Timeout, util::BoxService, Service};

use zebra_chain::{
    block::Height, chain_sync_status::ChainSyncStatus, chain_tip::ChainTip,
    transaction::UnminedTxId,
};
use zebra_consensus::{error::TransactionError, transaction};
use zebra_network as zn;
use zebra_node_services::mempool::{Gossip, Request, Response};
use zebra_state as zs;
use zebra_state::{ChainTipChange, TipAction};

use crate::components::sync::SyncStatus;

pub mod config;
mod crawler;
pub mod downloads;
mod error;
pub mod gossip;
mod queue_checker;
mod storage;

#[cfg(test)]
mod tests;

pub use crate::BoxError;

pub use config::Config;
pub use crawler::Crawler;
pub use error::MempoolError;
pub use gossip::gossip_mempool_transaction_id;
pub use queue_checker::QueueChecker;
pub use storage::{
    ExactTipRejectionError, SameEffectsChainRejectionError, SameEffectsTipRejectionError,
};

#[cfg(test)]
pub use self::{storage::tests::unmined_transactions_in_blocks, tests::UnboxMempoolError};

use downloads::{
    Downloads as TxDownloads, TRANSACTION_DOWNLOAD_TIMEOUT, TRANSACTION_VERIFY_TIMEOUT,
};

type Outbound = Buffer<BoxService<zn::Request, zn::Response, zn::BoxError>, zn::Request>;
type State = Buffer<BoxService<zs::Request, zs::Response, zs::BoxError>, zs::Request>;
type TxVerifier = Buffer<
    BoxService<transaction::Request, transaction::Response, TransactionError>,
    transaction::Request,
>;
type InboundTxDownloads = TxDownloads<Timeout<Outbound>, Timeout<TxVerifier>, State>;

/// The state of the mempool.
///
/// Indicates whether it is enabled or disabled and, if enabled, contains
/// the necessary data to run it.
//
// Zebra only has one mempool, so the enum variant size difference doesn't matter.
#[allow(clippy::large_enum_variant)]
#[derive(Default)]
enum ActiveState {
    /// The Mempool is disabled.
    #[default]
    Disabled,

    /// The Mempool is enabled.
    Enabled {
        /// The Mempool storage itself.
        ///
        /// # Correctness
        ///
        /// Only components internal to the [`Mempool`] struct are allowed to
        /// inject transactions into `storage`, as transactions must be verified beforehand.
        storage: storage::Storage,

        /// The transaction download and verify stream.
        tx_downloads: Pin<Box<InboundTxDownloads>>,
    },
}

impl ActiveState {
    /// Returns the current state, leaving [`Self::Disabled`] in its place.
    fn take(&mut self) -> Self {
        std::mem::take(self)
    }

    /// Returns a list of requests that will retry every stored and pending transaction.
    fn transaction_retry_requests(&self) -> Vec<Gossip> {
        match self {
            ActiveState::Disabled => Vec::new(),
            ActiveState::Enabled {
                storage,
                tx_downloads,
            } => {
                let mut transactions = Vec::new();

                let storage = storage.transactions().map(|tx| tx.clone().into());
                transactions.extend(storage);

                let pending = tx_downloads.transaction_requests().cloned();
                transactions.extend(pending);

                transactions
            }
        }
    }
}

/// Mempool async management and query service.
///
/// The mempool is the set of all verified transactions that this node is aware
/// of that have yet to be confirmed by the Zcash network. A transaction is
/// confirmed when it has been included in a block ('mined').
pub struct Mempool {
    /// The configurable options for the mempool, persisted between states.
    config: Config,

    /// The state of the mempool.
    active_state: ActiveState,

    /// Allows checking if we are near the tip to enable/disable the mempool.
    sync_status: SyncStatus,

    /// If the state's best chain tip has reached this height, always enable the mempool.
    debug_enable_at_height: Option<Height>,

    /// Allows efficient access to the best tip of the blockchain.
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
            config: config.clone(),
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
        service.update_state();

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
    ///
    /// Returns `true` if the state changed.
    fn update_state(&mut self) -> bool {
        let is_close_to_tip = self.sync_status.is_close_to_tip() || self.is_enabled_by_debug();

        if self.is_enabled() == is_close_to_tip {
            // the active state is up to date
            return false;
        }

        // Update enabled / disabled state
        if is_close_to_tip {
            info!(
                tip_height = ?self.latest_chain_tip.best_tip_height(),
                "activating mempool: Zebra is close to the tip"
            );

            let tx_downloads = Box::pin(TxDownloads::new(
                Timeout::new(self.outbound.clone(), TRANSACTION_DOWNLOAD_TIMEOUT),
                Timeout::new(self.tx_verifier.clone(), TRANSACTION_VERIFY_TIMEOUT),
                self.state.clone(),
            ));
            self.active_state = ActiveState::Enabled {
                storage: storage::Storage::new(&self.config),
                tx_downloads,
            };
        } else {
            info!(
                tip_height = ?self.latest_chain_tip.best_tip_height(),
                "deactivating mempool: Zebra is syncing lots of blocks"
            );

            // This drops the previous ActiveState::Enabled, cancelling its download tasks.
            // We don't preserve the previous transactions, because we are syncing lots of blocks.
            self.active_state = ActiveState::Disabled
        }

        true
    }

    /// Return whether the mempool is enabled or not.
    pub fn is_enabled(&self) -> bool {
        match self.active_state {
            ActiveState::Disabled => false,
            ActiveState::Enabled { .. } => true,
        }
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
}

impl Service<Request> for Mempool {
    type Response = Response;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let is_state_changed = self.update_state();

        tracing::trace!(is_enabled = ?self.is_enabled(), ?is_state_changed, "started polling the mempool...");

        // When the mempool is disabled we still return that the service is ready.
        // Otherwise, callers could block waiting for the mempool to be enabled.
        if !self.is_enabled() {
            return Poll::Ready(Ok(()));
        }

        let tip_action = self.chain_tip_change.last_tip_change();

        // If the mempool was just freshly enabled,
        // skip resetting and removing mined transactions for this tip.
        if is_state_changed {
            return Poll::Ready(Ok(()));
        }

        // Clear the mempool and cancel downloads if there has been a chain tip reset.
        if matches!(tip_action, Some(TipAction::Reset { .. })) {
            info!(
                tip_height = ?tip_action.as_ref().unwrap().best_tip_height(),
                "resetting mempool: switched best chain, skipped blocks, or activated network upgrade"
            );

            let previous_state = self.active_state.take();
            let tx_retries = previous_state.transaction_retry_requests();

            // Use the same code for dropping and resetting the mempool,
            // to avoid subtle bugs.
            //
            // Drop the current contents of the state,
            // cancelling any pending download tasks,
            // and dropping completed verification results.
            std::mem::drop(previous_state);

            // Re-initialise an empty state.
            self.update_state();

            // Re-verify the transactions that were pending or valid at the previous tip.
            // This saves us the time and data needed to re-download them.
            if let ActiveState::Enabled { tx_downloads, .. } = &mut self.active_state {
                info!(
                    transactions = tx_retries.len(),
                    "re-verifying mempool transactions after a chain fork"
                );

                for tx in tx_retries {
                    // This is just an efficiency optimisation, so we don't care if queueing
                    // transaction requests fails.
                    let _result = tx_downloads.download_if_needed_and_verify(tx);
                }
            }

            return Poll::Ready(Ok(()));
        }

        if let ActiveState::Enabled {
            storage,
            tx_downloads,
        } = &mut self.active_state
        {
            // Collect inserted transaction ids.
            let mut send_to_peers_ids = HashSet::<_>::new();

            // Clean up completed download tasks and add to mempool if successful.
            while let Poll::Ready(Some(r)) = tx_downloads.as_mut().poll_next(cx) {
                match r {
                    Ok(tx) => {
                        let insert_result = storage.insert(tx.clone());

                        tracing::trace!(
                            ?insert_result,
                            "got Ok(_) transaction verify, tried to store",
                        );

                        if let Ok(inserted_id) = insert_result {
                            // Save transaction ids that we will send to peers
                            send_to_peers_ids.insert(inserted_id);
                        }
                    }
                    Err((txid, error)) => {
                        tracing::debug!(?txid, ?error, "mempool transaction failed to verify");

                        metrics::counter!("mempool.failed.verify.tasks.total", 1, "reason" => error.to_string());
                        storage.reject_if_needed(txid, error);
                    }
                };
            }

            // Handle best chain tip changes
            if let Some(TipAction::Grow { block }) = tip_action {
                tracing::trace!(block_height = ?block.height, "handling blocks added to tip");

                // Cancel downloads/verifications/storage of transactions
                // with the same mined IDs as recently mined transactions.
                let mined_ids = block.transaction_hashes.iter().cloned().collect();
                tx_downloads.cancel(&mined_ids);
                storage.reject_and_remove_same_effects(&mined_ids, block.transactions);

                // Clear any transaction rejections if they might have become valid after
                // the new block was added to the tip.
                storage.clear_tip_rejections();
            }

            // Remove expired transactions from the mempool.
            //
            // Lock times never expire, because block times are strictly increasing.
            // So we don't need to check them here.
            if let Some(tip_height) = self.latest_chain_tip.best_tip_height() {
                let expired_transactions = storage.remove_expired_transactions(tip_height);
                // Remove transactions that are expired from the peers list
                send_to_peers_ids =
                    Self::remove_expired_from_peer_list(&send_to_peers_ids, &expired_transactions);

                if !expired_transactions.is_empty() {
                    tracing::debug!(
                        ?expired_transactions,
                        "removed expired transactions from the mempool",
                    );
                }
            }

            // Send transactions that were not rejected nor expired to peers
            if !send_to_peers_ids.is_empty() {
                tracing::trace!(?send_to_peers_ids, "sending new transactions to peers");

                self.transaction_sender.send(send_to_peers_ids)?;
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
                storage,
                tx_downloads,
            } => match req {
                // Queries
                Request::TransactionIds => {
                    trace!(?req, "got mempool request");

                    let res: HashSet<_> = storage.tx_ids().collect();

                    // This log line is checked by tests,
                    // because lightwalletd doesn't return mempool transactions at the moment.
                    //
                    // TODO: downgrade to trace level when we can check transactions via gRPC
                    info!(?req, res_count = ?res.len(), "answered mempool request");

                    async move { Ok(Response::TransactionIds(res)) }.boxed()
                }

                Request::TransactionsById(ref ids) => {
                    trace!(?req, "got mempool request");

                    let res: Vec<_> = storage.transactions_exact(ids.clone()).cloned().collect();

                    trace!(?req, res_count = ?res.len(), "answered mempool request");

                    async move { Ok(Response::Transactions(res)) }.boxed()
                }
                Request::TransactionsByMinedId(ref ids) => {
                    trace!(?req, "got mempool request");

                    let res: Vec<_> = storage
                        .transactions_same_effects(ids.clone())
                        .cloned()
                        .collect();

                    trace!(?req, res_count = ?res.len(), "answered mempool request");

                    async move { Ok(Response::Transactions(res)) }.boxed()
                }

                #[cfg(feature = "getblocktemplate-rpcs")]
                Request::FullTransactions => {
                    trace!(?req, "got mempool request");

                    let res: Vec<_> = storage.full_transactions().cloned().collect();

                    trace!(?req, res_count = ?res.len(), "answered mempool request");

                    async move { Ok(Response::FullTransactions(res)) }.boxed()
                }

                Request::RejectedTransactionIds(ref ids) => {
                    trace!(?req, "got mempool request");

                    let res = storage.rejected_transactions(ids.clone()).collect();

                    trace!(?req, ?res, "answered mempool request");

                    async move { Ok(Response::RejectedTransactionIds(res)) }.boxed()
                }

                // Queue mempool candidates
                Request::Queue(gossiped_txs) => {
                    trace!(req_count = ?gossiped_txs.len(), "got mempool Queue request");

                    let rsp: Vec<Result<(), BoxError>> = gossiped_txs
                        .into_iter()
                        .map(|gossiped_tx| -> Result<(), MempoolError> {
                            storage.should_download_or_verify(gossiped_tx.id())?;
                            tx_downloads.download_if_needed_and_verify(gossiped_tx)?;

                            Ok(())
                        })
                        .map(|result| result.map_err(BoxError::from))
                        .collect();
                    async move { Ok(Response::Queued(rsp)) }.boxed()
                }

                // Store successfully downloaded and verified transactions in the mempool
                Request::CheckForVerifiedTransactions => {
                    trace!(?req, "got mempool request");

                    // all the work for this request is done in poll_ready
                    async move { Ok(Response::CheckedForVerifiedTransactions) }.boxed()
                }
            },
            ActiveState::Disabled => {
                // TODO: add the name of the request, but not the content,
                //       like the command() or Display impls of network requests
                trace!("got mempool request while mempool is disabled");

                // We can't return an error since that will cause a disconnection
                // by the peer connection handler. Therefore, return successful
                // empty responses.
                let resp = match req {
                    // Return empty responses for queries.
                    Request::TransactionIds => Response::TransactionIds(Default::default()),

                    Request::TransactionsById(_) => Response::Transactions(Default::default()),
                    Request::TransactionsByMinedId(_) => Response::Transactions(Default::default()),
                    #[cfg(feature = "getblocktemplate-rpcs")]
                    Request::FullTransactions => Response::FullTransactions(Default::default()),

                    Request::RejectedTransactionIds(_) => {
                        Response::RejectedTransactionIds(Default::default())
                    }

                    // Don't queue mempool candidates, because there is no queue.
                    Request::Queue(gossiped_txs) => Response::Queued(
                        // Special case; we can signal the error inside the response,
                        // because the inbound service ignores inner errors.
                        iter::repeat(MempoolError::Disabled)
                            .take(gossiped_txs.len())
                            .map(BoxError::from)
                            .map(Err)
                            .collect(),
                    ),

                    // Check if the mempool should be enabled.
                    // This request makes sure mempools are debug-enabled in the acceptance tests.
                    Request::CheckForVerifiedTransactions => {
                        // all the work for this request is done in poll_ready
                        Response::CheckedForVerifiedTransactions
                    }
                };
                async move { Ok(resp) }.boxed()
            }
        }
    }
}
