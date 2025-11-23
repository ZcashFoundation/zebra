//! Block header roots for block templates.

use std::iter;

use derive_getters::Getters;
use derive_new::new;
use zebra_chain::{
    amount::NegativeOrZero,
    block::{
        merkle::{self, AuthDataRoot, AUTH_DIGEST_PLACEHOLDER},
        ChainHistoryBlockTxAuthCommitmentHash, ChainHistoryMmrRootHash, Height,
        CHAIN_HISTORY_ACTIVATION_RESERVED,
    },
    parameters::{Network, NetworkUpgrade},
    serialization::BytesInDisplayOrder,
    transaction::VerifiedUnminedTx,
};
use zebra_state::GetBlockTemplateChainInfo;

use crate::client::TransactionTemplate;

/// The block header roots for the transactions in a block template.
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
        coinbase: &TransactionTemplate<NegativeOrZero>,
        chain_history_root: ChainHistoryMmrRootHash,
        mempool_txs: &[VerifiedUnminedTx],
    ) -> Self {
        // TODO: (Performance)
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

        let merkle_root = iter::once(coinbase.hash)
            .chain(mempool_txs.iter().map(|tx| tx.transaction.id.mined_id()))
            .collect();

        let block_commitments_hash = ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
            chain_history_root,
            auth_data_root,
        );

        Self {
            merkle_root,
            chain_history_root,
            auth_data_root,
            block_commitments_hash,
        }
    }
}

/// Computes the block header roots.
pub fn compute_roots(
    net: &Network,
    height: Height,
    chain_info: &GetBlockTemplateChainInfo,
    coinbase: &TransactionTemplate<NegativeOrZero>,
    mempool_txs: &[VerifiedUnminedTx],
) -> (
    Option<[u8; 32]>,
    Option<[u8; 32]>,
    Option<ChainHistoryBlockTxAuthCommitmentHash>,
    Option<DefaultRoots>,
) {
    use NetworkUpgrade::*;

    let chain_history_root = chain_info.chain_history_root.or_else(|| {
        Heartwood
            .activation_height(net)
            .and_then(|h| (h == height).then_some(CHAIN_HISTORY_ACTIVATION_RESERVED.into()))
    });

    match NetworkUpgrade::current(net, height) {
        Genesis | BeforeOverwinter | Overwinter => (None, None, None, None),

        Sapling | Blossom => (
            Some(
                chain_info
                    .sapling_root
                    .expect("Sapling note commitment tree root must be available for Sapling+")
                    .bytes_in_display_order(),
            ),
            None,
            None,
            None,
        ),

        Heartwood | Canopy => {
            let chain_hist_root = chain_history_root
                .expect("chain history root must be available for Heartwood+")
                .bytes_in_display_order();

            (Some(chain_hist_root), Some(chain_hist_root), None, None)
        }

        Nu5 | Nu6 | Nu6_1 | Nu7 => {
            let default_roots = DefaultRoots::from_coinbase(
                coinbase,
                chain_history_root.expect("chain history root must be available for Nu5+"),
                mempool_txs,
            );

            (
                Some(default_roots.chain_history_root.bytes_in_display_order()),
                Some(default_roots.chain_history_root.bytes_in_display_order()),
                Some(default_roots.block_commitments_hash),
                Some(default_roots),
            )
        }
    }
}
