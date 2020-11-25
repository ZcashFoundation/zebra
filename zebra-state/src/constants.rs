/// The maturity threshold for transparent coinbase outputs.
///
/// A transaction MUST NOT spend a transparent output of a coinbase transaction
/// from a block less than 100 blocks prior to the spend. Note that transparent
/// outputs of coinbase transactions include Founders' Reward outputs.
pub const MIN_TRANSPARENT_COINBASE_MATURITY: u32 = 100;

/// The maximum chain reorganisation height.
///
/// Allowing reorganisations past this height could allow double-spends of
/// coinbase transactions.
pub const MAX_BLOCK_REORG_HEIGHT: u32 = MIN_TRANSPARENT_COINBASE_MATURITY - 1;

/// The database format version, incremented each time the database format changes.
pub const DATABASE_FORMAT_VERSION: u32 = 3;
