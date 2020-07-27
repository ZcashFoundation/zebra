//! The HistoryRootHash enum.

/// History root hashes.
///
/// The `BlockHeader.history_root_hash` field is interpreted differently, based
/// on the current network upgrade.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryRootHash {
    /// An all-zeroes reserved field.
    ///
    /// Used for pre-Sapling blocks.
    Reserved,

    /// The final Sapling treestate of this block.
    ///
    /// Used for Sapling and Blossom blocks.
    FinalSaplingRoot(SaplingNoteTreeRootHash),

    /// The root hash of a Merkle Mountain Range, which commits to various
    /// features of the chain history.
    ///
    /// Used from Heartwood onwards.
    // TODO: replace with a ChainHistoryMmrRootHash type.
    ChainHistoryRoot([u8; 32]),
}

// TODO:
//  - implement new([u8; 32], Network, BlockHeight)
