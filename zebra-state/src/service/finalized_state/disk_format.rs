//! Serialization formats for finalized data.

#[allow(unused_imports)]
pub use zebra_db::service::finalized_state::disk_format::{
    block, chain, shielded, transparent, FromDisk, IntoDisk, OutputLocation, RawBytes,
    TransactionIndex, TransactionLocation, MAX_ON_DISK_HEIGHT,
};

pub mod upgrade;

#[cfg(any(test, feature = "proptest-impl"))]
mod tests;

#[cfg(any(test, feature = "proptest-impl"))]
pub use tests::KV;
