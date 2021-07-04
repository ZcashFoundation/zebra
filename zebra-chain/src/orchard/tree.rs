//! Note Commitment Trees.
//!
//! A note commitment tree is an incremental Merkle tree of fixed depth
//! used to store note commitments that JoinSplit transfers or Spend
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
    collections::VecDeque,
    convert::TryFrom,
    fmt,
    hash::{Hash, Hasher},
    io,
};

use bitvec::prelude::*;
use halo2::{arithmetic::FieldExt, pasta::pallas};
use lazy_static::lazy_static;

use super::{commitment::NoteCommitment, sinsemilla::*};

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

/// Orchard Note Commitment Tree
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct NoteCommitmentTree {
    /// The root node of the tree (often used as an anchor).
    root: Root,
    /// The height of the tree (maximum height for Orchard is 32).
    height: u8,
    /// The number of leaves (note commitments) in this tree.
    count: u32,
}

impl From<Vec<NoteCommitment>> for NoteCommitmentTree {
    fn from(_values: Vec<NoteCommitment>) -> Self {
        unimplemented!();
    }
}

impl From<Vec<pallas::Base>> for NoteCommitmentTree {
    fn from(values: Vec<pallas::Base>) -> Self {
        if values.is_empty() {
            return NoteCommitmentTree {
                root: Root::default(),
                height: 0,
                count: 0,
            };
        }

        let count = values.len() as u32;
        let mut height = 0u8;
        let mut current_layer: VecDeque<pallas::Base> = values.into_iter().collect();

        while usize::from(height) < MERKLE_DEPTH {
            let mut next_layer_up = vec![];

            while !current_layer.is_empty() {
                let left = current_layer.pop_front().unwrap();
                let right;
                if current_layer.is_empty() {
                    right = EMPTY_ROOTS[height as usize];
                } else {
                    right = current_layer.pop_front().unwrap();
                }
                next_layer_up.push(merkle_crh_orchard(height, Some(left), Some(right)).unwrap());
            }

            height += 1;
            current_layer = next_layer_up.into();
        }

        assert!(current_layer.len() == 1);

        NoteCommitmentTree {
            root: Root(current_layer.pop_front().unwrap()),
            height,
            count,
        }
    }
}

impl NoteCommitmentTree {
    /// Get the Pallas-based Sinsemilla hash of root node of this merkle tree of
    /// note commitments.
    pub fn hash(&self) -> [u8; 32] {
        self.root.into()
    }

    /// An as-yet unused Orchard note commitment tree leaf node.
    ///
    /// Distinct for Orchard, a distinguished hash value of:
    ///
    /// Uncommitted^Orchard = I2LEBSP_l_MerkleOrchard(2)
    pub fn uncommitted() -> pallas::Base {
        pallas::Base::one().double()
    }
}

// TODO: check empty roots, incremental roots, as part of https://github.com/ZcashFoundation/zebra/issues/1287

#[cfg(test)]
mod tests {

    use super::*;
    use crate::orchard::tests::vectors;

    #[test]
    fn empty_roots() {
        zebra_test::init();

        for i in 0..EMPTY_ROOTS.len() {
            assert_eq!(EMPTY_ROOTS[i].to_bytes(), test_vectors::EMPTY_ROOTS[i]);
        }
    }
}
