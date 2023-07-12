//! Fixed test vectors for the finalized state.
//! These tests contain snapshots of the note commitment tree serialization format.
//!
//! We don't need to check empty trees, because the database format snapshot tests
//! use empty trees.

use hex::FromHex;
use rand::random;

use halo2::pasta::{group::ff::PrimeField, pallas};

use zebra_chain::{
    orchard::tree::NoteCommitmentTree as OrchardNoteCommitmentTree,
    sapling::tree::NoteCommitmentTree as SaplingNoteCommitmentTree,
    sprout::{
        tree::NoteCommitmentTree as SproutNoteCommitmentTree,
        NoteCommitment as SproutNoteCommitment,
    },
};

use crate::service::finalized_state::disk_format::{FromDisk, IntoDisk};

// Currently, these tests check these structs are equal:
//  * commitments -> tree struct
//  * commitments -> tree struct -> seralize -> deserialize -> tree struct
// And these serialized formats are equal:
//  * fixed serialized test vector
//  * commitments -> tree struct -> seralize
//  * commitments -> tree struct -> seralize -> deserialize -> tree struct -> serialize
//
// TODO: apply these tests to the new tree structs, and update the serialization format
//       (keeping the tests for the old format is optional, because the tests below cover it)
//
// TODO: test that old and new serializations produce the same format:
// Tree roots built from the same commitments should match:
//   * commitments -> old tree struct -> new tree struct -> un-cached root
//   * commitments -> new tree struct -> un-cached root
// Even when serialized and deserialized:
//   * commitments -> old tree struct -> old serialize -> old deserialize -> old tree struct ->  new tree struct -> un-cached root
//   * commitments -> new tree struct -> new serialize -> new deserialize -> new tree struct -> un-cached root
//   * commitments -> new tree struct -> un-cached root

/// Check that the sprout tree database serialization format has not changed.
#[test]
fn sprout_note_commitment_tree_serialization() {
    let _init_guard = zebra_test::init();

    let mut incremental_tree = SproutNoteCommitmentTree::default();

    // Some commitments from zebra-chain/src/sprout/tests/test_vectors.rs
    let hex_commitments = [
        "62fdad9bfbf17c38ea626a9c9b8af8a748e6b4367c8494caf0ca592999e8b6ba",
        "68eb35bc5e1ddb80a761718e63a1ecf4d4977ae22cc19fa732b85515b2a4c943",
        "836045484077cf6390184ea7cd48b460e2d0f22b2293b69633bb152314a692fb",
    ];

    for (idx, cm_hex) in hex_commitments.iter().enumerate() {
        let bytes = <[u8; 32]>::from_hex(cm_hex).unwrap();

        let cm = SproutNoteCommitment::from(bytes);
        incremental_tree.append(cm).unwrap();
        if random() {
            info!(?idx, "randomly caching root for note commitment tree index");
            // Cache the root half of the time to make sure it works in both cases
            let _ = incremental_tree.root();
        }
    }

    // Make sure the last root is cached
    let _ = incremental_tree.root();

    // This test vector was generated by the code itself.
    // The purpose of this test is to make sure the serialization format does
    // not change by accident.
    let expected_serialized_tree_hex = "010200836045484077cf6390184ea7cd48b460e2d0f22b2293b69633bb152314a692fb019f5b2b1e4bf7e7318d0a1f417ca6bca36077025b3d11e074b94cd55ce9f3861801c45297124f50dcd3f78eed017afd1e30764cd74cdf0a57751978270fd0721359";
    let serialized_tree = incremental_tree.as_bytes();
    assert_eq!(hex::encode(&serialized_tree), expected_serialized_tree_hex);

    let deserialized_tree = SproutNoteCommitmentTree::from_bytes(&serialized_tree);

    // This check isn't enough to show that the entire struct is the same, because it just compares
    // the cached serialized/deserialized roots. (NoteCommitmentTree::eq() also just compares
    // roots.)
    assert_eq!(incremental_tree.root(), deserialized_tree.root());

    incremental_tree.assert_frontier_eq(&deserialized_tree);

    // Double-check that the internal format is the same by re-serializing the tree.
    let re_serialized_tree = deserialized_tree.as_bytes();
    assert_eq!(serialized_tree, re_serialized_tree);
}

/// Check that the sprout tree database serialization format has not changed for one commitment.
#[test]
fn sprout_note_commitment_tree_serialization_one() {
    let _init_guard = zebra_test::init();

    let mut incremental_tree = SproutNoteCommitmentTree::default();

    // Some commitments from zebra-chain/src/sprout/tests/test_vectors.rs
    let hex_commitments = ["836045484077cf6390184ea7cd48b460e2d0f22b2293b69633bb152314a692fb"];

    for (idx, cm_hex) in hex_commitments.iter().enumerate() {
        let bytes = <[u8; 32]>::from_hex(cm_hex).unwrap();

        let cm = SproutNoteCommitment::from(bytes);
        incremental_tree.append(cm).unwrap();
        if random() {
            info!(?idx, "randomly caching root for note commitment tree index");
            // Cache the root half of the time to make sure it works in both cases
            let _ = incremental_tree.root();
        }
    }

    // Make sure the last root is cached
    let _ = incremental_tree.root();

    // This test vector was generated by the code itself.
    // The purpose of this test is to make sure the serialization format does
    // not change by accident.
    let expected_serialized_tree_hex = "010000836045484077cf6390184ea7cd48b460e2d0f22b2293b69633bb152314a692fb000193e5f97ce1d5d94d0c6e1b66a4a262c9ae89e56e28f3f6e4a557b6fb70e173a8";
    let serialized_tree = incremental_tree.as_bytes();
    assert_eq!(hex::encode(&serialized_tree), expected_serialized_tree_hex);

    let deserialized_tree = SproutNoteCommitmentTree::from_bytes(&serialized_tree);

    // This check isn't enough to show that the entire struct is the same, because it just compares
    // the cached serialized/deserialized roots. (NoteCommitmentTree::eq() also just compares
    // roots.)
    assert_eq!(incremental_tree.root(), deserialized_tree.root());

    incremental_tree.assert_frontier_eq(&deserialized_tree);

    // Double-check that the internal format is the same by re-serializing the tree.
    let re_serialized_tree = deserialized_tree.as_bytes();
    assert_eq!(serialized_tree, re_serialized_tree);
}

/// Check that the sprout tree database serialization format has not changed when the number of
/// commitments is a power of two.
///
/// Some trees have special handling for even numbers of roots, or powers of two,
/// so we also check that case.
#[test]
#[ignore]
fn sprout_note_commitment_tree_serialization_pow2() {
    let _init_guard = zebra_test::init();

    let mut incremental_tree = SproutNoteCommitmentTree::default();

    // Some commitments from zebra-chain/src/sprout/tests/test_vectors.rs
    let hex_commitments = [
        "62fdad9bfbf17c38ea626a9c9b8af8a748e6b4367c8494caf0ca592999e8b6ba",
        "68eb35bc5e1ddb80a761718e63a1ecf4d4977ae22cc19fa732b85515b2a4c943",
        "836045484077cf6390184ea7cd48b460e2d0f22b2293b69633bb152314a692fb",
        "92498a8295ea36d593eaee7cb8b55be3a3e37b8185d3807693184054cd574ae4",
    ];

    for (idx, cm_hex) in hex_commitments.iter().enumerate() {
        let bytes = <[u8; 32]>::from_hex(cm_hex).unwrap();

        let cm = SproutNoteCommitment::from(bytes);
        incremental_tree.append(cm).unwrap();
        if random() {
            info!(?idx, "randomly caching root for note commitment tree index");
            // Cache the root half of the time to make sure it works in both cases
            let _ = incremental_tree.root();
        }
    }

    // Make sure the last root is cached
    let _ = incremental_tree.root();

    // This test vector was generated by the code itself.
    // The purpose of this test is to make sure the serialization format does
    // not change by accident.
    let expected_serialized_tree_hex = "010301836045484077cf6390184ea7cd48b460e2d0f22b2293b69633bb152314a692fb92498a8295ea36d593eaee7cb8b55be3a3e37b8185d3807693184054cd574ae4019f5b2b1e4bf7e7318d0a1f417ca6bca36077025b3d11e074b94cd55ce9f3861801b61f588fcba9cea79e94376adae1c49583f716d2f20367141f1369a235b95c98";
    let serialized_tree = incremental_tree.as_bytes();
    assert_eq!(hex::encode(&serialized_tree), expected_serialized_tree_hex);

    let deserialized_tree = SproutNoteCommitmentTree::from_bytes(&serialized_tree);

    // This check isn't enough to show that the entire struct is the same, because it just compares
    // the cached serialized/deserialized roots. (NoteCommitmentTree::eq() also just compares
    // roots.)
    assert_eq!(incremental_tree.root(), deserialized_tree.root());

    incremental_tree.assert_frontier_eq(&deserialized_tree);

    // Double-check that the internal format is the same by re-serializing the tree.
    let re_serialized_tree = deserialized_tree.as_bytes();
    assert_eq!(serialized_tree, re_serialized_tree);
}

/// Check that the sapling tree database serialization format has not changed.
#[test]
fn sapling_note_commitment_tree_serialization() {
    let _init_guard = zebra_test::init();

    let mut incremental_tree = SaplingNoteCommitmentTree::default();

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
        if random() {
            info!(?idx, "randomly caching root for note commitment tree index");
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

    let deserialized_tree = SaplingNoteCommitmentTree::from_bytes(&serialized_tree);

    // This check isn't enough to show that the entire struct is the same, because it just compares
    // the cached serialized/deserialized roots. (NoteCommitmentTree::eq() also just compares
    // roots.)
    assert_eq!(incremental_tree.root(), deserialized_tree.root());

    incremental_tree.assert_frontier_eq(&deserialized_tree);

    // Double-check that the internal format is the same by re-serializing the tree.
    let re_serialized_tree = deserialized_tree.as_bytes();
    assert_eq!(serialized_tree, re_serialized_tree);
}

/// Check that the sapling tree database serialization format has not changed for one commitment.
#[test]
fn sapling_note_commitment_tree_serialization_one() {
    let _init_guard = zebra_test::init();

    let mut incremental_tree = SaplingNoteCommitmentTree::default();

    // Some commitments from zebra-chain/src/sapling/tests/test_vectors.rs
    let hex_commitments = ["225747f3b5d5dab4e5a424f81f85c904ff43286e0f3fd07ef0b8c6a627b11458"];

    for (idx, cm_u_hex) in hex_commitments.iter().enumerate() {
        let bytes = <[u8; 32]>::from_hex(cm_u_hex).unwrap();

        let cm_u = jubjub::Fq::from_bytes(&bytes).unwrap();
        incremental_tree.append(cm_u).unwrap();
        if random() {
            info!(?idx, "randomly caching root for note commitment tree index");
            // Cache the root half of the time to make sure it works in both cases
            let _ = incremental_tree.root();
        }
    }

    // Make sure the last root is cached
    let _ = incremental_tree.root();

    // This test vector was generated by the code itself.
    // The purpose of this test is to make sure the serialization format does
    // not change by accident.
    let expected_serialized_tree_hex = "010000225747f3b5d5dab4e5a424f81f85c904ff43286e0f3fd07ef0b8c6a627b1145800012c60c7de033d7539d123fb275011edfe08d57431676981d162c816372063bc71";
    let serialized_tree = incremental_tree.as_bytes();
    assert_eq!(hex::encode(&serialized_tree), expected_serialized_tree_hex);

    let deserialized_tree = SaplingNoteCommitmentTree::from_bytes(&serialized_tree);

    // This check isn't enough to show that the entire struct is the same, because it just compares
    // the cached serialized/deserialized roots. (NoteCommitmentTree::eq() also just compares
    // roots.)
    assert_eq!(incremental_tree.root(), deserialized_tree.root());

    incremental_tree.assert_frontier_eq(&deserialized_tree);

    // Double-check that the internal format is the same by re-serializing the tree.
    let re_serialized_tree = deserialized_tree.as_bytes();
    assert_eq!(serialized_tree, re_serialized_tree);
}

/// Check that the sapling tree database serialization format has not changed when the number of
/// commitments is a power of two.
///
/// Some trees have special handling for even numbers of roots, or powers of two,
/// so we also check that case.
#[test]
#[ignore]
fn sapling_note_commitment_tree_serialization_pow2() {
    let _init_guard = zebra_test::init();

    let mut incremental_tree = SaplingNoteCommitmentTree::default();

    // Some commitments from zebra-chain/src/sapling/tests/test_vectors.rs
    let hex_commitments = [
        "3a27fed5dbbc475d3880360e38638c882fd9b273b618fc433106896083f77446",
        "c7ca8f7df8fd997931d33985d935ee2d696856cc09cc516d419ea6365f163008",
        "f0fa37e8063b139d342246142fc48e7c0c50d0a62c97768589e06466742c3702",
        "e6d4d7685894d01b32f7e081ab188930be6c2b9f76d6847b7f382e3dddd7c608",
        "8cebb73be883466d18d3b0c06990520e80b936440a2c9fd184d92a1f06c4e826",
        "22fab8bcdb88154dbf5877ad1e2d7f1b541bc8a5ec1b52266095381339c27c03",
        "f43e3aac61e5a753062d4d0508c26ceaf5e4c0c58ba3c956e104b5d2cf67c41c",
        "3a3661bc12b72646c94bc6c92796e81953985ee62d80a9ec3645a9a95740ac15",
    ];

    for (idx, cm_u_hex) in hex_commitments.iter().enumerate() {
        let bytes = <[u8; 32]>::from_hex(cm_u_hex).unwrap();

        let cm_u = jubjub::Fq::from_bytes(&bytes).unwrap();
        incremental_tree.append(cm_u).unwrap();
        if random() {
            info!(?idx, "randomly caching root for note commitment tree index");
            // Cache the root half of the time to make sure it works in both cases
            let _ = incremental_tree.root();
        }
    }

    // Make sure the last root is cached
    let _ = incremental_tree.root();

    // This test vector was generated by the code itself.
    // The purpose of this test is to make sure the serialization format does
    // not change by accident.
    let expected_serialized_tree_hex = "010701f43e3aac61e5a753062d4d0508c26ceaf5e4c0c58ba3c956e104b5d2cf67c41c3a3661bc12b72646c94bc6c92796e81953985ee62d80a9ec3645a9a95740ac15025991131c5c25911b35fcea2a8343e2dfd7a4d5b45493390e0cb184394d91c349002df68503da9247dfde6585cb8c9fa94897cf21735f8fc1b32116ef474de05c01d23765f3d90dfd97817ed6d995bd253d85967f77b9f1eaef6ecbcb0ef6796812";
    let serialized_tree = incremental_tree.as_bytes();
    assert_eq!(hex::encode(&serialized_tree), expected_serialized_tree_hex);

    let deserialized_tree = SaplingNoteCommitmentTree::from_bytes(&serialized_tree);

    // This check isn't enough to show that the entire struct is the same, because it just compares
    // the cached serialized/deserialized roots. (NoteCommitmentTree::eq() also just compares
    // roots.)
    assert_eq!(incremental_tree.root(), deserialized_tree.root());

    incremental_tree.assert_frontier_eq(&deserialized_tree);

    // Double-check that the internal format is the same by re-serializing the tree.
    let re_serialized_tree = deserialized_tree.as_bytes();
    assert_eq!(serialized_tree, re_serialized_tree);
}

/// Check that the orchard tree database serialization format has not changed.
#[test]
fn orchard_note_commitment_tree_serialization() {
    let _init_guard = zebra_test::init();

    let mut incremental_tree = OrchardNoteCommitmentTree::default();

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
        if random() {
            info!(?idx, "randomly caching root for note commitment tree index");
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

    let deserialized_tree = OrchardNoteCommitmentTree::from_bytes(&serialized_tree);

    // This check isn't enough to show that the entire struct is the same, because it just compares
    // the cached serialized/deserialized roots. (NoteCommitmentTree::eq() also just compares
    // roots.)
    assert_eq!(incremental_tree.root(), deserialized_tree.root());

    incremental_tree.assert_frontier_eq(&deserialized_tree);

    // Double-check that the internal format is the same by re-serializing the tree.
    let re_serialized_tree = deserialized_tree.as_bytes();
    assert_eq!(serialized_tree, re_serialized_tree);
}

/// Check that the orchard tree database serialization format has not changed for one commitment.
#[test]
fn orchard_note_commitment_tree_serialization_one() {
    let _init_guard = zebra_test::init();

    let mut incremental_tree = OrchardNoteCommitmentTree::default();

    // Some commitments from zebra-chain/src/orchard/tests/tree.rs
    let commitments = [[
        0x68, 0x13, 0x5c, 0xf4, 0x99, 0x33, 0x22, 0x90, 0x99, 0xa4, 0x4e, 0xc9, 0x9a, 0x75, 0xe1,
        0xe1, 0xcb, 0x46, 0x40, 0xf9, 0xb5, 0xbd, 0xec, 0x6b, 0x32, 0x23, 0x85, 0x6f, 0xea, 0x16,
        0x39, 0x0a,
    ]];

    for (idx, cm_x_bytes) in commitments.iter().enumerate() {
        let cm_x = pallas::Base::from_repr(*cm_x_bytes).unwrap();
        incremental_tree.append(cm_x).unwrap();
        if random() {
            info!(?idx, "randomly caching root for note commitment tree index");
            // Cache the root half of the time to make sure it works in both cases
            let _ = incremental_tree.root();
        }
    }

    // Make sure the last root is cached
    let _ = incremental_tree.root();

    // This test vector was generated by the code itself.
    // The purpose of this test is to make sure the serialization format does
    // not change by accident.
    let expected_serialized_tree_hex = "01000068135cf49933229099a44ec99a75e1e1cb4640f9b5bdec6b3223856fea16390a000178afd4da59c541e9c2f317f9aff654f1fb38d14dc99431cbbfa93601c7068117";
    let serialized_tree = incremental_tree.as_bytes();
    assert_eq!(hex::encode(&serialized_tree), expected_serialized_tree_hex);

    let deserialized_tree = OrchardNoteCommitmentTree::from_bytes(&serialized_tree);

    // This check isn't enough to show that the entire struct is the same, because it just compares
    // the cached serialized/deserialized roots. (NoteCommitmentTree::eq() also just compares
    // roots.)
    assert_eq!(incremental_tree.root(), deserialized_tree.root());

    incremental_tree.assert_frontier_eq(&deserialized_tree);

    // Double-check that the internal format is the same by re-serializing the tree.
    let re_serialized_tree = deserialized_tree.as_bytes();
    assert_eq!(serialized_tree, re_serialized_tree);
}

/// Check that the orchard tree database serialization format has not changed when the number of
/// commitments is a power of two.
///
/// Some trees have special handling for even numbers of roots, or powers of two,
/// so we also check that case.
#[test]
#[ignore]
fn orchard_note_commitment_tree_serialization_pow2() {
    let _init_guard = zebra_test::init();

    let mut incremental_tree = OrchardNoteCommitmentTree::default();

    // Some commitments from zebra-chain/src/orchard/tests/tree.rs
    let commitments = [
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
        if random() {
            info!(?idx, "randomly caching root for note commitment tree index");
            // Cache the root half of the time to make sure it works in both cases
            let _ = incremental_tree.root();
        }
    }

    // Make sure the last root is cached
    let _ = incremental_tree.root();

    // This test vector was generated by the code itself.
    // The purpose of this test is to make sure the serialization format does
    // not change by accident.
    let expected_serialized_tree_hex = "01010178315008fb2998b430a5731d6726207dc0f0ec81ea64af5cf612956901e72f0eee9488053a30c596b43014105d3477e6f578c89240d1d1ee1743b77bb6adc40a0001d3d525931005e45f5a29bc82524e871e5ee1b6d77839deb741a6e50cd99fdf1a";
    let serialized_tree = incremental_tree.as_bytes();
    assert_eq!(hex::encode(&serialized_tree), expected_serialized_tree_hex);

    let deserialized_tree = OrchardNoteCommitmentTree::from_bytes(&serialized_tree);

    // This check isn't enough to show that the entire struct is the same, because it just compares
    // the cached serialized/deserialized roots. (NoteCommitmentTree::eq() also just compares
    // roots.)
    assert_eq!(incremental_tree.root(), deserialized_tree.root());

    incremental_tree.assert_frontier_eq(&deserialized_tree);

    // Double-check that the internal format is the same by re-serializing the tree.
    let re_serialized_tree = deserialized_tree.as_bytes();
    assert_eq!(serialized_tree, re_serialized_tree);
}
