//! Definitions of constants.

pub use zebra_chain::transparent::MIN_TRANSPARENT_COINBASE_MATURITY;

/// The maximum chain reorganisation height.
///
/// This threshold determines the maximum length of the best non-finalized chain.
/// Larger reorganisations would allow double-spends of coinbase transactions.
///
/// This threshold uses the relevant chain for the block being verified by the
/// non-finalized state.
///
/// For the best chain, coinbase spends are only allowed from blocks at or below
/// the finalized tip. For other chains, coinbase spends can use outputs from
/// early non-finalized blocks, or finalized blocks. But if that chain becomes
/// the best chain, all non-finalized blocks past the [`MAX_BLOCK_REORG_HEIGHT`]
/// will be finalized. This includes all mature coinbase outputs.
pub const MAX_BLOCK_REORG_HEIGHT: u32 = MIN_TRANSPARENT_COINBASE_MATURITY - 1;

/// The database format version, incremented each time the database format changes.
pub const DATABASE_FORMAT_VERSION: u32 = 25;

/// The maximum number of blocks to check for NU5 transactions,
/// before we assume we are on a pre-NU5 legacy chain.
pub const MAX_LEGACY_CHAIN_BLOCKS: usize = 100;

use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    /// Regex that matches the RocksDB error when its lock file is already open.
    pub static ref LOCK_FILE_ERROR: Regex = Regex::new("(lock file).*(temporarily unavailable)|(in use)|(being used by another process)").expect("regex is valid");
}
