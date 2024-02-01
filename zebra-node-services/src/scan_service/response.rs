//! `zebra_scan::service::ScanService` response types.

use std::sync::{mpsc, Arc};

use zebra_chain::{block::Height, transaction::Transaction};

#[derive(Debug)]
/// Response types for `zebra_scan::service::ScanService`
pub enum Response {
    /// Response to the `Info` request
    Info {
        /// The minimum sapling birthday height for the shielded scanner
        min_sapling_birthday_height: Height,
    },

    /// Response to Results request
    Results(Vec<Transaction>),

    /// Response to DeleteKeys request
    DeletedKeys,

    /// Response to ClearResults request
    ClearedResults,

    /// Response to SubscribeResults request
    SubscribeResults(mpsc::Receiver<Arc<Transaction>>),
}
