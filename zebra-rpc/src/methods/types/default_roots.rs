//! The `DefaultRoots` type is part of the `getblocktemplate` RPC method output.

use zebra_chain::block::{
    merkle::{self, AuthDataRoot},
    ChainHistoryBlockTxAuthCommitmentHash, ChainHistoryMmrRootHash,
};

/// The block header roots for [`GetBlockTemplate.transactions`].
///
/// If the transactions in the block template are modified, these roots must be recalculated
/// [according to the specification](https://zcash.github.io/rpc/getblocktemplate.html).
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DefaultRoots {
    /// The merkle root of the transaction IDs in the block.
    /// Used in the new block's header.
    #[serde(rename = "merkleroot")]
    #[serde(with = "hex")]
    pub merkle_root: merkle::Root,

    /// The root of the merkle mountain range of the chain history roots from the last network upgrade to the previous block.
    /// Unlike the other roots, this not cover any data from this new block, only from previous blocks.
    #[serde(rename = "chainhistoryroot")]
    #[serde(with = "hex")]
    pub chain_history_root: ChainHistoryMmrRootHash,

    /// The merkle root of the authorizing data hashes of the transactions in the new block.
    #[serde(rename = "authdataroot")]
    #[serde(with = "hex")]
    pub auth_data_root: AuthDataRoot,

    /// The block commitment for the new block's header.
    /// This hash covers `chain_history_root` and `auth_data_root`.
    ///
    /// `merkle_root` has its own field in the block header.
    #[serde(rename = "blockcommitmentshash")]
    #[serde(with = "hex")]
    pub block_commitments_hash: ChainHistoryBlockTxAuthCommitmentHash,
}
