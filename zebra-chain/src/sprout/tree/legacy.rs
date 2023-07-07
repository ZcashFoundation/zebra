//!
//!

use super::{Node, MERKLE_DEPTH};
use incrementalmerkletree::frontier::Frontier;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyFrontier<H, const DEPTH: u8> {
    frontier: Option<LegacyNonEmptyFrontier<H>>,
}

impl From<LegacyFrontier<Node, MERKLE_DEPTH>> for Frontier<Node, MERKLE_DEPTH> {
    fn from(frontier: LegacyFrontier<Node, MERKLE_DEPTH>) -> Self {
        if frontier.frontier.is_some() {
            Frontier::from_parts(
                u64::from(frontier.clone().frontier.unwrap().position.0 as u32).into(),
                *frontier.clone().frontier.unwrap().leaf.value(),
                frontier.frontier.unwrap().ommers,
            )
            .unwrap()
        } else {
            Frontier::empty()
        }
    }
}

impl LegacyFrontier<Node, MERKLE_DEPTH> {
    ///
    pub fn to_frontier_from_mut(&mut self) -> Frontier<Node, MERKLE_DEPTH> {
        Frontier::from(self.to_owned())
    }

    ///
    pub fn to_frontier(&self) -> Frontier<Node, MERKLE_DEPTH> {
        Frontier::from(self.clone())
    }

    ///
    pub fn to_legacy(frontier: Frontier<Node, MERKLE_DEPTH>) -> LegacyFrontier<Node, MERKLE_DEPTH> {
        LegacyFrontier {
            frontier: Some(LegacyNonEmptyFrontier {
                position: LegacyPosition(u64::from(frontier.value().unwrap().position()) as usize),
                leaf: LegacyLeaf::Left(frontier.root()),
                ommers: frontier.value().unwrap().ommers().to_vec(),
            }),
        }
    }

    ///
    pub fn empty() -> LegacyFrontier<Node, MERKLE_DEPTH> {
        LegacyFrontier { frontier: None }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyNonEmptyFrontier<H> {
    position: LegacyPosition,
    leaf: LegacyLeaf<H>,
    ommers: Vec<H>,
}

/// A set of leaves of a Merkle tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LegacyLeaf<A> {
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
pub struct LegacyPosition(usize);
