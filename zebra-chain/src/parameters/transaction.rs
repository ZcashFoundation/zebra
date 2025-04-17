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
///
/// Orchard transactions must use transaction version 5 and this version
/// group ID.
// TODO: FIXME: use a proper value!
#[cfg(feature = "tx-v6")]
pub const TX_V6_VERSION_GROUP_ID: u32 = 0x7777_7777;
