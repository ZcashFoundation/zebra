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
/// MerkleCRH^Orchard: {0..MerkleDepth^Orchard − 1} × P𝑥 ∪ {⊥} × P𝑥 ∪ {⊥} → P𝑥 ∪ {⊥}
///
/// MerkleCRH^Orchard(layer, left, right) := SinsemillaHash("z.cash:Orchard-MerkleCRH", l || left || right),
///
/// where l = I2LEBSP_10(MerkleDepth^Orchard − 1 − layer),  and left, right, and
/// the output are the x-coordinates of Pallas affine points.
///
/// https://zips.z.cash/protocol/protocol.pdf#orchardmerklecrh
/// https://zips.z.cash/protocol/protocol.pdf#constants
fn merkle_crh_orchard(
    layer: u8,
    maybe_left: Option<pallas::Base>,
    maybe_right: Option<pallas::Base>,
) -> Option<pallas::Base> {
    match (maybe_left, maybe_right) {
        (None, _) | (_, None) => None,
        (Some(left), Some(right)) => {
            let mut s = bitvec![Lsb0, u8;];

            // Prefix: l = I2LEBSP_10(MerkleDepth^Orchard − 1 − layer)
            let l = MERKLE_DEPTH - 1 - layer as usize;
            s.extend_from_bitslice(&BitArray::<Lsb0, _>::from([l, 0])[0..10]);
            s.extend_from_bitslice(&BitArray::<Lsb0, _>::from(left.to_bytes())[0..255]);
            s.extend_from_bitslice(&BitArray::<Lsb0, _>::from(right.to_bytes())[0..255]);

            sinsemilla_hash(b"z.cash:Orchard-MerkleCRH", &s)
        }
    }
}

lazy_static! {
    /// Orchard note commitment trees have a max depth of 32.
    ///
    /// https://zips.z.cash/protocol/nu5.pdf#constants
    static ref EMPTY_ROOTS: Vec<pallas::Base> = {

        // The empty leaf node, Uncommitted^Orchard = I2LEBSP_l_MerkleOrchard(2)
        let mut v = vec![NoteCommitmentTree::uncommitted()];

        // Starting with layer 31 (the first internal layer, after the leaves),
        // generate the empty roots up to layer 0, the root.
        for d in 0..MERKLE_DEPTH
        {
            let next = merkle_crh_orchard((MERKLE_DEPTH - 1  - d) as u8, Some(v[d]), Some(v[d])).unwrap();
            v.push(next);
        }

        v

    };
}

/// The index of a note’s commitment at the leafmost layer of its
/// `NoteCommitmentTree`.
///
/// https://zips.z.cash/protocol/nu5.pdf#merkletree
pub struct Position(pub(crate) u64);

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

/// Orchard Incremental Note Commitment Tree
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct NoteCommitmentTree {
    left: Option<pallas::Base>,
    right: Option<pallas::Base>,
    parents: Vec<Option<pallas::Base>>,
}

impl NoteCommitmentTree {
    /// Adds a note commitment x-coordinate to the tree.
    ///
    /// The leaves of the tree are actually a base field element, the
    /// x-coordinate of the commitment, the data that is actually stored on the
    /// chain and input into the proof.
    ///
    /// Returns an error if the tree is full.
    pub fn append(&mut self, cm_x: pallas::Base) -> Result<(), ()> {
        if self.is_complete() {
            // Tree is full
            return Err(());
        }

        // let cm_bytes = cm_x.into();

        match (self.left, self.right) {
            (None, _) => self.left = Some(cm_x),
            (_, None) => self.right = Some(cm_x),
            (Some(l), Some(r)) => {
                let mut combined =
                    merkle_crh_orchard((MERKLE_DEPTH - 1 - 0) as u8, Some(l), Some(r));
                self.left = Some(cm_x);
                self.right = None;

                for i in 0..MERKLE_DEPTH {
                    if i < self.parents.len() {
                        if let Some(p) = self.parents[i] {
                            combined = merkle_crh_orchard(
                                (MERKLE_DEPTH - 1 - (i + 1)) as u8,
                                Some(p),
                                combined,
                            );
                            self.parents[i] = None;
                        } else {
                            self.parents[i] = combined;
                            break;
                        }
                    } else {
                        self.parents.push(combined);
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    /// Returns the current root of the tree, used as an anchor in Orchard
    /// shielded transactions.
    pub fn root(&self) -> Root {
        // Hash left and right together, filling in empty leaves as needed.
        let leaf_root = merkle_crh_orchard(
            (MERKLE_DEPTH - 1 - 0) as u8,
            Some(self.left.unwrap_or_else(|| EMPTY_ROOTS[0])),
            Some(self.right.unwrap_or_else(|| EMPTY_ROOTS[0])),
        );

        // Hash in parents up to the currently-filled depth, roots of the empty
        // subtrees are used as needed.
        let mid_root = self
            .parents
            .iter()
            .enumerate()
            .fold(leaf_root, |root, (i, p)| match p {
                Some(node) => {
                    merkle_crh_orchard((MERKLE_DEPTH - 1 - (i + 1)) as u8, Some(*node), root)
                }
                None => merkle_crh_orchard(
                    (MERKLE_DEPTH - 1 - (i + 1)) as u8,
                    root,
                    Some(EMPTY_ROOTS[i + 1]),
                ),
            });

        // Hash in roots of the empty subtrees up to the final depth.
        Root(
            ((self.parents.len() + 1)..MERKLE_DEPTH)
                .fold(mid_root, |root, d| {
                    merkle_crh_orchard((MERKLE_DEPTH - 1 - d) as u8, root, Some(EMPTY_ROOTS[d]))
                })
                .unwrap(),
        )
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
    pub fn count(&self) -> u32 {
        self.parents.iter().enumerate().fold(
            match (self.left, self.right) {
                (None, None) => 0,
                (Some(_), None) => 1,
                (Some(_), Some(_)) => 2,
                (None, Some(_)) => unreachable!(),
            },
            |acc, (i, p)| {
                // Treat occupation of parents array as a binary number
                // (right-shifted by 1)
                acc + if p.is_some() { 1 << (i + 1) } else { 0 }
            },
        )
    }

    fn is_complete(&self) -> bool {
        self.left.is_some()
            && self.right.is_some()
            && self.parents.len() == MERKLE_DEPTH - 1
            && self.parents.iter().all(|p| p.is_some())
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
            assert_eq!(EMPTY_ROOTS[i].to_bytes(), test_vectors::EMPTY_ROOTS[i]);
        }
    }
}
