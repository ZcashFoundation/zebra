use halo2::arithmetic::FieldExt;
use halo2::pasta::pallas;

use crate::orchard::tests::test_vectors;
use crate::orchard::tree::*;

#[test]
fn empty_roots() {
    zebra_test::init();

    for i in 0..EMPTY_ROOTS.len() {
        assert_eq!(
            EMPTY_ROOTS[i].to_bytes(),
            // The test vector is in reversed order.
            test_vectors::EMPTY_ROOTS[MERKLE_DEPTH - i]
        );
    }
}

#[test]
fn incremental_roots() {
    zebra_test::init();

    let mut leaves = vec![];

    let mut incremental_tree = NoteCommitmentTree::default();

    for (i, commitment_set) in test_vectors::COMMITMENTS.iter().enumerate() {
        for cm_x in commitment_set.iter() {
            let cm_u = pallas::Base::from_bytes(&cm_x).unwrap();

            leaves.push(cm_u);

            let _ = incremental_tree.append(cm_u);
        }

        assert_eq!(
            hex::encode(incremental_tree.hash()),
            hex::encode(test_vectors::ROOTS[i].anchor)
        );

        assert_eq!(
            hex::encode((NoteCommitmentTree::from(leaves.clone())).hash()),
            hex::encode(test_vectors::ROOTS[i].anchor)
        );
    }
}
