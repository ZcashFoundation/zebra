//! State snapshots: a pruned node's finalized state at a height `H`.

pub use zebra_db::state_snapshot::*;

pub mod export;
pub mod import;
