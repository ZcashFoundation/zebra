//! Types and functions for note commitment tree RPCs.

use derive_getters::Getters;
use derive_new::new;
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
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize, Getters, new)]
pub struct GetSubtreesByIndexResponse {
    /// The shielded pool to which the subtrees belong.
    //
    // TODO: consider an enum with a string conversion?
    pub(crate) pool: String,

    /// The index of the first subtree.
    #[getter(copy)]
    pub(crate) start_index: NoteCommitmentSubtreeIndex,

    /// A sequential list of complete subtrees, in `index` order.
    ///
    /// The generic subtree root type is a hex-encoded Sapling or Orchard subtree root string.
    //
    // TODO: is this needed?
    //#[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) subtrees: Vec<SubtreeRpcData>,
}

impl Default for GetSubtreesByIndexResponse {
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
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize, Getters, new)]
pub struct GetTreestateResponse {
    /// The block hash corresponding to the treestate, hex-encoded.
    #[serde(with = "hex")]
    #[getter(copy)]
    hash: Hash,

    /// The block height corresponding to the treestate, numeric.
    #[getter(copy)]
    height: Height,

    /// Unix time when the block corresponding to the treestate was mined,
    /// numeric.
    ///
    /// UTC seconds since the Unix 1970-01-01 epoch.
    time: u32,

    /// A treestate containing a Sprout note commitment tree, hex-encoded.
    sprout: Treestate,

    /// A treestate containing a Sapling note commitment tree, hex-encoded.
    sapling: Treestate,

    /// A treestate containing an Orchard note commitment tree, hex-encoded.
    orchard: Treestate,
}

impl GetTreestateResponse {
    /// Constructs [`Treestate`] from its constituent parts.
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        hash: Hash,
        height: Height,
        time: u32,
        sprout_tree: Option<Vec<u8>>,
        sprout_root: Option<Vec<u8>>,
        sapling_tree: Option<Vec<u8>>,
        sapling_root: Option<Vec<u8>>,
        orchard_tree: Option<Vec<u8>>,
        orchard_root: Option<Vec<u8>>,
    ) -> Self {
        let sprout = Treestate {
            commitments: Commitments {
                final_root: sprout_root,
                final_state: sprout_tree,
            },
        };
        let sapling = Treestate {
            commitments: Commitments {
                final_root: sapling_root,
                final_state: sapling_tree,
            },
        };
        let orchard = Treestate {
            commitments: Commitments {
                final_root: orchard_root,
                final_state: orchard_tree,
            },
        };

        Self {
            hash,
            height,
            time,
            sprout,
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

impl Default for GetTreestateResponse {
    fn default() -> Self {
        Self {
            hash: Hash([0; 32]),
            height: Height::MIN,
            time: Default::default(),
            sprout: Default::default(),
            sapling: Default::default(),
            orchard: Default::default(),
        }
    }
}

/// A treestate that is included in the [`z_gettreestate`][1] RPC response.
///
/// [1]: https://zcash.github.io/rpc/z_gettreestate.html
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize, Getters, new)]
pub struct Treestate {
    /// Contains an Orchard or Sapling serialized note commitment tree,
    /// hex-encoded.
    commitments: Commitments,
}

impl Treestate {
    /// Returns a reference to the commitments.
    #[deprecated(note = "Use `commitments()` instead.")]
    pub fn inner(&self) -> &Commitments {
        self.commitments()
    }
}

impl Default for Treestate {
    fn default() -> Self {
        Self {
            commitments: Commitments {
                final_root: None,
                final_state: None,
            },
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
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize, Getters, new)]
pub struct Commitments {
    /// Orchard or Sapling serialized note commitment tree root, hex-encoded.
    #[serde_as(as = "Option<serde_with::hex::Hex>")]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "finalRoot")]
    final_root: Option<Vec<u8>>,
    /// Orchard or Sapling serialized note commitment tree, hex-encoded.
    #[serde_as(as = "Option<serde_with::hex::Hex>")]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "finalState")]
    final_state: Option<Vec<u8>>,
}

impl Commitments {
    /// Returns a reference to the optional `final_state`.
    #[deprecated(note = "Use `final_state()` instead.")]
    pub fn inner(&self) -> &Option<Vec<u8>> {
        &self.final_state
    }
}
