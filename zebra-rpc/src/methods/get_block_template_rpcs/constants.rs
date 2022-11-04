//! Constant values used in mining rpcs methods.

use lazy_static::lazy_static;

/// A range of valid nonces that goes from `u32::MIN` to `u32::MAX` as a string.
pub const GET_BLOCK_TEMPLATE_NONCE_RANGE_FIELD: &str = "00000000ffffffff";

lazy_static! {
    /// A hardcoded list of fields that the miner can change from the block.
    pub static ref GET_BLOCK_TEMPLATE_MUTABLE_FIELD: Vec<String> = vec![
        "time".to_string(),
        "transactions".to_string(),
        "prevblock".to_string()
    ];
}
