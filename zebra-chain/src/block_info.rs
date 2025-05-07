//! Extra per-block data tracked in the state.
use crate::{amount::NonNegative, value_balance::ValueBalance};

/// Extra per-block data tracked in the state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockInfo {
    value_pools: ValueBalance<NonNegative>,
    size: u32,
}

impl BlockInfo {
    /// Creates a new [`BlockData`] with the given value pools.
    pub fn new(value_pools: ValueBalance<NonNegative>, size: u32) -> Self {
        BlockInfo { value_pools, size }
    }

    /// Returns the value pools of this block.
    pub fn value_pools(&self) -> &ValueBalance<NonNegative> {
        &self.value_pools
    }

    /// Returns the size of this block.
    pub fn size(&self) -> u32 {
        self.size
    }
}
