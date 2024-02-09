//! `zebra_scan::service::ScanService` response types.

use std::{
    collections::BTreeMap,
    sync::{mpsc, Arc},
};

use zebra_chain::{block::Height, transaction::Hash};

#[derive(Debug)]
/// Response types for `zebra_scan::service::ScanService`
pub enum Response {
    /// Response to the [`Info`](super::request::Request::Info) request
    Info {
        /// The minimum sapling birthday height for the shielded scanner
        min_sapling_birthday_height: Height,
    },

    /// Response to [`RegisterKeys`](super::request::Request::RegisterKeys) request
    ///
    /// Contains the keys that the scanner accepted.
    RegisteredKeys(Vec<String>),

    /// Response to [`Results`](super::request::Request::Results) request
    ///
    /// We use the nested `BTreeMap` so we don't repeat any piece of response data.
    Results(BTreeMap<String, BTreeMap<Height, Vec<Hash>>>),

    /// Response to [`DeleteKeys`](super::request::Request::DeleteKeys) request
    DeletedKeys,

    /// Response to [`ClearResults`](super::request::Request::ClearResults) request
    ClearedResults,

    /// Response to `SubscribeResults` request
    SubscribeResults(mpsc::Receiver<Arc<Hash>>),
}
