//! Shielded transaction scanner for the Zcash blockchain.

#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://docs.rs/zebra_scan")]

#[macro_use]
extern crate tracing;

pub mod config;
pub mod init;
pub mod storage;

use zebra_node_services::scan_service::{request::Request, response::Response};

pub mod service;

pub use service::scan_task::scan;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod tests;

pub use config::Config;
pub use init::{init_with_server, spawn_init};

pub use sapling_crypto::{zip32::DiversifiableFullViewingKey, SaplingIvk};
