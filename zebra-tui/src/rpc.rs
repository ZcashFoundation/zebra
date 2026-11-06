//! A blocking JSON-RPC client for the zebrad methods that the dashboard reads.

use std::{collections::HashMap, time::Duration};

use base64::Engine;
use serde::{de::DeserializeOwned, de::IgnoredAny, Deserialize};
use serde_json::{json, Value};

/// An error from a JSON-RPC call to zebrad.
#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    /// The HTTP request did not complete.
    #[error("cannot reach node: {0}")]
    Transport(#[from] ureq::Error),

    /// The node returned a JSON-RPC error object.
    #[error("node returned RPC error {code}: {message}")]
    Rpc { code: i64, message: String },

    /// The response was not the expected JSON shape.
    #[error("unexpected response shape: {0}")]
    Decode(#[from] serde_json::Error),

    /// The response contained neither a result nor an error.
    #[error("response has no result")]
    MissingResult,

    /// The cookie file could not be read.
    #[error("cannot read cookie file: {0}")]
    Cookie(#[from] std::io::Error),
}

/// A capability to call zebrad's JSON-RPC methods at one endpoint.
pub struct NodeRpc {
    agent: ureq::Agent,
    url: String,
    authorization: Option<String>,
}

impl NodeRpc {
    /// Creates a client for the node at `url`.
    ///
    /// If `cookie_file` is set, each request authenticates with the
    /// `user:password` pair that the file contains.
    pub fn new(
        url: String,
        cookie_file: Option<&std::path::Path>,
        timeout: Duration,
    ) -> Result<Self, RpcError> {
        let authorization =
            cookie_file
                .map(std::fs::read_to_string)
                .transpose()?
                .map(|credentials| {
                    format!(
                        "Basic {}",
                        base64::engine::general_purpose::STANDARD.encode(credentials.trim())
                    )
                });
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .build();
        Ok(Self {
            agent: config.into(),
            url,
            authorization,
        })
    }

    fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T, RpcError> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let request = self
            .agent
            .post(&self.url)
            .header("content-type", "application/json");
        let request = match &self.authorization {
            Some(value) => request.header("authorization", value),
            None => request,
        };
        let mut response = request.send_json(&body)?;
        let envelope: Envelope = response.body_mut().read_json()?;
        match (envelope.result, envelope.error) {
            (_, Some(error)) => Err(RpcError::Rpc {
                code: error.code,
                message: error.message,
            }),
            (Some(result), None) => Ok(serde_json::from_value(result)?),
            (None, None) => Err(RpcError::MissingResult),
        }
    }

    /// Returns the node's view of the best chain.
    pub fn blockchain_info(&self) -> Result<RpcBlockchainInfo, RpcError> {
        self.call("getblockchaininfo", json!([]))
    }

    /// Returns the block with `hash`, including every transaction in full.
    pub fn block(&self, hash: &str) -> Result<RpcBlock, RpcError> {
        self.call("getblock", json!([hash, 2]))
    }

    /// Returns the node's connected peers.
    pub fn peer_info(&self) -> Result<Vec<RpcPeer>, RpcError> {
        self.call("getpeerinfo", json!([]))
    }

    /// Returns the mempool's transaction count and sizes.
    pub fn mempool_info(&self) -> Result<RpcMempoolInfo, RpcError> {
        self.call("getmempoolinfo", json!([]))
    }

    /// Returns every mempool transaction's size, fee, entry time, and in-mempool dependencies.
    pub fn mempool_entries(&self) -> Result<HashMap<String, RpcMempoolEntry>, RpcError> {
        self.call("getrawmempool", json!([true]))
    }
}

#[derive(Deserialize)]
struct Envelope {
    result: Option<Value>,
    error: Option<RpcErrorObject>,
}

#[derive(Deserialize)]
struct RpcErrorObject {
    code: i64,
    message: String,
}

/// The `getblockchaininfo` fields that the dashboard reads.
#[derive(Clone, Debug, Deserialize)]
pub struct RpcBlockchainInfo {
    pub blocks: u32,
    pub bestblockhash: String,
    #[serde(default)]
    pub estimatedheight: Option<u32>,
    /// Network upgrades, keyed by consensus branch ID.
    #[serde(default)]
    pub upgrades: HashMap<String, RpcUpgrade>,
    #[serde(default)]
    pub consensus: Option<RpcConsensus>,
}

/// A network upgrade in a `getblockchaininfo` response.
#[derive(Clone, Debug, Deserialize)]
pub struct RpcUpgrade {
    pub name: String,
    pub activationheight: u32,
}

/// The consensus branch IDs that govern the chain tip and the next block.
#[derive(Clone, Debug, Deserialize)]
pub struct RpcConsensus {
    pub chaintip: String,
}

/// The `getblock` verbosity 2 fields that the dashboard reads.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcBlock {
    pub hash: String,
    pub height: u32,
    pub size: u64,
    pub time: i64,
    #[serde(default)]
    pub tx: Vec<RpcTransaction>,
    #[serde(default)]
    pub value_pools: Vec<RpcValuePool>,
}

/// One transaction in a `getblock` verbosity 2 response.
///
/// Only the lengths of the input and output lists are read.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RpcTransaction {
    #[serde(default)]
    pub vin: Vec<RpcTransparentInput>,
    #[serde(default)]
    pub vout: Vec<IgnoredAny>,
    #[serde(default)]
    pub vjoinsplit: Vec<IgnoredAny>,
    #[serde(default, rename = "vShieldedSpend")]
    pub sapling_spends: Vec<IgnoredAny>,
    #[serde(default, rename = "vShieldedOutput")]
    pub sapling_outputs: Vec<IgnoredAny>,
    #[serde(default)]
    pub orchard: Option<RpcActionBundle>,
    #[serde(default)]
    pub ironwood: Option<RpcActionBundle>,
}

/// A transparent input. Coinbase inputs carry a `coinbase` field.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RpcTransparentInput {
    #[serde(default)]
    pub coinbase: Option<IgnoredAny>,
}

/// An Orchard-style bundle, where each action spends one note and creates one note.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RpcActionBundle {
    #[serde(default)]
    pub actions: Vec<IgnoredAny>,
}

/// A value pool's balance after a block, and the block's change to it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcValuePool {
    pub id: String,
    pub chain_value_zat: i64,
    #[serde(default)]
    pub value_delta_zat: Option<i64>,
}

/// The `getpeerinfo` fields that the dashboard reads.
#[derive(Clone, Debug, Deserialize)]
pub struct RpcPeer {
    pub addr: String,
    #[serde(default)]
    pub inbound: bool,
    #[serde(default)]
    pub startingheight: i64,
    #[serde(default)]
    pub subver: String,
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub lastrecv: i64,
    #[serde(default)]
    pub pingtime: Option<f64>,
    #[serde(default)]
    pub connection_state: String,
}

/// The `getmempoolinfo` fields that the dashboard reads.
#[derive(Clone, Debug, Deserialize)]
pub struct RpcMempoolInfo {
    /// The number of transactions.
    pub size: usize,
    /// The sum of the transaction sizes.
    pub bytes: u64,
    /// The memory that the mempool uses.
    pub usage: u64,
}

/// One transaction in a `getrawmempool` verbose response.
#[derive(Clone, Debug, Deserialize)]
pub struct RpcMempoolEntry {
    pub size: u64,
    /// The fee in ZEC.
    pub fee: f64,
    /// The time that the transaction entered the mempool, in Unix seconds.
    pub time: i64,
    /// The mempool transactions whose outputs this transaction spends.
    #[serde(default)]
    pub depends: Vec<IgnoredAny>,
}
