//! Response data types shared with state services.

use zebra_chain::block;

use crate::SnapshotBase;

/// Availability of bodies for known blocks, independent of any retention setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryCapabilities {
    /// The first retained non-genesis body height, or zero when no prefix is missing.
    ///
    /// Genesis remains available even when the suffix starts above it. Unknown blocks are not
    /// made available by this boundary, and missing bodies within the suffix indicate corruption.
    pub lowest_retained_height: block::Height,

    /// The first non-genesis height whose block the database knows at all, by hash and header:
    /// the snapshot window start of a database imported from a state snapshot, or zero.
    ///
    /// Genesis is always known. Below this height there are no blocks, only the validation
    /// state the snapshot carried.
    pub lowest_known_height: block::Height,

    /// The state snapshot the database was imported from, if any.
    pub snapshot_base: Option<SnapshotBase>,
}

impl Default for HistoryCapabilities {
    fn default() -> Self {
        Self {
            lowest_retained_height: block::Height::MIN,
            lowest_known_height: block::Height::MIN,
            snapshot_base: None,
        }
    }
}

impl HistoryCapabilities {
    /// Returns whether a non-genesis body prefix is unavailable.
    ///
    /// Missing bytes cannot distinguish intentional pruning from a corrupted archive prefix.
    pub fn is_pruned(self) -> bool {
        self.lowest_retained_height > block::Height::MIN
    }

    /// Returns whether a known block's body is retained, including the genesis exception.
    ///
    /// Callers must separately establish that the block exists.
    pub fn has_block_body(self, height: block::Height) -> bool {
        height == block::Height::MIN || height >= self.lowest_retained_height
    }

    /// Returns whether a best-chain block at `height` is known by hash and header, including
    /// the genesis exception.
    ///
    /// A database imported from a state snapshot knows no blocks between genesis and its window
    /// start; below that start, only indexes carried by the snapshot can refer to a height.
    pub fn knows_block(self, height: block::Height) -> bool {
        height == block::Height::MIN || height >= self.lowest_known_height
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// An enum of block stores in the state where a block hash could be found.
pub enum KnownBlock {
    /// Block is in the finalized portion of the best chain.
    Finalized,

    /// Block is in the best chain.
    BestChain,

    /// Block is in a side chain.
    SideChain,

    /// Block is in a block write channel
    WriteChannel,

    /// Block is queued to be validated and committed, or rejected and dropped.
    Queue,
}

impl std::fmt::Display for KnownBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KnownBlock::Finalized => write!(f, "finalized state"),
            KnownBlock::BestChain => write!(f, "best chain"),
            KnownBlock::SideChain => write!(f, "side chain"),
            KnownBlock::WriteChannel => write!(f, "block write channel"),
            KnownBlock::Queue => write!(f, "validation/commit queue"),
        }
    }
}
