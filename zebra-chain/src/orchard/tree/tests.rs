use bitvec::prelude::*;
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

/// The independent reference builds the message bit-by-bit and retains the
/// exceptional-case behavior of the variable-length Sinsemilla implementation.
fn merkle_crh_orchard_uncached(layer: u8, left: pallas::Base, right: pallas::Base) -> pallas::Base {
    let mut message = bitvec![u8, Lsb0;];
    let level = MERKLE_DEPTH - 1 - layer;
    message.extend_from_bitslice(&BitArray::<_, Lsb0>::from([level, 0])[..10]);
    message.extend_from_bitslice(&BitArray::<_, Lsb0>::from(left.to_repr())[..255]);
    message.extend_from_bitslice(&BitArray::<_, Lsb0>::from(right.to_repr())[..255]);

    crate::orchard::sinsemilla::sinsemilla_hash(b"z.cash:Orchard-MerkleCRH", &message)
        .unwrap_or(pallas::Base::zero())
}

/// Field elements spanning the full child encoding, including the largest
/// canonical values and raw limbs reduced modulo the field modulus.
fn full_width_field_elements() -> Vec<pallas::Base> {
    vec![
        pallas::Base::zero() - pallas::Base::one(),
        pallas::Base::zero() - pallas::Base::from(2),
        pallas::Base::from_raw([u64::MAX, u64::MAX, u64::MAX, u64::MAX]),
        pallas::Base::from_raw([0, 0, 0, u64::MAX]),
        pallas::Base::from_raw([
            0x0123_4567_89ab_cdef,
            0xfedc_ba98_7654_3210,
            0xdead_beef_cafe_babe,
            0x0f1e_2d3c_4b5a_6978,
        ]),
    ]
}

/// Differential checks on ordinary inputs cover all layers, child-word
/// alignments, and the cross-child word. They do not establish equivalence
/// on the exceptional inputs discussed in the weighted evaluator's reduction.
#[test]
fn weighted_merkle_crh_matches_fresh_domain_all_layers() {
    let mut values: Vec<pallas::Base> = [0u64, 1, 2, 7, 65_535, u64::MAX]
        .into_iter()
        .map(pallas::Base::from)
        .collect();
    values.extend(full_width_field_elements());

    for layer in 0..MERKLE_DEPTH {
        for &left in &values {
            for &right in &values {
                assert_eq!(
                    merkle_crh_orchard(layer, left, right).to_repr(),
                    merkle_crh_orchard_uncached(layer, left, right).to_repr(),
                    "weighted evaluator must match fresh domain at layer {layer}",
                );
            }
        }
    }
}

proptest::proptest! {
    /// Random ordinary inputs check full-width child packing and weighted hashing.
    #[test]
    fn weighted_merkle_crh_matches_fresh_domain_random(
        layer in 0u8..MERKLE_DEPTH,
        left_limbs in proptest::prelude::any::<[u64; 4]>(),
        right_limbs in proptest::prelude::any::<[u64; 4]>(),
    ) {
        let left = pallas::Base::from_raw(left_limbs);
        let right = pallas::Base::from_raw(right_limbs);

        proptest::prop_assert_eq!(
            merkle_crh_orchard(layer, left, right).to_repr(),
            merkle_crh_orchard_uncached(layer, left, right).to_repr(),
            "weighted evaluator must match fresh domain at layer {}", layer
        );
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
