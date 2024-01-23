//! `zebra_scan::service::ScanService` response types.

use std::sync::{mpsc, Arc};

use zebra_chain::transaction::Transaction;

#[derive(Debug)]
/// Response types for `zebra_scan::service::ScanService`
pub enum Response {
    /// Response to SubscribeResults
    ResultsReceiver(mpsc::Receiver<Arc<Transaction>>),
}
