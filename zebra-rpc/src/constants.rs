//! Constants for RPC methods and server responses.

use jsonrpc_core::ErrorCode;

/// The RPC error code used by `zcashd` for missing blocks.
///
/// `lightwalletd` expects error code `-8` when a block is not found:
/// <https://github.com/adityapk00/lightwalletd/blob/c1bab818a683e4de69cd952317000f9bb2932274/common/common.go#L251-L254>
pub const MISSING_BLOCK_ERROR_CODE: ErrorCode = ErrorCode::ServerError(-8);
