//! A Zebra Remote Procedure Call (RPC) interface

#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://docs.rs/zebra_rpc")]

pub mod client;
pub mod config;
pub mod indexer;
pub mod lightwalletd;
pub mod methods;
pub mod queue;
pub mod server;
pub mod sync;

#[cfg(test)]
mod tests;

pub use methods::types::{
    get_block_template::{
        constants::MEMPOOL_LONG_POLL_INTERVAL, fetch_chain_info,
        proposal::proposal_block_from_template, zip317::select_mempool_transactions,
        BlockTemplateRequest, BlockTemplateResponse, CoinbaseCache, MinerParams,
    },
    long_poll::LongPollInput,
    submit_block::SubmitBlockChannel,
    transaction::TransactionTemplate,
};
