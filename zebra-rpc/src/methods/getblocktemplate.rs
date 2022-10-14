//! RPC methods related to mining pools, only available with `getblocktemplate-rpcs` rust feature.
use jsonrpc_core::{self, Error, ErrorCode, Result};
use jsonrpc_derive::rpc;
use tower::Service;

use crate::methods::{Rpc, RpcImpl};
use zebra_chain::chain_tip::ChainTip;
use zebra_node_services::{mempool, BoxError};

#[rpc(server)]
/// Additional RPC methods for mining pools.
pub trait GetBlockTemplateRpc: Rpc {
    /// Returns the height of the most recent block in the best valid block chain (equivalently,
    /// the number of blocks in this chain excluding the genesis block).
    ///
    /// zcashd reference: [`getblockcount`](https://zcash.github.io/rpc/getblockcount.html)
    #[rpc(name = "getblockcount")]
    fn get_block_count(&self) -> Result<u32>;
}

impl<Mempool, State, Tip> GetBlockTemplateRpc for RpcImpl<Mempool, State, Tip>
where
    Mempool:
        tower::Service<mempool::Request, Response = mempool::Response, Error = BoxError> + 'static,
    Mempool::Future: Send,
    State: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    State::Future: Send,
    Tip: ChainTip + Send + Sync + 'static,
{
    fn get_block_count(&self) -> Result<u32> {
        self.latest_chain_tip
            .best_tip_height()
            .map(|height| height.0)
            .ok_or(Error {
                code: ErrorCode::ServerError(0),
                message: "No blocks in state".to_string(),
                data: None,
            })
    }
}
