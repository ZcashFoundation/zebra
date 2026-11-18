//! Level-wise batch reduction for the Orchard and Ironwood note commitment trees.

use crate::parallel::batch_frontier::BatchNode;

use super::{Node, MERKLE_DEPTH};

impl BatchNode for Node {
    /// Reduces all chunks together, so each level's independent parent hashes share one batch.
    fn chunk_roots<C, F>(chunks: &[&[C]], to_node: &F) -> Vec<Self>
    where
        C: Sync,
        F: Fn(&C) -> Self + Sync,
    {
        let mut roots = vec![None; chunks.len()];
        let mut nodes: Vec<Self> = chunks
            .iter()
            .flat_map(|leaves| leaves.iter().map(to_node))
            .collect();
        let mut next = Vec::with_capacity(nodes.len() / 2);
        for level in 0..MERKLE_DEPTH {
            // Remove finished chunks. Every remaining chunk has even width, so adjacent
            // pairs never cross a subtree boundary.
            let mut read = 0;
            let mut write = 0;
            for (leaves, root) in chunks.iter().zip(&mut roots) {
                let width = leaves.len() >> level;
                if width == 1 {
                    *root = Some(nodes[read]);
                } else if width > 1 {
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
            Self::combine_batch(level, &nodes, &mut next);
            std::mem::swap(&mut nodes, &mut next);
        }

        roots
            .into_iter()
            .map(|root| root.expect("every aligned chunk was reduced to a root"))
            .collect()
    }
}
