//! The `DefaultRoots` type is part of the `getblocktemplate` RPC method output.

use zebra_chain::block::{
    merkle::{self, AuthDataRoot},
    ChainHistoryBlockTxAuthCommitmentHash, ChainHistoryMmrRootHash,
};

/// Documentation to be added in #5452 or #5455.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DefaultRoots {
    /// The merkle root of the transaction IDs in the block.
    #[serde(rename = "merkleroot")]
    #[serde(with = "hex")]
    pub merkle_root: merkle::Root,

    /// Add documentation.
    #[serde(rename = "chainhistoryroot")]
    #[serde(with = "hex")]
    pub chain_history_root: ChainHistoryMmrRootHash,

    /// The merkle root of the authorizing data hashes of the transactions in the block.
    #[serde(rename = "authdataroot")]
    #[serde(with = "hex")]
    pub auth_data_root: AuthDataRoot,

    /// Add documentation.
    #[serde(rename = "blockcommitmentshash")]
    #[serde(with = "hex")]
    pub block_commitments_hash: ChainHistoryBlockTxAuthCommitmentHash,
}
