//! The `GetBlockTempate` type is the output of the `getblocktemplate` RPC method.

use crate::methods::get_block_template_rpcs::types::{
    coinbase::Coinbase, default_roots::DefaultRoots, transaction::Transaction,
};

/// Documentation to be added after we document all the individual fields.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetBlockTemplate {
    /// Add documentation.
    pub capabilities: Vec<String>,

    /// Add documentation.
    pub version: usize,
    /// Add documentation.
    #[serde(rename = "previousblockhash")]
    pub previous_block_hash: GetBlockHash,
    /// Add documentation.
    #[serde(rename = "blockcommitmentshash")]
    pub block_commitments_hash: ChainHistoryBlockTxAuthCommitmentHash,
    /// Add documentation.
    #[serde(rename = "lightclientroothash")]
    pub light_client_root_hash: ChainHistoryBlockTxAuthCommitmentHash,
    /// Add documentation.
    #[serde(rename = "finalsaplingroothash")]
    pub final_sapling_root_hash: ChainHistoryBlockTxAuthCommitmentHash,
    /// Add documentation.
    #[serde(rename = "defaultroots")]
    pub default_roots: DefaultRoots,
    /// Add documentation.
    pub transactions: Vec<Transaction>,
    /// Add documentation.
    #[serde(rename = "coinbasetxn")]
    pub coinbase_txn: Coinbase,
    /// Add documentation.
    pub target: String,
    /// Add documentation.
    #[serde(rename = "mintime")]
    pub min_time: u32,
    /// Add documentation.
    pub mutable: Vec<String>,
    /// Add documentation.
    #[serde(rename = "noncerange")]
    pub nonce_range: String,
    /// Add documentation.
    #[serde(rename = "sigoplimit")]
    pub sigop_limit: u32,
    /// Add documentation.
    #[serde(rename = "sizelimit")]
    pub size_limit: u32,
    /// Add documentation.
    #[serde(rename = "curtime")]
    pub cur_time: u32,
    /// Add documentation.
    pub bits: String,
    /// Add documentation.
    pub height: u32,
}
