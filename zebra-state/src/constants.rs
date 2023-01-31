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
///
/// Zebra usually only has to check back a few blocks, but on testnet it can be a long time between v5 transactions.
pub const MAX_LEGACY_CHAIN_BLOCKS: usize = 100_000;

/// The maximum number of block hashes allowed in `getblocks` responses in the Zcash network protocol.
pub const MAX_FIND_BLOCK_HASHES_RESULTS: u32 = 500;

/// The maximum number of block headers allowed in `getheaders` responses in the Zcash network protocol.
const MAX_FIND_BLOCK_HEADERS_RESULTS_FOR_PROTOCOL: u32 = 160;

/// The maximum number of block headers sent by Zebra in `getheaders` responses.
///
/// Older versions of Zcashd will blindly request more block headers as long as it
/// got 160 block headers in response to a previous query,
/// _even if those headers are already known_.
///
/// To avoid this behavior, return slightly fewer than the maximum,
/// so `zcashd` thinks it has reached our chain tip.
///
/// <https://github.com/bitcoin/bitcoin/pull/4468/files#r17026905>
pub const MAX_FIND_BLOCK_HEADERS_RESULTS_FOR_ZEBRA: u32 =
    MAX_FIND_BLOCK_HEADERS_RESULTS_FOR_PROTOCOL - 2;

use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    /// Regex that matches the RocksDB error when its lock file is already open.
    pub static ref LOCK_FILE_ERROR: Regex = Regex::new("(lock file).*(temporarily unavailable)|(in use)|(being used by another process)").expect("regex is valid");
}
