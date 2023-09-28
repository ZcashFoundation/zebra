//! Iterators for blocks in the non-finalized and finalized state.

use std::sync::Arc;

use zebra_chain::block::{self, Block};

use crate::service::non_finalized_state::NonFinalizedState;

use super::finalized_state::ZebraDb;

/// State chain iterator by block height or hash.
/// Can be used for blocks, block headers, or any type indexed by [`HashOrHeight`].
///
/// Starts at any hash or height in any non-finalized or finalized chain,
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

        // This is efficient because the number of chains is limited, and the blocks are in memory.
        if let Some(prev_hash) = non_finalized_state.any_prev_block_hash_for_hash(hash) {
            self.state = IterState::NonFinalized(prev_hash);
        } else {
            self.state = IterState::Finished;
        }

        non_finalized_state.any_block_by_hash(hash)
    }

    #[allow(clippy::unwrap_in_result)]
    fn next_finalized_block(&mut self) -> Option<Arc<Block>> {
        let height = match self.state {
            IterState::Finalized(height) => Some(height),
            // This is efficient because we only read the database once, when transitioning between
            // non-finalized hashes and finalized heights.
            IterState::NonFinalized(hash) => self.any_height_by_hash(hash),
            IterState::Finished => unreachable!(),
        };

        // We're finished unless we have a valid height and a valid previous height.
        self.state = IterState::Finished;

        if let Some(height) = height {
            if let Ok(prev_height) = height.previous() {
                self.state = IterState::Finalized(prev_height);
            }

            return self.db.block(height.into());
        }

        None
    }

    /// Return the height for the block at `hash` in any chain.
    fn any_height_by_hash(&self, hash: block::Hash) -> Option<block::Height> {
        // This is efficient because we check the blocks in memory first, then read a small column
        // family if needed.
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
