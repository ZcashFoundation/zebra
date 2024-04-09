//! Error conversions for Zebra's RPC methods.

use jsonrpc_core::ErrorCode;

pub(crate) trait MapServerError<T, E> {
    fn map_server_error(self) -> std::result::Result<T, jsonrpc_core::Error>;
}

pub(crate) trait OkOrServerError<T> {
    fn ok_or_server_error<S: ToString>(
        self,
        message: S,
    ) -> std::result::Result<T, jsonrpc_core::Error>;
}

impl<T, E> MapServerError<T, E> for Result<T, E>
where
    E: ToString,
{
    fn map_server_error(self) -> Result<T, jsonrpc_core::Error> {
        self.map_err(|error| jsonrpc_core::Error {
            code: ErrorCode::ServerError(0),
            message: error.to_string(),
            data: None,
        })
    }
}

impl<T> OkOrServerError<T> for Option<T> {
    fn ok_or_server_error<S: ToString>(self, message: S) -> Result<T, jsonrpc_core::Error> {
        self.ok_or(jsonrpc_core::Error {
            code: ErrorCode::ServerError(0),
            message: message.to_string(),
            data: None,
        })
    }
}
