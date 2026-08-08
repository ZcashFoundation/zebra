//! Per-block metadata used to schedule and accelerate synchronization.
//!
//! These are *scheduling hints and verifiable summaries*, not consensus
//! data: a synchronizing node uses them to estimate download and validation
//! work before fetching blocks, and to skip recomputing note commitment
//! trees for historical blocks. Every value is checkable against the blocks
//! themselves once they arrive, so a node must not rely on them for any
//! consensus purpose until it has done so.
//!
//! They are defined here because both the state that produces them and the
//! network that serves them need the same types.

use crate::block;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// A best-chain block hash, with the aggregated metadata of the blocks in
/// its span.
///
/// A *span* is the blocks above the previous entry's height, up to this
/// entry's own. With a stride of one, a span is a single block.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct SyncHashEntry {
    /// The hash of the block at this entry's height.
    pub hash: block::Hash,

    /// The sum of the span's blocks' size values: each block's serialized
    /// size divided by the size unit, rounded up.
    pub span_size: u64,

    /// The total number of transactions in the span's blocks, including
    /// coinbase transactions.
    pub span_txs: u64,

    /// The total number of note commitments added by the span's blocks.
    pub span_notes: u64,
}

/// The note commitment tree roots after one block, its per-pool transaction
/// counts, and its authorizing data commitment.
///
/// For a pool that is not active at this block's height, the root is 32
/// zero bytes and the count is zero.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct TreeRootsEntry {
    /// The root of the Sapling note commitment tree after this block.
    pub sapling_root: [u8; 32],

    /// The root of the Orchard note commitment tree after this block.
    pub orchard_root: [u8; 32],

    /// The root of the Ironwood note commitment tree after this block.
    pub ironwood_root: [u8; 32],

    /// The number of transactions with Sapling components in this block, as
    /// counted by its ZIP 221 chain history tree leaf.
    pub sapling_txs: u64,

    /// The number of transactions with Orchard-pool components.
    pub orchard_txs: u64,

    /// The number of transactions with Ironwood-pool components.
    pub ironwood_txs: u64,

    /// The block's ZIP 244 authorizing data commitment.
    pub auth_data_root: [u8; 32],
}
