//! Sapling serialization legacy code.
//!
//! We create a [`LegacyNoteCommitmentTree`] which is a copy of [`NoteCommitmentTree`] but where serialization and
//! deserialization can be derived.
//! To do this we create a [`LegacyFrontier`] which is a legacy `Frontier` structure that can be found in [1],
//! In order to make [`LegacyFrontier`] serializable we also have our own versions of `NonEmptyFrontier` ([`LegacyNonEmptyFrontier`]),
//! `Leaf`([`LegacyLeaf`]) and `Position`([`LegacyPosition`]) that can be found in [1] or [2].
//!
//! Conversions methods to/from [`LegacyNoteCommitmentTree`] to/from [`NoteCommitmentTree`] are defined also in this file.
//!
//! [1]:
//! https://github.com/zcash/incrementalmerkletree/blob/incrementalmerkletree-v0.3.1/src/bridgetree.rs
//! [2]:
//! https://github.com/zcash/incrementalmerkletree/blob/incrementalmerkletree-v0.3.1/src/lib.rs

use incrementalmerkletree::{frontier::Frontier, Position};

use super::{Node, NoteCommitmentTree, Root, MERKLE_DEPTH};

/// A legacy version of [`NoteCommitmentTree`].
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
    fn from(nct: LegacyNoteCommitmentTree) -> Self {
        NoteCommitmentTree {
            inner: nct.inner.into(),
            cached_root: nct.cached_root,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Frontier")]
#[allow(missing_docs)]
pub struct LegacyFrontier<H, const DEPTH: u8> {
    pub frontier: Option<LegacyNonEmptyFrontier<H>>,
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
            if frontier_data.position().is_right_child() {
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
#[allow(missing_docs)]
pub struct LegacyNonEmptyFrontier<H> {
    pub position: LegacyPosition,
    pub leaf: LegacyLeaf<H>,
    pub ommers: Vec<H>,
}

/// A set of leaves of a Merkle tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Leaf")]
#[allow(missing_docs)]
pub enum LegacyLeaf<A> {
    Left(A),
    Right(A, A),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
#[allow(missing_docs)]
pub struct LegacyPosition(pub usize);
