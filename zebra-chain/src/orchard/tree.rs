//! Note Commitment Trees.
//!
//! A note commitment tree is an incremental Merkle tree of fixed depth
//! used to store note commitments that Action
//! transfers produce. Just as the unspent transaction output set (UTXO
//! set) used in Bitcoin, it is used to express the existence of value and
//! the capability to spend it. However, unlike the UTXO set, it is not
//! the job of this tree to protect against double-spending, as it is
//! append-only.
//!
//! A root of a note commitment tree is associated with each treestate.

#![allow(clippy::unit_arg)]
#![allow(clippy::derive_hash_xor_eq)]
#![allow(dead_code)]

use std::{
    convert::TryFrom,
    fmt,
    hash::{Hash, Hasher},
    io,
};

use bitvec::prelude::*;
use halo2::{arithmetic::FieldExt, pasta::pallas};
use incrementalmerkletree::{bridgetree, Frontier};
use lazy_static::lazy_static;

use super::sinsemilla::*;

use crate::serialization::{
    serde_helpers, ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize,
};

const MERKLE_DEPTH: usize = 32;

/// MerkleCRH^Orchard Hash Function
///
/// Used to hash incremental Merkle tree hash values for Orchard.
///
/// MerkleCRH^Orchard: {0..MerkleDepth^Orchard − 1} × P𝑥 × P𝑥 → P𝑥
///
/// MerkleCRH^Orchard(layer, left, right) := 0 if hash == ⊥; hash otherwise
///
/// where hash = SinsemillaHash("z.cash:Orchard-MerkleCRH", l || left || right),
/// l = I2LEBSP_10(MerkleDepth^Orchard − 1 − layer),  and left, right, and
/// the output are the x-coordinates of Pallas affine points.
///
/// https://zips.z.cash/protocol/protocol.pdf#orchardmerklecrh
/// https://zips.z.cash/protocol/protocol.pdf#constants
fn merkle_crh_orchard(layer: u8, left: pallas::Base, right: pallas::Base) -> pallas::Base {
    let mut s = bitvec![Lsb0, u8;];

    // Prefix: l = I2LEBSP_10(MerkleDepth^Orchard − 1 − layer)
    let l = MERKLE_DEPTH - 1 - layer as usize;
    s.extend_from_bitslice(&BitArray::<Lsb0, _>::from([l, 0])[0..10]);
    s.extend_from_bitslice(&BitArray::<Lsb0, _>::from(left.to_bytes())[0..255]);
    s.extend_from_bitslice(&BitArray::<Lsb0, _>::from(right.to_bytes())[0..255]);

    match sinsemilla_hash(b"z.cash:Orchard-MerkleCRH", &s) {
        Some(h) => h,
        None => pallas::Base::zero(),
    }
}

lazy_static! {
    /// List of "empty" Orchard note commitment nodes, one for each layer.
    ///
    /// The list is indexed by the layer number (0: root; MERKLE_DEPTH: leaf).
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#constants
    static ref EMPTY_ROOTS: Vec<pallas::Base> = {
        // The empty leaf node. This is layer 32.
        let mut v = vec![NoteCommitmentTree::uncommitted()];

        // Starting with layer 31 (the first internal layer, after the leaves),
        // generate the empty roots up to layer 0, the root.
        for layer in (0..MERKLE_DEPTH).rev()
        {
            // The vector is generated from the end, pushing new nodes to its beginning.
            // For this reason, the layer below is v[0].
            let next = merkle_crh_orchard(layer as u8, v[0], v[0]);
            v.insert(0, next);
        }

        v

    };
}

/// Orchard note commitment tree root node hash.
///
/// The root hash in LEBS2OSP256(rt) encoding of the Orchard note commitment
/// tree corresponding to the final Orchard treestate of this block. A root of a
/// note commitment tree is associated with each treestate.
#[derive(Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Root(#[serde(with = "serde_helpers::Base")] pub(crate) pallas::Base);

impl fmt::Debug for Root {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Root")
            .field(&hex::encode(&self.0.to_bytes()))
            .finish()
    }
}

impl From<Root> for [u8; 32] {
    fn from(root: Root) -> Self {
        root.0.into()
    }
}

impl Hash for Root {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.to_bytes().hash(state)
    }
}

impl TryFrom<[u8; 32]> for Root {
    type Error = SerializationError;

    fn try_from(bytes: [u8; 32]) -> Result<Self, Self::Error> {
        let possible_point = pallas::Base::from_bytes(&bytes);

        if possible_point.is_some().into() {
            Ok(Self(possible_point.unwrap()))
        } else {
            Err(SerializationError::Parse(
                "Invalid pallas::Base value for Orchard note commitment tree root",
            ))
        }
    }
}

impl ZcashSerialize for Root {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&<[u8; 32]>::from(*self)[..])?;

        Ok(())
    }
}

impl ZcashDeserialize for Root {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Self::try_from(reader.read_32_bytes()?)
    }
}

// A node of the Orchard Incremental Note Commitment Tree.
#[derive(Clone, Debug)]
struct Node(pallas::Base);

impl incrementalmerkletree::Hashable for Node {
    fn empty_leaf() -> Self {
        Self(NoteCommitmentTree::uncommitted())
    }

    /// Combine two nodes to generate a new node in the given level.
    /// Level 0 is the layer above the leaves (layer 31).
    /// Level 31 is the root (layer 0).
    fn combine(level: incrementalmerkletree::Altitude, a: &Self, b: &Self) -> Self {
        let layer = (MERKLE_DEPTH - 1) as u8 - u8::from(level);
        Self(merkle_crh_orchard(layer, a.0, b.0))
    }

    /// Return the node for the level below the given level. (A quirk of the API)
    fn empty_root(level: incrementalmerkletree::Altitude) -> Self {
        let layer_below: usize = MERKLE_DEPTH - usize::from(level);
        Self(EMPTY_ROOTS[layer_below])
    }
}

impl From<pallas::Base> for Node {
    fn from(x: pallas::Base) -> Self {
        Node(x)
    }
}

/// Orchard Incremental Note Commitment Tree
#[derive(Clone, Debug)]
struct NoteCommitmentTree {
    /// The tree represented as a Frontier.
    ///
    /// A Frontier is a subset of the tree that allows to fully specify it.
    /// It consists of nodes along the rightmost (newer) branch of the tree that
    /// has non-empty nodes. Upper (near root) empty nodes of the branch are not
    /// stored.
    inner: bridgetree::Frontier<Node, { MERKLE_DEPTH as u8 }>,
}

impl NoteCommitmentTree {
    /// Adds a note commitment x-coordinate to the tree.
    ///
    /// The leaves of the tree are actually a base field element, the
    /// x-coordinate of the commitment, the data that is actually stored on the
    /// chain and input into the proof.
    ///
    /// Returns an error if the tree is full.
    #[allow(clippy::identity_op)]
    pub fn append(&mut self, cm_x: pallas::Base) -> Result<(), ()> {
        if self.inner.append(&cm_x.into()) {
            Ok(())
        } else {
            Err(())
        }
    }

    /// Returns the current root of the tree, used as an anchor in Orchard
    /// shielded transactions.
    pub fn root(&self) -> Root {
        Root(self.inner.root().0)
    }

    /// Get the Pallas-based Sinsemilla hash / root node of this merkle tree of
    /// note commitments.
    pub fn hash(&self) -> [u8; 32] {
        self.root().into()
    }

    /// An as-yet unused Orchard note commitment tree leaf node.
    ///
    /// Distinct for Orchard, a distinguished hash value of:
    ///
    /// Uncommitted^Orchard = I2LEBSP_l_MerkleOrchard(2)
    pub fn uncommitted() -> pallas::Base {
        pallas::Base::one().double()
    }

    /// Count of note commitments added to the tree.
    ///
    /// For Orchard, the tree is capped at 2^32.
    pub fn count(&self) -> u64 {
        self.inner
            .position()
            .map_or(0, |pos| usize::from(pos) as u64 + 1)
    }
}

impl Default for NoteCommitmentTree {
    fn default() -> Self {
        Self {
            inner: bridgetree::Frontier::new(),
        }
    }
}

impl Eq for NoteCommitmentTree {}

impl PartialEq for NoteCommitmentTree {
    fn eq(&self, other: &Self) -> bool {
        self.hash() == other.hash()
    }
}

impl From<Vec<pallas::Base>> for NoteCommitmentTree {
    /// Compute the tree from a whole bunch of note commitments at once.
    fn from(values: Vec<pallas::Base>) -> Self {
        let mut tree = Self::default();

        if values.is_empty() {
            return tree;
        }

        for cm_x in values {
            let _ = tree.append(cm_x);
        }

        tree
    }
}

// TODO: check empty roots, incremental roots, as part of https://github.com/ZcashFoundation/zebra/issues/1287

#[cfg(test)]
mod tests {

    use super::*;
    use crate::orchard::tests::test_vectors;

    #[test]
    fn empty_roots() {
        zebra_test::init();

        for i in 0..EMPTY_ROOTS.len() {
            assert_eq!(
                EMPTY_ROOTS[i].to_bytes(),
                // The test vector is in reversed order.
                test_vectors::EMPTY_ROOTS[MERKLE_DEPTH - i]
            );
        }
    }

    #[test]
    fn incremental_roots() {
        zebra_test::init();
        let commitments = [
            [
                0x68, 0x13, 0x5c, 0xf4, 0x99, 0x33, 0x22, 0x90, 0x99, 0xa4, 0x4e, 0xc9, 0x9a, 0x75,
                0xe1, 0xe1, 0xcb, 0x46, 0x40, 0xf9, 0xb5, 0xbd, 0xec, 0x6b, 0x32, 0x23, 0x85, 0x6f,
                0xea, 0x16, 0x39, 0x0a,
            ],
            [
                0x78, 0x31, 0x50, 0x08, 0xfb, 0x29, 0x98, 0xb4, 0x30, 0xa5, 0x73, 0x1d, 0x67, 0x26,
                0x20, 0x7d, 0xc0, 0xf0, 0xec, 0x81, 0xea, 0x64, 0xaf, 0x5c, 0xf6, 0x12, 0x95, 0x69,
                0x01, 0xe7, 0x2f, 0x0e,
            ],
            [
                0xee, 0x94, 0x88, 0x05, 0x3a, 0x30, 0xc5, 0x96, 0xb4, 0x30, 0x14, 0x10, 0x5d, 0x34,
                0x77, 0xe6, 0xf5, 0x78, 0xc8, 0x92, 0x40, 0xd1, 0xd1, 0xee, 0x17, 0x43, 0xb7, 0x7b,
                0xb6, 0xad, 0xc4, 0x0a,
            ],
            [
                0x9d, 0xdc, 0xe7, 0xf0, 0x65, 0x01, 0xf3, 0x63, 0x76, 0x8c, 0x5b, 0xca, 0x3f, 0x26,
                0x46, 0x60, 0x83, 0x4d, 0x4d, 0xf4, 0x46, 0xd1, 0x3e, 0xfc, 0xd7, 0xc6, 0xf1, 0x7b,
                0x16, 0x7a, 0xac, 0x1a,
            ],
            [
                0xbd, 0x86, 0x16, 0x81, 0x1c, 0x6f, 0x5f, 0x76, 0x9e, 0xa4, 0x53, 0x9b, 0xba, 0xff,
                0x0f, 0x19, 0x8a, 0x6c, 0xdf, 0x3b, 0x28, 0x0d, 0xd4, 0x99, 0x26, 0x16, 0x3b, 0xd5,
                0x3f, 0x53, 0xa1, 0x21,
            ],
        ];

        let roots = [[
            0xc8, 0x75, 0xbe, 0x2d, 0x60, 0x87, 0x3f, 0x8b, 0xcd, 0xeb, 0x91, 0x28, 0x2e, 0x64,
            0x2e, 0x0c, 0xc6, 0x5f, 0xf7, 0xd0, 0x64, 0x2d, 0x13, 0x7b, 0x28, 0xcf, 0x28, 0xcc,
            0x9c, 0x52, 0x7f, 0x0e,
        ]];

        let mut leaves = vec![];

        let mut incremental_tree = NoteCommitmentTree::default();

        for (i, cm_u) in commitments.iter().enumerate() {
            let cm_u = pallas::Base::from_bytes(&cm_u).unwrap();

            leaves.push(cm_u);

            let _ = incremental_tree.append(cm_u);
        }

        assert_eq!(hex::encode(incremental_tree.hash()), hex::encode(roots[0]));

        assert_eq!(
            hex::encode((NoteCommitmentTree::from(leaves.clone())).hash()),
            hex::encode(roots[0])
        );
    }
}
