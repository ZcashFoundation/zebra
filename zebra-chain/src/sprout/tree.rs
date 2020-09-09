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

use std::fmt;

use byteorder::{ByteOrder, LittleEndian};
#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;
use sha2::digest::generic_array::{typenum::U64, GenericArray};

/// MerkleCRH^Sprout Hash Function
///
/// MerkleCRH^Sprout(layer, left, right) := SHA256Compress(left || right)
///
/// `layer` is unused for Sprout but used for the Sapling equivalent.
///
/// https://zips.z.cash/protocol/protocol.pdf#merklecrh
fn merkle_crh_sprout(left: [u8; 32], right: [u8; 32]) -> [u8; 32] {
    let mut state = [0u32; 8];
    let mut block = GenericArray::<u8, U64>::default();

    block.as_mut_slice()[0..32].copy_from_slice(&left[..]);
    block.as_mut_slice()[32..63].copy_from_slice(&right[..]);

    sha2::compress256(&mut state, &[block]);

    let mut derived_bytes = [0u8; 32];
    LittleEndian::write_u32_into(&state, &mut derived_bytes);

    derived_bytes
}

/// Sprout note commitment tree root node hash.
///
/// The root hash in LEBS2OSP256(rt) encoding of the Sprout note
/// commitment tree corresponding to the final Sprout treestate of
/// this block. A root of a note commitment tree is associated with
/// each treestate.
#[derive(Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct Root([u8; 32]);

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
    fn from(rt: Root) -> [u8; 32] {
        rt.0
    }
}
