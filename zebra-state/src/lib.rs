//! State storage code for Zebra. 🦓

#![doc(html_favicon_url = "https://www.zfnd.org/images/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_state")]
#![warn(missing_docs)]
#![allow(clippy::try_err)]

mod config;
mod constants;
mod request;
mod response;
mod sled_state;
mod util;

// TODO: move these to integration tests.
#[cfg(test)]
mod tests;

pub use config::Config;
pub use request::Request;
pub use response::Response;
pub use sled_state::init;
