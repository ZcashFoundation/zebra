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
    Downloads as TxDownloads, GossipedTx, TRANSACTION_DOWNLOAD_TIMEOUT, TRANSACTION_VERIFY_TIMEOUT,
};

type Outbound = Buffer<BoxService<zn::Request, zn::Response, zn::BoxError>, zn::Request>;
type State = Buffer<BoxService<zs::Request, zs::Response, zs::BoxError>, zs::Request>;
type TxVerifier = Buffer<
    BoxService<transaction::Request, transaction::Response, TransactionError>,
    transaction::Request,
>;
type InboundTxDownloads = TxDownloads<Timeout<Outbound>, Timeout<TxVerifier>, State>;

/// The result of attempting to queue a transaction for downloading/
/// verifying.
#[derive(Debug)]
pub enum DownloadAction {
    DownloadAction(self::downloads::DownloadAction),
    InMempool,
    Rejected,
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum Request {
    TransactionIds,
    TransactionsById(HashSet<UnminedTxId>),
    RejectedTransactionIds(HashSet<UnminedTxId>),
    DownloadAndVerify(Vec<GossipedTx>),
}

#[derive(Debug)]
pub enum Response {
    Transactions(Vec<UnminedTx>),
    TransactionIds(Vec<UnminedTxId>),
    RejectedTransactionIds(Vec<UnminedTxId>),
    DownloadActions(Vec<DownloadAction>),
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
}

impl Mempool {
    #[allow(dead_code)]
    pub(crate) fn new(
        _network: Network,
        outbound: Outbound,
        state: State,
        tx_verifier: TxVerifier,
    ) -> Self {
        let tx_downloads = Box::pin(TxDownloads::new(
            Timeout::new(outbound, TRANSACTION_DOWNLOAD_TIMEOUT),
            Timeout::new(tx_verifier, TRANSACTION_VERIFY_TIMEOUT),
            state,
        ));
        Mempool {
            storage: Default::default(),
            tx_downloads,
        }
    }

    ///  Get the storage field of the mempool for testing purposes.
    #[cfg(test)]
    pub fn storage(&mut self) -> &mut storage::Storage {
        &mut self.storage
    }
}

impl Service<Request> for Mempool {
    type Response = Response;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
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
                let res = self.storage.clone().tx_ids();
                async move { Ok(Response::TransactionIds(res)) }.boxed()
            }
            Request::TransactionsById(ids) => {
                let rsp = Ok(self.storage.clone().transactions(ids)).map(Response::Transactions);
                async move { rsp }.boxed()
            }
            Request::RejectedTransactionIds(ids) => {
                let rsp = Ok(self.storage.clone().rejected_transactions(ids))
                    .map(Response::RejectedTransactionIds);
                async move { rsp }.boxed()
            }
            Request::DownloadAndVerify(gossiped_txs) => {
                let rsp = gossiped_txs
                    .into_iter()
                    .map(|gossiped_tx| {
                        let r = self.should_download_or_verify(gossiped_tx.id());
                        if let Err(action) = r {
                            return action;
                        }
                        DownloadAction::DownloadAction(
                            self.tx_downloads.download_if_needed_and_verify(gossiped_tx),
                        )
                    })
                    .collect();
                async move { Ok(Response::DownloadActions(rsp)) }.boxed()
            }
        }
    }
}

impl Mempool {
    /// Check if transaction should be downloaded and/or verified.
    ///
    /// If it is already in the mempool (or in its rejected list)
    /// then it shouldn't be downloaded/verified.
    fn should_download_or_verify(&mut self, txid: UnminedTxId) -> Result<(), DownloadAction> {
        // Check if the transaction is already in the mempool.
        if self.storage.clone().contains(&txid) {
            return Err(DownloadAction::InMempool);
        }
        if self.storage.clone().contains_rejected(&txid) {
            return Err(DownloadAction::Rejected);
        }
        Ok(())
    }
}
