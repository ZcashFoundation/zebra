//! State storage code for Zebra. 🦓

#![doc(html_favicon_url = "https://www.zfnd.org/images/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_state")]
#![warn(missing_docs)]
#![allow(clippy::try_err)]
// Disable some broken or unwanted clippy nightly lints
// Build without warnings on nightly 2021-01-17 and later and stable 1.51 and later
#![allow(unknown_lints)]
// Disable old lint warnings on nightly until 1.51 is stable
#![allow(renamed_and_removed_lints)]
// Use the old lint name to build without warnings on stable until 1.51 is stable
#![allow(clippy::unknown_clippy_lints)]
// The actual lints we want to disable
#![allow(clippy::unnecessary_wraps)]

mod config;
pub mod constants;
mod error;
mod request;
mod response;
mod service;
mod util;
mod utxo;

// TODO: move these to integration tests.
#[cfg(test)]
mod tests;

pub use config::Config;
pub use constants::MAX_BLOCK_REORG_HEIGHT;
pub use error::{BoxError, CloneError, CommitBlockError, ValidateContextError};
pub use request::{FinalizedBlock, HashOrHeight, PreparedBlock, Request};
pub use response::Response;
pub use service::init;
pub use utxo::Utxo;
