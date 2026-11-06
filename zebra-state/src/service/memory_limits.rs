//! Memory limits zebrad passes to the state service when it opens it.

use crate::service::{finalized_state::RocksDbMemoryLimits, queued_blocks::MAX_QUEUED_BYTES};

#[cfg(test)]
mod tests;

/// The atomic sentinel for no non-finalized resident target.
pub(crate) const NO_NON_FINALIZED_RESIDENT_LIMIT: u64 = u64::MAX;

/// Memory limits for the state service, derived from zebrad's memory budget and passed to
/// [`init_with_limits`](crate::init_with_limits).
///
/// [`MemoryLimits::default()`] is today's behaviour: [`init`](crate::init) opens the state with
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryLimits {
    /// The memory bounds of the finalized state's RocksDB database.
    pub rocksdb: RocksDbMemoryLimits,

    /// The target resident bytes of distinct non-finalized blocks, charged with
    /// [`block_retained_charge`](crate::block_retained_charge). Above the target, the writer
    /// finalizes best-chain roots until it reaches the target or preserves only the minimum
    /// 100-block rollback window.
    ///
    /// `None` finalizes only past the usual 1,000-block reorg limit, which is today's behaviour.
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
