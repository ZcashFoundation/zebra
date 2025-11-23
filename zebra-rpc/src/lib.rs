//! A Zebra Remote Procedure Call (RPC) interface

#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://docs.rs/zebra_rpc")]

pub mod client;
pub mod config;
pub mod indexer;
pub mod methods;
pub mod queue;
pub mod server;
pub mod sync;

#[cfg(test)]
mod tests;

pub use methods::types::{
    get_block_template::{
        proposal::proposal_block_from_template, roots::compute_roots, MinerParams,
    },
    submit_block::SubmitBlockChannel,
};
