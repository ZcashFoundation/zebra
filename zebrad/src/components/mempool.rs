//! Zebra mempool.

use std::{
    collections::HashSet,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures::future::FutureExt;
use tower::Service;

use zebra_chain::{
    parameters::Network,
    transaction::{UnminedTx, UnminedTxId},
};

use crate::BoxError;

mod crawler;
mod error;
mod storage;

pub use self::crawler::Crawler;
pub use self::error::MempoolError;

#[derive(Debug)]
#[allow(dead_code)]
pub enum Request {
    TransactionIds,
    TransactionsById(HashSet<UnminedTxId>),
}

#[derive(Debug)]
pub enum Response {
    Transactions(Vec<UnminedTx>),
    TransactionIds(Vec<UnminedTxId>),
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
    /// inject transactions into `storage`, as thye must be verified beforehand.
    storage: storage::Storage,
}

impl Mempool {
    #[allow(dead_code)]
    pub(crate) fn new(_network: Network) -> Self {
        Mempool {
            storage: Default::default(),
        }
    }
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
            Request::TransactionIds => {
                let res = self.storage.clone().tx_ids();
                async move { Ok(Response::TransactionIds(res)) }.boxed()
            }
            Request::TransactionsById(ids) => {
                let rsp = Ok(self.storage.clone().transactions(ids)).map(Response::Transactions);
                async move { rsp }.boxed()
            }
        }
    }
}
