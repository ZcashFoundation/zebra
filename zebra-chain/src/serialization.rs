//! Consensus-critical serialization.
//!
//! This module contains four traits: `ZcashSerialize` and `ZcashDeserialize`,
//! analogs of the Serde `Serialize` and `Deserialize` traits but intended for
//! consensus-critical Zcash serialization formats, and `WriteZcashExt` and
//! `ReadZcashExt`, extension traits for `io::Read` and `io::Write` with utility functions
//! for reading and writing data (e.g., the Bitcoin variable-integer format).

mod compact_size;
mod constraint;
mod date_time;
mod error;
mod read_zcash;
mod write_zcash;
mod zcash_deserialize;
mod zcash_serialize;

pub mod sha256d;

pub(crate) mod serde_helpers;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod arbitrary;

#[cfg(test)]
pub mod tests;

pub use compact_size::{CompactSize64, CompactSizeMessage};
pub use constraint::AtLeastOne;
pub use date_time::{DateTime32, Duration32};
pub use error::SerializationError;
pub use read_zcash::ReadZcashExt;
pub use serde_helpers::{HexBytes, HexSignature};
pub use write_zcash::WriteZcashExt;
pub use zcash_deserialize::{
    zcash_deserialize_bytes_external_count, zcash_deserialize_external_count,
    zcash_deserialize_string_external_count, TrustedPreallocate, ZcashDeserialize,
    ZcashDeserializeInto,
};
pub use zcash_serialize::{
    zcash_serialize_bytes, zcash_serialize_bytes_external_count, zcash_serialize_empty_list,
    zcash_serialize_external_count, FakeWriter, ZcashSerialize, MAX_PROTOCOL_MESSAGE_LEN,
};
