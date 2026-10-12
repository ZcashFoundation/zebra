use incrementalmerkletree::frontier::Frontier;
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaChaRng;

use super::*;

fn commitment(rng: &mut ChaChaRng) -> NoteCommitmentUpdate {
    loop {
        if let Some(value) = Option::from(pallas::Base::from_repr(rng.random())) {
            return value;
        }
    }
}

#[test]
fn cached_merkle_crh_matches_fresh_domain_all_layers() {
    let mut rng = ChaChaRng::seed_from_u64(0x6f726368617264);
    let zero = pallas::Base::zero();
    let one = pallas::Base::one();
    let mut pairs = vec![
        (zero, zero),
        (zero, -one),
        (-one, zero),
        (-one, -one),
        (one, one),
    ];
    pairs.extend((0..16).map(|_| (commitment(&mut rng), commitment(&mut rng))));

    for layer in 0..MERKLE_DEPTH {
        for &(left, right) in &pairs {
            // Independent reference keeps the original allocated bit-message construction.
            let mut message = bitvec![u8, Lsb0;];
            let level = MERKLE_DEPTH - 1 - layer;
            message.extend_from_bitslice(&BitArray::<_, Lsb0>::from([level, 0])[..10]);
            message.extend_from_bitslice(&BitArray::<_, Lsb0>::from(left.to_repr())[..255]);
            message.extend_from_bitslice(&BitArray::<_, Lsb0>::from(right.to_repr())[..255]);
            let expected =
                crate::orchard::sinsemilla::sinsemilla_hash(b"z.cash:Orchard-MerkleCRH", &message)
                    .unwrap_or(zero);

            assert_eq!(
                merkle_crh_orchard(layer, left, right),
                expected,
                "layer {layer}"
            );
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
                Node(commitment(&mut rng)),
                (0..(size - 1).count_ones())
                    .map(|_| Node(commitment(&mut rng)))
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
        let orchard = NoteCommitmentTrees::update_orchard_note_commitment_tree(
            initial.clone(),
            commitments.clone(),
        );
        let ironwood =
            NoteCommitmentTrees::update_ironwood_note_commitment_tree(initial, commitments);
        match expected {
            Ok(subtree) => {
                for updated in [orchard, ironwood] {
                    let (tree, recorded_subtree) =
                        updated.expect("the sequential update succeeded");
                    assert_eq!(recorded_subtree, subtree);
                    tree.assert_frontier_eq(&sequential);
                }
            }
            Err(error) => {
                assert_eq!(orchard.unwrap_err(), PoolError::Orchard(error));
                assert_eq!(ironwood.unwrap_err(), PoolError::Ironwood(error));
            }
        }
    }
}
