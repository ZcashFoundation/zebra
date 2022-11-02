//! The `GetBlockTempate` type is the output of the `getblocktemplate` RPC method.

use zebra_chain::{amount, block::ChainHistoryBlockTxAuthCommitmentHash};

use crate::methods::{
    get_block_template_rpcs::types::{
        default_roots::DefaultRoots, transaction::TransactionTemplate,
    },
    GetBlockHash,
};

/// Documentation to be added after we document all the individual fields.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetBlockTemplate {
    /// Add documentation.
    pub capabilities: Vec<String>,

    /// The version of the block format.
    /// Always 4 for new Zcash blocks.
    //
    // TODO: add a default block version constant to zebra-chain.
    pub version: u32,

    /// Add documentation.
    #[serde(rename = "previousblockhash")]
    pub previous_block_hash: GetBlockHash,

    /// Add documentation.
    #[serde(rename = "blockcommitmentshash")]
    #[serde(with = "hex")]
    pub block_commitments_hash: ChainHistoryBlockTxAuthCommitmentHash,

    /// Add documentation.
    #[serde(rename = "lightclientroothash")]
    #[serde(with = "hex")]
    pub light_client_root_hash: ChainHistoryBlockTxAuthCommitmentHash,

    /// Add documentation.
    #[serde(rename = "finalsaplingroothash")]
    #[serde(with = "hex")]
    pub final_sapling_root_hash: ChainHistoryBlockTxAuthCommitmentHash,

    /// Add documentation.
    #[serde(rename = "defaultroots")]
    pub default_roots: DefaultRoots,

    /// The non-coinbase transactions selected for this block template.
    ///
    /// TODO: select these transactions using ZIP-317 (#5473)
    pub transactions: Vec<TransactionTemplate<amount::NonNegative>>,

    /// The coinbase transaction generated from `transactions` and `height`.
    #[serde(rename = "coinbasetxn")]
    pub coinbase_txn: TransactionTemplate<amount::NegativeOrZero>,

    /// Add documentation.
    // TODO: use ExpandedDifficulty type.
    pub target: String,

    /// Add documentation.
    #[serde(rename = "mintime")]
    // TODO: use DateTime32 type?
    pub min_time: u32,

    /// Add documentation.
    pub mutable: Vec<String>,

    /// Add documentation.
    #[serde(rename = "noncerange")]
    pub nonce_range: String,

    /// Add documentation.
    ///
    /// The same as `MAX_BLOCK_SIGOPS`.
    #[serde(rename = "sigoplimit")]
    pub sigop_limit: u64,

    /// Add documentation.
    ///
    /// The same as `MAX_BLOCK_BYTES`.
    #[serde(rename = "sizelimit")]
    pub size_limit: u64,

    /// Add documentation.
    // TODO: use DateTime32 type?
    #[serde(rename = "curtime")]
    pub cur_time: u32,

    /// Add documentation.
    // TODO: use CompactDifficulty type.
    pub bits: String,

    /// Add documentation.
    // TODO: use Height type?
    pub height: u32,
}
