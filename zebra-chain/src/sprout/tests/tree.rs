//! Tests for Sprout Note Commitment Trees.

use hex::FromHex;

use crate::sprout::commitment::NoteCommitment;
use crate::sprout::tests::test_vectors;
use crate::sprout::tree;

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

#[test]
fn incremental_roots() {
    zebra_test::init();

    let mut leaves = vec![];

    let mut incremental_tree = tree::NoteCommitmentTree::default();

    for (i, cm) in test_vectors::COMMITMENTS.iter().enumerate() {
        let bytes = <[u8; 32]>::from_hex(cm).unwrap();

        let cm = NoteCommitment::from(bytes);

        leaves.push(cm);

        let _ = incremental_tree.append(cm);

        assert_eq!(hex::encode(incremental_tree.hash()), test_vectors::ROOTS[i]);

        // assert_eq!(
        //     hex::encode((NoteCommitmentTree::from(leaves.clone())).hash()),
        //     roots[i]
        // );
    }
}
