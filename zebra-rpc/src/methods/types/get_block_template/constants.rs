//! Constant values used in mining rpcs methods.

use jsonrpsee_types::ErrorCode;

use zebra_chain::parameters::subsidy::FundingStreamReceiver::{self, *};

/// The backstop interval for template refreshes and RPC sync/committed-tip checks, in seconds.
pub const MEMPOOL_LONG_POLL_INTERVAL: u64 = 5;

/// How long an RPC waits for the mempool to publish work for the committed tip.
pub(crate) const NEW_TIP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// Allow a shielded coinbase proof to finish before reporting unavailable mining work.
pub(crate) const SHIELDED_NEW_TIP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// A range of valid block template nonces, that goes from `u32::MIN` to `u32::MAX` as a string.
pub const NONCE_RANGE_FIELD: &str = "00000000ffffffff";

/// A hardcoded list of fields that the miner can change from the block template.
///
/// The required coinbase commits to the selected transaction fees and contextual subsidy.
/// Changing transactions or the parent requires a new template, not a mutation of this one.
///
/// <https://en.bitcoin.it/wiki/BIP_0023#Mutations>
pub const MUTABLE_FIELD: &[&str] = &["time"];

/// A hardcoded list of Zebra's getblocktemplate RPC capabilities.
///
/// <https://en.bitcoin.it/wiki/BIP_0023#Block_Proposal>
pub const CAPABILITIES_FIELD: &[&str] = &["proposal"];

/// The maximum tip age accepted by mining RPCs.
///
/// Preserves the allowance of 100 blocks at Blossom's 75-second spacing (125 minutes),
/// covering the protocol's two-hour clock-skew tolerance without shrinking at NU7.
pub const MAX_TIME_SINCE_CHAIN_TIP: chrono::Duration = chrono::Duration::seconds(7_500);

/// The RPC error code used by `zcashd` for when it's still downloading initial blocks.
///
/// `s-nomp` mining pool expects error code `-10` when the node is not synced:
/// <https://github.com/s-nomp/node-stratum-pool/blob/d86ae73f8ff968d9355bb61aac05e0ebef36ccb5/lib/pool.js#L142>
pub const NOT_SYNCED_ERROR_CODE: ErrorCode = ErrorCode::ServerError(-10);

/// The default window size specifying how many blocks to check when estimating the chain's solution rate.
///
/// Based on default value in zcashd.
pub const DEFAULT_SOLUTION_RATE_WINDOW_SIZE: i32 = 120;

/// The funding stream order in `zcashd` RPC responses.
///
/// [`zcashd`]: https://github.com/zcash/zcash/blob/3f09cfa00a3c90336580a127e0096d99e25a38d6/src/consensus/funding.cpp#L13-L32
pub const ZCASHD_FUNDING_STREAM_ORDER: &[FundingStreamReceiver] =
    &[Ecc, ZcashFoundation, MajorGrants];
