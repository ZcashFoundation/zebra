use super::*;

/// Cheap, order- and level-sensitive hashes keep the multi-subtree regression small.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Node(u64);

impl Hashable for Node {
    fn empty_leaf() -> Self {
        Self(0)
    }

    fn combine(level: incrementalmerkletree::Level, left: &Self, right: &Self) -> Self {
        Self(
            left.0.rotate_left(7)
                ^ right.0.wrapping_mul(0x9e3779b97f4a7c15)
                ^ u64::from(u8::from(level)),
        )
    }
}

#[test]
fn append_batch_keeps_last_completed_subtree() {
    let mut sequential = Frontier::<Node, DEPTH>::empty();
    for value in 0..3 {
        assert!(sequential.append(Node(value)));
    }
    let mut batch = sequential.clone();
    let commitments: Vec<_> = (3..(2u64 << TRACKED_SUBTREE_HEIGHT) + 97).collect();
    let mut expected = None;
    for &commitment in &commitments {
        assert!(sequential.append(Node(commitment)));
        let frontier = sequential.value().expect("a leaf was just appended");
        if frontier
            .position()
            .is_complete_subtree(TRACKED_SUBTREE_HEIGHT.into())
        {
            let index = (u64::from(frontier.position()) >> TRACKED_SUBTREE_HEIGHT)
                .try_into()
                .expect("the test only completes two subtrees");
            expected = Some((
                NoteCommitmentSubtreeIndex(index),
                frontier.root(Some(TRACKED_SUBTREE_HEIGHT.into())),
            ));
        }
    }

    assert_eq!(
        append_batch(&mut batch, &commitments, |value| Node(*value)),
        Ok(expected)
    );
    assert_eq!(
        expected.map(|(index, _)| index),
        Some(NoteCommitmentSubtreeIndex(1))
    );
    assert_eq!(batch, sequential);
    assert_eq!(batch.root(), sequential.root());
}
