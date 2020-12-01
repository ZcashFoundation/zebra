use std::io;

use thiserror::Error;

/// A serialization error.
// XXX refine error types -- better to use boxed errors?
#[derive(Error, Debug)]
pub enum SerializationError {
    /// An io error that prevented deserialization
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    /// The data to be deserialized was malformed.
    // XXX refine errors
    #[error("parse error: {0}")]
    Parse(&'static str),
    /// An error caused when validating a zatoshi `Amount`
    #[error("input couldn't be parsed as a zatoshi `Amount`: {source}")]
    Amount {
        /// The source error indicating how the num failed to validate
        #[from]
        source: crate::amount::Error,
    },
}
