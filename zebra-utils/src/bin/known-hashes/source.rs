//! Block data sources for the known-hashes sweep.
//!
//! The sweep logic only needs two queries, expressed as the [`BlockSource`]
//! trait so it can be tested against a fixture node. The production
//! implementation is [`RpcBlockSource`], which talks JSON-RPC to a local
//! `zebrad` or `zcashd` node.

use std::{future::Future, net::SocketAddr};

use color_eyre::eyre::{ensure, eyre, Result};
use hex::FromHex;
use serde_json::Value;

use zebra_chain::{
    block::{self, Block, SyncHashEntry},
    serialization::ZcashDeserialize,
};
use zebra_node_services::rpc_client::RpcRequestClient;

/// The JSON-RPC subset used by the sweep.
///
/// Implementations must be cheap to clone: the sweep clones the source into
/// each concurrent per-height task.
pub trait BlockSource: Clone + Send + Sync + 'static {
    /// Returns the source's current best chain tip height.
    fn tip_height(&self) -> impl Future<Output = Result<u32>> + Send;

    /// Returns the block at `height` on the source's best chain.
    ///
    /// Implementations must check that the block is at `height`.
    fn swept_block(&self, height: u32) -> impl Future<Output = Result<SweptBlock>> + Send;
}

/// What the sweep records for one height.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SweptBlock {
    /// The block's `get-hashes` entry.
    pub entry: SyncHashEntry,

    /// The hash of the block's parent, from its header.
    ///
    /// The sweep checks it against the swept hash one height below, so every
    /// hash between two checkpoint anchors is chained to them.
    pub previous_block_hash: block::Hash,
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

    async fn swept_block(&self, height: u32) -> Result<SweptBlock> {
        // The raw block gives every field, with no dependence on how the
        // node reports sizes or counts in its verbose `getblock` output.
        let block = self.call("getblock", format!(r#"["{height}", 0]"#)).await?;

        let block = block
            .as_str()
            .ok_or_else(|| eyre!("getblock {height} 0: expected a hex string response"))?;
        let bytes = Vec::<u8>::from_hex(block)
            .map_err(|err| eyre!("getblock {height} 0: invalid hex: {err}"))?;

        swept_block(&bytes, height).map_err(|err| eyre!("getblock {height} 0: {err}"))
    }
}

/// Returns the swept data for the serialized block `bytes`, which must be the
/// block at `height`.
///
/// The counts come from [`SyncHashEntry::for_block`], which the state's sync
/// metadata also uses, so a node serving `get-hashes` and this tool produce
/// identical chunk bytes.
///
/// # Errors
///
/// - `bytes` is not a block;
/// - the block's coinbase height is not `height`.
pub fn swept_block(bytes: &[u8], height: u32) -> Result<SweptBlock> {
    let block = Block::zcash_deserialize(bytes)?;

    let coinbase_height = block.coinbase_height();
    ensure!(
        coinbase_height == Some(block::Height(height)),
        "the node returned a block with coinbase height {coinbase_height:?} \
         for height {height}",
    );

    Ok(SweptBlock {
        entry: SyncHashEntry::for_block(&block, bytes.len()),
        previous_block_hash: block.header.previous_block_hash,
    })
}
