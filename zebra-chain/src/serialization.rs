//! Consensus-critical serialization.
//!
//! This module contains four traits: `ZcashSerialize` and `ZcashDeserialize`,
//! analogs of the Serde `Serialize` and `Deserialize` traits but intended for
//! consensus-critical Zcash serialization formats, and `WriteZcashExt` and
//! `ReadZcashExt`, extension traits for `io::Read` and `io::Write` with utility functions
//! for reading and writing data (e.g., the Bitcoin variable-integer format).

mod error;
mod read_zcash;
mod write_zcash;
mod zcash_deserialize;
mod zcash_serialize;

pub(crate) mod serde_helpers;

pub mod sha256d;

pub use error::SerializationError;
pub use read_zcash::ReadZcashExt;
pub use write_zcash::WriteZcashExt;
pub use zcash_deserialize::{
    zcash_deserialize_bytes_external_count, zcash_deserialize_external_count, TrustedPreallocate,
    ZcashDeserialize, ZcashDeserializeInto,
};
pub use zcash_serialize::{
    zcash_serialize_external_count, ZcashSerialize, MAX_PROTOCOL_MESSAGE_LEN,
};

#[cfg(test)]
mod proptests;
