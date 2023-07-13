//! Orchard serialization legacy code.

use incrementalmerkletree::{frontier::Frontier, Position};

use super::{Node, NoteCommitmentTree, Root, MERKLE_DEPTH};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename = "NoteCommitmentTree")]
#[allow(missing_docs)]
pub struct LegacyNoteCommitmentTree {
    pub inner: LegacyFrontier<Node, MERKLE_DEPTH>,
    cached_root: std::sync::RwLock<Option<Root>>,
}

impl From<NoteCommitmentTree> for LegacyNoteCommitmentTree {
    fn from(nct: NoteCommitmentTree) -> Self {
        LegacyNoteCommitmentTree {
            inner: nct.inner.into(),
            cached_root: nct.cached_root,
        }
    }
}

impl From<LegacyNoteCommitmentTree> for NoteCommitmentTree {
    fn from(legacy_nct: LegacyNoteCommitmentTree) -> Self {
        NoteCommitmentTree {
            inner: legacy_nct.inner.into(),
            cached_root: legacy_nct.cached_root,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Frontier")]
#[allow(missing_docs)]
pub struct LegacyFrontier<H, const DEPTH: u8> {
    frontier: Option<LegacyNonEmptyFrontier<H>>,
}

impl From<LegacyFrontier<Node, MERKLE_DEPTH>> for Frontier<Node, MERKLE_DEPTH> {
    fn from(legacy_frontier: LegacyFrontier<Node, MERKLE_DEPTH>) -> Self {
        if let Some(legacy_frontier_data) = legacy_frontier.frontier {
            let mut ommers = legacy_frontier_data.ommers;
            let position = Position::from(
                u64::try_from(legacy_frontier_data.position.0)
                    .expect("old `usize` always fits in `u64`"),
            );
            let leaf = match legacy_frontier_data.leaf {
                LegacyLeaf::Left(a) => a,
                LegacyLeaf::Right(a, b) => {
                    ommers.insert(0, a);
                    b
                }
            };
            Frontier::from_parts(
                position,
                leaf,
                ommers,
            )
            .expect("We should be able to construct a frontier from parts given legacy frontier is not empty")
        } else {
            Frontier::empty()
        }
    }
}

impl From<Frontier<Node, MERKLE_DEPTH>> for LegacyFrontier<Node, MERKLE_DEPTH> {
    fn from(frontier: Frontier<Node, MERKLE_DEPTH>) -> Self {
        if let Some(frontier_data) = frontier.value() {
            let leaf_from_frontier = *frontier_data.leaf();
            let mut leaf = LegacyLeaf::Left(leaf_from_frontier);
            let mut ommers = frontier_data.ommers().to_vec();
            let position = usize::try_from(u64::from(frontier_data.position()))
                .expect("new position should fit in a `usize`");
            if frontier_data.position().is_odd() {
                let left = ommers.remove(0);
                leaf = LegacyLeaf::Right(left, leaf_from_frontier);
            }
            LegacyFrontier {
                frontier: Some(LegacyNonEmptyFrontier {
                    position: LegacyPosition(position),
                    leaf,
                    ommers: ommers.to_vec(),
                }),
            }
        } else {
            LegacyFrontier { frontier: None }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "NonEmptyFrontier")]
struct LegacyNonEmptyFrontier<H> {
    position: LegacyPosition,
    leaf: LegacyLeaf<H>,
    ommers: Vec<H>,
}

/// A set of leaves of a Merkle tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Leaf")]
enum LegacyLeaf<A> {
    Left(A),
    Right(A, A),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
struct LegacyPosition(usize);
