//! Constants that impact state behaviour.

use lazy_static::lazy_static;
use regex::Regex;
use semver::Version;

use zebra_chain::parameters::{Network, NetworkKind};

// For doc comment links
#[allow(unused_imports)]
use crate::{
    config::{self, Config},
    constants,
};

pub use zebra_chain::transparent::MIN_TRANSPARENT_COINBASE_MATURITY;

/// The maximum chain reorganisation height.
///
/// This threshold determines the maximum length of the best non-finalized
/// chain. Once the chain grows past this height, Zebra finalizes its oldest
/// blocks; deeper reorganisations are outside Zebra's rollback window.
///
/// This is a local-only node policy; it is not part of consensus. The window is
/// sized as a defence-in-depth measure against sustained consensus splits.
//
// TODO: change to HeightDiff
pub const MAX_BLOCK_REORG_HEIGHT: u32 = 1000;

/// The directory name used to distinguish the state database from Zebra's other databases or flat files.
pub const STATE_DATABASE_KIND: &str = "state";

/// The minimum retention window allowed in pruned storage mode.
///
/// Pruned mode deletes historical raw transaction data at `tip - retention`. The
/// retention window must be strictly greater than [`MAX_BLOCK_REORG_HEIGHT`], so
/// that pruning can never delete data that a reorg or rollback could still read.
///
/// This floor is sized well above the reorg window (10x) to also cover coinbase
/// maturity ([`MIN_TRANSPARENT_COINBASE_MATURITY`]) and leave operational
/// headroom. At the current 75-second block target, it retains more than a week
/// of raw transaction data. Configs below this value are rejected at startup.
///
/// This is the floor for Mainnet and Testnet; use [`min_pruning_retention`] to
/// get the network-specific floor.
pub const MIN_PRUNING_RETENTION: u32 = 10_000;

/// The minimum retention window allowed in pruned storage mode on `network`.
///
/// Every network's floor stays strictly greater than [`MAX_BLOCK_REORG_HEIGHT`],
/// so pruning can never delete data that a reorg or rollback could still read.
///
/// Mainnet and Testnet use [`MIN_PRUNING_RETENTION`], which adds operational
/// headroom above the reorg window. Regtest drops that headroom (but still covers
/// the reorg window) so tests can cross the retention boundary without committing
/// a 10_000+ block chain.
pub fn min_pruning_retention(network: &Network) -> u32 {
    match network.kind() {
        NetworkKind::Regtest => MAX_BLOCK_REORG_HEIGHT + 1,
        NetworkKind::Mainnet | NetworkKind::Testnet => MIN_PRUNING_RETENTION,
    }
}

/// The maximum number of block heights pruned in a single block commit.
///
/// In steady state, each committed block makes exactly one new height eligible
/// for pruning, so this limit is not reached. It bounds per-commit work if a
/// pruning progress marker falls behind the retention boundary, keeping
/// individual write batches small.
pub const MAX_PRUNE_HEIGHTS_PER_COMMIT: u32 = 100;

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
const DATABASE_FORMAT_VERSION: u64 = 27;

/// The database format minor version, incremented each time the on-disk database format has a
/// significant data format change.
///
/// Significant changes include:
/// - adding new column families,
/// - changing the format of a column family in a compatible way, or
/// - breaking changes with compatibility code in all supported Zebra versions.
const DATABASE_FORMAT_MINOR_VERSION: u64 = 2;

/// The database format patch version, incremented each time the on-disk database format has a
/// significant format compatibility fix.
const DATABASE_FORMAT_PATCH_VERSION: u64 = 0;

/// Returns the full semantic version of the currently running state database format code.
///
/// This is the version implemented by the Zebra code that's currently running,
/// the version on disk can be different.
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

/// The name of the file containing the database version.
///
/// Note: This file has historically omitted the major database version.
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
/// This limits non-finalized chain memory, in the worst case, to around:
/// `10 forks * 1000 blocks * 2 MB per block = 20 GB`
pub const MAX_NON_FINALIZED_CHAIN_FORKS: usize = 10;

/// The maximum number of block hashes allowed in `getblocks` responses in the Zcash network protocol.
pub const MAX_FIND_BLOCK_HASHES_RESULTS: u32 = 500;

/// The maximum number of block headers allowed in `getheaders` responses in the Zcash network protocol.
pub const MAX_FIND_BLOCK_HEADERS_RESULTS: u32 = 160;

/// The maximum number of headers returned by native Zakura header-sync range reads.
///
/// This must match `zebra-network`'s stream-5 hard cap, but lives here to avoid
/// an upward dependency from `zebra-state` to `zebra-network`.
pub const MAX_HEADER_SYNC_HEIGHT_RANGE: u32 = 4000;

/// The maximum number of invalidated block records.
///
/// Each record can hold a chain of invalidated descendants up to the rollback
/// window ([`MAX_BLOCK_REORG_HEIGHT`]) deep, so this limits the memory use, in
/// the worst case, to around:
/// `100 entries * up to 1000 blocks * 2 MB per block = 200 GB`
pub const MAX_INVALIDATED_BLOCKS: usize = 100;

lazy_static! {
    /// Regex that matches the RocksDB error when its lock file is already open.
    pub static ref LOCK_FILE_ERROR: Regex = Regex::new("(lock file).*(temporarily unavailable)|(in use)|(being used by another process)|(Database likely already open)").expect("regex is valid");
}
