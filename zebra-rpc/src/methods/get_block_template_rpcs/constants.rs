//! Constant values used in mining rpcs methods.

use jsonrpc_core::ErrorCode;

use zebra_chain::block;
use zebra_consensus::FundingStreamReceiver::{self, *};

/// When long polling, the amount of time we wait between mempool queries.
/// (And sync status queries, which we do right before mempool queries.)
///
/// State tip changes make long polling return immediately. But miners can re-use old work
/// with an old set of transactions, so they don't need to know about mempool changes immediately.
///
/// Sync status changes are rare, and the blocks they download cause a chain tip change anyway.
///
/// `zcashd` waits 10 seconds between checking the state
/// <https://github.com/zcash/zcash/blob/420f8dfe38fd6b2465a665324366c2ae14aa98f4/src/rpc/mining.cpp#L626>
pub const GET_BLOCK_TEMPLATE_MEMPOOL_LONG_POLL_INTERVAL: u64 = 5;

/// A range of valid block template nonces, that goes from `u32::MIN` to `u32::MAX` as a string.
pub const GET_BLOCK_TEMPLATE_NONCE_RANGE_FIELD: &str = "00000000ffffffff";

/// A hardcoded list of fields that the miner can change from the block template.
///
/// <https://en.bitcoin.it/wiki/BIP_0023#Mutations>
pub const GET_BLOCK_TEMPLATE_MUTABLE_FIELD: &[&str] = &[
    // Standard mutations, copied from zcashd
    "time",
    "transactions",
    "prevblock",
];

/// A hardcoded list of Zebra's getblocktemplate RPC capabilities.
///
/// <https://en.bitcoin.it/wiki/BIP_0023#Block_Proposal>
pub const GET_BLOCK_TEMPLATE_CAPABILITIES_FIELD: &[&str] = &["proposal"];

/// The max estimated distance to the chain tip for the getblocktemplate method.
///
/// Allows the same clock skew as the Zcash network, which is 100 blocks, based on the standard rule:
/// > A full validator MUST NOT accept blocks with nTime more than two hours in the future
/// > according to its clock. This is not strictly a consensus rule because it is nondeterministic,
/// > and clock time varies between nodes.
/// >
/// > <https://zips.z.cash/protocol/protocol.pdf#blockheader>
pub const MAX_ESTIMATED_DISTANCE_TO_NETWORK_CHAIN_TIP: block::HeightDiff = 100;

/// The RPC error code used by `zcashd` for when it's still downloading initial blocks.
///
/// `s-nomp` mining pool expects error code `-10` when the node is not synced:
/// <https://github.com/s-nomp/node-stratum-pool/blob/d86ae73f8ff968d9355bb61aac05e0ebef36ccb5/lib/pool.js#L142>
pub const NOT_SYNCED_ERROR_CODE: ErrorCode = ErrorCode::ServerError(-10);

/// The default window size specifying how many blocks to check when estimating the chain's solution rate.
///
/// Based on default value in zcashd.
pub const DEFAULT_SOLUTION_RATE_WINDOW_SIZE: usize = 120;

/// The funding stream order in `zcashd` RPC responses.
///
/// [`zcashd`]: https://github.com/zcash/zcash/blob/3f09cfa00a3c90336580a127e0096d99e25a38d6/src/consensus/funding.cpp#L13-L32
pub const ZCASHD_FUNDING_STREAM_ORDER: &[FundingStreamReceiver] =
    &[Ecc, ZcashFoundation, MajorGrants];
