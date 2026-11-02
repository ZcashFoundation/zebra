use rand::{RngExt, SeedableRng};
use rand_chacha::ChaChaRng;

use super::*;

fn commitment(rng: &mut ChaChaRng) -> NoteCommitmentUpdate {
    loop {
        if let Some(value) = Option::from(NoteCommitmentUpdate::from_bytes(&rng.random())) {
            return value;
        }
    }
}

#[test]
fn append_batch_matches_sequential_frontiers_and_subtrees() {
    let mut rng = ChaChaRng::seed_from_u64(0x6261746368);
    let capacity = 1u64 << MERKLE_DEPTH;
    let subtree_size = 1u64 << TRACKED_SUBTREE_HEIGHT;
    let sizes = [
        0,
        1,
        3,
        31,
        32,
        33,
        subtree_size - 33,
        subtree_size - 1,
        subtree_size,
        subtree_size + 1,
        capacity - 64,
        capacity - 1,
        capacity,
    ];
    let mut cases: Vec<_> = sizes
        .into_iter()
        .flat_map(|size| [0, 1, 31, 32, 33, 96].map(|len| (size, len)))
        .collect();
    cases.extend((0..16).map(|_| (rng.random_range(0..=capacity), rng.random_range(0..=256))));

    for (size, len) in cases {
        // Synthetic valid frontiers exercise high positions without billions of appends.
        let inner = if size == 0 {
            Frontier::empty()
        } else {
            Frontier::from_parts(
                (size - 1).into(),
                sapling_crypto::Node::from_cmu(&commitment(&mut rng)),
                (0..(size - 1).count_ones())
                    .map(|_| sapling_crypto::Node::from_cmu(&commitment(&mut rng)))
                    .collect(),
            )
            .expect("the ommer count equals the number of set position bits")
        };
        let mut batch = NoteCommitmentTree {
            inner,
            cached_root: Default::default(),
        };
        let _ = batch.root();
        let initial = std::sync::Arc::new(batch.clone());
        let mut sequential = batch.clone();
        let commitments: Vec<_> = (0..len).map(|_| commitment(&mut rng)).collect();
        let expected: Result<_, NoteCommitmentTreeError> =
            commitments.iter().try_fold(None, |completed, commitment| {
                sequential.append(*commitment)?;
                Ok(sequential.completed_subtree_index_and_root().or(completed))
            });

        assert_eq!(
            batch.append_batch(&commitments),
            expected,
            "size {size}, batch {len}"
        );
        batch.assert_frontier_eq(&sequential);
        assert_eq!(batch.root(), sequential.root(), "size {size}, batch {len}");
        assert_eq!(
            batch.completed_subtree_index_and_root(),
            sequential.completed_subtree_index_and_root()
        );

        use crate::parallel::tree::{NoteCommitmentTreeError as PoolError, NoteCommitmentTrees};
        let updated =
            NoteCommitmentTrees::update_sapling_note_commitment_tree(initial, commitments);
        match expected {
            Ok(subtree) => {
                let (tree, recorded_subtree) = updated.expect("the sequential update succeeded");
                assert_eq!(recorded_subtree, subtree);
                tree.assert_frontier_eq(&sequential);
            }
            Err(error) => assert_eq!(updated.unwrap_err(), PoolError::Sapling(error)),
        }
    }
}
