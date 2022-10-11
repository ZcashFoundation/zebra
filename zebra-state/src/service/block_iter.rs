//! Iterators for blocks in the non-finalized and finalized state.

use std::sync::Arc;

use zebra_chain::block::{self, Block};

use crate::{service::non_finalized_state::NonFinalizedState, HashOrHeight};

use super::finalized_state::ZebraDb;

/// Iterator for state blocks.
///
/// Starts at any block in any non-finalized or finalized chain,
/// and iterates in reverse height order. (Towards the genesis block.)
pub(crate) struct Iter<'a> {
    pub(super) non_finalized_state: &'a NonFinalizedState,
    pub(super) db: &'a ZebraDb,
    pub(super) state: IterState,
}

pub(super) enum IterState {
    NonFinalized(block::Hash),
    Finalized(block::Height),
    Finished,
}

impl Iter<'_> {
    fn next_non_finalized_block(&mut self) -> Option<Arc<Block>> {
        let Iter {
            non_finalized_state,
            db: _,
            state,
        } = self;

        let hash = match state {
            IterState::NonFinalized(hash) => *hash,
            IterState::Finalized(_) | IterState::Finished => unreachable!(),
        };

        if let Some(block) = non_finalized_state.any_block_by_hash(hash) {
            let hash = block.header.previous_block_hash;
            self.state = IterState::NonFinalized(hash);
            Some(block)
        } else {
            None
        }
    }

    #[allow(clippy::unwrap_in_result)]
    fn next_finalized_block(&mut self) -> Option<Arc<Block>> {
        let Iter {
            non_finalized_state: _,
            db,
            state,
        } = self;

        let hash_or_height: HashOrHeight = match *state {
            IterState::Finalized(height) => height.into(),
            IterState::NonFinalized(hash) => hash.into(),
            IterState::Finished => unreachable!(),
        };

        if let Some(block) = db.block(hash_or_height) {
            let height = block
                .coinbase_height()
                .expect("valid blocks have a coinbase height");

            if let Some(next_height) = height - 1 {
                self.state = IterState::Finalized(next_height);
            } else {
                self.state = IterState::Finished;
            }

            Some(block)
        } else {
            self.state = IterState::Finished;
            None
        }
    }

    /// Return the height for the block at `hash` in any chain.
    fn any_height_by_hash(&self, hash: block::Hash) -> Option<block::Height> {
        self.non_finalized_state
            .any_height_by_hash(hash)
            .or_else(|| self.db.height(hash))
    }
}

impl Iterator for Iter<'_> {
    type Item = Arc<Block>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.state {
            IterState::NonFinalized(_) => self
                .next_non_finalized_block()
                .or_else(|| self.next_finalized_block()),
            IterState::Finalized(_) => self.next_finalized_block(),
            IterState::Finished => None,
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();
        (len, Some(len))
    }
}

impl std::iter::FusedIterator for Iter<'_> {}

impl ExactSizeIterator for Iter<'_> {
    fn len(&self) -> usize {
        match self.state {
            IterState::NonFinalized(hash) => self
                .any_height_by_hash(hash)
                .map(|height| (height.0 + 1) as _)
                .unwrap_or(0),
            IterState::Finalized(height) => (height.0 + 1) as _,
            IterState::Finished => 0,
        }
    }
}

/// Return an iterator over the relevant chain of the block identified by
/// `hash`, in order from the largest height to the genesis block.
///
/// The block identified by `hash` is included in the chain of blocks yielded
/// by the iterator. `hash` can come from any chain.
pub(crate) fn any_ancestor_blocks<'a>(
    non_finalized_state: &'a NonFinalizedState,
    db: &'a ZebraDb,
    hash: block::Hash,
) -> Iter<'a> {
    Iter {
        non_finalized_state,
        db,
        state: IterState::NonFinalized(hash),
    }
}
