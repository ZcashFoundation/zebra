//! Converting delivered blocks on rayon:
//! deserialize, check the merkle root, check `coinbase_height == expected`,
//! check for duplicate transactions and the transactions' branch IDs, and
//! precompute the auth-data root. Equihash and difficulty are skipped.
//!
//! Ported from `ibd:zebrad/src/components/ibd/convert.rs` (`convert` and the
//! rayon bridge of `VerifyAndCommit`), without its disk-cache payloads and
//! supplied trees. The block's header hash was already checked against the
//! unit's hash chain by the stream reader, so it is trusted here; `convert`
//! extends that trust from the header to the transaction bodies with the
//! checks of [`zebra_consensus::merkle_root_validity`]: the merkle root, the
//! CVE-2012-2459 duplicate-txid check and the transaction branch-ID
//! consistency check. It runs them itself, so that the merkle root is
//! checked before the coinbase height and is computed only once.

use std::{collections::HashSet, sync::Arc};

use bytes::Bytes;
use thiserror::Error;
use tokio::sync::oneshot;

use zebra_chain::{
    block::{self, Block},
    parameters::Network,
    serialization::{SerializationError, ZcashDeserializeInto},
    transaction, transparent,
};
use zebra_consensus::BlockError;
use zebra_state::{CheckpointVerifiedBlock, SemanticallyVerifiedBlock};

/// Why a delivered block could not be converted.
#[derive(Debug, Error)]
pub enum ConvertError {
    /// The bytes are not a block. The stream reader parsed only the header.
    #[error("block {hash:?} at {height:?} does not deserialize: {source}")]
    Deserialize {
        /// The expected height.
        height: block::Height,
        /// The hash the stream reader verified.
        hash: block::Hash,
        /// The deserialization error.
        #[source]
        source: SerializationError,
    },

    /// The coinbase height of a body that matches its header differs from
    /// the height the hash chain gives.
    ///
    /// Unreachable for blocks hash-chained to a correct checkpoint, because
    /// the merkle check bound the body to the header first: the engine
    /// treats it as a checkpoint-list defect.
    #[error(
        "block {hash:?} at {height:?} has coinbase height {coinbase_height:?}: \
         the checkpoint list is defective"
    )]
    WrongHeight {
        /// The expected height.
        height: block::Height,
        /// The hash the stream reader verified.
        hash: block::Hash,
        /// The coinbase height in the block.
        coinbase_height: Option<block::Height>,
    },

    /// The transactions do not match the header's merkle root, contain a
    /// duplicate transaction, or have inconsistent branch IDs: the peer sent
    /// a body that does not belong to the header.
    #[error("block {hash:?} at {height:?} has a body that does not match its header: {source}")]
    BadBody {
        /// The expected height.
        height: block::Height,
        /// The hash the stream reader verified.
        hash: block::Hash,
        /// The merkle check error.
        #[source]
        source: BlockError,
    },

    /// The rayon pool dropped the conversion: Zebra is shutting down.
    #[error("the rayon pool dropped a block conversion")]
    RayonShutdown,
}

impl ConvertError {
    /// Whether the block's delivering peer is to blame (`report_violation`),
    /// rather than the checkpoint list or the local node.
    pub fn is_peer_fault(&self) -> bool {
        matches!(
            self,
            ConvertError::Deserialize { .. } | ConvertError::BadBody { .. }
        )
    }
}

/// Converts the delivered block `raw`, at `height`, whose header hash the
/// stream reader verified to be `hash`, into a [`CheckpointVerifiedBlock`]
/// with its auth-data root precomputed.
///
/// This is pure and CPU-heavy (deserialization, transaction hashing, the
/// merkle tree and the `new_outputs` precomputation): the engine runs it on
/// rayon with [`spawn_convert`].
pub fn convert(
    network: &Network,
    height: block::Height,
    hash: block::Hash,
    raw: &[u8],
) -> Result<CheckpointVerifiedBlock, ConvertError> {
    let block: Block =
        raw.zcash_deserialize_into()
            .map_err(|source| ConvertError::Deserialize {
                height,
                hash,
                source,
            })?;

    // Bind the body to the header first: the stream reader verified only
    // the header hash, so until the merkle root matches, every other field
    // of the body, the coinbase height included, is the peer's.
    let transaction_hashes: Arc<[transaction::Hash]> =
        block.transactions.iter().map(|tx| tx.hash()).collect();
    let merkle_root: block::merkle::Root = transaction_hashes.iter().cloned().collect();
    if block.header.merkle_root != merkle_root {
        return Err(ConvertError::BadBody {
            height,
            hash,
            source: BlockError::BadMerkleRoot {
                actual: merkle_root,
                expected: block.header.merkle_root,
            },
        });
    }

    let coinbase_height = block.coinbase_height();
    if coinbase_height != Some(height) {
        return Err(ConvertError::WrongHeight {
            height,
            hash,
            coinbase_height,
        });
    }

    // The rest of `zebra_consensus::merkle_root_validity`, without computing
    // the merkle root again.
    //
    // Duplicating transactions can leave the merkle root unchanged
    // (CVE-2012-2459), so a body that matches the header must also have
    // unique transactions.
    if transaction_hashes.len() != transaction_hashes.iter().collect::<HashSet<_>>().len() {
        return Err(ConvertError::BadBody {
            height,
            hash,
            source: BlockError::DuplicateTransaction,
        });
    }
    // This needs the coinbase height, which was checked above.
    block
        .check_transaction_network_upgrade_consistency(network)
        .map_err(|_| ConvertError::BadBody {
            height,
            hash,
            source: BlockError::WrongTransactionConsensusBranchId,
        })?;

    // The coinbase height was checked above, so `new_ordered_outputs`'
    // coinbase-height invariant holds.
    let new_outputs = transparent::new_ordered_outputs(&block, &transaction_hashes);
    let block = CheckpointVerifiedBlock::from(SemanticallyVerifiedBlock {
        block: Arc::new(block),
        hash,
        height,
        new_outputs,
        transaction_hashes,
        received_time: None,
        retained_charge: None,
    });

    Ok(block.with_precomputed_auth_data_root())
}

/// Runs [`convert`] on the rayon pool, in FIFO order, and returns its result
/// without blocking the calling task.
pub async fn spawn_convert(
    network: Network,
    height: block::Height,
    hash: block::Hash,
    raw: Bytes,
) -> Result<CheckpointVerifiedBlock, ConvertError> {
    let (tx, rx) = oneshot::channel();
    rayon::spawn_fifo(move || {
        // The receiver is only dropped when the engine is shutting down.
        let _ = tx.send(convert(&network, height, hash, &raw));
    });

    rx.await.map_err(|_| ConvertError::RayonShutdown)?
}

/// Rebuilds the [`CheckpointVerifiedBlock`] that [`convert`] made from
/// `block`, whose hash is `hash`, for a resubmission: the block passed
/// [`convert`]'s checks, so its coinbase height is valid.
pub fn rebuild(block: Arc<Block>, hash: block::Hash) -> CheckpointVerifiedBlock {
    CheckpointVerifiedBlock::with_hash(block, hash).with_precomputed_auth_data_root()
}

/// Runs [`rebuild`] on the rayon pool, in FIFO order.
pub async fn spawn_rebuild(
    block: Arc<Block>,
    hash: block::Hash,
) -> Result<CheckpointVerifiedBlock, ConvertError> {
    let (tx, rx) = oneshot::channel();
    rayon::spawn_fifo(move || {
        // The receiver is only dropped when the engine is shutting down.
        let _ = tx.send(rebuild(block, hash));
    });

    rx.await.map_err(|_| ConvertError::RayonShutdown)
}

#[cfg(test)]
mod tests {
    use zebra_chain::serialization::ZcashDeserializeInto;

    use super::*;

    fn mainnet_block(height: u32) -> (Bytes, block::Hash) {
        let raw = zebra_test::vectors::MAINNET_BLOCKS[&height];
        let block: Block = raw.zcash_deserialize_into().unwrap();
        (Bytes::from_static(raw), block.hash())
    }

    #[test]
    fn converts_mainnet_blocks() {
        let _init_guard = zebra_test::init();

        for height in 0..=10 {
            let (raw, hash) = mainnet_block(height);
            let block = convert(&Network::Mainnet, block::Height(height), hash, &raw).unwrap();
            assert_eq!(block.hash, hash);
            assert_eq!(block.height, block::Height(height));
        }
    }

    #[test]
    fn rejects_wrong_heights() {
        let _init_guard = zebra_test::init();

        let (raw, hash) = mainnet_block(5);
        let error = convert(&Network::Mainnet, block::Height(6), hash, &raw).unwrap_err();
        assert!(matches!(error, ConvertError::WrongHeight { .. }));
        assert!(!error.is_peer_fault());
    }

    #[test]
    fn rejects_forged_coinbase_heights_as_bad_bodies() {
        let _init_guard = zebra_test::init();

        // Block 5's header with block 6's coinbase: the coinbase height is
        // wrong, but so is the merkle root, so the peer is to blame and the
        // checkpoint list is not.
        let (raw, hash) = mainnet_block(5);
        let mut block: Block = raw.zcash_deserialize_into().unwrap();
        let (raw6, _) = mainnet_block(6);
        let block6: Block = raw6.zcash_deserialize_into().unwrap();
        block.transactions[0] = block6.transactions[0].clone();
        assert_eq!(block.coinbase_height(), Some(block::Height(6)));
        let raw =
            zebra_chain::serialization::ZcashSerialize::zcash_serialize_to_vec(&block).unwrap();

        let error = convert(&Network::Mainnet, block::Height(5), hash, &raw).unwrap_err();
        assert!(matches!(error, ConvertError::BadBody { .. }), "{error:?}");
        assert!(error.is_peer_fault());
    }

    #[test]
    fn rejects_bodies_that_do_not_match_the_header() {
        let _init_guard = zebra_test::init();

        let (raw, hash) = mainnet_block(5);
        let mut block: Block = raw.zcash_deserialize_into().unwrap();
        // Change the header's merkle root: the body no longer matches it.
        let mut header = *block.header;
        header.merkle_root = [0x42; 32].into();
        block.header = header.into();
        let raw =
            zebra_chain::serialization::ZcashSerialize::zcash_serialize_to_vec(&block).unwrap();

        let error = convert(&Network::Mainnet, block::Height(5), hash, &raw).unwrap_err();
        assert!(matches!(error, ConvertError::BadBody { .. }), "{error:?}");
        assert!(error.is_peer_fault());

        let error = convert(&Network::Mainnet, block::Height(5), hash, &raw[..100]).unwrap_err();
        assert!(
            matches!(error, ConvertError::Deserialize { .. }),
            "{error:?}"
        );
    }

    /// Duplicating the final transaction of an odd-length list leaves the
    /// merkle root unchanged (CVE-2012-2459): the duplicate-txid check
    /// rejects the malleated body, and blames the peer.
    #[test]
    fn rejects_duplicate_transactions_behind_an_unchanged_merkle_root() {
        let _init_guard = zebra_test::init();

        let mut block: Block = zebra_test::vectors::BLOCK_MAINNET_1046401_BYTES
            .zcash_deserialize_into()
            .unwrap();
        let hash = block.hash();
        assert!(
            block.transactions.len() >= 3 && block.transactions.len() % 2 == 1,
            "this vector needs an odd transaction count of at least 3, \
             so that duplicating the final transaction preserves the merkle root",
        );
        let last = block.transactions.last().unwrap().clone();
        block.transactions.push(last);

        let malleated_root: block::merkle::Root =
            block.transactions.iter().map(|tx| tx.hash()).collect();
        assert_eq!(
            malleated_root, block.header.merkle_root,
            "the malleation preserves the merkle root, so only the duplicate-txid check rejects it",
        );
        let raw =
            zebra_chain::serialization::ZcashSerialize::zcash_serialize_to_vec(&block).unwrap();

        let error = convert(&Network::Mainnet, block::Height(1_046_401), hash, &raw).unwrap_err();
        assert!(
            matches!(
                error,
                ConvertError::BadBody {
                    source: BlockError::DuplicateTransaction,
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(error.is_peer_fault());
    }

    #[tokio::test]
    async fn spawn_convert_runs_on_rayon() {
        let _init_guard = zebra_test::init();

        let (raw, hash) = mainnet_block(3);
        let block = spawn_convert(Network::Mainnet, block::Height(3), hash, raw)
            .await
            .unwrap();
        assert_eq!(block.height, block::Height(3));

        let rebuilt = spawn_rebuild(block.block.clone(), block.hash)
            .await
            .unwrap();
        assert_eq!(rebuilt, block, "a rebuilt block is the converted block");
    }
}
