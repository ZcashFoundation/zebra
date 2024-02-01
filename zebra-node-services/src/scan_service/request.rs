//! `zebra_scan::service::ScanService` request types.

#[derive(Debug)]
/// Request types for `zebra_scan::service::ScanService`
pub enum Request {
    /// Requests general info about the scanner
    Info,

    /// TODO: Accept `KeyHash`es and return key hashes that are registered
    CheckKeyHashes(Vec<()>),

    /// TODO: Accept `ViewingKeyWithHash`es and return Ok(()) if successful or an error
    RegisterKeys(Vec<()>),

    /// Deletes viewing keys and their results from the database.
    DeleteKeys(Vec<String>),

    /// Accept keys and return transaction data
    Results(Vec<String>),

    /// TODO: Accept `KeyHash`es and return a channel receiver
    SubscribeResults(Vec<()>),

    /// TODO: Accept `KeyHash`es and return transaction ids
    ClearResults(Vec<()>),
}
