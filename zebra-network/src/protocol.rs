//! Zcash network protocol handling.

/// The external Bitcoin-based protocol.
#[allow(clippy::arc_with_non_send_sync)]
pub mod external;
#[allow(clippy::arc_with_non_send_sync)]
/// The internal request/response protocol.
pub mod internal;
/// Newtype wrappers giving semantic meaning to primitive datatypes.
pub mod types;
