//! State contextual verification and storage code for Zebra. 🦓
//!
//! # Correctness
//!
//! Await UTXO and block commit requests should be wrapped in a timeout, because:
//! - await UTXO requests wait for a block containing that UTXO, and
//! - contextual verification and state updates wait for all previous blocks.
//!
//! Otherwise, verification of out-of-order and invalid blocks can hang indefinitely.

#![doc(html_favicon_url = "https://www.zfnd.org/images/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_state")]
// Standard lints
#![warn(missing_docs)]
#![allow(clippy::try_err)]
#![deny(clippy::await_holding_lock)]
#![forbid(unsafe_code)]

mod config;
pub mod constants;
mod error;
mod request;
mod response;
mod service;
mod util;

// TODO: move these to integration tests.
#[cfg(test)]
mod tests;

pub use config::Config;
pub use constants::MAX_BLOCK_REORG_HEIGHT;
pub use error::{BoxError, CloneError, CommitBlockError, ValidateContextError};
pub use request::{FinalizedBlock, HashOrHeight, PreparedBlock, Request};
pub use response::Response;
pub use service::{check::utxo::validate_transparent_coinbase_spend, init};

pub(crate) use request::ContextuallyValidBlock;
