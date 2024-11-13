//! Constants for RPC methods and server responses.

use jsonrpc_core::{Error, ErrorCode};

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
