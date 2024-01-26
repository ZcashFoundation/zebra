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

    /// TODO: Accept `KeyHash`es and return Ok(`Vec<KeyHash>`) with hashes of deleted keys
    DeleteKeys(Vec<()>),

    /// TODO: Accept `KeyHash`es and return `Transaction`s
    Results(Vec<()>),

    /// TODO: Accept `KeyHash`es and return a channel receiver
    SubscribeResults(Vec<()>),

    /// TODO: Accept `KeyHash`es and return transaction ids
    ClearResults(Vec<()>),
}
