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
    parameters::Network,
    transaction::{UnminedTx, UnminedTxId},
};
use zebra_consensus::{error::TransactionError, transaction};
use zebra_network as zn;
use zebra_state as zs;
use zs::ChainTipChange;

pub use crate::BoxError;

mod crawler;
pub mod downloads;
mod error;
mod storage;

#[cfg(test)]
mod tests;

pub use self::crawler::Crawler;
pub use self::error::MempoolError;
#[cfg(test)]
pub use self::storage::tests::unmined_transactions_in_blocks;

use self::downloads::{
    Downloads as TxDownloads, Gossip, TRANSACTION_DOWNLOAD_TIMEOUT, TRANSACTION_VERIFY_TIMEOUT,
};

#[cfg(test)]
use super::sync::RecentSyncLengths;
use super::sync::SyncStatus;

type OutboundService = Buffer<BoxService<zn::Request, zn::Response, zn::BoxError>, zn::Request>;
type StateService = Buffer<BoxService<zs::Request, zs::Response, zs::BoxError>, zs::Request>;
type TxVerifierService = Buffer<
    BoxService<transaction::Request, transaction::Response, TransactionError>,
    transaction::Request,
>;
type InboundTxDownloads =
    TxDownloads<Timeout<OutboundService>, Timeout<TxVerifierService>, StateService>;

#[derive(Debug)]
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
enum State {
    /// The Mempool is disabled.
    Disabled,
    /// The Mempool is enabled.
    Enabled {
        /// The Mempool storage itself.
        ///
        /// ##: Correctness: only components internal to the [`Mempool`] struct are allowed to
        /// inject transactions into `storage`, as transactions must be verified beforehand.
        storage: storage::Storage,
        /// The transaction dowload and verify stream.
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
    state: State,

    /// Allows checking if we are near the tip to enable/disable the mempool.
    #[allow(dead_code)]
    sync_status: SyncStatus,

    /// Allows the detection of chain tip resets.
    #[allow(dead_code)]
    chain_tip_change: ChainTipChange,

    /// Handle to the outbound service.
    /// Used to construct the transaction downloader.
    outbound_service: OutboundService,

    /// Handle to the state service.
    /// Used to construct the transaction downloader.
    state_service: StateService,

    /// Handle to the transaction verifier service.
    /// Used to construct the transaction downloader.
    tx_verifier_service: TxVerifierService,
}

impl Mempool {
    #[allow(dead_code)]
    pub(crate) fn new(
        _network: Network,
        outbound_service: OutboundService,
        state_service: StateService,
        tx_verifier_service: TxVerifierService,
        sync_status: SyncStatus,
        chain_tip_change: ChainTipChange,
    ) -> Self {
        Mempool {
            state: State::Disabled,
            sync_status,
            chain_tip_change,
            outbound_service,
            state_service,
            tx_verifier_service,
        }
    }

    /// Update the mempool state (enabled / disabled) depending on how close to
    /// the tip is the synchronization, including side effects to state changes.
    fn update_state(&mut self) {
        // Update enabled / disabled state
        let is_close_to_tip = self.sync_status.is_close_to_tip();
        if is_close_to_tip && matches!(self.state, State::Disabled) {
            let tx_downloads = Box::pin(TxDownloads::new(
                Timeout::new(self.outbound_service.clone(), TRANSACTION_DOWNLOAD_TIMEOUT),
                Timeout::new(self.tx_verifier_service.clone(), TRANSACTION_VERIFY_TIMEOUT),
                self.state_service.clone(),
            ));
            self.state = State::Enabled {
                storage: Default::default(),
                tx_downloads,
            };
        } else if !is_close_to_tip && matches!(self.state, State::Enabled { .. }) {
            self.state = State::Disabled
        }
    }

    /// Return wether the mempool is enabled or not.
    #[allow(dead_code)]
    pub fn enabled(&self) -> bool {
        match self.state {
            State::Disabled => false,
            State::Enabled { .. } => true,
        }
    }

    /// Get the storage field of the mempool for testing purposes.
    #[cfg(test)]
    pub fn storage(&mut self) -> &mut storage::Storage {
        match &mut self.state {
            State::Disabled => panic!("mempool must be enabled"),
            State::Enabled { storage, .. } => storage,
        }
    }

    /// Get the transaction downloader of the mempool for testing purposes.
    #[cfg(test)]
    pub fn tx_downloads(&self) -> &Pin<Box<InboundTxDownloads>> {
        match &self.state {
            State::Disabled => panic!("mempool must be enabled"),
            State::Enabled { tx_downloads, .. } => tx_downloads,
        }
    }

    /// Enable the mempool by pretending the synchronization is close to the tip.
    #[cfg(test)]
    pub async fn enable(&mut self, recent_syncs: &mut RecentSyncLengths) {
        use tower::ServiceExt;
        // Pretend we're close to tip
        SyncStatus::sync_close_to_tip(recent_syncs);
        // Wait for the mempool to make it enable itself
        let _ = self.ready_and().await;
    }

    /// Disable the mempool by pretending the synchronization is far from the tip.
    #[cfg(test)]
    pub async fn disable(&mut self, recent_syncs: &mut RecentSyncLengths) {
        use tower::ServiceExt;
        // Pretend we're far from the tip
        SyncStatus::sync_far_from_tip(recent_syncs);
        // Wait for the mempool to make it enable itself
        let _ = self.ready_and().await;
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
        if storage.contains(&txid) {
            return Err(MempoolError::InMempool);
        }
        if storage.contains_rejected(&txid) {
            return Err(MempoolError::Rejected);
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

        match &mut self.state {
            State::Enabled {
                storage,
                tx_downloads,
            } => {
                // Clean up completed download tasks and add to mempool if successful
                while let Poll::Ready(Some(r)) = tx_downloads.as_mut().poll_next(cx) {
                    if let Ok(tx) = r {
                        // TODO: should we do something with the result?
                        let _ = storage.insert(tx);
                    }
                }
            }
            State::Disabled => {
                // When the mempool is disabled we still return that the service is ready.
                // Otherwise, callers could block waiting for the mempool to be enabled,
                // which may not be the desired behaviour.
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
        match &mut self.state {
            State::Enabled {
                storage,
                tx_downloads,
            } => match req {
                Request::TransactionIds => {
                    let res = storage.tx_ids();
                    async move { Ok(Response::TransactionIds(res)) }.boxed()
                }
                Request::TransactionsById(ids) => {
                    let rsp = Ok(storage.transactions(ids)).map(Response::Transactions);
                    async move { rsp }.boxed()
                }
                Request::RejectedTransactionIds(ids) => {
                    let rsp = Ok(storage.rejected_transactions(ids))
                        .map(Response::RejectedTransactionIds);
                    async move { rsp }.boxed()
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
            State::Disabled => {
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
