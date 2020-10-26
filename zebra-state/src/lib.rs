//! State storage code for Zebra. 🦓

#![doc(html_favicon_url = "https://www.zfnd.org/images/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_state")]
#![warn(missing_docs)]
#![allow(clippy::try_err)]

mod config;
mod constants;
mod error;
mod request;
mod response;
mod service;
mod sled_state;
mod util;

// TODO: move these to integration tests.
#[cfg(test)]
mod tests;

use service::QueuedBlock;
use sled_state::FinalizedState;

pub use config::Config;
pub use constants::MAX_BLOCK_REORG_HEIGHT;
pub use error::{BoxError, CloneError, CommitBlockError, ValidateContextError};
pub use request::{HashOrHeight, Request};
pub use response::Response;
pub use service::init;
