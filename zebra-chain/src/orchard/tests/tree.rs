use halo2::pasta::group::ff::PrimeField;
use orchard::note::ExtractedNoteCommitment;

use crate::orchard::tests::vectors;
use crate::orchard::tree::*;

#[test]
fn empty_roots() {
    let _init_guard = zebra_test::init();

    for i in 0..EMPTY_ROOTS.len() {
        assert_eq!(
            EMPTY_ROOTS[i].to_repr(),
            // The test vector is in reversed order.
            vectors::EMPTY_ROOTS[usize::from(MERKLE_DEPTH) - i]
        );
    }
}

#[test]
fn incremental_roots() {
    let _init_guard = zebra_test::init();

    let mut leaves = vec![];

    let mut incremental_tree = NoteCommitmentTree::default();

    for (i, commitment_set) in vectors::COMMITMENTS.iter().enumerate() {
        for cm_x_bytes in commitment_set.iter() {
            let cm_x = ExtractedNoteCommitment::from_bytes(cm_x_bytes).unwrap();

            leaves.push(cm_x);

            let _ = incremental_tree.append(cm_x);
        }

        assert_eq!(
            hex::encode(incremental_tree.hash()),
            hex::encode(vectors::ROOTS[i].anchor)
        );

        assert_eq!(
            hex::encode((NoteCommitmentTree::from(leaves.clone())).hash()),
            hex::encode(vectors::ROOTS[i].anchor)
        );
    }
}
