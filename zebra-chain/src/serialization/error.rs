//! Errors for Zcash consensus-critical serialization.

use std::{array::TryFromSliceError, io, num::TryFromIntError, str::Utf8Error, sync::Arc};

use hex::FromHexError;
use thiserror::Error;
use zcash_transparent::coinbase;

/// A serialization error.
// TODO: refine error types -- better to use boxed errors?
#[derive(Clone, Error, Debug)]
pub enum SerializationError {
    /// An io error that prevented deserialization
    #[error("io error: {0}")]
    Io(#[from] Arc<io::Error>),

    /// The data to be deserialized was malformed.
    // TODO: refine errors
    #[error("parse error: {0}")]
    Parse(&'static str),

    /// A string was not UTF-8.
    ///
    /// Note: Rust `String` and `str` are always UTF-8.
    #[error("string was not UTF-8: {0}")]
    Utf8Error(#[from] Utf8Error),

    /// A slice was an unexpected length during deserialization.
    #[error("slice was the wrong length: {0}")]
    TryFromSliceError(#[from] TryFromSliceError),

    /// The length of a vec is too large to convert to a usize (and thus, too large to allocate on this platform)
    #[error("CompactSize too large: {0}")]
    TryFromIntError(#[from] TryFromIntError),

    /// A string was not valid hexadecimal.
    #[error("string was not hex: {0}")]
    FromHexError(#[from] FromHexError),

    /// An error caused when validating a zatoshi `Amount`
    #[error("input couldn't be parsed as a zatoshi `Amount`: {source}")]
    Amount {
        /// The source error indicating how the num failed to validate
        #[from]
        source: crate::amount::Error,
    },

    /// Invalid transaction with a non-zero balance and no Sapling shielded spends or outputs.
    ///
    /// Transaction does not conform to the Sapling [consensus
    /// rule](https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus).
    #[error("transaction balance is non-zero but doesn't have Sapling shielded spends or outputs")]
    BadTransactionBalance,

    /// Invalid coinbase transaction.
    #[error("coinbase error: {0}")]
    Coinbase(#[from] coinbase::Error),
}

impl From<SerializationError> for io::Error {
    fn from(e: SerializationError) -> Self {
        match e {
            SerializationError::Io(e) => {
                Arc::try_unwrap(e).unwrap_or_else(|e| io::Error::new(e.kind(), e.to_string()))
            }
            SerializationError::Parse(msg) => io::Error::new(io::ErrorKind::InvalidData, msg),
            SerializationError::Utf8Error(e) => io::Error::new(io::ErrorKind::InvalidData, e),
            SerializationError::TryFromSliceError(e) => {
                io::Error::new(io::ErrorKind::InvalidData, e)
            }
            SerializationError::TryFromIntError(e) => io::Error::new(io::ErrorKind::InvalidData, e),
            SerializationError::FromHexError(e) => io::Error::new(io::ErrorKind::InvalidData, e),
            SerializationError::Amount { source } => {
                io::Error::new(io::ErrorKind::InvalidData, source)
            }
            SerializationError::BadTransactionBalance => io::Error::new(
                io::ErrorKind::InvalidData,
                "bad transaction balance: non-zero with no Sapling shielded spends or outputs",
            ),
            SerializationError::Coinbase(e) => io::Error::new(io::ErrorKind::InvalidData, e),
        }
    }
}

impl From<crate::Error> for SerializationError {
    fn from(e: crate::Error) -> Self {
        match e {
            crate::Error::InvalidConsensusBranchId => Self::Parse("invalid consensus branch id"),
            crate::Error::Io(e) => Self::Io(e),
            crate::Error::MissingNetworkUpgrade => Self::Parse("missing network upgrade"),
            crate::Error::Amount(_) => Self::BadTransactionBalance,
            crate::Error::Conversion(_) => {
                Self::Parse("Zebra's type could not be converted to its librustzcash equivalent")
            }
        }
    }
}

/// Allow converting `io::Error` to `SerializationError`; we need this since we
/// use `Arc<io::Error>` in `SerializationError::Io`.
impl From<io::Error> for SerializationError {
    fn from(value: io::Error) -> Self {
        Arc::new(value).into()
    }
}
