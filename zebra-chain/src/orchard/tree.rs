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
#![allow(dead_code)]

use std::{collections::VecDeque, fmt};

use bitvec::prelude::*;
use halo2::arithmetic::FieldExt;
use lazy_static::lazy_static;
#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

use super::{commitment::NoteCommitment, sinsemilla::*};

const MERKLE_DEPTH: usize = 32;

/// MerkleCRH^Orchard Hash Function
///
/// Used to hash incremental Merkle tree hash values for Orchard.
///
/// MerkleCRH^Orchard(layer, left, right) := SinsemillaHash("z.cash:Orchard-MerkleCRH", l || left || right),
///
/// where l = I2LEBSP_10(MerkleDepth^Orchard − 1 − layer) and left, right, and
/// the output are all technically 255 bits (l_MerkleOrchard), not 256.
///
/// https://zips.z.cash/protocol/nu5.pdf#merklecrh
/// https://zips.z.cash/protocol/nu5.pdf#constants
fn merkle_crh_orchard(layer: u8, left: [u8; 32], right: [u8; 32]) -> [u8; 32] {
    let mut s = bitvec![Lsb0, u8;];

    // Prefix: l = I2LEBSP_10(MerkleDepth^Orchard − 1 − layer)
    s.extend_from_slice(&layer.bits::<Lsb0>()[0..10]);
    s.extend_from_slice(&left.bits::<Lsb0>()[0..255]);
    s.extend_from_slice(&right.bits::<Lsb0>()[0..255]);

    sinsemilla_hash(b"z.cash:Orchard-MerkleCRH", &s).to_bytes()
}

lazy_static! {
    /// Orchard note commitment trees have a max depth of 32.
    ///
    /// https://zips.z.cash/protocol/nu5.pdf#constants
    static ref EMPTY_ROOTS: Vec<[u8; 32]> = {
        // Uncommitted^Orchard = I2LEBSP_l_MerkleOrchard(1)
        let mut v = vec![jubjub::Fq::one().to_bytes()];

        for d in 0..MERKLE_DEPTH {
            let next = merkle_crh_orchard(d as u8, v[d], v[d]);
            v.push(next);
        }

        v

    };
}

/// The index of a note’s commitment at the leafmost layer of its
/// `NoteCommitmentTree`.
///
/// https://zips.z.cash/protocol/nu5.pdf#merkletree
// XXX: dedupe with sapling?
pub struct Position(pub(crate) u64);

/// Orchard note commitment tree root node hash.
///
/// The root hash in LEBS2OSP256(rt) encoding of the Orchard note commitment
/// tree corresponding to the final Orchard treestate of this block. A root of a
/// note commitment tree is associated with each treestate.
#[derive(Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
// XXX: dedupe with sapling?
pub struct Root(pub [u8; 32]);

impl fmt::Debug for Root {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Root").field(&hex::encode(&self.0)).finish()
    }
}

impl From<[u8; 32]> for Root {
    fn from(bytes: [u8; 32]) -> Root {
        Self(bytes)
    }
}

impl From<Root> for [u8; 32] {
    fn from(root: Root) -> Self {
        root.0
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

impl From<Vec<jubjub::Fq>> for NoteCommitmentTree {
    fn from(values: Vec<jubjub::Fq>) -> Self {
        if values.is_empty() {
            return NoteCommitmentTree {
                root: Root::default(),
                height: 0,
                count: 0,
            };
        }

        let count = values.len() as u32;
        let mut height = 0u8;
        let mut current_layer: VecDeque<[u8; 32]> =
            values.into_iter().map(|cm_u| cm_u.to_bytes()).collect();

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
                next_layer_up.push(merkle_crh_orchard(height, left, right));
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
    /// Get the Pallas-based Pedersen hash of root node of this merkle tree of
    /// commitment notes.
    pub fn hash(&self) -> [u8; 32] {
        self.root.0
    }
}

// TODO: check empty roots, incremental roots, as part of https://github.com/ZcashFoundation/zebra/issues/1287
