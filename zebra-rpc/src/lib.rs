//! A Zebra Remote Procedure Call (RPC) interface

#![doc(html_favicon_url = "https://www.zfnd.org/images/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_rpc")]

pub mod config;
pub mod methods;
pub mod server;
#[cfg(test)]
mod tests;
