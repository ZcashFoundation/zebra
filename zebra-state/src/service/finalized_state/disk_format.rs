//! Serialization formats for finalized data.

pub use zebra_db::service::finalized_state::disk_format::*;

pub mod upgrade;

#[cfg(any(test, feature = "proptest-impl"))]
mod tests;

#[cfg(any(test, feature = "proptest-impl"))]
pub use tests::KV;
