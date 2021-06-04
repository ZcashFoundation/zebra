//! Definitions of constants.

/// The maturity threshold for transparent coinbase outputs.
///
/// This threshold uses the relevant chain for the block being verified by the
/// non-finalized state.
///
/// For the best chain, coinbase spends are only allowed from blocks at or below
/// the finalized tip. For other chains, coinbase spends can use outputs from
/// early non-finalized blocks, or finalized blocks. But if that chain becomes
/// the best chain, all non-finalized blocks past the [`MAX_BLOCK_REORG_HEIGHT`]
/// will be finalized. This includes all mature coinbase outputs.
///
/// "A transaction MUST NOT spend a transparent output of a coinbase transaction
/// from a block less than 100 blocks prior to the spend. Note that transparent
/// outputs of coinbase transactions include Founders' Reward outputs and
/// transparent Funding Stream outputs."
/// [7.1](https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus)
pub const MIN_TRANSPARENT_COINBASE_MATURITY: u32 = 100;

/// The maximum chain reorganisation height.
///
/// This threshold determines the maximum length of the best non-finalized chain.
///
/// Larger reorganisations would allow double-spends of coinbase transactions.
pub const MAX_BLOCK_REORG_HEIGHT: u32 = MIN_TRANSPARENT_COINBASE_MATURITY - 1;

/// The database format version, incremented each time the database format changes.
pub const DATABASE_FORMAT_VERSION: u32 = 5;

use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    /// Regex that matches the RocksDB error when its lock file is already open.
    pub static ref LOCK_FILE_ERROR: Regex = Regex::new("(lock file).*(temporarily unavailable)|(in use)|(being used by another process)").expect("regex is valid");
}
