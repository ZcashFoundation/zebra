//! Constants for RPC methods and server responses.

use jsonrpc_core::ErrorCode;

/// The RPC error code used by `zcashd` for incorrect RPC parameters.
///
/// [`jsonrpc_core`] uses these codes:
/// <https://github.com/paritytech/jsonrpc/blob/609d7a6cc160742d035510fa89fb424ccf077660/core/src/types/error.rs#L25-L36>
///
/// `node-stratum-pool` mining pool library expects error code `-1` to detect available RPC methods:
/// <https://github.com/s-nomp/node-stratum-pool/blob/d86ae73f8ff968d9355bb61aac05e0ebef36ccb5/lib/pool.js#L459>
pub const INVALID_PARAMETERS_ERROR_CODE: ErrorCode = ErrorCode::ServerError(-1);

/// The RPC error code used by `zcashd` for missing blocks.
///
/// `lightwalletd` expects error code `-8` when a block is not found:
/// <https://github.com/adityapk00/lightwalletd/blob/c1bab818a683e4de69cd952317000f9bb2932274/common/common.go#L251-L254>
pub const MISSING_BLOCK_ERROR_CODE: ErrorCode = ErrorCode::ServerError(-8);
