//! RPC methods related to mining only available with `getblocktemplate-rpcs` rust feature.
use zebra_chain::chain_tip::ChainTip;

use jsonrpc_core::{self, Error, ErrorCode, Result};
use jsonrpc_derive::rpc;

/// getblocktemplate RPC method signatures.
#[rpc(server)]
pub trait GetBlockTemplateRpc {
    /// Returns the height of the most recent block in the best valid block chain (equivalently,
    /// the number of blocks in this chain excluding the genesis block).
    ///
    /// zcashd reference: [`getblockcount`](https://zcash.github.io/rpc/getblockcount.html)
    ///
    /// # Notes
    ///
    /// This rpc method is available only if zebra is built with `--features getblocktemplate-rpcs`.
    #[rpc(name = "getblockcount")]
    fn get_block_count(&self) -> Result<u32>;
}

/// RPC method implementations.
pub struct GetBlockTemplateRpcImpl<Tip>
where
    Tip: ChainTip,
{
    // TODO: Add the other fields from the [`Rpc`] struct as-needed
    /// Allows efficient access to the best tip of the blockchain.
    latest_chain_tip: Tip,
}

impl<Tip> GetBlockTemplateRpcImpl<Tip>
where
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    /// Create a new instance of the RPC handler.
    pub fn new(latest_chain_tip: Tip) -> Self {
        Self { latest_chain_tip }
    }
}

impl<Tip> GetBlockTemplateRpc for GetBlockTemplateRpcImpl<Tip>
where
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
