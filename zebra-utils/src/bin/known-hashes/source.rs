//! Block data sources for the known-hashes sweep.
//!
//! The sweep logic only needs three queries, expressed as the [`BlockSource`]
//! trait so it can be tested against a mock node. The production
//! implementation is [`RpcBlockSource`], which talks JSON-RPC to a local
//! `zebrad` or `zcashd` node.

use std::{future::Future, net::SocketAddr};

use color_eyre::eyre::{ensure, eyre, Result};
use serde_json::Value;

use zebra_chain::block;
use zebra_node_services::rpc_client::RpcRequestClient;

/// The JSON-RPC subset used by the sweep.
///
/// Implementations must be cheap to clone: the sweep clones the source into
/// each concurrent per-height task.
pub trait BlockSource: Clone + Send + Sync + 'static {
    /// Returns the source's current best chain tip height.
    fn tip_height(&self) -> impl Future<Output = Result<u32>> + Send;

    /// Returns the hash of the block at `height` on the source's best chain.
    fn block_hash(&self, height: u32) -> impl Future<Output = Result<block::Hash>> + Send;

    /// Returns the serialized size in bytes of the block at `height` on the
    /// source's best chain.
    fn block_size(&self, height: u32) -> impl Future<Output = Result<u32>> + Send;
}

/// A [`BlockSource`] backed by a node's JSON-RPC endpoint.
#[derive(Clone, Debug)]
pub struct RpcBlockSource {
    /// The JSON-RPC client connected to the node.
    client: RpcRequestClient,
}

impl RpcBlockSource {
    /// Creates a source that connects to the JSON-RPC endpoint at `addr`.
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            client: RpcRequestClient::new(addr),
        }
    }

    /// Calls `method` with `params`, returning the JSON `result` value.
    async fn call(&self, method: &str, params: String) -> Result<Value> {
        self.client
            .json_result_from_call(method, params)
            .await
            .map_err(|err| eyre!("{method} request failed: {err}"))
    }
}

impl BlockSource for RpcBlockSource {
    async fn tip_height(&self) -> Result<u32> {
        let info = self.call("getblockchaininfo", "[]".to_string()).await?;

        let tip = info["blocks"]
            .as_u64()
            .ok_or_else(|| eyre!("getblockchaininfo: missing or non-numeric `blocks` field"))?;

        Ok(u32::try_from(tip)?)
    }

    async fn block_hash(&self, height: u32) -> Result<block::Hash> {
        let hash = self.call("getblockhash", format!("[{height}]")).await?;

        let hash = hash
            .as_str()
            .ok_or_else(|| eyre!("getblockhash {height}: expected a hex string response"))?;

        // `block::Hash` parses display-order hex into internal byte order.
        hash.parse()
            .map_err(|err| eyre!("getblockhash {height}: invalid block hash: {err}"))
    }

    async fn block_size(&self, height: u32) -> Result<u32> {
        // `getblock` verbosity 1 returns a `size` field on both `zcashd` and
        // `zebrad`, but `zebrad` reads it from its block info index, which can
        // be missing (serialized as `null`) for blocks committed before that
        // index existed.
        let block = self.call("getblock", format!(r#"["{height}", 1]"#)).await?;

        if let Some(size) = block["size"].as_u64() {
            return Ok(u32::try_from(size)?);
        }

        // Fall back to the serialized block: its size is half the hex length.
        let block = self.call("getblock", format!(r#"["{height}", 0]"#)).await?;

        let block = block
            .as_str()
            .ok_or_else(|| eyre!("getblock {height} 0: expected a hex string response"))?;

        ensure!(
            !block.is_empty() && block.len() % 2 == 0,
            "getblock {height} 0: response is not valid hex: length {}",
            block.len(),
        );

        Ok(u32::try_from(block.len() / 2)?)
    }
}
