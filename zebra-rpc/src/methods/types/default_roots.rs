//! The `DefaultRoots` type is part of the `getblocktemplate` RPC method output.

use std::iter;

use derive_getters::Getters;
use derive_new::new;
use zebra_chain::{
    amount::NegativeOrZero,
    block::{
        merkle::{self, AuthDataRoot, AUTH_DIGEST_PLACEHOLDER},
        ChainHistoryBlockTxAuthCommitmentHash, ChainHistoryMmrRootHash, Height,
    },
    parameters::{Network, NetworkUpgrade},
    transaction::VerifiedUnminedTx,
};

use crate::client::TransactionTemplate;

/// The block header roots for [`GetBlockTemplate.transactions`].
///
/// If the transactions in the block template are modified, these roots must be recalculated
/// [according to the specification](https://zcash.github.io/rpc/getblocktemplate.html).
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize, Getters, new)]
pub struct DefaultRoots {
    /// The merkle root of the transaction IDs in the block.
    /// Used in the new block's header.
    #[serde(rename = "merkleroot")]
    #[serde(with = "hex")]
    #[getter(copy)]
    pub(crate) merkle_root: merkle::Root,

    /// The root of the merkle mountain range of the chain history roots from the last network upgrade to the previous block.
    /// Unlike the other roots, this not cover any data from this new block, only from previous blocks.
    #[serde(rename = "chainhistoryroot")]
    #[serde(with = "hex")]
    #[getter(copy)]
    pub(crate) chain_history_root: ChainHistoryMmrRootHash,

    /// The merkle root of the authorizing data hashes of the transactions in the new block.
    #[serde(rename = "authdataroot")]
    #[serde(with = "hex")]
    #[getter(copy)]
    pub(crate) auth_data_root: AuthDataRoot,

    /// The block commitment for the new block's header.
    /// This hash covers `chain_history_root` and `auth_data_root`.
    ///
    /// `merkle_root` has its own field in the block header.
    #[serde(rename = "blockcommitmentshash")]
    #[serde(with = "hex")]
    #[getter(copy)]
    pub(crate) block_commitments_hash: ChainHistoryBlockTxAuthCommitmentHash,
}

impl DefaultRoots {
    /// Creates a new [`DefaultRoots`] instance from the given coinbase tx template.
    pub fn from_coinbase(
        net: &Network,
        height: Height,
        coinbase: &TransactionTemplate<NegativeOrZero>,
        chain_history_root: Option<ChainHistoryMmrRootHash>,
        mempool_txs: &[VerifiedUnminedTx],
    ) -> Self {
        let chain_history_root = chain_history_root
            .or_else(|| {
                (NetworkUpgrade::Heartwood.activation_height(net) == Some(height))
                    .then_some([0; 32].into())
            })
            .expect("history root is required for block templates");

        // TODO:
        // Computing `auth_data_root` and `merkle_root` gets more expensive as `mempool_txs` grows.
        // It might be worth doing it in rayon.

        let auth_data_root = iter::once(coinbase.auth_digest)
            .chain(mempool_txs.iter().map(|tx| {
                tx.transaction
                    .id
                    .auth_digest()
                    .unwrap_or(AUTH_DIGEST_PLACEHOLDER)
            }))
            .collect();

        Self {
            merkle_root: iter::once(coinbase.hash)
                .chain(mempool_txs.iter().map(|tx| tx.transaction.id.mined_id()))
                .collect(),
            chain_history_root,
            auth_data_root,
            block_commitments_hash: if chain_history_root == [0; 32].into() {
                [0; 32].into()
            } else {
                ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
                    &chain_history_root,
                    &auth_data_root,
                )
            },
        }
    }
}
