//! The Commitment enum, used for the corresponding block header field.

use std::fmt;

use hex::{FromHex, ToHex};
use thiserror::Error;

use crate::{
    block::{self, merkle::AuthDataRoot},
    parameters::{
        Network,
        NetworkUpgrade::{self, *},
    },
    sapling,
    serialization::BytesInDisplayOrder,
};

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
    /// NU5 and later upgrades use the [`Commitment::ChainHistoryBlockTxAuthCommitment`]
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
    /// See also the [`Commitment::ChainHistoryRoot`] variant.
    ///
    /// TODO: this field is verified during contextual verification
    ChainHistoryBlockTxAuthCommitment(ChainHistoryBlockTxAuthCommitmentHash),
}

/// The required value of reserved `Commitment`s.
pub(crate) const CHAIN_HISTORY_ACTIVATION_RESERVED: [u8; 32] = [0; 32];

impl Commitment {
    /// Returns `bytes` as the Commitment variant for `network` and `height`.
    //
    // TODO: rename as from_bytes_in_serialized_order()
    pub(super) fn from_bytes(
        bytes: [u8; 32],
        network: &Network,
        height: block::Height,
    ) -> Result<Commitment, CommitmentError> {
        use Commitment::*;
        use CommitmentError::*;

        match NetworkUpgrade::current_with_activation_height(network, height) {
            (Genesis | BeforeOverwinter | Overwinter, _) => Ok(PreSaplingReserved(bytes)),
            (Sapling | Blossom, _) => match sapling::tree::Root::try_from(bytes) {
                Ok(root) => Ok(FinalSaplingRoot(root)),
                _ => Err(InvalidSapingRootBytes),
            },
            (Heartwood, activation_height) if height == activation_height => {
                if bytes == CHAIN_HISTORY_ACTIVATION_RESERVED {
                    Ok(ChainHistoryActivationReserved)
                } else {
                    Err(InvalidChainHistoryActivationReserved { actual: bytes })
                }
            }
            // NetworkUpgrade::current() returns the latest network upgrade that's activated at the provided height, so
            // on Regtest for heights above height 0, it could return NU6, and it's possible for the current network upgrade
            // to be NU6 (or Canopy, or any network upgrade above Heartwood) at the Heartwood activation height.
            (Canopy | Nu5 | Nu6 | Nu6_1 | Nu7, activation_height)
                if height == activation_height
                    && Some(height) == Heartwood.activation_height(network) =>
            {
                if bytes == CHAIN_HISTORY_ACTIVATION_RESERVED {
                    Ok(ChainHistoryActivationReserved)
                } else {
                    Err(InvalidChainHistoryActivationReserved { actual: bytes })
                }
            }
            (Heartwood | Canopy, _) => Ok(ChainHistoryRoot(ChainHistoryMmrRootHash(bytes))),
            (Nu5 | Nu6 | Nu6_1 | Nu7, _) => Ok(ChainHistoryBlockTxAuthCommitment(
                ChainHistoryBlockTxAuthCommitmentHash(bytes),
            )),

            #[cfg(zcash_unstable = "zfuture")]
            (ZFuture, _) => Ok(ChainHistoryBlockTxAuthCommitment(
                ChainHistoryBlockTxAuthCommitmentHash(bytes),
            )),
        }
    }

    /// Returns the serialized bytes for this Commitment.
    //
    // TODO: refactor as bytes_in_serialized_order(&self)
    #[cfg(test)]
    pub(super) fn to_bytes(self) -> [u8; 32] {
        use Commitment::*;

        match self {
            PreSaplingReserved(bytes) => bytes,
            FinalSaplingRoot(hash) => hash.0.into(),
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
#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize, Default)]
pub struct ChainHistoryMmrRootHash([u8; 32]);

impl fmt::Display for ChainHistoryMmrRootHash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.encode_hex::<String>())
    }
}

impl fmt::Debug for ChainHistoryMmrRootHash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("ChainHistoryMmrRootHash")
            .field(&self.encode_hex::<String>())
            .finish()
    }
}

impl From<[u8; 32]> for ChainHistoryMmrRootHash {
    fn from(hash: [u8; 32]) -> Self {
        ChainHistoryMmrRootHash(hash)
    }
}

impl From<ChainHistoryMmrRootHash> for [u8; 32] {
    fn from(hash: ChainHistoryMmrRootHash) -> Self {
        hash.0
    }
}

impl BytesInDisplayOrder<true> for ChainHistoryMmrRootHash {
    fn bytes_in_serialized_order(&self) -> [u8; 32] {
        self.0
    }

    fn from_bytes_in_serialized_order(bytes: [u8; 32]) -> Self {
        ChainHistoryMmrRootHash(bytes)
    }
}

impl ToHex for &ChainHistoryMmrRootHash {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex_upper()
    }
}

impl ToHex for ChainHistoryMmrRootHash {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex_upper()
    }
}

impl FromHex for ChainHistoryMmrRootHash {
    type Error = <[u8; 32] as FromHex>::Error;

    fn from_hex<T: AsRef<[u8]>>(hex: T) -> Result<Self, Self::Error> {
        let mut hash = <[u8; 32]>::from_hex(hex)?;
        hash.reverse();

        Ok(hash.into())
    }
}

/// A block commitment to chain history and transaction auth.
/// - the chain history tree for all ancestors in the current network upgrade,
///   and
/// - the transaction authorising data in this block.
///
/// Introduced in NU5.
#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChainHistoryBlockTxAuthCommitmentHash([u8; 32]);

impl fmt::Display for ChainHistoryBlockTxAuthCommitmentHash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.encode_hex::<String>())
    }
}

impl fmt::Debug for ChainHistoryBlockTxAuthCommitmentHash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("ChainHistoryBlockTxAuthCommitmentHash")
            .field(&self.encode_hex::<String>())
            .finish()
    }
}

impl From<[u8; 32]> for ChainHistoryBlockTxAuthCommitmentHash {
    fn from(hash: [u8; 32]) -> Self {
        ChainHistoryBlockTxAuthCommitmentHash(hash)
    }
}

impl From<ChainHistoryBlockTxAuthCommitmentHash> for [u8; 32] {
    fn from(hash: ChainHistoryBlockTxAuthCommitmentHash) -> Self {
        hash.0
    }
}

impl BytesInDisplayOrder<true> for ChainHistoryBlockTxAuthCommitmentHash {
    fn bytes_in_serialized_order(&self) -> [u8; 32] {
        self.0
    }

    fn from_bytes_in_serialized_order(bytes: [u8; 32]) -> Self {
        ChainHistoryBlockTxAuthCommitmentHash(bytes)
    }
}

impl ChainHistoryBlockTxAuthCommitmentHash {
    /// Compute the block commitment from the history tree root and the
    /// authorization data root, as specified in [ZIP-244].
    ///
    /// `history_tree_root` is the root of the history tree up to and including
    /// the *previous* block.
    /// `auth_data_root` is the root of the Merkle tree of authorizing data
    /// commmitments of each transaction in the *current* block.
    ///
    ///  [ZIP-244]: https://zips.z.cash/zip-0244#block-header-changes
    pub fn from_commitments(
        history_tree_root: &ChainHistoryMmrRootHash,
        auth_data_root: &AuthDataRoot,
    ) -> Self {
        // > The value of this hash [hashBlockCommitments] is the BLAKE2b-256 hash personalized
        // > by the string "ZcashBlockCommit" of the following elements:
        // >   hashLightClientRoot (as described in ZIP 221)
        // >   hashAuthDataRoot    (as described below)
        // >   terminator          [0u8;32]
        let hash_block_commitments: [u8; 32] = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(b"ZcashBlockCommit")
            .to_state()
            .update(&<[u8; 32]>::from(*history_tree_root)[..])
            .update(&<[u8; 32]>::from(*auth_data_root))
            .update(&[0u8; 32])
            .finalize()
            .as_bytes()
            .try_into()
            .expect("32 byte array");
        Self(hash_block_commitments)
    }
}

impl ToHex for &ChainHistoryBlockTxAuthCommitmentHash {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex_upper()
    }
}

impl ToHex for ChainHistoryBlockTxAuthCommitmentHash {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex_upper()
    }
}

impl FromHex for ChainHistoryBlockTxAuthCommitmentHash {
    type Error = <[u8; 32] as FromHex>::Error;

    fn from_hex<T: AsRef<[u8]>>(hex: T) -> Result<Self, Self::Error> {
        let mut hash = <[u8; 32]>::from_hex(hex)?;
        hash.reverse();

        Ok(hash.into())
    }
}

/// Errors that can occur when checking RootHash consensus rules.
///
/// Each error variant corresponds to a consensus rule, so enumerating
/// all possible verification failures enumerates the consensus rules we
/// implement, and ensures that we don't reject blocks or transactions
/// for a non-enumerated reason.
#[allow(missing_docs)]
#[derive(Error, Clone, Debug, PartialEq, Eq)]
pub enum CommitmentError {
    #[error(
        "invalid final sapling root: expected {:?}, actual: {:?}",
        hex::encode(expected),
        hex::encode(actual)
    )]
    InvalidFinalSaplingRoot {
        // TODO: are these fields a security risk? If so, open a ticket to remove
        // similar fields across Zebra
        expected: [u8; 32],
        actual: [u8; 32],
    },

    #[error("invalid chain history activation reserved block commitment: expected all zeroes, actual: {:?}",  hex::encode(actual))]
    InvalidChainHistoryActivationReserved { actual: [u8; 32] },

    #[error(
        "invalid chain history root: expected {:?}, actual: {:?}",
        hex::encode(expected),
        hex::encode(actual)
    )]
    InvalidChainHistoryRoot {
        expected: [u8; 32],
        actual: [u8; 32],
    },

    #[error(
        "invalid block commitment root: expected {:?}, actual: {:?}",
        hex::encode(expected),
        hex::encode(actual)
    )]
    InvalidChainHistoryBlockTxAuthCommitment {
        expected: [u8; 32],
        actual: [u8; 32],
    },

    #[error("missing required block height: block commitments can't be parsed without a block height, block hash: {block_hash:?}")]
    MissingBlockHeight { block_hash: block::Hash },

    #[error("provided bytes are not a valid sapling root")]
    InvalidSapingRootBytes,
}
