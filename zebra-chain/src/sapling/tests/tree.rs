use hex::FromHex;

use crate::sapling::tests::test_vectors;
use crate::sapling::tree::*;

#[test]
fn empty_roots() {
    zebra_test::init();

    for i in 0..EMPTY_ROOTS.len() {
        assert_eq!(
            hex::encode(EMPTY_ROOTS[i]),
            // The test vector is in reversed order.
            test_vectors::HEX_EMPTY_ROOTS[MERKLE_DEPTH - i]
        );
    }
}

#[test]
fn incremental_roots() {
    zebra_test::init();

    let mut leaves = vec![];

    let mut incremental_tree = NoteCommitmentTree::default();

    for (i, cm_u) in test_vectors::COMMITMENTS.iter().enumerate() {
        let bytes = <[u8; 32]>::from_hex(cm_u).unwrap();

        let cm_u = jubjub::Fq::from_bytes(&bytes).unwrap();

        leaves.push(cm_u);

        let _ = incremental_tree.append(cm_u);

        assert_eq!(hex::encode(incremental_tree.hash()), test_vectors::ROOTS[i]);

        assert_eq!(
            hex::encode((NoteCommitmentTree::from(leaves.clone())).hash()),
            test_vectors::ROOTS[i]
        );
    }
}
