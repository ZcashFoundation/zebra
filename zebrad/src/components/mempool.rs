//! Zebra mempool.

use std::{
    collections::HashSet,
    future::Future,
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
#[cfg(test)]
pub use self::storage::tests::unmined_transactions_in_blocks;

use self::downloads::{
    Downloads as TxDownloads, Gossip, TRANSACTION_DOWNLOAD_TIMEOUT, TRANSACTION_VERIFY_TIMEOUT,
};

use super::sync::SyncStatus;

type Outbound = Buffer<BoxService<zn::Request, zn::Response, zn::BoxError>, zn::Request>;
type State = Buffer<BoxService<zs::Request, zs::Response, zs::BoxError>, zs::Request>;
type TxVerifier = Buffer<
    BoxService<transaction::Request, transaction::Response, TransactionError>,
    transaction::Request,
>;
type InboundTxDownloads = TxDownloads<Timeout<Outbound>, Timeout<TxVerifier>, State>;

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

/// Mempool async management and query service.
///
/// The mempool is the set of all verified transactions that this node is aware
/// of that have yet to be confirmed by the Zcash network. A transaction is
/// confirmed when it has been included in a block ('mined').
pub struct Mempool {
    /// The Mempool storage itself.
    ///
    /// ##: Correctness: only components internal to the [`Mempool`] struct are allowed to
    /// inject transactions into `storage`, as transactions must be verified beforehand.
    storage: storage::Storage,

    /// The transaction dowload and verify stream.
    tx_downloads: Pin<Box<InboundTxDownloads>>,

    /// Allows checking if we are near the tip to enable/disable the mempool.
    #[allow(dead_code)]
    sync_status: SyncStatus,

    chain_tip_change: ChainTipChange,
}

impl Mempool {
    #[allow(dead_code)]
    pub(crate) fn new(
        _network: Network,
        outbound: Outbound,
        state: State,
        tx_verifier: TxVerifier,
        sync_status: SyncStatus,
        chain_tip_change: ChainTipChange,
    ) -> Self {
        let tx_downloads = Box::pin(TxDownloads::new(
            Timeout::new(outbound, TRANSACTION_DOWNLOAD_TIMEOUT),
            Timeout::new(tx_verifier, TRANSACTION_VERIFY_TIMEOUT),
            state,
        ));

        Mempool {
            storage: Default::default(),
            tx_downloads,
            sync_status,
            chain_tip_change,
        }
    }

    /// Get the storage field of the mempool for testing purposes.
    #[cfg(test)]
    pub fn storage(&mut self) -> &mut storage::Storage {
        &mut self.storage
    }

    /// Check if transaction should be downloaded and/or verified.
    ///
    /// If it is already in the mempool (or in its rejected list)
    /// then it shouldn't be downloaded/verified.
    fn should_download_or_verify(&mut self, txid: UnminedTxId) -> Result<(), MempoolError> {
        // Check if the transaction is already in the mempool.
        if self.storage.contains(&txid) {
            return Err(MempoolError::InMempool);
        }
        if self.storage.contains_rejected(&txid) {
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
        // Clear the mempool if there has been a chain tip reset.
        if let Some(TipAction::Reset { height: _, hash: _ }) =
            self.chain_tip_change.get_tip_change()
        {
            self.storage.clear();
        }

        // Clean up completed download tasks and add to mempool if successful
        while let Poll::Ready(Some(r)) = self.tx_downloads.as_mut().poll_next(cx) {
            if let Ok(tx) = r {
                // TODO: should we do something with the result?
                let _ = self.storage.insert(tx);
            }
        }
        Poll::Ready(Ok(()))
    }

    #[instrument(name = "mempool", skip(self, req))]
    fn call(&mut self, req: Request) -> Self::Future {
        match req {
            Request::TransactionIds => {
                let res = self.storage.tx_ids();
                async move { Ok(Response::TransactionIds(res)) }.boxed()
            }
            Request::TransactionsById(ids) => {
                let rsp = Ok(self.storage.transactions(ids)).map(Response::Transactions);
                async move { rsp }.boxed()
            }
            Request::RejectedTransactionIds(ids) => {
                let rsp = Ok(self.storage.rejected_transactions(ids))
                    .map(Response::RejectedTransactionIds);
                async move { rsp }.boxed()
            }
            Request::Queue(gossiped_txs) => {
                let rsp: Vec<Result<(), MempoolError>> = gossiped_txs
                    .into_iter()
                    .map(|gossiped_tx| {
                        self.should_download_or_verify(gossiped_tx.id())?;
                        self.tx_downloads
                            .download_if_needed_and_verify(gossiped_tx)?;
                        Ok(())
                    })
                    .collect();
                async move { Ok(Response::Queued(rsp)) }.boxed()
            }
        }
    }
}
