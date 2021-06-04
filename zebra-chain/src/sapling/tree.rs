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
use lazy_static::lazy_static;
#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

use super::commitment::{pedersen_hashes::pedersen_hash, NoteCommitment};

const MERKLE_DEPTH: usize = 32;

/// MerkleCRH^Sapling Hash Function
///
/// Used to hash incremental Merkle tree hash values for Sapling.
///
/// MerkleCRH^Sapling(layer, left, right) := PedersenHash("Zcash_PH", l || left || right)
/// where l = I2LEBSP_6(MerkleDepth^Sapling − 1 − layer) and
/// left, right, and the output are all technically 255 bits (l_MerkleSapling), not 256.
///
/// https://zips.z.cash/protocol/protocol.pdf#merklecrh
fn merkle_crh_sapling(layer: u8, left: [u8; 32], right: [u8; 32]) -> [u8; 32] {
    let mut s = bitvec![Lsb0, u8;];

    // Prefix: l = I2LEBSP_6(MerkleDepth^Sapling − 1 − layer)
    s.extend_from_slice(&layer.bits::<Lsb0>()[0..6]);
    s.extend_from_slice(&left.bits::<Lsb0>()[0..255]);
    s.extend_from_slice(&right.bits::<Lsb0>()[0..255]);

    pedersen_hash(*b"Zcash_PH", &s).to_bytes()
}

lazy_static! {
    /// Sapling note commitment trees have a max depth of 32.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#constants
    static ref EMPTY_ROOTS: Vec<[u8; 32]> = {
        // Uncommitted^Sapling = I2LEBSP_l_MerkleSapling(1)
        let mut v = vec![jubjub::Fq::one().to_bytes()];

        for d in 0..MERKLE_DEPTH {
            let next = merkle_crh_sapling(d as u8, v[d], v[d]);
            v.push(next);
        }

        v

    };
}

/// The index of a note's commitment at the leafmost layer of its Note
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

impl From<&[u8; 32]> for Root {
    fn from(bytes: &[u8; 32]) -> Root {
        (*bytes).into()
    }
}

impl From<&Root> for [u8; 32] {
    fn from(root: &Root) -> Self {
        (*root).into()
    }
}

/// Sapling Note Commitment Tree
#[derive(Clone, Debug, Default, Eq, PartialEq)]
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
                next_layer_up.push(merkle_crh_sapling(height, left, right));
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
    fn empty_roots() {
        zebra_test::init();

        // From https://github.com/zcash/librustzcash/blob/master/zcash_primitives/src/merkle_tree.rs#L512
        const HEX_EMPTY_ROOTS: [&str; 33] = [
            "0100000000000000000000000000000000000000000000000000000000000000",
            "817de36ab2d57feb077634bca77819c8e0bd298c04f6fed0e6a83cc1356ca155",
            "ffe9fc03f18b176c998806439ff0bb8ad193afdb27b2ccbc88856916dd804e34",
            "d8283386ef2ef07ebdbb4383c12a739a953a4d6e0d6fb1139a4036d693bfbb6c",
            "e110de65c907b9dea4ae0bd83a4b0a51bea175646a64c12b4c9f931b2cb31b49",
            "912d82b2c2bca231f71efcf61737fbf0a08befa0416215aeef53e8bb6d23390a",
            "8ac9cf9c391e3fd42891d27238a81a8a5c1d3a72b1bcbea8cf44a58ce7389613",
            "d6c639ac24b46bd19341c91b13fdcab31581ddaf7f1411336a271f3d0aa52813",
            "7b99abdc3730991cc9274727d7d82d28cb794edbc7034b4f0053ff7c4b680444",
            "43ff5457f13b926b61df552d4e402ee6dc1463f99a535f9a713439264d5b616b",
            "ba49b659fbd0b7334211ea6a9d9df185c757e70aa81da562fb912b84f49bce72",
            "4777c8776a3b1e69b73a62fa701fa4f7a6282d9aee2c7a6b82e7937d7081c23c",
            "ec677114c27206f5debc1c1ed66f95e2b1885da5b7be3d736b1de98579473048",
            "1b77dac4d24fb7258c3c528704c59430b630718bec486421837021cf75dab651",
            "bd74b25aacb92378a871bf27d225cfc26baca344a1ea35fdd94510f3d157082c",
            "d6acdedf95f608e09fa53fb43dcd0990475726c5131210c9e5caeab97f0e642f",
            "1ea6675f9551eeb9dfaaa9247bc9858270d3d3a4c5afa7177a984d5ed1be2451",
            "6edb16d01907b759977d7650dad7e3ec049af1a3d875380b697c862c9ec5d51c",
            "cd1c8dbf6e3acc7a80439bc4962cf25b9dce7c896f3a5bd70803fc5a0e33cf00",
            "6aca8448d8263e547d5ff2950e2ed3839e998d31cbc6ac9fd57bc6002b159216",
            "8d5fa43e5a10d11605ac7430ba1f5d81fb1b68d29a640405767749e841527673",
            "08eeab0c13abd6069e6310197bf80f9c1ea6de78fd19cbae24d4a520e6cf3023",
            "0769557bc682b1bf308646fd0b22e648e8b9e98f57e29f5af40f6edb833e2c49",
            "4c6937d78f42685f84b43ad3b7b00f81285662f85c6a68ef11d62ad1a3ee0850",
            "fee0e52802cb0c46b1eb4d376c62697f4759f6c8917fa352571202fd778fd712",
            "16d6252968971a83da8521d65382e61f0176646d771c91528e3276ee45383e4a",
            "d2e1642c9a462229289e5b0e3b7f9008e0301cbb93385ee0e21da2545073cb58",
            "a5122c08ff9c161d9ca6fc462073396c7d7d38e8ee48cdb3bea7e2230134ed6a",
            "28e7b841dcbc47cceb69d7cb8d94245fb7cb2ba3a7a6bc18f13f945f7dbd6e2a",
            "e1f34b034d4a3cd28557e2907ebf990c918f64ecb50a94f01d6fda5ca5c7ef72",
            "12935f14b676509b81eb49ef25f39269ed72309238b4c145803544b646dca62d",
            "b2eed031d4d6a4f02a097f80b54cc1541d4163c6b6f5971f88b6e41d35c53814",
            "fbc2f4300c01f0b7820d00e3347c8da4ee614674376cbc45359daa54f9b5493e",
        ];

        for i in 0..EMPTY_ROOTS.len() {
            assert_eq!(hex::encode(EMPTY_ROOTS[i]), HEX_EMPTY_ROOTS[i]);
        }
    }

    #[test]
    fn incremental_roots() {
        zebra_test::init();
        // From https://github.com/zcash/zcash/blob/master/src/test/data/merkle_commitments_sapling.json
        // Byte-reversed from those ones because the original test vectors are loaded using uint256S()
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

        // Calculated by modifying TestCommitmentTree in
        // https://github.com/zcash/librustzcash/blob/master/zcash_primitives/src/merkle_tree.rs
        // to compute the full Sapling height root (32).
        let roots = [
            "ee880ed73e96ba0739578c87ba8e6a4bc33b5e63bb98875e6e2f04b214e9fb59",
            "321aef631f1a9b7914d40d7bab34c29145ac6cf69d24bf0fc566b33ac9029972",
            "ddaa1ab86de5c153993414f34ba97e9674c459dfadde112b89eeeafa0e5a204c",
            "0b337c75535b09468955d499e37cb7e2466f1f0c861ddea929aa13c699c1a454",
            "5a9b9764d76a45848012eec306d6f6bface319ad5d9bf88db96b3b19edded716",
            "004075c72e360d7b2ab113555e97dcf4fb50f211d74841eafb05aaff705e3235",
            "ebf2139c2ef10d51f21fee18521963b91b64987f2743d908be2b80b4ae29e622",
            "70d07f5662eafaf054327899abce515b1c1cbac6600edea86297c2800e806534",
            "f72dad9cd0f4d4783444f6dc64d9be2edc74cffddcb60bf244e56eada508c22a",
            "7635d357c7755c91ea4d6b53e8fd42756329118577fe8b9ade3d33b316fa4948",
            "fca0c26ce07fc7e563b031d9187f829fa41715f193f08bd0ac25e5122ac75c2e",
            "0b727c9c6f66c3c749ef9c1df6c5356db8adf80fcc3c1d7fdf56b82cb8d47a3c",
            "d77d030ed3c2521567eae9555b95eca89442b0c263b82fea4359f802e0f31668",
            "3d84c8b65e5a8036d115161bb6e3ca2a556e42d376abc3d74a16bc22685b7d61",
            "84f752458538a24483e9731e32fa95cabf56aebbbc6bff8475f45299bcdcba35",
            "bb3cc8f85773c05f3332a25cc8281a68450a90807cef859b49b2f1d9d2d3a64d",
        ];

        let mut leaves = vec![];

        for (i, cm_u) in commitments.iter().enumerate() {
            let bytes = <[u8; 32]>::from_hex(cm_u).unwrap();

            leaves.push(jubjub::Fq::from_bytes(&bytes).unwrap());

            let tree = NoteCommitmentTree::from(leaves.clone());

            assert_eq!(hex::encode(tree.hash()), roots[i]);
        }
    }
}
