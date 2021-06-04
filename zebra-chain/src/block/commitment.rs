//! The Commitment enum, used for the corresponding block header field.

use thiserror::Error;

use crate::parameters::{Network, NetworkUpgrade, NetworkUpgrade::*};
use crate::sapling;

use super::super::block;

/// Zcash blocks contain different kinds of commitments to their contents,
/// depending on the network and height.
///
/// The `Header.commitment_bytes` field is interpreted differently, based on the
/// network and height. The interpretation changes in the network upgrade
/// activation block, or in the block immediately after network upgrade
/// activation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Commitment {
    /// [Pre-Sapling] "A reserved field, to be ignored."
    ///
    /// This field is not verified.
    PreSaplingReserved([u8; 32]),

    /// [Sapling and Blossom] The final Sapling treestate of this block.
    ///
    /// The root LEBS2OSP256(rt) of the Sapling note commitment tree
    /// corresponding to the final Sapling treestate of this block.
    ///
    /// Subsequent `Commitment` variants also commit to the `FinalSaplingRoot`,
    /// via their `EarliestSaplingRoot` and `LatestSaplingRoot` fields.
    ///
    /// Since Zebra checkpoints on Canopy, we don't need to validate this
    /// field, but since it's included in the ChainHistoryRoot, we are
    /// already calculating it, so we might as well validate it.
    ///
    /// TODO: this field is verified during semantic verification
    FinalSaplingRoot(sapling::tree::Root),

    /// [Heartwood activation block] Reserved field.
    ///
    /// The value of this field MUST be all zeroes.
    ///
    /// This MUST NOT be interpreted as a root hash.
    /// See ZIP-221 for details.
    ///
    /// This field is verified in `Commitment::from_bytes`.
    ChainHistoryActivationReserved,

    /// [(Heartwood activation block + 1) to Canopy] The root of a Merkle
    /// Mountain Range chain history tree.
    ///
    /// This root hash commits to various features of the chain's history,
    /// including the Sapling commitment tree. This commitment supports the
    /// FlyClient protocol. See ZIP-221 for details.
    ///
    /// The commitment in each block covers the chain history from the most
    /// recent network upgrade, through to the previous block. In particular,
    /// an activation block commits to the entire previous network upgrade, and
    /// the block after activation commits only to the activation block. (And
    /// therefore transitively to all previous network upgrades covered by a
    /// chain history hash in their activation block, via the previous block
    /// hash field.)
    ///
    /// Since Zebra's mandatory checkpoint includes Canopy activation, we only
    /// need to verify the chain history root from `Canopy + 1 block` onwards,
    /// using a new history tree based on the `Canopy` activation block.
    ///
    /// NU5 and later upgrades use the [`ChainHistoryBlockTxAuthCommitment`]
    /// variant.
    ///
    /// TODO: this field is verified during contextual verification
    ChainHistoryRoot(ChainHistoryMmrRootHash),

    /// [NU5 activation onwards] A commitment to:
    /// - the chain history Merkle Mountain Range tree, and
    /// - the auth data merkle tree covering this block.
    ///
    /// The chain history Merkle Mountain Range tree commits to the previous
    /// block and all ancestors in the current network upgrade. (A new chain
    /// history tree starts from each network upgrade's activation block.)
    ///
    /// The auth data merkle tree commits to this block.
    ///
    /// This commitment supports the FlyClient protocol and non-malleable
    /// transaction IDs. See ZIP-221 and ZIP-244 for details.
    ///
    /// See also the [`ChainHistoryRoot`] variant.
    ///
    /// TODO: this field is verified during contextual verification
    ChainHistoryBlockTxAuthCommitment(ChainHistoryBlockTxAuthCommitmentHash),
}

/// The required value of reserved `Commitment`s.
pub(crate) const CHAIN_HISTORY_ACTIVATION_RESERVED: [u8; 32] = [0; 32];

impl Commitment {
    /// Returns `bytes` as the Commitment variant for `network` and `height`.
    pub(super) fn from_bytes(
        bytes: [u8; 32],
        network: Network,
        height: block::Height,
    ) -> Result<Commitment, CommitmentError> {
        use Commitment::*;
        use CommitmentError::*;

        match NetworkUpgrade::current(network, height) {
            Genesis | BeforeOverwinter | Overwinter => Ok(PreSaplingReserved(bytes)),
            Sapling | Blossom => Ok(FinalSaplingRoot(sapling::tree::Root(bytes))),
            Heartwood if Some(height) == Heartwood.activation_height(network) => {
                if bytes == CHAIN_HISTORY_ACTIVATION_RESERVED {
                    Ok(ChainHistoryActivationReserved)
                } else {
                    Err(InvalidChainHistoryActivationReserved { actual: bytes })
                }
            }
            Heartwood | Canopy => Ok(ChainHistoryRoot(ChainHistoryMmrRootHash(bytes))),
            Nu5 => Ok(ChainHistoryBlockTxAuthCommitment(
                ChainHistoryBlockTxAuthCommitmentHash(bytes),
            )),
        }
    }

    /// Returns the serialized bytes for this Commitment.
    #[allow(dead_code)]
    pub(super) fn to_bytes(self) -> [u8; 32] {
        use Commitment::*;

        match self {
            PreSaplingReserved(bytes) => bytes,
            FinalSaplingRoot(hash) => hash.0,
            ChainHistoryActivationReserved => CHAIN_HISTORY_ACTIVATION_RESERVED,
            ChainHistoryRoot(hash) => hash.0,
            ChainHistoryBlockTxAuthCommitment(hash) => hash.0,
        }
    }
}

/// The root hash of a Merkle Mountain Range chain history tree.
// TODO:
//    - add methods for maintaining the MMR peaks, and calculating the root
//      hash from the current set of peaks
//    - move to a separate file
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChainHistoryMmrRootHash([u8; 32]);

/// A block commitment to chain history and transaction auth.
/// - the chain history tree for all ancestors in the current network upgrade,
///   and
/// - the transaction authorising data in this block.
///
/// Introduced in NU5.
//
// TODO:
//    - add auth data type
//    - add a method for hashing chain history and auth data together
//    - move to a separate file
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChainHistoryBlockTxAuthCommitmentHash([u8; 32]);

/// Errors that can occur when checking RootHash consensus rules.
///
/// Each error variant corresponds to a consensus rule, so enumerating
/// all possible verification failures enumerates the consensus rules we
/// implement, and ensures that we don't reject blocks or transactions
/// for a non-enumerated reason.
#[allow(dead_code, missing_docs)]
#[derive(Error, Debug, PartialEq)]
pub enum CommitmentError {
    #[error("invalid final sapling root: expected {expected:?}, actual: {actual:?}")]
    InvalidFinalSaplingRoot {
        // TODO: are these fields a security risk? If so, open a ticket to remove
        // similar fields across Zebra
        expected: [u8; 32],
        actual: [u8; 32],
    },

    #[error("invalid chain history activation reserved block committment: expected all zeroes, actual: {actual:?}")]
    InvalidChainHistoryActivationReserved { actual: [u8; 32] },

    #[error("invalid chain history root: expected {expected:?}, actual: {actual:?}")]
    InvalidChainHistoryRoot {
        expected: [u8; 32],
        actual: [u8; 32],
    },

    #[error("invalid chain history + block transaction auth commitment: expected {expected:?}, actual: {actual:?}")]
    InvalidChainHistoryBlockTxAuthCommitment {
        expected: [u8; 32],
        actual: [u8; 32],
    },

    #[error("missing required block height: block commitments can't be parsed without a block height, block hash: {block_hash:?}")]
    MissingBlockHeight { block_hash: block::Hash },
}
