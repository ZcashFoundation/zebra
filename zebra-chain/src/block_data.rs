//! Extra per-block data tracked in the state.
use crate::{amount::NonNegative, value_balance::ValueBalance};

/// Extra per-block data tracked in the state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockData {
    value_pools: ValueBalance<NonNegative>,
}

impl BlockData {
    /// Creates a new [`BlockData`] with the given value pools.
    pub fn new(value_pools: ValueBalance<NonNegative>) -> Self {
        BlockData { value_pools }
    }

    /// Returns the value pools of this block.
    pub fn value_pools(&self) -> &ValueBalance<NonNegative> {
        &self.value_pools
    }
}
