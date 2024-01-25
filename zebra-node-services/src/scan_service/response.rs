//! `zebra_scan::service::ScanService` response types.

use std::sync::{mpsc, Arc};

use zebra_chain::transaction::Transaction;

#[derive(Debug)]
/// Response types for `zebra_scan::service::ScanService`
pub enum Response {
    /// Response to Results request
    Results(Vec<Transaction>),

    /// Response to SubscribeResults request
    SubscribeResults(mpsc::Receiver<Arc<Transaction>>),
}
