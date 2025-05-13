//! A Zebra Remote Procedure Call (RPC) interface

#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://docs.rs/zebra_rpc")]

pub mod config;
pub mod methods;
pub mod queue;
pub mod server;

#[cfg(feature = "indexer-rpcs")]
pub mod sync;

#[cfg(feature = "indexer-rpcs")]
pub mod indexer;

#[cfg(test)]
mod tests;
