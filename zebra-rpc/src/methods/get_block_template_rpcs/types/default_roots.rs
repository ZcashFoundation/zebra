//! The `DefaultRoots` type is part of the `getblocktemplate` RPC method output.

use zebra_chain::block::{
    merkle::{self, AuthDataRoot},
    ChainHistoryBlockTxAuthCommitmentHash, ChainHistoryMmrRootHash,
};

/// Documentation to be added in #5452 or #5455.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DefaultRoots {
    /// Add documentation.
    #[serde(rename = "merkleroot")]
    pub merkle_root: merkle::Root,
    /// Add documentation.
    #[serde(rename = "chainhistoryroot")]
    pub chain_history_root: ChainHistoryMmrRootHash,
    /// Add documentation.
    #[serde(rename = "authdataroot")]
    pub auth_data_root: AuthDataRoot,
    /// Add documentation.
    #[serde(rename = "blockcommitmentshash")]
    pub block_commitments_hash: ChainHistoryBlockTxAuthCommitmentHash,
}
