//! The `DefaultRoots` type is part of the `getblocktemplate` RPC method output.

/// Documentation to be added in #5452 or #5455.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DefaultRoots {
    /// Add documentation.
    #[serde(rename = "merkleroot")]
    pub merkle_root: String,
    /// Add documentation.
    #[serde(rename = "chainhistoryroot")]
    pub chain_history_root: String,
    /// Add documentation.
    #[serde(rename = "authdataroot")]
    pub auth_data_root: String,
    /// Add documentation.
    #[serde(rename = "blockcommitmentshash")]
    pub block_commitments_hash: String,
}
