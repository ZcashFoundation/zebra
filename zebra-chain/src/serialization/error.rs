use std::io;

use thiserror::Error;

/// A serialization error.
// XXX refine error types -- better to use boxed errors?
#[derive(Error, Debug)]
pub enum SerializationError {
    /// An io error that prevented deserialization
    #[error("unable to deserialize type")]
    Io(#[from] io::Error),
    /// The data to be deserialized was malformed.
    // XXX refine errors
    #[error("parse error: {0}")]
    Parse(&'static str),
    /// An error caused when validating a zatoshi `Amount`
    #[error("input couldn't be parsed as a zatoshi `Amount`")]
    Amount {
        /// The source error indicating how the num failed to validate
        #[from]
        source: crate::amount::Error,
    },
}
