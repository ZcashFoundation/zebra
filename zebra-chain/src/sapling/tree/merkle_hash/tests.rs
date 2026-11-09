//! The fast Sapling Merkle hash and frontier root match `sapling_crypto` and
//! `incrementalmerkletree`.

use group::ff::PrimeField;
use incrementalmerkletree::{frontier::NonEmptyFrontier, Hashable, Level};
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaChaRng;

use crate::parallel::batch_frontier::{frontier_root, TreeHash};

fn random_node(rng: &mut ChaChaRng) -> sapling_crypto::Node {
    loop {
        if let Some(node) = Option::from(sapling_crypto::Node::from_bytes(rng.random())) {
            return node;
        }
    }
}

/// Nodes at the edges of the field and of the tree, plus random ones.
fn test_nodes(rng: &mut ChaChaRng) -> Vec<sapling_crypto::Node> {
    let p_minus_one = -jubjub::Base::one();
    let mut nodes = vec![
        sapling_crypto::Node::from_scalar(jubjub::Base::zero()),
        sapling_crypto::Node::from_scalar(jubjub::Base::one()),
        sapling_crypto::Node::from_scalar(p_minus_one),
        sapling_crypto::Node::from_scalar(jubjub::Base::from_u128(1 << 127)),
        sapling_crypto::Node::empty_leaf(),
    ];
    nodes.extend((0..32u8).map(|level| sapling_crypto::Node::empty_root(level.into())));
    nodes.extend((0..64).map(|_| random_node(rng)));
    nodes
}

#[test]
fn combine_matches_sapling_crypto() {
    let mut rng = ChaChaRng::seed_from_u64(0x7361706c);
    let nodes = test_nodes(&mut rng);
    for level in 0..32u8 {
        let level = Level::from(level);
        let mut pairs = Vec::new();
        for (index, lhs) in nodes.iter().enumerate() {
            let rhs = &nodes[(index * 7 + usize::from(u8::from(level))) % nodes.len()];
            let expected = sapling_crypto::Node::combine(level, lhs, rhs);
            assert_eq!(sapling_crypto::Node::combine_one(level, lhs, rhs), expected);
            pairs.extend([*lhs, *rhs]);
        }
        let expected: Vec<_> = pairs
            .chunks_exact(2)
            .map(|pair| sapling_crypto::Node::combine(level, &pair[0], &pair[1]))
            .collect();
        assert_eq!(sapling_crypto::Node::combine_level(level, &pairs), expected);
    }
}

#[test]
fn frontier_root_matches_incrementalmerkletree() {
    let mut rng = ChaChaRng::seed_from_u64(0x726f6f74);
    let max_position = (1u64 << 32) - 1;
    let mut positions = vec![
        0,
        1,
        2,
        3,
        (1 << 16) - 1,
        1 << 16,
        max_position - 1,
        max_position,
    ];
    positions.extend((0..64).map(|_| rng.random_range(0..=max_position)));
    for position in positions {
        let frontier = NonEmptyFrontier::from_parts(
            position.into(),
            random_node(&mut rng),
            (0..position.count_ones())
                .map(|_| random_node(&mut rng))
                .collect(),
        )
        .expect("one ommer per set position bit");
        for root_level in [16u8, 32] {
            if root_level == 16 && position >> 16 != 0 && rng.random::<bool>() {
                continue;
            }
            assert_eq!(
                frontier_root(&frontier, root_level),
                frontier.root(Some(root_level.into())),
                "position {position}, root level {root_level}"
            );
        }
    }
}
