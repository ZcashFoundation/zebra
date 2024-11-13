//! RPC error codes & their handling.

/// Bitcoin RPC error codes
///
/// Drawn from https://github.com/zcash/zcash/blob/master/src/rpc/protocol.h#L32-L80.
///
/// ## Notes
///
/// - All explicit discriminants fit within `i64`.
#[derive(Default)]
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

impl From<LegacyCode> for jsonrpc_core::ErrorCode {
    fn from(code: LegacyCode) -> Self {
        Self::ServerError(code as i64)
    }
}

pub(crate) trait MapError<T> {
    fn map_error(
        self,
        code: impl Into<jsonrpc_core::ErrorCode>,
    ) -> std::result::Result<T, jsonrpc_core::Error>;
}

pub(crate) trait OkOrError<T> {
    fn ok_or_error(self, message: impl ToString) -> std::result::Result<T, jsonrpc_core::Error>;
}

impl<T, E> MapError<T> for Result<T, E>
where
    E: ToString,
{
    fn map_error(self, code: impl Into<jsonrpc_core::ErrorCode>) -> Result<T, jsonrpc_core::Error> {
        self.map_err(|error| jsonrpc_core::Error {
            code: code.into(),
            message: error.to_string(),
            data: None,
        })
    }
}

impl<T> OkOrError<T> for Option<T> {
    fn ok_or_error(self, message: impl ToString) -> Result<T, jsonrpc_core::Error> {
        self.ok_or(jsonrpc_core::Error {
            code: jsonrpc_core::ErrorCode::ServerError(0),
            message: message.to_string(),
            data: None,
        })
    }
}
