//! Scan [`tower::Service`] response types.

use std::sync::{mpsc, Arc};

use zebra_chain::transaction::Transaction;

#[derive(Debug)]
/// Response types for [`ScanService`](crate::service::ScanService)
pub enum Response {
    /// Response to SubscribeResults
    ResultsReceiver(mpsc::Receiver<Arc<Transaction>>),
}
