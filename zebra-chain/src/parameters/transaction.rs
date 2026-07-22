//! Transaction consensus and utility parameters.

/// The version group ID for Overwinter transactions.
pub const OVERWINTER_VERSION_GROUP_ID: u32 = 0x03C4_8270;

/// The version group ID for Sapling transactions.
pub const SAPLING_VERSION_GROUP_ID: u32 = 0x892F_2085;

/// The version group ID for version 5 transactions.
///
/// Orchard transactions must use transaction version 5 and this version
/// group ID. Sapling transactions can use v4 or v5 transactions.
pub const TX_V5_VERSION_GROUP_ID: u32 = 0x26A7_270A;

/// The version group ID for version 6 transactions.
pub const TX_V6_VERSION_GROUP_ID: u32 = 0xD884_B698;

/// The version group ID for version 7 (tachyon) transactions.
///
/// The v7 transaction reuses the v6 (Ironwood) field layout and additionally carries a
/// [tachyon][`crate::transaction::TachyonShieldedData`] bundle. It is only produced and accepted
/// in tachyon builds (`cfg(all(zcash_unstable = "nu7", feature = "tx_v7"))`).
///
/// NOTE: this value is a placeholder chosen by the tachyon fork ("tach" in ASCII); it is not yet
/// specified in a ZIP. Replace it once the tachyon transaction format is finalized upstream.
pub const TX_V7_VERSION_GROUP_ID: u32 = 0x7461_6368;
