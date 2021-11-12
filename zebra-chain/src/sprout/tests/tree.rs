//! Tests for Sprout Note Commitment Trees.

use hex::FromHex;

use crate::sprout::commitment::NoteCommitment;
use crate::sprout::tests::test_vectors;
use crate::sprout::tree;

/// Tests if empty roots are generated correctly.
#[test]
fn empty_roots() {
    zebra_test::init();

    for i in 0..tree::EMPTY_ROOTS.len() {
        assert_eq!(
            hex::encode(tree::EMPTY_ROOTS[i]),
            // The test vector is in reversed order.
            test_vectors::HEX_EMPTY_ROOTS[tree::MERKLE_DEPTH - i]
        );
    }
}

/// Tests if we have the right unused (empty) leaves.
#[test]
fn empty_leaf() {
    assert_eq!(
        tree::NoteCommitmentTree::uncommitted(),
        test_vectors::EMPTY_LEAF
    );
}

/// Tests if we can build the tree correctly.
#[test]
fn incremental_roots() {
    zebra_test::init();

    let mut leaves = vec![];

    let mut incremental_tree = tree::NoteCommitmentTree::default();

    for (i, cm) in test_vectors::COMMITMENTS.iter().enumerate() {
        let bytes = <[u8; 32]>::from_hex(cm).unwrap();
        let cm = NoteCommitment::from(bytes);

        // Test if we can append a new note commitment to the tree.
        let _ = incremental_tree.append(cm);
        let incremental_root_hash = incremental_tree.hash();
        assert_eq!(hex::encode(incremental_root_hash), test_vectors::ROOTS[i]);

        // The root hashes should match.
        let incremental_root = incremental_tree.root();
        assert_eq!(<[u8; 32]>::from(incremental_root), incremental_root_hash);

        // Test if the note commitments are counted correctly.
        assert_eq!(incremental_tree.count(), (i + 1) as u64);

        // Test if we can build the tree from a vector of note commitments
        // instead of appending only one note commitment to the tree.
        leaves.push(cm);
        let ad_hoc_tree = tree::NoteCommitmentTree::from(leaves.clone());
        let ad_hoc_root_hash = ad_hoc_tree.hash();
        assert_eq!(hex::encode(ad_hoc_root_hash), test_vectors::ROOTS[i]);

        // The root hashes should match.
        let ad_hoc_root = ad_hoc_tree.root();
        assert_eq!(<[u8; 32]>::from(ad_hoc_root), ad_hoc_root_hash);

        // Test if the note commitments are counted correctly.
        assert_eq!(ad_hoc_tree.count(), (i + 1) as u64);
    }
}
