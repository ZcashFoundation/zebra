//! RPC error codes & their handling.
use jsonrpsee_types::{ErrorCode, ErrorObject, ErrorObjectOwned};

/// Bitcoin RPC error codes
///
/// Drawn from <https://github.com/zcash/zcash/blob/99ad6fdc3a549ab510422820eea5e5ce9f60a5fd/src/rpc/protocol.h#L32-L80>.
///
/// ## Notes
///
/// - All explicit discriminants fit within `i64`.
#[derive(Default, Debug)]
pub enum LegacyCode {
    // General application defined errors
    /// `std::exception` thrown in command handling
    #[default]
    Misc = -1,
    /// Server is in safe mode, and command is not allowed in safe mode
    ForbiddenBySafeMode = -2,
    /// Unexpected type was passed as parameter
    Type = -3,
    /// Invalid address or key
    InvalidAddressOrKey = -5,
    /// Ran out of memory during operation
    OutOfMemory = -7,
    /// Invalid, missing or duplicate parameter
    InvalidParameter = -8,
    /// Database error
    Database = -20,
    /// Error parsing or validating structure in raw format
    Deserialization = -22,
    /// General error during transaction or block submission
    Verify = -25,
    /// Transaction or block was rejected by network rules
    VerifyRejected = -26,
    /// Transaction already in chain
    VerifyAlreadyInChain = -27,
    /// Client still warming up
    InWarmup = -28,

    // P2P client errors
    /// Bitcoin is not connected
    ClientNotConnected = -9,
    /// Still downloading initial blocks
    ClientInInitialDownload = -10,
    /// Node is already added
    ClientNodeAlreadyAdded = -23,
    /// Node has not been added before
    ClientNodeNotAdded = -24,
    /// Node to disconnect not found in connected nodes
    ClientNodeNotConnected = -29,
    /// Invalid IP/Subnet
    ClientInvalidIpOrSubnet = -30,
}

impl From<LegacyCode> for ErrorCode {
    fn from(code: LegacyCode) -> Self {
        Self::ServerError(code as i32)
    }
}

impl From<LegacyCode> for i32 {
    fn from(code: LegacyCode) -> Self {
        code as i32
    }
}

/// A trait for mapping errors to [`jsonrpsee_types::ErrorObjectOwned`].
pub(crate) trait MapError<T>: Sized {
    /// Maps errors to [`jsonrpsee_types::ErrorObjectOwned`] with a specific error code.
    fn map_error(self, code: impl Into<ErrorCode>) -> std::result::Result<T, ErrorObjectOwned>;

    /// Maps errors to [`jsonrpsee_types::ErrorObjectOwned`] with a prefixed message and a specific error code.
    fn map_error_with_prefix(
        self,
        code: impl Into<ErrorCode>,
        msg_prefix: impl ToString,
    ) -> Result<T, ErrorObjectOwned>;

    /// Maps errors to [`jsonrpsee_types::ErrorObjectOwned`] with a [`LegacyCode::Misc`] error code.
    fn map_misc_error(self) -> std::result::Result<T, ErrorObjectOwned> {
        self.map_error(LegacyCode::Misc)
    }
}

/// A trait for conditionally converting a value into a `Result<T, jsonrpc_core::Error>`.
pub(crate) trait OkOrError<T>: Sized {
    /// Converts the implementing type to `Result<T, jsonrpc_core::Error>`, using an error code and
    /// message if conversion is to `Err`.
    fn ok_or_error(
        self,
        code: impl Into<ErrorCode>,
        message: impl ToString,
    ) -> std::result::Result<T, ErrorObjectOwned>;

    /// Converts the implementing type to `Result<T, jsonrpc_core::Error>`, using a [`LegacyCode::Misc`] error code.
    fn ok_or_misc_error(self, message: impl ToString) -> std::result::Result<T, ErrorObjectOwned> {
        self.ok_or_error(LegacyCode::Misc, message)
    }
}

impl<T, E> MapError<T> for Result<T, E>
where
    E: ToString,
{
    fn map_error(self, code: impl Into<ErrorCode>) -> Result<T, ErrorObjectOwned> {
        self.map_err(|error| ErrorObject::owned(code.into().code(), error.to_string(), None::<()>))
    }

    fn map_error_with_prefix(
        self,
        code: impl Into<ErrorCode>,
        msg_prefix: impl ToString,
    ) -> Result<T, ErrorObjectOwned> {
        self.map_err(|error| {
            ErrorObject::owned(
                code.into().code(),
                format!("{}: {}", msg_prefix.to_string(), error.to_string()),
                None::<()>,
            )
        })
    }
}

impl<T> OkOrError<T> for Option<T> {
    fn ok_or_error(
        self,
        code: impl Into<ErrorCode>,
        message: impl ToString,
    ) -> Result<T, ErrorObjectOwned> {
        self.ok_or(ErrorObject::owned(
            code.into().code(),
            message.to_string(),
            None::<()>,
        ))
    }
}
