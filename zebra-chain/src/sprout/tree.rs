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

use byteorder::{BigEndian, ByteOrder};
use lazy_static::lazy_static;
#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;
use sha2::digest::generic_array::GenericArray;

use super::commitment::NoteCommitment;

const MERKLE_DEPTH: usize = 29;

/// MerkleCRH^Sprout Hash Function
///
/// Used to hash incremental Merkle tree hash values for Sprout.
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

    // H256: Sha256 initial state
    // https://github.com/RustCrypto/hashes/blob/master/sha2/src/consts.rs#L170
    let mut state = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    sha2::compress256(&mut state, &[GenericArray::clone_from_slice(&other_block)]);

    // Yes, sha256 does big endian here.
    // https://github.com/RustCrypto/hashes/blob/master/sha2/src/sha256.rs#L40
    let mut derived_bytes = [0u8; 32];
    BigEndian::write_u32_into(&state, &mut derived_bytes);

    derived_bytes
}

lazy_static! {
    /// Sprout note commitment trees have a max depth of 29.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#constants
    static ref EMPTY_ROOTS: Vec<[u8; 32]> = {
        // Uncommitted^Sprout = = [0]^l_MerkleSprout
        let mut v = vec![[0u8; 32]];

        for d in 0..MERKLE_DEPTH {
            v.push(merkle_crh_sprout(v[d], v[d]));
        }

        v
    };
}

/// The index of a note's commitment at the leafmost layer of its Note
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
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
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
        let mut current_layer: VecDeque<[u8; 32]> =
            values.into_iter().map(|cm| cm.into()).collect();

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
                let node = merkle_crh_sprout(left, right);

                next_layer_up.push(node);
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

    use super::*;

    #[test]
    fn empty_roots() {
        zebra_test::init();

        // From https://github.com/zcash/zcash/blob/master/src/zcash/IncrementalMerkleTree.cpp#L439
        let hex_empty_roots = [
            "0000000000000000000000000000000000000000000000000000000000000000",
            "da5698be17b9b46962335799779fbeca8ce5d491c0d26243bafef9ea1837a9d8",
            "dc766fab492ccf3d1e49d4f374b5235fa56506aac2224d39f943fcd49202974c",
            "3f0a406181105968fdaee30679e3273c66b72bf9a7f5debbf3b5a0a26e359f92",
            "26b0052694fc42fdff93e6fb5a71d38c3dd7dc5b6ad710eb048c660233137fab",
            "0109ecc0722659ff83450b8f7b8846e67b2859f33c30d9b7acd5bf39cae54e31",
            "3f909b8ce3d7ffd8a5b30908f605a03b0db85169558ddc1da7bbbcc9b09fd325",
            "40460fa6bc692a06f47521a6725a547c028a6a240d8409f165e63cb54da2d23f",
            "8c085674249b43da1b9a31a0e820e81e75f342807b03b6b9e64983217bc2b38e",
            "a083450c1ba2a3a7be76fad9d13bc37be4bf83bd3e59fc375a36ba62dc620298",
            "1ddddabc2caa2de9eff9e18c8c5a39406d7936e889bc16cfabb144f5c0022682",
            "c22d8f0b5e4056e5f318ba22091cc07db5694fbeb5e87ef0d7e2c57ca352359e",
            "89a434ae1febd7687eceea21d07f20a2512449d08ce2eee55871cdb9d46c1233",
            "7333dbffbd11f09247a2b33a013ec4c4342029d851e22ba485d4461851370c15",
            "5dad844ab9466b70f745137195ca221b48f346abd145fb5efc23a8b4ba508022",
            "507e0dae81cbfbe457fd370ef1ca4201c2b6401083ddab440e4a038dc1e358c4",
            "bdcdb3293188c9807d808267018684cfece07ac35a42c00f2c79b4003825305d",
            "bab5800972a16c2c22530c66066d0a5867e987bed21a6d5a450b683cf1cfd709",
            "11aa0b4ad29b13b057a31619d6500d636cd735cdd07d811ea265ec4bcbbbd058",
            "5145b1b055c2df02b95675e3797b91de1b846d25003c0a803d08900728f2cd6a",
            "0323f2850bf3444f4b4c5c09a6057ec7169190f45acb9e46984ab3dfcec4f06a",
            "671546e26b1da1af754531e26d8a6a51073a57ddd72dc472efb43fcb257cffff",
            "bb23a9bba56de57cb284b0d2b01c642cf79c9a5563f0067a21292412145bd78a",
            "f30cc836b9f71b4e7ee3c72b1fd253268af9a27e9d7291a23d02821b21ddfd16",
            "58a2753dade103cecbcda50b5ebfce31e12d41d5841dcc95620f7b3d50a1b9a1",
            "925e6d474a5d8d3004f29da0dd78d30ae3824ce79dfe4934bb29ec3afaf3d521",
            "08f279618616bcdd4eadc9c7a9062691a59b43b07e2c1e237f17bd189cd6a8fe",
            "c92b32db42f42e2bf0a59df9055be5c669d3242df45357659b75ae2c27a76f50",
            "c0db2a74998c50eb7ba6534f6d410efc27c4bb88acb0222c7906ea28a327b511",
            "d7c612c817793191a1e68652121876d6b3bde40f4fa52bc314145ce6e5cdd259",
        ];

        for i in 0..EMPTY_ROOTS.len() {
            assert_eq!(hex::encode(EMPTY_ROOTS[i]), hex_empty_roots[i]);
        }
    }

    #[test]
    fn incremental_roots() {
        zebra_test::init();

        // From https://github.com/zcash/zcash/blob/master/src/test/data/merkle_commitments.json
        //
        // Byte-reversed from those ones because the original test vectors are
        // loaded using uint256S()
        let commitments = [
            "62fdad9bfbf17c38ea626a9c9b8af8a748e6b4367c8494caf0ca592999e8b6ba",
            "68eb35bc5e1ddb80a761718e63a1ecf4d4977ae22cc19fa732b85515b2a4c943",
            "836045484077cf6390184ea7cd48b460e2d0f22b2293b69633bb152314a692fb",
            "92498a8295ea36d593eaee7cb8b55be3a3e37b8185d3807693184054cd574ae4",
            "ff7c360374a6508ae0904c782127ff5dce90918f3ee81cf92ef1b69afb8bf443",
            "68c4d0f69d1f18b756c2ee875c14f1c6cd38682e715ded14bf7e3c1c5610e9fc",
            "8b16cd3ec44875e4856e30344c0b4a68a6f929a68be5117b225b80926301e7b1",
            "50c0b43061c39191c3ec529734328b7f9cafeb6fd162cc49a4495442d9499a2d",
            "70ffdd5fa0f3aea18bd4700f1ac2e2e03cf5d4b7b857e8dd93b862a8319b9653",
            "d81ef64a0063573d80cd32222d8d04debbe807345ad7af2e9edf0f44bdfaf817",
            "8b92a4ec694271fe1b16cc0ea8a433bf19e78eb5ca733cc137f38e5ecb05789b",
            "04e963ab731e4aaaaaf931c3c039ea8c9d7904163936e19a8929434da9adeba3",
            "be3f6c181f162824191ecf1f78cae3ffb0ddfda671bb93277ce6ebc9201a0912",
            "1880967fc8226380a849c63532bba67990f7d0a10e9c90b848f58d634957c6e9",
            "c465bb2893cba233351094f259396301c23d73a6cf6f92bc63428a43f0dd8f8e",
            "84c834e7cb38d6f08d82f5cf4839b8920185174b11c7af771fd38dd02b206a20",
        ];

        // Calculated by the above implementation for MERKLE_DEPTH = 29 by the
        // same code confirmed to produce the test vectors from
        // https://github.com/zcash/zcash/blob/master/src/test/data/merkle_roots.json
        // when MERKLE_DEPTH = 4.
        let roots = [
            "b8e10b6c157be92c43a733e2c9bddb963a2fb9ea80ebcb307acdcc5fc89f1656",
            "83a7754b8240699dd1b63bf70cf70db28ffeb74ef87ce2f4dd32c28ae5009f4f",
            "c45297124f50dcd3f78eed017afd1e30764cd74cdf0a57751978270fd0721359",
            "b61f588fcba9cea79e94376adae1c49583f716d2f20367141f1369a235b95c98",
            "a3165c1708f0cc028014b9bf925a81c30091091ca587624de853260cd151b524",
            "6bb8c538c550abdd26baa2a7510a4ae50a03dc00e52818b9db3e4ffaa29c1f41",
            "e04e4731085ba95e3fa7c8f3d5eb9a56af63363403b783bc68802629c3fe505b",
            "c3714ab74d8e3984e8b58a2b4806934d20f6e67d7246cf8f5b2762305294a0ea",
            "63657edeead4bc45610b6d5eb80714a0622aad5788119b7d9961453e3aacda21",
            "e31b80819221718440c5351525dbb902d60ed16b74865a2528510959a1960077",
            "872f13df2e12f5503c39100602930b0f91ea360e5905a9f5ceb45d459efc36b2",
            "bdd7105febb3590832e946aa590d07377d1366cf5e7267507efa399dd0febdbc",
            "0f45f4adcb846a8bb56833ca0cae96f2fb8747958daa191a46d0f9d93268260a",
            "41c6e456e2192ab74f72cb27c444a2734ca8ade5a4788c1bc2546118dda01778",
            "8261355fd9bafc52a08d738fed29a859fbe15f2e74a5353954b150be200d0e16",
            "90665cb8a43001f0655169952399590cd17f99165587c1dd842eb674fb9f0afe",
        ];

        let mut leaves = vec![];

        for (i, cm) in commitments.iter().enumerate() {
            let mut bytes = [0u8; 32];
            let _ = hex::decode_to_slice(cm, &mut bytes);

            leaves.push(NoteCommitment::from(bytes));

            let tree = NoteCommitmentTree::from(leaves.clone());

            assert_eq!(hex::encode(tree.hash()), roots[i]);
        }
    }
}
