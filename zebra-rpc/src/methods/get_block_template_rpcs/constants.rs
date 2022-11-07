//! Constant values used in mining rpcs methods.

/// A range of valid nonces that goes from `u32::MIN` to `u32::MAX` as a string.
pub const GET_BLOCK_TEMPLATE_NONCE_RANGE_FIELD: &str = "00000000ffffffff";

/// A hardcoded list of fields that the miner can change from the block.
pub const GET_BLOCK_TEMPLATE_MUTABLE_FIELD: &[&str] = &["time", "transactions", "prevblock"];
