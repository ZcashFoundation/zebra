//! Fixed test vectors for the finalized state.

use halo2::pasta::{group::ff::PrimeField, pallas};
use hex::FromHex;

use zebra_chain::{orchard, sapling};

#[test]
fn sapling_note_commitment_tree_serialization() {
    let _init_guard = zebra_test::init();

    let mut incremental_tree = sapling::tree::NoteCommitmentTree::default();

    // Some commitments from zebra-chain/src/sapling/tests/test_vectors.rs
    let hex_commitments = [
        "b02310f2e087e55bfd07ef5e242e3b87ee5d00c9ab52f61e6bd42542f93a6f55",
        "225747f3b5d5dab4e5a424f81f85c904ff43286e0f3fd07ef0b8c6a627b11458",
        "7c3ea01a6e3a3d90cf59cd789e467044b5cd78eb2c84cc6816f960746d0e036c",
    ];

    for (idx, cm_u_hex) in hex_commitments.iter().enumerate() {
        let bytes = <[u8; 32]>::from_hex(cm_u_hex).unwrap();

        let cm_u = jubjub::Fq::from_bytes(&bytes).unwrap();
        incremental_tree.append(cm_u).unwrap();
        if idx % 2 == 0 {
            // Cache the root half of the time to make sure it works in both cases
            let _ = incremental_tree.root();
        }
    }

    // Make sure the last root is cached
    let _ = incremental_tree.root();

    // This test vector was generated by the code itself.
    // The purpose of this test is to make sure the serialization format does
    // not change by accident.
    let expected_serialized_tree_hex = "0102007c3ea01a6e3a3d90cf59cd789e467044b5cd78eb2c84cc6816f960746d0e036c0162324ff2c329e99193a74d28a585a3c167a93bf41a255135529c913bd9b1e66601ddaa1ab86de5c153993414f34ba97e9674c459dfadde112b89eeeafa0e5a204c";
    let serialized_tree = incremental_tree.as_bytes();
    assert_eq!(hex::encode(&serialized_tree), expected_serialized_tree_hex);

    let deserialized_tree = sapling::tree::NoteCommitmentTree::from_bytes(serialized_tree);

    assert_eq!(incremental_tree.root(), deserialized_tree.root());
}

#[test]
fn orchard_note_commitment_tree_serialization() {
    let _init_guard = zebra_test::init();

    let mut incremental_tree = orchard::tree::NoteCommitmentTree::default();

    // Some commitments from zebra-chain/src/orchard/tests/tree.rs
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
    ];

    for (idx, cm_x_bytes) in commitments.iter().enumerate() {
        let cm_x = pallas::Base::from_repr(*cm_x_bytes).unwrap();
        incremental_tree.append(cm_x).unwrap();
        if idx % 2 == 0 {
            // Cache the root half of the time to make sure it works in both cases
            let _ = incremental_tree.root();
        }
    }

    // Make sure the last root is cached
    let _ = incremental_tree.root();

    // This test vector was generated by the code itself.
    // The purpose of this test is to make sure the serialization format does
    // not change by accident.
    let expected_serialized_tree_hex = "010200ee9488053a30c596b43014105d3477e6f578c89240d1d1ee1743b77bb6adc40a01a34b69a4e4d9ccf954d46e5da1004d361a5497f511aeb4d481d23c0be177813301a0be6dab19bc2c65d8299258c16e14d48ec4d4959568c6412aa85763c222a702";
    let serialized_tree = incremental_tree.as_bytes();
    assert_eq!(hex::encode(&serialized_tree), expected_serialized_tree_hex);

    let deserialized_tree = orchard::tree::NoteCommitmentTree::from_bytes(serialized_tree);

    assert_eq!(incremental_tree.root(), deserialized_tree.root());
}
