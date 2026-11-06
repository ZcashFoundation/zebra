//! Low-level access to RocksDB using database-specific types.

pub use zebra_db::service::finalized_state::disk_db::*;

#[cfg(any(test, feature = "proptest-impl"))]
mod tests;
