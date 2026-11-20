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
            let expected = sinsemilla::HashDomain::new("z.cash:Orchard-MerkleCRH")
                .hash(message.iter().by_vals())
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
    // Pool updates append the first two in two batches, completing a subtree in the first batch
    // or filling the tree in the second.
    cases.extend([
        (subtree_size - 100, 400),
        (capacity - 300, 400),
        (0, 1 << TRACKED_SUBTREE_HEIGHT),
        (subtree_size - 2048, 4097),
    ]);

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

#[test]
fn batch_merkle_crh_matches_scalar_at_every_level() {
    let mut rng = ChaChaRng::seed_from_u64(0x616666696e65);
    // from_Q bypasses the weighted tables, keeping this oracle on the serial recurrence.
    let scalar = sinsemilla::HashDomain::from_Q(MERKLE_CRH_DOMAIN.Q());
    for count in [0, 1, 2, 7, 8, 9, 17, 64] {
        let mut nodes: Vec<_> = (0..count * 2).map(|_| Node(commitment(&mut rng))).collect();
        for (node, value) in nodes.iter_mut().zip([
            pallas::Base::zero(),
            pallas::Base::zero(),
            pallas::Base::one(),
            -pallas::Base::one(),
            -pallas::Base::one(),
            -pallas::Base::one(),
        ]) {
            *node = Node(value);
        }
        let mut out = vec![Node::default(); count];
        for level in 0..MERKLE_DEPTH {
            Node::combine_batch(level, &nodes, &mut out);
            for (pair, actual) in nodes.chunks_exact(2).zip(&out) {
                let prefix = BitArray::<_, Lsb0>::from([level, 0]);
                let left = BitArray::<_, Lsb0>::from(pair[0].0.to_repr());
                let right = BitArray::<_, Lsb0>::from(pair[1].0.to_repr());
                let expected = scalar
                    .hash(
                        prefix[..10]
                            .iter()
                            .by_vals()
                            .chain(left[..255].iter().by_vals())
                            .chain(right[..255].iter().by_vals()),
                    )
                    .unwrap_or(pallas::Base::zero());
                assert_eq!(actual.0, expected, "level {level}, width {count}");
            }
        }
    }
}

#[test]
fn batch_tree_matches_every_serial_intermediate_node() {
    let mut rng = ChaChaRng::seed_from_u64(0x6c6576656c73);
    let mut sizes = vec![0, 1, 2, 3, 15, 16, 17, 31, 32, 33, 127, 128, 129, 1024];
    sizes.extend((0..8).map(|_| rng.random_range(0..2048)));
    sizes.push(1 << TRACKED_SUBTREE_HEIGHT);
    for count in sizes {
        let mut nodes: Vec<_> = (0..count).map(|_| Node(commitment(&mut rng))).collect();
        if nodes.is_empty() {
            nodes.push(Node::empty_leaf());
        }
        for level in 0..MERKLE_DEPTH {
            if nodes.len() % 2 != 0 {
                nodes.push(Node::empty_root(level.into()));
            }
            let expected: Vec<_> = nodes
                .chunks_exact(2)
                .map(|pair| Node::combine(level.into(), &pair[0], &pair[1]))
                .collect();
            let mut actual = vec![Node::default(); expected.len()];
            Node::combine_batch(level, &nodes, &mut actual);
            assert_eq!(actual, expected, "leaves {count}, level {level}");
            nodes = actual;
        }
        assert_eq!(nodes.len(), 1);
    }
}

#[test]
fn batch_merkle_bottom_maps_to_zero_like_scalar() {
    use halo2::pasta::group::Group;

    // An identity starting accumulator forces bottom without solving a discrete logarithm
    // relation for the fixed production domain. from_Q deliberately disables table dispatch.
    let domain = sinsemilla::HashDomain::from_Q(pallas::Point::identity());
    let nodes = vec![Node::default(); 16];
    let words = vec![[0u16; 52]; 8];
    assert!(bool::from(domain.hash_many_vartime(&words)[0].is_none()));
    let scalar = domain.hash(std::iter::repeat_n(false, 520));
    assert!(bool::from(scalar.is_none()));
    let mut out = vec![Node(pallas::Base::one()); 8];
    merkle_crh_orchard_batch(&domain, 0, &nodes, &mut out);
    assert!(out
        .iter()
        .all(|node| node.0 == scalar.unwrap_or(pallas::Base::zero())));
}

/// Precomputed updates that complete a subtree give a block the same trees and subtrees as the
/// block's own updates, whether all of the block's updates are precomputed or only one of them.
#[test]
fn precomputed_updates_that_complete_a_subtree_match_the_computed_updates() {
    use std::sync::Arc;

    use proptest::{
        arbitrary::Arbitrary,
        strategy::{Strategy, ValueTree},
        test_runner::TestRunner,
    };

    use crate::{
        block::{Block, Height},
        parallel::tree::{NoteCommitmentTrees, PrecomputedTreeUpdate},
        parameters::NetworkUpgrade,
        LedgerState,
    };

    let _init_guard = zebra_test::init();

    const GENERATED_BLOCKS: usize = 32;

    let mut rng = ChaChaRng::seed_from_u64(0x73756274726565);

    // A tree whose next note commitment completes its first subtree.
    let mut almost_complete_subtree = || {
        let last_position = (1u64 << TRACKED_SUBTREE_HEIGHT) - 2;
        let tree = NoteCommitmentTree {
            inner: Frontier::from_parts(
                last_position.into(),
                Node(commitment(&mut rng)),
                (0..last_position.count_ones())
                    .map(|_| Node(commitment(&mut rng)))
                    .collect(),
            )
            .expect("the ommer count equals the number of set position bits"),
            cached_root: Default::default(),
        };
        let _ = tree.root();
        Arc::new(tree)
    };

    let strategy = LedgerState::height_strategy(Height(1), NetworkUpgrade::Nu6_3, 6, false)
        .prop_flat_map(Block::arbitrary_with);
    let mut runner = TestRunner::deterministic();

    let mut all_precomputed = 0;
    let mut one_precomputed = 0;

    for index in 0..GENERATED_BLOCKS {
        let block = Arc::new(
            strategy
                .new_tree(&mut runner)
                .expect("the block strategy generates blocks")
                .current(),
        );
        let orchard: Vec<_> = block.orchard_note_commitments().collect();
        let ironwood: Vec<_> = block.ironwood_note_commitments().collect();
        if orchard.is_empty() && ironwood.is_empty() {
            continue;
        }

        let trees = NoteCommitmentTrees {
            orchard: almost_complete_subtree(),
            ironwood: almost_complete_subtree(),
            ..Default::default()
        };
        let precomputed: Vec<_> = [
            (&trees.orchard, orchard.clone()),
            (&trees.ironwood, ironwood.clone()),
        ]
        .into_iter()
        .filter(|(_tree, note_commitments)| !note_commitments.is_empty())
        .map(|(tree, note_commitments)| {
            Arc::new(
                PrecomputedTreeUpdate::new(tree.clone(), note_commitments)
                    .expect("the tree has room"),
            )
        })
        .collect();

        let mut computed = trees.clone();
        computed
            .update_trees_parallel(&block)
            .expect("the trees have room");
        assert_eq!(computed.orchard_subtree.is_some(), !orchard.is_empty());
        assert_eq!(computed.ironwood_subtree.is_some(), !ironwood.is_empty());

        // All of the block's updates, and each one of them without the others.
        let mut cases = vec![precomputed.clone()];
        if precomputed.len() == 2 {
            cases.extend(precomputed.iter().map(|update| vec![update.clone()]));
        }

        for precomputed in cases {
            let mut reused = trees.clone();
            let completed = reused
                .update_trees_parallel_with_precomputed(&block, &precomputed, || false)
                .expect("the trees have room");
            assert!(completed);
            assert_eq!(reused, computed, "generated block {index}");

            if trees.has_update_work(&block, &precomputed) {
                one_precomputed += 1;
            } else {
                all_precomputed += 1;
            }
        }
    }

    assert!(
        all_precomputed > 0 && one_precomputed > 0,
        "some generated blocks have Orchard and Ironwood note commitments: {all_precomputed} \
         and {one_precomputed}",
    );
}
