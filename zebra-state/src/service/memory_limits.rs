//! Memory limits zebrad passes to the state service when it opens it.

use crate::service::{finalized_state::RocksDbMemoryLimits, queued_blocks::MAX_QUEUED_BYTES};

#[cfg(test)]
mod tests;

/// Memory limits for the state service, derived from zebrad's memory budget and passed to
/// [`init_with_limits`](crate::init_with_limits).
///
/// [`MemoryLimits::default()`] is today's behaviour: [`init`](crate::init) opens the state with
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryLimits {
    /// The memory bounds of the finalized state's RocksDB database.
    pub rocksdb: RocksDbMemoryLimits,

    /// The target resident bytes of non-finalized block bodies, charged with
    /// [`block_retained_charge`](crate::block_retained_charge).
    ///
    /// `None` never evicts a body from memory, which is today's behaviour.
    pub non_finalized_resident_bytes: Option<u64>,

    /// The retained bytes of the blocks queued while they wait for their parent block, above
    /// which the highest queued blocks are dropped.
    pub queued_blocks_bytes: usize,
}

impl Default for MemoryLimits {
    fn default() -> Self {
        Self {
            rocksdb: RocksDbMemoryLimits::default(),
            non_finalized_resident_bytes: None,
            queued_blocks_bytes: MAX_QUEUED_BYTES,
        }
    }
}
