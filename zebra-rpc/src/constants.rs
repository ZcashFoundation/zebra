//! Constants for RPC methods and server responses.

use jsonrpc_core::{Error, ErrorCode};

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
/// <https://github.com/zcash/lightwalletd/blob/v0.4.16/common/common.go#L287-L290>
pub const MISSING_BLOCK_ERROR_CODE: ErrorCode = ErrorCode::ServerError(-8);

/// The RPC error code used by `zcashd` when there are no blocks in the state.
///
/// `lightwalletd` expects error code `0` when there are no blocks in the state.
//
// TODO: find the source code that expects or generates this error
pub const NO_BLOCKS_IN_STATE_ERROR_CODE: ErrorCode = ErrorCode::ServerError(0);

/// The RPC error used by `zcashd` when there are no blocks in the state.
//
// TODO: find the source code that expects or generates this error text, if there is any
//       replace literal Error { ... } with this error
pub fn no_blocks_in_state_error() -> Error {
    Error {
        code: NO_BLOCKS_IN_STATE_ERROR_CODE,
        message: "No blocks in state".to_string(),
        data: None,
    }
}

/// When logging parameter data, only log this much data.
pub const MAX_PARAMS_LOG_LENGTH: usize = 100;
