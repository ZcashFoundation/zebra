use jsonrpc_core::types::error::{Error, ErrorCode};

pub(crate) fn make_server_error(message: impl std::fmt::Display) -> Error {
    Error {
        code: ErrorCode::ServerError(0),
        message: message.to_string(),
        data: None,
    }
}
