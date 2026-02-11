//! Consensus-critical serialization.
//!
//! This module contains four traits: `ZcashSerialize` and `ZcashDeserialize`,
//! analogs of the Serde `Serialize` and `Deserialize` traits but intended for
//! consensus-critical Zcash serialization formats, and `WriteZcashExt` and
//! `ReadZcashExt`, extension traits for `io::Read` and `io::Write` with utility functions
//! for reading and writing data (e.g., the Bitcoin variable-integer format).

use std::{cell::Cell, marker::PhantomData};

mod compact_size;
mod constraint;
mod date_time;
mod error;
mod read_zcash;
mod write_zcash;
mod zcash_deserialize;
mod zcash_serialize;

pub mod display_order;
pub mod sha256d;

pub(crate) mod serde_helpers;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod arbitrary;

#[cfg(test)]
pub mod tests;

pub use compact_size::{CompactSize64, CompactSizeMessage};
pub use constraint::AtLeastOne;
pub use date_time::{DateTime32, Duration32};
pub use display_order::BytesInDisplayOrder;
pub use error::SerializationError;
pub use read_zcash::ReadZcashExt;
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

// Trusted deserialization context

thread_local! {
    static TRUSTED_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Guard that sets the trusted deserialization flag on creation and unsets it on drop.
///
/// When active, `ZcashDeserialize` implementations may skip expensive cryptographic
/// validation for data that was already validated before being stored (e.g. blocks
/// read from the finalized state database).
///
/// Guards are re-entrant: nesting multiple guards is safe and the trusted flag
/// remains active until the outermost guard is dropped.
///
/// This guard is `!Send` because the underlying flag is thread-local. It must be
/// created and dropped on the same thread.
///
/// # Correct usage
///
/// Only create this guard when deserializing data from a **trusted source** (the
/// finalized state DB). Never hold this guard while deserializing data from the
/// network or any other untrusted source, as it would skip cryptographic validation.
pub struct TrustedDeserializationGuard(PhantomData<*const ()>);

impl TrustedDeserializationGuard {
    /// Create a new guard, incrementing the thread-local trusted depth counter.
    ///
    /// # Panics
    ///
    /// Panics if the depth counter overflows (impossible in practice).
    pub fn new() -> Self {
        let depth = TRUSTED_DEPTH.get();
        TRUSTED_DEPTH.set(
            depth
                .checked_add(1)
                .expect("TrustedDeserializationGuard nesting depth overflow"),
        );
        Self(PhantomData)
    }
}

impl Drop for TrustedDeserializationGuard {
    fn drop(&mut self) {
        TRUSTED_DEPTH.set(TRUSTED_DEPTH.get() - 1);
    }
}

/// Returns `true` if the current thread is deserializing from a trusted source.
pub fn is_trusted_source() -> bool {
    TRUSTED_DEPTH.get() > 0
}
