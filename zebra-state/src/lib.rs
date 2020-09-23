//! State storage code for Zebra. 🦓

#![doc(html_favicon_url = "https://www.zfnd.org/images/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_state")]
#![warn(missing_docs)]
#![allow(clippy::try_err)]
#![allow(unused_imports, unused_variables, dead_code)]

mod config;
mod constants;
mod memory_state;
mod request;
mod response;
mod service;
mod sled_state;
mod util;

// TODO: move these to integration tests.
#[cfg(test)]
mod tests;

use memory_state::MemoryState;
use service::QueuedBlock;
use sled_state::SledState;

pub use config::Config;
pub use request::{HashOrHeight, Request};
pub use response::Response;
pub use service::init;

/// A boxed [`std::error::Error`].
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;
