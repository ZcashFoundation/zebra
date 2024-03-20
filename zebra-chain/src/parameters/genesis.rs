//! Genesis consensus parameters for each Zcash network.

/// The previous block hash for the genesis block.
///
/// All known networks use the Bitcoin `null` value for the parent of the
/// genesis block. (In Bitcoin, `null` is `[0; 32]`.)
pub const GENESIS_PREVIOUS_BLOCK_HASH: crate::block::Hash = crate::block::Hash([0; 32]);
