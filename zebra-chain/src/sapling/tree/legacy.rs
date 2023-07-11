//!
//!

use incrementalmerkletree::{frontier::Frontier, Position};

use super::{Node, NoteCommitmentTree, Root, MERKLE_DEPTH};

///
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename = "NoteCommitmentTree")]
pub struct LegacyNoteCommitmentTree {
    inner: LegacyFrontier<Node, MERKLE_DEPTH>,
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
    fn from(nct: LegacyNoteCommitmentTree) -> Self {
        NoteCommitmentTree {
            inner: nct.inner.into(),
            cached_root: nct.cached_root,
        }
    }
}

///
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Frontier")]
struct LegacyFrontier<H, const DEPTH: u8> {
    frontier: Option<LegacyNonEmptyFrontier<H>>,
}

impl From<LegacyFrontier<Node, MERKLE_DEPTH>> for Frontier<Node, MERKLE_DEPTH> {
    fn from(legacy_frontier: LegacyFrontier<Node, MERKLE_DEPTH>) -> Self {
        if let Some(legacy_frontier_data) = legacy_frontier.frontier {
            Frontier::from_parts(
                Position::from(legacy_frontier_data.position.0 as u64),
                *legacy_frontier_data.leaf.value(),
                legacy_frontier_data.ommers,
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
            let leaf = LegacyLeaf::Left(*frontier_data.leaf());

            LegacyFrontier {
                frontier: Some(LegacyNonEmptyFrontier {
                    position: LegacyPosition(u64::from(frontier_data.position()) as usize),
                    leaf,
                    ommers: frontier_data.ommers().to_vec(),
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

impl<A> LegacyLeaf<A> {
    ///
    pub fn value(&self) -> &A {
        match self {
            LegacyLeaf::Left(a) => a,
            LegacyLeaf::Right(_, a) => a,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
struct LegacyPosition(usize);
