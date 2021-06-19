//! Errors that can occur when checking Block consensus rules.

use thiserror::Error;

#[allow(dead_code, missing_docs)]
#[derive(Error, Debug, PartialEq)]
pub enum BlockError {
    #[error("invalid network upgrade")]
    InvalidNetworkUpgrade,
}
