use std::sync::atomic::{AtomicUsize, Ordering};

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
        append_batch_with_cancel(&mut batch, &commitments, |value| Node(*value), || false),
        Ok(expected)
    );
    assert_eq!(
        expected.map(|(index, _)| index),
        Some(NoteCommitmentSubtreeIndex(1))
    );
    assert_eq!(batch, sequential);
    assert_eq!(batch.root(), sequential.root());
}

#[test]
fn cancelled_batch_does_no_hashing_or_appends() {
    static COMBINE_COUNT: AtomicUsize = AtomicUsize::new(0);

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct CountingNode(u64);

    impl Hashable for CountingNode {
        fn empty_leaf() -> Self {
            Self(0)
        }

        fn combine(_level: incrementalmerkletree::Level, _left: &Self, _right: &Self) -> Self {
            COMBINE_COUNT.fetch_add(1, Ordering::Relaxed);
            Self(0)
        }
    }

    let commitments: Vec<_> = (0..128).collect();
    let mut batch = Frontier::<CountingNode, DEPTH>::empty();
    let before = batch.clone();

    let result = append_batch_with_cancel(
        &mut batch,
        &commitments,
        |value| CountingNode(*value),
        || true,
    );

    assert!(matches!(result, Err(AppendBatchError::Cancelled)));
    assert_eq!(COMBINE_COUNT.load(Ordering::Relaxed), 0);
    assert_eq!(batch, before);
}
