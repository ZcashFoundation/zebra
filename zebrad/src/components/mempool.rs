//! Zebra mempool.

use std::{
    collections::HashSet,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures::future::FutureExt;
use tower::Service;

use zebra_chain::transaction::{UnminedTx, UnminedTxId};

use crate::BoxError;

mod crawler;
mod error;
mod storage;

pub use self::crawler::Crawler;
pub use self::error::MempoolError;

pub enum Request {
    Store(UnminedTx),
    TransactionIds,
    TransactionsById(HashSet<UnminedTxId>),
}

pub enum Response {
    Stored(UnminedTxId),
    TransactionIds(Vec<UnminedTxId>),
    Transactions(Vec<UnminedTx>),
}

/// Mempool async management and query service.
///
/// The mempool is the set of all verified transactions that this node is aware
/// of that have yet to be confirmed by the Zcash network. A transaction is
/// confirmed when it has been included in a block ('mined').
#[derive(Default)]
pub struct Mempool {
    storage: storage::Storage,
}

impl Service<Request> for Mempool {
    type Response = Response;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    #[instrument(name = "mempool", skip(self, req))]
    fn call(&mut self, req: Request) -> Self::Future {
        match req {
            Request::Store(tx) => {
                let rsp = self.storage.insert(tx).map(Response::Stored);

                async move { rsp }.boxed()
            }
            Request::TransactionIds => {
                let res = self.storage.tx_ids();
                async move { Ok(Response::TransactionIds(res)) }.boxed()
            }
            Request::TransactionsById(ids) => {
                let rsp = Ok(self.storage.transactions(ids)).map(Response::Transactions);
                async move { rsp }.boxed()
            }
        }
    }
}
