//! Batch appends shared by the depth-32 Sapling and Orchard note commitment trees.

use incrementalmerkletree::{frontier::Frontier, Hashable};
use rayon::prelude::*;

use crate::subtree::{NoteCommitmentSubtreeIndex, TRACKED_SUBTREE_HEIGHT};

#[cfg(test)]
mod tests;

const DEPTH: u8 = 32;
// Keep small batches and reduction tasks serial to amortize scheduling over dozens of hashes.
const PARALLEL_THRESHOLD: usize = 32;

/// An append failure, or cancellation before the append completed.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum AppendBatchError {
    Frontier,
    Cancelled,
}

/// Appends in order, stopping when `is_cancelled` returns true.
#[allow(clippy::unwrap_in_result)]
pub(crate) fn append_batch_with_cancel<H, C, F, G>(
    frontier: &mut Frontier<H, DEPTH>,
    mut commitments: &[C],
    to_node: F,
    is_cancelled: G,
) -> Result<Option<(NoteCommitmentSubtreeIndex, H)>, AppendBatchError>
where
    H: Hashable + Copy + Send + Sync,
    C: Sync,
    F: Fn(&C) -> H + Sync,
    G: Fn() -> bool + Sync,
{
    let subtree_size = 1u64 << TRACKED_SUBTREE_HEIGHT;
    let mut completed = None;

    while !commitments.is_empty() {
        if is_cancelled() {
            return Err(AppendBatchError::Cancelled);
        }

        let size = frontier.tree_size();
        if size == 1u64 << DEPTH {
            return Err(AppendBatchError::Frontier);
        }

        // Stop at each boundary so a completed subtree is captured before its frontier changes.
        let remaining = usize::try_from(subtree_size - size % subtree_size)
            .expect("a tracked subtree has at most 2^16 leaves");
        let count = commitments.len().min(remaining);
        let (segment, rest) = commitments.split_at(count);
        append_segment(frontier, segment, &to_node, &is_cancelled)?;
        commitments = rest;

        if count == remaining {
            if is_cancelled() {
                return Err(AppendBatchError::Cancelled);
            }

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
#[allow(clippy::unwrap_in_result)]
fn append_segment<H, C, F, G>(
    frontier: &mut Frontier<H, DEPTH>,
    commitments: &[C],
    to_node: &F,
    is_cancelled: &G,
) -> Result<(), AppendBatchError>
where
    H: Hashable + Copy + Send + Sync,
    C: Sync,
    F: Fn(&C) -> H + Sync,
    G: Fn() -> bool + Sync,
{
    if commitments.len() < PARALLEL_THRESHOLD {
        for commitment in commitments {
            if is_cancelled() {
                return Err(AppendBatchError::Cancelled);
            }

            assert!(
                frontier.append(to_node(commitment)),
                "the segment fits in the current tracked subtree"
            );
        }
        return Ok(());
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
        merge(&mut slots, 0, *old.leaf(), is_cancelled)?;
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

    chunks.par_iter_mut().try_for_each(|(_, leaves, root)| {
        *root = Some(perfect_subtree_root(leaves, to_node, is_cancelled)?);
        Ok::<(), AppendBatchError>(())
    })?;
    // Chunks stay in leaf order; carries must merge left to right.
    for (level, _, root) in chunks {
        merge(
            &mut slots,
            level,
            root.expect("every chunk was reduced by the parallel iterator"),
            is_cancelled,
        )?;
    }

    if is_cancelled() {
        return Err(AppendBatchError::Cancelled);
    }

    *frontier = Frontier::from_parts(
        position.into(),
        to_node(tip),
        slots.into_iter().flatten().collect(),
    )
    .expect("aligned subtree merges produce one ommer per set position bit within capacity");

    Ok(())
}

/// Carries a newer complete subtree through occupied slots, always hashing older roots on the left.
fn merge<H, G>(
    slots: &mut [Option<H>],
    mut level: u8,
    mut node: H,
    is_cancelled: &G,
) -> Result<(), AppendBatchError>
where
    H: Hashable + Copy,
    G: Fn() -> bool + Sync,
{
    while let Some(left) = slots[usize::from(level)].take() {
        if is_cancelled() {
            return Err(AppendBatchError::Cancelled);
        }

        node = H::combine(level.into(), &left, &node);
        level += 1;
    }
    slots[usize::from(level)] = Some(node);
    Ok(())
}

/// Reduces a power-of-two slice at its natural Merkle levels.
#[allow(clippy::unwrap_in_result)]
fn perfect_subtree_root<H, C, F, G>(
    leaves: &[C],
    to_node: &F,
    is_cancelled: &G,
) -> Result<H, AppendBatchError>
where
    H: Hashable + Send,
    C: Sync,
    F: Fn(&C) -> H + Sync,
    G: Fn() -> bool + Sync,
{
    if is_cancelled() {
        return Err(AppendBatchError::Cancelled);
    }

    if leaves.len() == 1 {
        return Ok(to_node(&leaves[0]));
    }

    let half = leaves.len() / 2;
    let (left, right) = leaves.split_at(half);
    let (left, right) = if leaves.len() >= PARALLEL_THRESHOLD {
        rayon::join(
            || perfect_subtree_root(left, to_node, is_cancelled),
            || perfect_subtree_root(right, to_node, is_cancelled),
        )
    } else {
        (
            perfect_subtree_root(left, to_node, is_cancelled),
            perfect_subtree_root(right, to_node, is_cancelled),
        )
    };
    let (left, right) = (left?, right?);
    let level = u8::try_from(half.ilog2()).expect("a segment has at most 2^16 leaves");
    Ok(H::combine(level.into(), &left, &right))
}
