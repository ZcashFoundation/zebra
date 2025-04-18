//! Constants that impact state behaviour.

use lazy_static::lazy_static;
use regex::Regex;
use semver::Version;

// For doc comment links
#[allow(unused_imports)]
use crate::{
    config::{self, Config},
    constants,
};

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
//
// TODO: change to HeightDiff
pub const MAX_BLOCK_REORG_HEIGHT: u32 = MIN_TRANSPARENT_COINBASE_MATURITY - 1;

/// The directory name used to distinguish the state database from Zebra's other databases or flat files.
pub const STATE_DATABASE_KIND: &str = "state";

/// The database format major version, incremented each time the on-disk database format has a
/// breaking data format change.
///
/// Breaking changes include:
/// - deleting a column family, or
/// - changing a column family's data format in an incompatible way.
///
/// Breaking changes become minor version changes if:
/// - we previously added compatibility code, and
/// - it's available in all supported Zebra versions.
///
/// Instead of using this constant directly, use [`constants::state_database_format_version_in_code()`]
/// or [`config::database_format_version_on_disk()`] to get the full semantic format version.
const DATABASE_FORMAT_VERSION: u64 = 26;

/// The database format minor version, incremented each time the on-disk database format has a
/// significant data format change.
///
/// Significant changes include:
/// - adding new column families,
/// - changing the format of a column family in a compatible way, or
/// - breaking changes with compatibility code in all supported Zebra versions.
const DATABASE_FORMAT_MINOR_VERSION: u64 = 1;

/// The database format patch version, incremented each time the on-disk database format has a
/// significant format compatibility fix.
const DATABASE_FORMAT_PATCH_VERSION: u64 = 0;

/// Returns the full semantic version of the currently running state database format code.
///
/// This is the version implemented by the Zebra code that's currently running,
/// the minor and patch versions on disk can be different.
pub fn state_database_format_version_in_code() -> Version {
    Version {
        major: DATABASE_FORMAT_VERSION,
        minor: DATABASE_FORMAT_MINOR_VERSION,
        patch: DATABASE_FORMAT_PATCH_VERSION,
        pre: semver::Prerelease::EMPTY,
        #[cfg(feature = "indexer")]
        build: semver::BuildMetadata::new("indexer").expect("hard-coded value should be valid"),
        #[cfg(not(feature = "indexer"))]
        build: semver::BuildMetadata::EMPTY,
    }
}

/// The name of the file containing the minor and patch database versions.
///
/// Use [`Config::version_file_path()`] to get the path to this file.
pub(crate) const DATABASE_FORMAT_VERSION_FILE_NAME: &str = "version";

/// The maximum number of blocks to check for NU5 transactions,
/// before we assume we are on a pre-NU5 legacy chain.
///
/// Zebra usually only has to check back a few blocks on mainnet, but on testnet it can be a long
/// time between v5 transactions.
pub const MAX_LEGACY_CHAIN_BLOCKS: usize = 100_000;

/// The maximum number of non-finalized chain forks Zebra will track.
/// When this limit is reached, we drop the chain with the lowest work.
///
/// When the network is under heavy transaction load, there are around 5 active forks in the last
/// 100 blocks. (1 fork per 20 blocks.) When block propagation is efficient, there is around
/// 1 fork per 300 blocks.
///
/// This limits non-finalized chain memory to around:
/// `10 forks * 100 blocks * 2 MB per block = 2 GB`
pub const MAX_NON_FINALIZED_CHAIN_FORKS: usize = 10;

/// The maximum number of block hashes allowed in `getblocks` responses in the Zcash network protocol.
pub const MAX_FIND_BLOCK_HASHES_RESULTS: u32 = 500;

/// The maximum number of block headers allowed in `getheaders` responses in the Zcash network protocol.
pub const MAX_FIND_BLOCK_HEADERS_RESULTS: u32 = 160;

/// These database versions can be recreated from their directly preceding versions.
pub const RESTORABLE_DB_VERSIONS: [u64; 1] = [26];

/// The maximum number of invalidated block records.
///
/// This limits the memory use to around:
/// `100 entries * up to 99 blocks * 2 MB per block = 20 GB`
pub const MAX_INVALIDATED_BLOCKS: usize = 100;

lazy_static! {
    /// Regex that matches the RocksDB error when its lock file is already open.
    pub static ref LOCK_FILE_ERROR: Regex = Regex::new("(lock file).*(temporarily unavailable)|(in use)|(being used by another process)").expect("regex is valid");
}
