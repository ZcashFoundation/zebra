//! Zebra mempool.

use std::{
    collections::HashSet,
    future::Future,
    iter,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{future::FutureExt, stream::Stream};
use tower::{buffer::Buffer, timeout::Timeout, util::BoxService, Service};

use zebra_chain::{
    chain_tip::ChainTip,
    parameters::Network,
    transaction::{UnminedTx, UnminedTxId},
};
use zebra_consensus::{error::TransactionError, transaction};
use zebra_network as zn;
use zebra_state as zs;
use zebra_state::{ChainTipChange, TipAction};

pub use crate::BoxError;

mod crawler;
pub mod downloads;
mod error;
mod storage;

#[cfg(test)]
mod tests;

pub use self::crawler::Crawler;
pub use self::error::MempoolError;
pub use self::storage::{
    ExactTipRejectionError, SameEffectsChainRejectionError, SameEffectsTipRejectionError,
};

#[cfg(test)]
pub use self::storage::tests::unmined_transactions_in_blocks;

use self::downloads::{
    Downloads as TxDownloads, Gossip, TransactionDownloadVerifyError, TRANSACTION_DOWNLOAD_TIMEOUT,
    TRANSACTION_VERIFY_TIMEOUT,
};

use super::sync::SyncStatus;

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
enum ActiveState {
    /// The Mempool is disabled.
    Disabled,
    /// The Mempool is enabled.
    Enabled {
        /// The Mempool storage itself.
        ///
        /// ##: Correctness: only components internal to the [`Mempool`] struct are allowed to
        /// inject transactions into `storage`, as transactions must be verified beforehand.
        storage: storage::Storage,
        /// The transaction download and verify stream.
        tx_downloads: Pin<Box<InboundTxDownloads>>,
    },
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

    /// Allow efficient access to the best tip of the blockchain.
    latest_chain_tip: zs::LatestChainTip,
    /// Allows the detection of chain tip resets.
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
}

impl Mempool {
    pub(crate) fn new(
        _network: Network,
        outbound: Outbound,
        state: State,
        tx_verifier: TxVerifier,
        sync_status: SyncStatus,
        latest_chain_tip: zs::LatestChainTip,
        chain_tip_change: ChainTipChange,
    ) -> Self {
        Mempool {
            active_state: ActiveState::Disabled,
            sync_status,
            latest_chain_tip,
            chain_tip_change,
            outbound,
            state,
            tx_verifier,
        }
    }

    /// Update the mempool state (enabled / disabled) depending on how close to
    /// the tip is the synchronization, including side effects to state changes.
    fn update_state(&mut self) {
        let is_close_to_tip = self.sync_status.is_close_to_tip();
        if self.is_enabled() == is_close_to_tip {
            // the active state is up to date
            return;
        }

        // Update enabled / disabled state
        if is_close_to_tip {
            let tx_downloads = Box::pin(TxDownloads::new(
                Timeout::new(self.outbound.clone(), TRANSACTION_DOWNLOAD_TIMEOUT),
                Timeout::new(self.tx_verifier.clone(), TRANSACTION_VERIFY_TIMEOUT),
                self.state.clone(),
            ));
            self.active_state = ActiveState::Enabled {
                storage: Default::default(),
                tx_downloads,
            };
        } else {
            self.active_state = ActiveState::Disabled
        }
    }

    /// Return whether the mempool is enabled or not.
    pub fn is_enabled(&self) -> bool {
        match self.active_state {
            ActiveState::Disabled => false,
            ActiveState::Enabled { .. } => true,
        }
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
        self.update_state();

        match &mut self.active_state {
            ActiveState::Enabled {
                storage,
                tx_downloads,
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

                // Clean up completed download tasks and add to mempool if successful.
                while let Poll::Ready(Some(r)) = tx_downloads.as_mut().poll_next(cx) {
                    match r {
                        Ok(tx) => {
                            // Storage handles conflicting transactions or a full mempool internally,
                            // so just ignore the storage result here
                            let _ = storage.insert(tx);
                        }
                        Err((txid, e)) => {
                            reject_if_needed(storage, txid, e);
                            // TODO: should we also log the result?
                        }
                    };
                }

                // Remove expired transactions from the mempool.
                if let Some(tip_height) = self.latest_chain_tip.best_tip_height() {
                    remove_expired_transactions(storage, tip_height);
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
                storage,
                tx_downloads,
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
) {
    let mut txid_set = HashSet::new();

    for t in storage.transactions() {
        if let Some(expiry_height) = t.transaction.expiry_height() {
            if tip_height >= expiry_height {
                txid_set.insert(t.id.mined_id());
            }
        }
    }

    // expiry height is effecting data, so we match by non-malleable TXID
    storage.remove_same_effects(&txid_set);
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
            storage.reject(txid, storage::State::Invalid(e))
        }
    }
}
