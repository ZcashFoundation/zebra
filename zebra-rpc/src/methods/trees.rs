//! Types and functions for note commitment tree RPCs.

use zebra_chain::{
    block::Hash,
    block::Height,
    subtree::{NoteCommitmentSubtreeData, NoteCommitmentSubtreeIndex},
};

/// A subtree data type that can hold Sapling or Orchard subtree roots.
pub type SubtreeRpcData = NoteCommitmentSubtreeData<String>;

/// Response to a `z_getsubtreesbyindex` RPC request.
///
/// Contains the Sapling or Orchard pool label, the index of the first subtree in the list,
/// and a list of subtree roots and end heights.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct GetSubtrees {
    /// The shielded pool to which the subtrees belong.
    //
    // TODO: consider an enum with a string conversion?
    pub pool: String,

    /// The index of the first subtree.
    pub start_index: NoteCommitmentSubtreeIndex,

    /// A sequential list of complete subtrees, in `index` order.
    ///
    /// The generic subtree root type is a hex-encoded Sapling or Orchard subtree root string.
    //
    // TODO: is this needed?
    //#[serde(skip_serializing_if = "Vec::is_empty")]
    pub subtrees: Vec<SubtreeRpcData>,
}

impl Default for GetSubtrees {
    fn default() -> Self {
        Self {
            pool: "sapling | orchard".to_string(),
            start_index: NoteCommitmentSubtreeIndex(u16::default()),
            subtrees: vec![],
        }
    }
}

/// Response to a `z_gettreestate` RPC request.
///
/// Contains hex-encoded Sapling & Orchard note commitment trees and their corresponding
/// [`struct@Hash`], [`Height`], and block time.
///
/// The format of the serialized trees represents `CommitmentTree`s from the crate
/// `incrementalmerkletree` and not `Frontier`s from the same crate, even though `zebrad`'s
/// `NoteCommitmentTree`s are implemented using `Frontier`s. Zebra follows the former format to stay
/// consistent with `zcashd`'s RPCs.
///
/// The formats are semantically equivalent. The difference is that in `Frontier`s, the vector of
/// ommers is dense (we know where the gaps are from the position of the leaf in the overall tree);
/// whereas in `CommitmentTree`, the vector of ommers is sparse with [`None`] values in the gaps.
///
/// The dense format might be used in future RPCs.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct GetTreestate {
    /// The block hash corresponding to the treestate, hex-encoded.
    #[serde(with = "hex")]
    hash: Hash,

    /// The block height corresponding to the treestate, numeric.
    height: Height,

    /// Unix time when the block corresponding to the treestate was mined,
    /// numeric.
    ///
    /// UTC seconds since the Unix 1970-01-01 epoch.
    time: u32,

    /// A treestate containing a Sapling note commitment tree, hex-encoded.
    sapling: Treestate<Vec<u8>>,

    /// A treestate containing an Orchard note commitment tree, hex-encoded.
    orchard: Treestate<Vec<u8>>,
}

impl GetTreestate {
    /// Constructs [`GetTreestate`] from its constituent parts.
    pub fn from_parts(
        hash: Hash,
        height: Height,
        time: u32,
        sapling: Option<Vec<u8>>,
        orchard: Option<Vec<u8>>,
    ) -> Self {
        let sapling = Treestate {
            commitments: Commitments {
                final_state: sapling,
            },
        };
        let orchard = Treestate {
            commitments: Commitments {
                final_state: orchard,
            },
        };

        Self {
            hash,
            height,
            time,
            sapling,
            orchard,
        }
    }

    /// Returns the contents of ['GetTreeState'].
    pub fn into_parts(self) -> (Hash, Height, u32, Option<Vec<u8>>, Option<Vec<u8>>) {
        (
            self.hash,
            self.height,
            self.time,
            self.sapling.commitments.final_state,
            self.orchard.commitments.final_state,
        )
    }
}

impl Default for GetTreestate {
    fn default() -> Self {
        Self {
            hash: Hash([0; 32]),
            height: Height::MIN,
            time: Default::default(),
            sapling: Default::default(),
            orchard: Default::default(),
        }
    }
}

/// A treestate that is included in the [`z_gettreestate`][1] RPC response.
///
/// [1]: https://zcash.github.io/rpc/z_gettreestate.html
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct Treestate<Tree: AsRef<[u8]>> {
    /// Contains an Orchard or Sapling serialized note commitment tree,
    /// hex-encoded.
    commitments: Commitments<Tree>,
}

impl<Tree: AsRef<[u8]>> Treestate<Tree> {
    /// Returns a new instance of ['Treestate'].
    pub fn new(commitments: Commitments<Tree>) -> Self {
        Treestate { commitments }
    }

    /// Returns a reference to the commitments.
    pub fn inner(&self) -> &Commitments<Tree> {
        &self.commitments
    }
}

impl Default for Treestate<Vec<u8>> {
    fn default() -> Self {
        Self {
            commitments: Commitments { final_state: None },
        }
    }
}

/// A wrapper that contains either an Orchard or Sapling note commitment tree.
///
/// Note that in the original [`z_gettreestate`][1] RPC, [`Commitments`] also
/// contains the field `finalRoot`. Zebra does *not* use this field.
///
/// [1]: https://zcash.github.io/rpc/z_gettreestate.html
#[serde_with::serde_as]
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct Commitments<Tree: AsRef<[u8]>> {
    /// Orchard or Sapling serialized note commitment tree, hex-encoded.
    #[serde_as(as = "Option<serde_with::hex::Hex>")]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "finalState")]
    final_state: Option<Tree>,
}

impl<Tree: AsRef<[u8]>> Commitments<Tree> {
    /// Returns a new instance of ['Commitments'] with optional `final_state`.
    pub fn new(final_state: Option<Tree>) -> Self {
        Commitments { final_state }
    }

    /// Returns a reference to the optional `final_state`.
    pub fn inner(&self) -> &Option<Tree> {
        &self.final_state
    }
}
