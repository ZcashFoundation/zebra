//! Constant values used in mining rpcs methods.

use jsonrpc_core::ErrorCode;

/// A range of valid block template nonces, that goes from `u32::MIN` to `u32::MAX` as a string.
pub const GET_BLOCK_TEMPLATE_NONCE_RANGE_FIELD: &str = "00000000ffffffff";

/// A hardcoded list of fields that the miner can change from the block template.
pub const GET_BLOCK_TEMPLATE_MUTABLE_FIELD: &[&str] = &[
    // Standard mutations, copied from zcashd
    "time",
    "transactions",
    "prevblock",
];

/// A hardcoded list of Zebra's getblocktemplate RPC capabilities.
pub const GET_BLOCK_TEMPLATE_CAPABILITIES_FIELD: &[&str] = &[
    // > miners which support long polling SHOULD provide a list including the String "longpoll"
    //
    // https://en.bitcoin.it/wiki/BIP_0022#Optional:_Long_Polling
    "longpoll",
];

/// The max estimated distance to the chain tip for the getblocktemplate method.
///
/// Allows the same clock skew as the Zcash network, which is 100 blocks, based on the standard rule:
/// > A full validator MUST NOT accept blocks with nTime more than two hours in the future
/// > according to its clock. This is not strictly a consensus rule because it is nondeterministic,
/// > and clock time varies between nodes.
pub const MAX_ESTIMATED_DISTANCE_TO_NETWORK_CHAIN_TIP: i32 = 100;

/// The RPC error code used by `zcashd` for when it's still downloading initial blocks.
///
/// `s-nomp` mining pool expects error code `-10` when the node is not synced:
/// <https://github.com/s-nomp/node-stratum-pool/blob/d86ae73f8ff968d9355bb61aac05e0ebef36ccb5/lib/pool.js#L142>
pub const NOT_SYNCED_ERROR_CODE: ErrorCode = ErrorCode::ServerError(-10);

/// The default window size specifying how many blocks to check when estimating the chain's solution rate.
///
/// Based on default value in zcashd.
pub const DEFAULT_SOLUTION_RATE_WINDOW_SIZE: usize = 120;
