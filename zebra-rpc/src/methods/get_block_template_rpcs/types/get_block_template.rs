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
    /// The getblocktemplate RPC capabilities supported by Zebra.
    ///
    /// At the moment, Zebra does not support any of the extra capabilities from the specification:
    /// - `proposal`: <https://en.bitcoin.it/wiki/BIP_0023#Block_Proposal>
    /// - `longpoll`: <https://en.bitcoin.it/wiki/BIP_0022#Optional:_Long_Polling>
    /// - `serverlist`: <https://en.bitcoin.it/wiki/BIP_0023#Logical_Services>
    pub capabilities: Vec<String>,

    /// The version of the block format.
    /// Always 4 for new Zcash blocks.
    pub version: u32,

    /// The hash of the previous block.
    #[serde(rename = "previousblockhash")]
    pub previous_block_hash: GetBlockHash,

    /// The block commitment for the new block's header.
    ///
    /// Same as [`DefaultRoots.block_commitments_hash`], see that field for details.
    #[serde(rename = "blockcommitmentshash")]
    #[serde(with = "hex")]
    pub block_commitments_hash: ChainHistoryBlockTxAuthCommitmentHash,

    /// Legacy backwards-compatibility header root field.
    ///
    /// Same as [`DefaultRoots.block_commitments_hash`], see that field for details.
    #[serde(rename = "lightclientroothash")]
    #[serde(with = "hex")]
    pub light_client_root_hash: ChainHistoryBlockTxAuthCommitmentHash,

    /// Legacy backwards-compatibility header root field.
    ///
    /// Same as [`DefaultRoots.block_commitments_hash`], see that field for details.
    #[serde(rename = "finalsaplingroothash")]
    #[serde(with = "hex")]
    pub final_sapling_root_hash: ChainHistoryBlockTxAuthCommitmentHash,

    /// The block header roots for [`GetBlockTemplate.transactions`].
    ///
    /// If the transactions in the block template are modified, these roots must be recalculated
    /// [according to the specification](https://zcash.github.io/rpc/getblocktemplate.html).
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

    /// Hardcoded list of block fields the miner is allowed to change.
    pub mutable: Vec<String>,

    /// A range of valid nonces that goes from `u32::MIN` to `u32::MAX`.
    #[serde(rename = "noncerange")]
    pub nonce_range: String,

    /// Max legacy signature operations in the block.
    #[serde(rename = "sigoplimit")]
    pub sigop_limit: u64,

    /// Max block size in bytes
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
