//! Batch appends shared by the depth-32 Sapling and Orchard note commitment trees.

use incrementalmerkletree::{
    frontier::{Frontier, FrontierError},
    Hashable,
};
use rayon::prelude::*;

use crate::subtree::{NoteCommitmentSubtreeIndex, TRACKED_SUBTREE_HEIGHT};

#[cfg(test)]
mod tests;

const DEPTH: u8 = 32;
// Keep small batches and reduction tasks serial to amortize scheduling over dozens of hashes.
const PARALLEL_THRESHOLD: usize = 32;

/// Appends in order and returns the last completed tracked subtree, if any.
/// On overflow, retains the prefix that fits, matching sequential `Frontier::append` calls.
#[allow(clippy::unwrap_in_result)]
pub(crate) fn append_batch<H, C, F>(
    frontier: &mut Frontier<H, DEPTH>,
    mut commitments: &[C],
    to_node: F,
) -> Result<Option<(NoteCommitmentSubtreeIndex, H)>, FrontierError>
where
    H: Hashable + Copy + Send + Sync,
    C: Sync,
    F: Fn(&C) -> H + Sync,
{
    let subtree_size = 1u64 << TRACKED_SUBTREE_HEIGHT;
    let mut completed = None;

    while !commitments.is_empty() {
        let size = frontier.tree_size();
        if size == 1u64 << DEPTH {
            return Err(FrontierError::MaxDepthExceeded { depth: DEPTH + 1 });
        }

        // Stop at each boundary so a completed subtree is captured before its frontier changes.
        let remaining = usize::try_from(subtree_size - size % subtree_size)
            .expect("a tracked subtree has at most 2^16 leaves");
        let count = commitments.len().min(remaining);
        let (segment, rest) = commitments.split_at(count);
        append_segment(frontier, segment, &to_node);
        commitments = rest;

        if count == remaining {
            let index = ((frontier.tree_size() >> TRACKED_SUBTREE_HEIGHT) - 1)
                .try_into()
                .expect("a depth-32 tree has at most 2^16 tracked subtrees");
            let root = frontier
                .value()
                .expect("the segment appended at least one leaf")
                .root(Some(TRACKED_SUBTREE_HEIGHT.into()));
            completed = Some((NoteCommitmentSubtreeIndex(index), root));
        }
    }

    Ok(completed)
}

/// Appends a nonempty segment that fits in the current tracked subtree.
fn append_segment<H, C, F>(frontier: &mut Frontier<H, DEPTH>, commitments: &[C], to_node: &F)
where
    H: Hashable + Copy + Send + Sync,
    C: Sync,
    F: Fn(&C) -> H + Sync,
{
    if commitments.len() < PARALLEL_THRESHOLD {
        for commitment in commitments {
            assert!(
                frontier.append(to_node(commitment)),
                "the segment fits in the current tracked subtree"
            );
        }
        return;
    }

    // Slots describe complete subtrees before the new tip, ordered from low to high level.
    let mut slots = [None; 32];
    if let Some(old) = frontier.value() {
        let position = u64::from(old.position());
        let mut ommers = old.ommers().iter();
        for level in 0..DEPTH {
            if position & (1u64 << level) != 0 {
                slots[usize::from(level)] = Some(
                    *ommers
                        .next()
                        .expect("a frontier has one ommer per set position bit"),
                );
            }
        }
        merge(&mut slots, 0, *old.leaf());
    }

    // The newest leaf stays unhashed, as required by Frontier::from_parts.
    let (tip, mut leaves) = commitments
        .split_last()
        .expect("parallel segments contain at least 32 commitments");
    let mut position = frontier.tree_size();
    let mut chunks = Vec::new();
    while !leaves.is_empty() {
        let level = u8::try_from(position.trailing_zeros().min(leaves.len().ilog2()))
            .expect("a segment has at most 2^16 leaves");
        let count = 1usize << level;
        let (chunk, rest) = leaves.split_at(count);
        chunks.push((level, chunk, None));
        leaves = rest;
        position += 1u64 << level;
    }

    chunks.par_iter_mut().for_each(|(_, leaves, root)| {
        *root = Some(perfect_subtree_root(leaves, to_node));
    });
    // Chunks stay in leaf order; carries must merge left to right.
    for (level, _, root) in chunks {
        merge(
            &mut slots,
            level,
            root.expect("every chunk was reduced by the parallel iterator"),
        );
    }

    *frontier = Frontier::from_parts(
        position.into(),
        to_node(tip),
        slots.into_iter().flatten().collect(),
    )
    .expect("aligned subtree merges produce one ommer per set position bit within capacity");
}

/// Carries a newer complete subtree through occupied slots, always hashing older roots on the left.
fn merge<H: Hashable + Copy>(slots: &mut [Option<H>], mut level: u8, mut node: H) {
    while let Some(left) = slots[usize::from(level)].take() {
        node = H::combine(level.into(), &left, &node);
        level += 1;
    }
    slots[usize::from(level)] = Some(node);
}

/// Reduces a power-of-two slice at its natural Merkle levels.
fn perfect_subtree_root<H, C, F>(leaves: &[C], to_node: &F) -> H
where
    H: Hashable + Send,
    C: Sync,
    F: Fn(&C) -> H + Sync,
{
    if leaves.len() == 1 {
        return to_node(&leaves[0]);
    }

    let half = leaves.len() / 2;
    let (left, right) = leaves.split_at(half);
    let (left, right) = if leaves.len() >= PARALLEL_THRESHOLD {
        rayon::join(
            || perfect_subtree_root(left, to_node),
            || perfect_subtree_root(right, to_node),
        )
    } else {
        (
            perfect_subtree_root(left, to_node),
            perfect_subtree_root(right, to_node),
        )
    };
    let level = u8::try_from(half.ilog2()).expect("a segment has at most 2^16 leaves");
    H::combine(level.into(), &left, &right)
}
