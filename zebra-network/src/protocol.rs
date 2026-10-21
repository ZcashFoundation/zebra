//! Zcash network protocol handling.

/// The external Bitcoin-based protocol.
pub mod external;
/// The internal request/response protocol.
pub mod internal;
/// Newtype wrappers giving semantic meaning to primitive datatypes.
pub mod types;
/// The version 2 stream-based protocol.
// Used by the version 2 handshake and peer set, which later commits add.
#[allow(dead_code)]
pub(crate) mod v2;
