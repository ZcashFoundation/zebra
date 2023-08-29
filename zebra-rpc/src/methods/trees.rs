//! Types and functions for note commitment tree RPCs.
//
// TODO: move the *Tree* and *Commitment* types into this module.

use zebra_chain::subtree::{NoteCommitmentSubtreeData, NoteCommitmentSubtreeIndex};

/// Response to a `z_getsubtreesbyindex` RPC request.
///
/// Contains the Sapling or Orchard pool label, the index of the first subtree in the list,
/// and a list of subtree roots and end heights.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct GetSubtrees {
    /// The shielded pool to which the subtrees belong.
    //
    // TODO: consider an enum with a string conversion?
    pool: String,

    /// The index of the first subtree.
    start_index: NoteCommitmentSubtreeIndex,

    /// A sequential list of complete subtrees, in `index` order.
    ///
    /// The generic subtree root type is a hex-encoded Sapling or Orchard subtree root string.
    //
    // TODO: is this needed?
    //#[serde(skip_serializing_if = "Vec::is_empty")]
    subtrees: Vec<NoteCommitmentSubtreeData<String>>,
}
