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

use std::fmt;

use byteorder::{ByteOrder, LittleEndian};
use lazy_static::lazy_static;
#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;
use sha2::digest::generic_array::GenericArray;

use super::commitment::NoteCommitment;

const MERKLE_DEPTH: usize = 29;

/// MerkleCRH^Sprout Hash Function
///
/// MerkleCRH^Sprout(layer, left, right) := SHA256Compress(left || right)
///
/// `layer` is unused for Sprout but used for the Sapling equivalent.
///
/// https://zips.z.cash/protocol/protocol.pdf#merklecrh
fn merkle_crh_sprout(left: [u8; 32], right: [u8; 32]) -> [u8; 32] {
    let mut other_block = [0u8; 64];
    other_block[..32].copy_from_slice(&left[..]);
    other_block[32..].copy_from_slice(&right[..]);

    let mut state = [0u32; 8];
    sha2::compress256(&mut state, &[GenericArray::clone_from_slice(&other_block)]);

    let mut derived_bytes = [0u8; 32];
    LittleEndian::write_u32_into(&state, &mut derived_bytes);

    println!("derived: {:?}", hex::encode(derived_bytes));

    derived_bytes
}

lazy_static! {
    /// Sprout note commitment trees have a max depth of 29.
    ///
    /// https://zips.z.cash/protocol/canopy.pdf#constants
    static ref EMPTY_ROOTS: Vec<[u8; 32]> = {
        // Uncommitted^Sprout = = [0]^l_MerkleSprout
        let mut v = vec![[0u8; 32]];

        for d in 0..MERKLE_DEPTH {

            let next = merkle_crh_sprout(v[d], v[d]);
            v.push(next);
        }

        v
    };
}

/// The index of a note’s commitment at the leafmost layer of its Note
/// Commitment Tree.
///
/// https://zips.z.cash/protocol/protocol.pdf#merkletree
pub struct Position(pub(crate) u64);

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

/// Sprout Note Commitment Tree
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(test, derive(Arbitrary))]
struct NoteCommitmentTree {
    /// The root node of the tree (often used as an anchor).
    root: Root,
    /// The height of the tree (maximum height for Sprout is 29).
    height: u8,
    /// The number of leaves (note commitments) in this tree.
    count: u32,
}

impl From<Vec<NoteCommitment>> for NoteCommitmentTree {
    fn from(values: Vec<NoteCommitment>) -> Self {
        if values.is_empty() {
            return NoteCommitmentTree {
                root: Root::default(),
                height: 0,
                count: 0,
            };
        }

        let count = values.len() as u32;
        let mut height = 0u8;
        let mut current_layer: Vec<[u8; 32]> = values.into_iter().map(|cm| cm.into()).collect();

        while usize::from(height) < MERKLE_DEPTH {
            let mut next_layer_up = vec![];

            while !current_layer.is_empty() {
                let left = current_layer.remove(0);
                let right;
                if current_layer.is_empty() {
                    right = EMPTY_ROOTS[height as usize];
                } else {
                    right = current_layer.remove(0);
                }
                next_layer_up.push(merkle_crh_sprout(left, right));
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

    use super::*;

    // XXX: Computed these myself, not sure if they're correct.
    //
    // TODO: pull these in with bytes reversed:
    // https://github.com/zcash/zcash/blob/master/src/test/data/merkle_roots.json
    // or these? they're different:
    // https://github.com/zcash/zcash/blob/master/src/zcash/IncrementalMerkleTree.cpp#L439
    const HEX_EMPTY_ROOTS: [&str; 30] = [
        "0000000000000000000000000000000000000000000000000000000000000000",
        "1416a57ca83b5c422fdd54cee70a02c2d174e5b60f6d1398cc267eaea6e70bbf",
        "8e595d8337af9321e04f70caea0eb95a87ecea7c8075e027ccf38d99995e9ccb",
        "0ed30a29eb7464122b5a1497b9b9ac5e32c7b1e8b27cd3fcbf0addee5552a7dd",
        "9830b6c3261a9a9af3e706234794e3daa20f7d09b2c185553e777169274758f7",
        "949fe462a5bb6b35071f5c05f1b2d623c11bf8306816b24cb6c7efb5d96bf920",
        "021174939ba7450ed5bf8694362205fdf85bfbe617d2ad4b7ef103e45eebdaa4",
        "1b5db558fbe8b50aaef1be056a3f23980395635180c73ab1da8afa3329ef32c9",
        "b77cae0af63b4a239c830ee116373523cb5008f1f3b03c6e7c346e5c8f97fa3d",
        "86475c6708c9f1f5b4688b4c31723874e9d714890bb1b422519887c85c90b410",
        "d7365f73b4d33b00ed3cd55dbb638dd7801ae5d7323e957a06b9a2691563e748",
        "288049b23c01d1acf83a15e3fd1e19ec6cb4a550bc9e00305096a67e93dc001c",
        "20258febd62395043264fd2d5bf0e849fe74f4ae49dd8765630e9cde200e37a2",
        "2b9d0c9e7d8f5ca38175f2d40cd059a0dda76d7b0408ad27f6bec3d93ea2d0ce",
        "89be2f7d2c0e7c487578aeb3c5434042edeb30f31727c7699c33a6662fae96bc",
        "7d10d2153404df9762efa3c4796f67483a253577717a4bbefbef0b9c50a9283b",
        "18c0d669ce9cf39b86e10a8c17f580faeca5894dee5f8c444bb7c7307bd94625",
        "6855202bad86070cb0f1f8d69b0a757d365dfa57cd5f50e7003b470aa363a3f7",
        "3bf276545ddb5660752b22dbacea87495ecd77bedac75452673a56b319090a67",
        "9af95f9f3e6b2db4b159490b5c2e49b868b2c927cf3bedd7e787a635173e1195",
        "508f30499fb8d6682b4a743f2aa2a0ea2d37b55d8033c888ff6ab543f0332430",
        "9a16a3c6883170fee9d27f372b1dafb4c98911949e0e2cf8e748a56ff0bbc30c",
        "a691f83055fa4ab4be344eb7455a20cd7a54dc84159869564f2622a6ea139d69",
        "7e3ad117fb8cacf4d3cbb4416dda6bdc3a048cef02743dbee598a2c6593b2fd3",
        "8e9a5f34f491326caa1e2ca6de200d39d5c8aabe8e07f022a81449aa49051f8a",
        "a8bc7bfa549f88994f4a6b4dde74c1ceb9723f0da40ceab558a39640f6a503fc",
        "be36d79c79d4968c1ddc81c6ed398e6c99f2490c2d0b915f63f83463be6c148b",
        "eefb051a4ae623dec48e3368f4f877af771ae04b91011d4956bbc5bc03ae6260",
        "fcb9aac1104f3cbcc4da853dd8bee0faf4b4d47079147dbaaf31530e077da089",
        "25136bcbdfc1066ad6a942242ee0ae3ca3ccdc53b68d393f682734241622498d",
    ];

    #[test]
    fn empty_roots() {
        for i in 0..EMPTY_ROOTS.len() {
            println!("computed root: {:?}", hex::encode(EMPTY_ROOTS[i]));

            // assert_eq!(hex::encode(EMPTY_ROOTS[i]), HEX_EMPTY_ROOTS[i]);
        }
    }

    #[test]
    fn incremental_roots() {
        // From https://github.com/zcash/zcash/blob/master/src/test/data/merkle_commitments.json
        // Byte-reversed (?) from those ones because the original test vectors are loaded using uint256S()
        let commitments = [
            "bab6e8992959caf0ca94847c36b4e648a7f88a9b9c6a62ea387cf1fb9badfd62",
            "43c9a4b21555b832a79fc12ce27a97d4f4eca1638e7161a780db1d5ebc35eb68",
            "fb92a6142315bb3396b693222bf2d0e260b448cda74e189063cf774048456083",
            "e44a57cd544018937680d385817be3a3e35bb5b87ceeea93d536ea95828a4992",
            "43f48bfb9ab6f12ef91ce83e8f9190ce5dff2721784c90e08a50a67403367cff",
            "fce910561c3c7ebf14ed5d712e6838cdc6f1145c87eec256b7181f9df6d0c468",
            "b1e7016392805b227b11e58ba629f9a6684a0b4c34306e85e47548c43ecd168b",
            "2d9a49d9425449a449cc62d16febaf9c7f8b32349752ecc39191c36130b4c050",
            "53969b31a862b893dde857b8b7d4f53ce0e2c21a0f70d48ba1aef3a05fddff70",
            "17f8fabd440fdf9e2eafd75a3407e8bbde048d2d2232cd803d5763004af61ed8",
            "9b7805cb5e8ef337c13c73cab58ee719bf33a4a80ecc161bfe714269eca4928b",
            "a3ebada94d4329899ae136391604799d8cea39c0c331f9aaaa4a1e73ab63e904",
            "12091a20c9ebe67c2793bb71a6fdddb0ffe3ca781fcf1e192428161f186c3fbe",
            "e9c65749638df548b8909c0ea1d0f79079a6bb3235c649a8806322c87f968018",
            "8e8fddf0438a4263bc926fcfa6733dc201633959f294103533a2cb9328bb65c4",
            "206a202bd08dd31f77afc7114b17850192b83948cff5828df0d638cbe734c884",
        ];

        // From https://github.com/zcash/zcash/blob/master/src/test/data/merkle_roots.json
        let roots = [
            "95bf71d8e803b8601c14b5949d0f92690181154ef9d82eb3e24852266823317a",
            "73f18d3f9cd11010aa01d4f444039e566f14ef282109df9649b2eb75e7a53ed1",
            "dcde8a273c9672bee1a894d7f7f4abb81078f52b498e095f2a87d0aec5addf25",
            "4677d481ec6d1e97969afbc530958d1cbb4f1c047af6fdad4687cd10830e02bd",
            "74cd9d82de30c4222a06d420b75522ae1273729c1d8419446adf1184df61dc69",
            "2ff57f5468c6afdad30ec0fb6c2cb67289f12584e2c20c4e0065f66748697d77",
            "27e4ce010670801911c5765a003b15f75cde31d7378bd36540f593c8a44b3011",
            "62231ef2ec8c4da461072871ab7bc9de10253fcb40e164ddbad05b47e0b7fb69",
            "733a4ce688fdf07efb9e9f5a4b2dafff87cfe198fbe1dff71e028ef4cdee1f1b",
            "df39ed31924facdd69a93db07311d45fceac7a4987c091648044f37e6ecbb0d2",
            "87795c069bdb55281c666b9cb872d13174334ce135c12823541e9536489a9107",
            "438c80f532903b283230446514e400c329b29483db4fe9e279fdfc79e8f4347d",
            "08afb2813eda17e94aba1ab28ec191d4af99283cd4f1c5a04c0c2bc221bc3119",
            "a8b3ab3284f3288f7caa21bd2b69789a159ab4188b0908825b34723305c1228c",
            "db9b289e620de7dca2ae8fdac96808752e32e7a2c6d97ce0755dcebaa03123ab",
            "0bf622cb9f901b7532433ea2e7c1b7632f5935899b62dcf897a71551997dc8cc",
        ];

        let mut leaves = vec![];

        for (i, cm) in commitments.iter().enumerate() {
            let mut bytes = [0u8; 32];
            hex::decode_to_slice(cm, &mut bytes);
            leaves.push(NoteCommitment::from(bytes));

            let tree = NoteCommitmentTree::from(leaves.clone());

            assert_eq!(hex::encode(tree.hash()), roots[i]);
        }
    }
}
