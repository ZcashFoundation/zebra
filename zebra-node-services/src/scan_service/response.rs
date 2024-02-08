//! `zebra_scan::service::ScanService` response types.

use std::{collections::BTreeMap, sync::mpsc};

use zebra_chain::{block::Height, transaction::Hash};

#[derive(Debug)]
/// Response types for `zebra_scan::service::ScanService`
pub enum Response {
    /// Response to the `Info` request
    Info {
        /// The minimum sapling birthday height for the shielded scanner
        min_sapling_birthday_height: Height,
    },

    /// Response to Results request
    ///
    /// We use the nested `BTreeMap` so we don't repeat any piece of response data.
    Results(BTreeMap<String, BTreeMap<Height, Vec<Hash>>>),

    /// Response to DeleteKeys request
    DeletedKeys,

    /// Response to ClearResults request
    ClearedResults,

    /// Response to SubscribeResults request
    SubscribeResults(mpsc::Receiver<Hash>),
}
