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

use bitvec::prelude::*;
#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

use super::commitment::{pedersen_hashes::pedersen_hash, NoteCommitment};

/// MerkleCRH^Sapling Hash Function
///
/// MerkleCRH^Sapling(layer, left, right) := PedersenHash(“Zcash_PH”, l || left || right)
/// where l = I2LEBSP_6(MerkleDepth^Sapling − 1 − layer)
///
/// https://zips.z.cash/protocol/protocol.pdf#merklecrh
fn merkle_crh_sapling(layer: u8, left: [u8; 32], right: [u8; 32]) -> [u8; 32] {
    let mut s: BitVec<Lsb0, u8> = BitVec::new();

    // Prefix: l = I2LEBSP_6(MerkleDepth^Sapling − 1 − layer)
    s.append(&mut bitvec![31 - layer; 1]);
    s.append(&mut BitVec::<Lsb0, u8>::from_slice(&left[..]));
    s.append(&mut BitVec::<Lsb0, u8>::from_slice(&right[..]));

    pedersen_hash(*b"Zcash_PH", &s).to_bytes()
}

/// The index of a note’s commitment at the leafmost layer of its Note
/// Commitment Tree.
///
/// https://zips.z.cash/protocol/protocol.pdf#merkletree
pub struct Position(pub(crate) u64);

/// Sapling note commitment tree root node hash.
///
/// The root hash in LEBS2OSP256(rt) encoding of the Sapling note
/// commitment tree corresponding to the final Sapling treestate of
/// this block. A root of a note commitment tree is associated with
/// each treestate.
#[derive(Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct Root(pub [u8; 32]);

impl fmt::Debug for Root {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Root").field(&hex::encode(&self.0)).finish()
    }
}

/// Sapling Note Commitment Tree
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
struct NoteCommitmentTree {
    /// The root node of the tree (often used as an anchor).
    root: Root,
    /// The height of the tree (maximum height for Sapling is 32).
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
    // XXX broken
    fn from(values: Vec<jubjub::Fq>) -> Self {
        if values.is_empty() {
            return NoteCommitmentTree {
                root: Root::default(),
                height: 0,
                count: 0,
            };
        }

        let count = values.len() as u32;
        let mut height = 0;
        let mut current_layer: Vec<[u8; 32]> =
            values.into_iter().map(|cm_u| cm_u.to_bytes()).collect();

        while current_layer.len() > 1 {
            let mut next_layer_up = vec![];

            while !current_layer.is_empty() {
                let left = current_layer.remove(0);
                let right;
                if current_layer.is_empty() {
                    right = jubjub::Fq::one().to_bytes();
                //next_layer_up.push(current_layer.remove(0));
                } else {
                    //let left = current_layer.remove(0);
                    right = current_layer.remove(0);
                }
                next_layer_up.push(merkle_crh_sapling(height, left, right));
            }

            height += 1;
            current_layer = next_layer_up;
        }

        assert!(current_layer.len() == 1);

        NoteCommitmentTree {
            root: Root(current_layer.remove(0)),
            height,
            count,
        }
    }
}

impl NoteCommitmentTree {
    /// Get the Jubjub-based Pedersen hash of root node of this merkle tree of
    /// commitment notes.
    pub fn hash(&self) -> [u8; 32] {
        self.root.0
    }
}

#[cfg(test)]
mod tests {

    use hex::FromHex;

    use super::*;

    #[test]
    fn incremental_roots() {
        // From https://github.com/zcash/zcash/blob/master/src/test/data/merkle_commitments_sapling.json
        // Byte-reversed from those oens because the original test vectors are loaded using uint256S()
        let commitments = [
            "b02310f2e087e55bfd07ef5e242e3b87ee5d00c9ab52f61e6bd42542f93a6f55",
            "225747f3b5d5dab4e5a424f81f85c904ff43286e0f3fd07ef0b8c6a627b11458",
            "7c3ea01a6e3a3d90cf59cd789e467044b5cd78eb2c84cc6816f960746d0e036c",
            "50421d6c2c94571dfaaa135a4ff15bf916681ebd62c0e43e69e3b90684d0a030",
            "aaec63863aaa0b2e3b8009429bdddd455e59be6f40ccab887a32eb98723efc12",
            "f76748d40d5ee5f9a608512e7954dd515f86e8f6d009141c89163de1cf351a02",
            "bc8a5ec71647415c380203b681f7717366f3501661512225b6dc3e121efc0b2e",
            "da1adda2ccde9381e11151686c121e7f52d19a990439161c7eb5a9f94be5a511",
            "3a27fed5dbbc475d3880360e38638c882fd9b273b618fc433106896083f77446",
            "c7ca8f7df8fd997931d33985d935ee2d696856cc09cc516d419ea6365f163008",
            "f0fa37e8063b139d342246142fc48e7c0c50d0a62c97768589e06466742c3702",
            "e6d4d7685894d01b32f7e081ab188930be6c2b9f76d6847b7f382e3dddd7c608",
            "8cebb73be883466d18d3b0c06990520e80b936440a2c9fd184d92a1f06c4e826",
            "22fab8bcdb88154dbf5877ad1e2d7f1b541bc8a5ec1b52266095381339c27c03",
            "f43e3aac61e5a753062d4d0508c26ceaf5e4c0c58ba3c956e104b5d2cf67c41c",
            "3a3661bc12b72646c94bc6c92796e81953985ee62d80a9ec3645a9a95740ac15",
        ];

        // From https://github.com/zcash/zcash/blob/master/src/test/data/merkle_roots_sapling.json
        let _roots = [
            "8c3daa300c9710bf24d2595536e7c80ff8d147faca726636d28e8683a0c27703",
            "8611f17378eb55e8c3c3f0a5f002e2b0a7ca39442fc928322b8072d1079c213d",
            "3db73b998d536be0e1c2ec124df8e0f383ae7b602968ff6a5276ca0695023c46",
            "7ac2e6442fec5970e116dfa4f2ee606f395366cafb1fa7dfd6c3de3ce18c4363",
            "6a8f11ab2a11c262e39ed4ea3825ae6c94739ccf94479cb69402c5722b034532",
            "149595eed0b54a7e694cc8a68372525b9ae2c7b102514f527460db91eb690565",
            "8c0432f1994a2381a7a4b5fda770336011f9e0b30784f9a5597901619c797045",
            "e780c48d70420601f3313ff8488d7766b70c059c53aa3cda2ff1ef57ff62383c",
            "f919f03caaed8a2c60f58c0d43838f83e670dc7e8ccd25daa04a13f3e8f45541",
            "74f32b36629724038e71cbd6823b5a666440205a7d1a9242e95870b53d81f34a",
            "a4af205a4e1ee02102866b23a68930ac33efda9235832f49b17fcc4939be4525",
            "a946a42f1636045a16e65b2308e036d9da70089686c87c692e45912bd1cab772",
            "a1db2dbac055364c1cb43cbeb49c7e2815bff855122602a2ad0fb981a91e0e39",
            "16329b3ba4f0640f4d306532d9ea6ba0fbf0e70e44ed57d27b4277ed9cda6849",
            "7b6523b2d9b23f72fec6234aa6a1f8fae3dba1c6a266023ea8b1826feba7a25c",
            "5c0bea7e17bde5bee4eb795c2eec3d389a68da587b36dd687b134826ecc09308",
        ];

        let mut leaves = vec![];

        for cm_u in commitments.iter() {
            let bytes = <[u8; 32]>::from_hex(cm_u).unwrap();

            println!("bytes: {:?}", hex::encode(bytes));

            leaves.push(jubjub::Fq::from_bytes(&bytes).unwrap());

            let tree = NoteCommitmentTree::from(leaves.clone());

            println!("root: {:x?}", hex::encode(tree.hash()));
        }
    }
}
