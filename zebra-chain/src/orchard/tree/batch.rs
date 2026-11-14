//! Level-wise batch appends for the Orchard and Ironwood note commitment trees.

use incrementalmerkletree::frontier::{Frontier, FrontierError};

use super::{Hashable, Node, NoteCommitmentUpdate, MERKLE_DEPTH};
use crate::subtree::{NoteCommitmentSubtreeIndex, TRACKED_SUBTREE_HEIGHT};

// Small batches do not provide enough independent hashes to amortize level buffers.
const BATCH_THRESHOLD: usize = 32;

/// Appends in order and returns the last completed tracked subtree, if any.
/// On overflow, retains the prefix that fits, matching sequential `Frontier::append` calls.
#[allow(clippy::unwrap_in_result)]
pub(super) fn append_batch(
    frontier: &mut Frontier<Node, MERKLE_DEPTH>,
    mut commitments: &[NoteCommitmentUpdate],
) -> Result<Option<(NoteCommitmentSubtreeIndex, Node)>, FrontierError> {
    let subtree_size = 1u64 << TRACKED_SUBTREE_HEIGHT;
    let mut completed = None;

    while !commitments.is_empty() {
        let size = frontier.tree_size();
        if size == 1u64 << MERKLE_DEPTH {
            return Err(FrontierError::MaxDepthExceeded {
                depth: MERKLE_DEPTH + 1,
            });
        }

        // Stop at each boundary so a completed subtree is captured before its frontier changes.
        let remaining = usize::try_from(subtree_size - size % subtree_size)
            .expect("a tracked subtree has at most 2^16 leaves");
        let count = commitments.len().min(remaining);
        let (segment, rest) = commitments.split_at(count);
        append_segment(frontier, segment);
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
fn append_segment(
    frontier: &mut Frontier<Node, MERKLE_DEPTH>,
    commitments: &[NoteCommitmentUpdate],
) {
    if commitments.len() < BATCH_THRESHOLD {
        for commitment in commitments {
            assert!(
                frontier.append(Node::from(*commitment)),
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
        for level in 0..MERKLE_DEPTH {
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
        .expect("batched segments contain at least 32 commitments");
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

    reduce_chunks(&mut chunks);
    // Chunks stay in leaf order; carries must merge left to right.
    for (level, _, root) in chunks {
        merge(
            &mut slots,
            level,
            root.expect("every aligned chunk was reduced to a root"),
        );
    }

    *frontier = Frontier::from_parts(
        position.into(),
        Node::from(*tip),
        slots.into_iter().flatten().collect(),
    )
    .expect("aligned subtree merges produce one ommer per set position bit within capacity");
}

/// Carries a newer complete subtree through occupied slots, always hashing older roots on the left.
fn merge(slots: &mut [Option<Node>], mut level: u8, mut node: Node) {
    while let Some(left) = slots[usize::from(level)].take() {
        node = Node::combine(level.into(), &left, &node);
        level += 1;
    }
    slots[usize::from(level)] = Some(node);
}

/// Reduces aligned chunks together so each level uses one batch of independent hashes.
fn reduce_chunks(chunks: &mut [(u8, &[NoteCommitmentUpdate], Option<Node>)]) {
    let mut nodes: Vec<Node> = chunks
        .iter()
        .flat_map(|(_, leaves, _)| leaves.iter().map(|commitment| Node::from(*commitment)))
        .collect();
    let mut next = Vec::with_capacity(nodes.len() / 2);
    let max_level = chunks.iter().map(|(level, _, _)| *level).max().unwrap_or(0);
    for level in 0..=max_level {
        // Remove finished chunks. Every remaining chunk has even width, so adjacent
        // pairs never cross a subtree boundary.
        let mut read = 0;
        let mut write = 0;
        for (height, _, root) in chunks.iter_mut() {
            if *height < level {
                continue;
            }
            let width = 1usize << (*height - level);
            if width == 1 {
                *root = Some(nodes[read]);
            } else {
                if read != write {
                    nodes.copy_within(read..read + width, write);
                }
                write += width;
            }
            read += width;
        }
        nodes.truncate(write);
        if nodes.is_empty() {
            break;
        }
        next.resize(nodes.len() / 2, nodes[0]);
        Node::combine_batch(level, &nodes, &mut next);
        std::mem::swap(&mut nodes, &mut next);
    }
}
