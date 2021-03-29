//! The Commitment enum, used for the corresponding block header field.

use crate::parameters::{Network, NetworkUpgrade, NetworkUpgrade::*};
use crate::sapling::tree::Root;

use super::Height;

/// Zcash blocks contain different kinds of commitments to their contents,
/// depending on the network and height.
///
/// The `Header.commitment_bytes` field is interpreted differently, based on the
/// network and height. The interpretation changes in the network upgrade
/// activation block, or in the block immediately after network upgrade
/// activation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Commitment {
    /// [Pre-Sapling] Reserved field.
    ///
    /// All zeroes.
    PreSaplingReserved([u8; 32]),

    /// [Sapling and Blossom] The final Sapling treestate of this block.
    ///
    /// The root LEBS2OSP256(rt) of the Sapling note commitment tree
    /// corresponding to the final Sapling treestate of this block.
    ///
    /// Subsequent `Commitment` variants also commit to the `FinalSaplingRoot`,
    /// via their `EarliestSaplingRoot` and `LatestSaplingRoot` fields.
    FinalSaplingRoot(Root),

    /// [Heartwood activation block] Reserved field.
    ///
    /// All zeroes. This MUST NOT be interpreted as a root hash.
    /// See ZIP-221 for details.
    ChainHistoryActivationReserved([u8; 32]),

    /// [After Heartwood activation block] The root of a Merkle Mountain
    /// Range chain history tree.
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
    ChainHistoryRoot(ChainHistoryMmrRootHash),
}

impl Commitment {
    /// Returns `bytes` as the Commitment variant for `network` and `height`.
    pub(super) fn from_bytes(bytes: [u8; 32], network: Network, height: Height) -> Commitment {
        use Commitment::*;

        match NetworkUpgrade::current(network, height) {
            Genesis | BeforeOverwinter | Overwinter => PreSaplingReserved(bytes),
            Sapling | Blossom => FinalSaplingRoot(Root(bytes)),
            Heartwood if Some(height) == Heartwood.activation_height(network) => {
                ChainHistoryActivationReserved(bytes)
            }
            Heartwood | Canopy => ChainHistoryRoot(ChainHistoryMmrRootHash(bytes)),
            Nu5 => unimplemented!("Nu5 uses hashAuthDataRoot as specified in ZIP-244"),
        }
    }

    /// Returns the serialized bytes for this Commitment.
    #[allow(dead_code)]
    pub(super) fn to_bytes(self) -> [u8; 32] {
        use Commitment::*;

        match self {
            PreSaplingReserved(b) => b,
            FinalSaplingRoot(v) => v.0,
            ChainHistoryActivationReserved(b) => b,
            ChainHistoryRoot(v) => v.0,
        }
    }
}

/// The root hash of a Merkle Mountain Range chain history tree.
// TODO:
//    - add methods for maintaining the MMR peaks, and calculating the root
//      hash from the current set of peaks.
//    - move to a separate file.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChainHistoryMmrRootHash([u8; 32]);
