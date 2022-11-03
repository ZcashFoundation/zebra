use jsonrpc_core::types::error::{Error, ErrorCode};

/// Returns a jsonrpc_core [`Error`] with an [`ErrorCode::ServerError(0)`]
/// with the provided message.
// TODO: Remove the feature flag and replace repetitive closures passed to `map_err`
//       in rpc methods.
pub(crate) fn make_server_error(message: impl std::fmt::Display) -> Error {
    Error {
        code: ErrorCode::ServerError(0),
        message: message.to_string(),
        data: None,
    }
}
