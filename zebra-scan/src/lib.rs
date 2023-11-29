//! Shielded transaction scanner for the Zcash blockchain.

#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://docs.rs/zebra_scan")]

pub mod config;
pub mod init;
pub mod scan;
pub mod storage;

#[cfg(test)]
mod tests;

pub use config::Config;
pub use init::init;
