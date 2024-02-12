//! `zebra_scan::service::ScanService` response types.

use std::collections::BTreeMap;

use zebra_chain::{block::Height, transaction};

/// A relevant transaction for a key and the block height where it was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanResult {
    /// The key that successfully decrypts the transaction
    pub key: String,

    /// The height of the block with the transaction
    pub height: Height,

    /// A transaction ID, which uniquely identifies mined v5 transactions,
    /// and all v1-v4 transactions.
    pub tx_id: transaction::Hash,
}

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
    Results(BTreeMap<String, BTreeMap<Height, Vec<transaction::Hash>>>),

    /// Response to [`DeleteKeys`](super::request::Request::DeleteKeys) request
    DeletedKeys,

    /// Response to [`ClearResults`](super::request::Request::ClearResults) request
    ClearedResults,

    /// Response to [`SubscribeResults`](super::request::Request::SubscribeResults) request
    SubscribeResults(tokio::sync::mpsc::Receiver<ScanResult>),
}
