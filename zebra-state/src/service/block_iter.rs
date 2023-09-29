//! Iterators for blocks in the non-finalized and finalized state.

use std::{marker::PhantomData, sync::Arc};

use zebra_chain::block::{self, Block, Height};

use crate::{
    service::{finalized_state::ZebraDb, non_finalized_state::NonFinalizedState, read},
    HashOrHeight,
};

/// Generic state chain iterator, which iterates by block height or hash.
/// Can be used for blocks, block headers, or any type indexed by [`HashOrHeight`].
///
/// Starts at any hash or height in any non-finalized or finalized chain,
/// and iterates in reverse height order. (Towards the genesis block.)
#[derive(Clone, Debug)]
pub(crate) struct Iter<'a, Iterable: ChainIterable> {
    // TODO: replace these lifetimes with Arc<NonFinalizedState> and a cloned ZebraDb.
    /// The non-finalized state we're iterating.
    //
    // TODO: consider using an `Arc<Chain>` here instead, and iterating by height.
    pub(super) non_finalized_state: &'a NonFinalizedState,

    /// The finalized database we're iterating.
    pub(super) db: &'a ZebraDb,

    /// The internal state of the iterator: which block will be yielded next.
    pub(super) state: IterState,

    /// An internal marker type that lets the Rust type system track what we're iterating.
    iterable: PhantomData<Iterable::Type>,
}

/// Internal state of the chain iterator.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum IterState {
    /// Iterating non-finalized or finalized hashes, using previous block hashes.
    ///
    /// This state is used for:
    /// - iterating through the non-finalized state,
    /// - the initial iteration of a hash in the finalized state, and
    /// - the transition between the root of a non-finalized chain and the finalized state.
    NonFinalizedOrFinalized(block::Hash),

    /// Iterating finalized heights using previous heights, which are unique in the finalized state.
    Finalized(Height),

    /// Finished iterating, or an invalid iterator.
    Finished,
}

impl<Iterable> Iter<'_, Iterable>
where
    Iterable: ChainIterable,
{
    /// Returns an item by hash, and updates the iterator's internal state to point to the previous
    /// hash or height.
    fn yield_by_hash(&mut self, hash: block::Hash) -> Option<Iterable::Type> {
        // This is efficient because we check the blocks in memory first, then read a small column
        // family if needed.
        if let Some(prev_hash) = self.non_finalized_state.any_prev_block_hash_for_hash(hash) {
            // Iterating through a non-finalized chain.
            //
            // TODO:
            // Always check that the root of the chain connects to the finalized state. Cloned
            // chains can become disconnected if they are concurrently pruned by a finalized block
            // from another chain fork.
            // If that happens, the iterator is invalid and should return nothing.
            self.state = IterState::NonFinalizedOrFinalized(prev_hash);
        } else if let Some(prev_height) = self.db.prev_block_height_for_hash(hash) {
            // First iteration of a hash in the finalized state, whether from the chain root
            // or the initial iteration of a new iterator.
            self.state = IterState::Finalized(prev_height);
        } else {
            // The iterator is invalid if the block with `hash` is not in the state.
            // The iterator is finished if the current block is the genesis block.
            self.state = IterState::Finished;
        }

        Iterable::read(self.non_finalized_state, self.db, hash)
    }

    /// Returns an item by height, and updates the iterator's internal state to point to the
    /// previous height.
    fn yield_by_height(&mut self, height: Height) -> Option<Iterable::Type> {
        if let Ok(prev_height) = height.previous() {
            self.state = IterState::Finalized(prev_height);
        } else {
            // The iterator is finished if the current block is the genesis block.
            self.state = IterState::Finished;
        }

        Iterable::read(self.non_finalized_state, self.db, height)
    }

    /// Return the height for the block with `hash` in any chain.
    fn any_height_by_hash(&self, hash: block::Hash) -> Option<Height> {
        // This is efficient because we check the blocks in memory first, then read a small column
        // family if needed.
        self.non_finalized_state
            .any_height_by_hash(hash)
            .or_else(|| self.db.height(hash))
    }
}

impl<Iterable> Iterator for Iter<'_, Iterable>
where
    Iterable: ChainIterable,
{
    type Item = Iterable::Type;

    fn next(&mut self) -> Option<Self::Item> {
        match self.state {
            IterState::NonFinalizedOrFinalized(hash) => self.yield_by_hash(hash),
            IterState::Finalized(height) => self.yield_by_height(height),
            IterState::Finished => None,
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();
        (len, Some(len))
    }
}

impl<'a, Iterable> ExactSizeIterator for Iter<'a, Iterable>
where
    Iterable: ChainIterable,
    // If the Iterable can return None in the middle of iteration, we can't calculate its length
    // using the block height.
    Iter<'a, Block>: std::iter::FusedIterator,
{
    fn len(&self) -> usize {
        match self.state {
            // TODO:
            // Always check that the root of the chain connects to the finalized state. Cloned
            // chains can become disconnected if they are concurrently pruned by a finalized block
            // from another chain fork.
            // If that happens, the iterator is invalid and the length should be zero.
            IterState::NonFinalizedOrFinalized(hash) => self
                .any_height_by_hash(hash)
                // Add a block for the genesis block.
                .and_then(|height| height.next().ok())
                // If the iterator hash is invalid then there are no blocks in it.
                .map_or(0, Height::as_usize),
            // Add a block for the genesis block.
            IterState::Finalized(height) => height.as_usize() + 1,
            IterState::Finished => 0,
        }
    }
}

pub(crate) trait ChainIterable {
    type Type;

    /// Read the `Type` at `hash_or_height` from the non-finalized state or finalized `db`.
    fn read(
        non_finalized_state: &NonFinalizedState,
        db: &ZebraDb,
        hash_or_height: impl Into<HashOrHeight>,
    ) -> Option<Self::Type>;
}

impl ChainIterable for Block {
    type Type = Arc<Block>;

    fn read(
        non_finalized_state: &NonFinalizedState,
        db: &ZebraDb,
        hash_or_height: impl Into<HashOrHeight>,
    ) -> Option<Self::Type> {
        let hash_or_height = hash_or_height.into();

        // Since the block read method takes a chain, we need to look up the relevant chain first.
        let chain =
            non_finalized_state.find_chain(|chain| chain.contains_hash_or_height(hash_or_height));

        read::block(chain, db, hash_or_height)
    }
}

/// Blocks are continuous with no gaps, so they are exhausted the first time they return `None`.
impl std::iter::FusedIterator for Iter<'_, Block> {}

/// Return an iterator over the relevant chain of the block identified by
/// `hash`, in order from the largest height to the genesis block.
///
/// The block identified by `hash` is included in the chain of blocks yielded
/// by the iterator. `hash` can come from any chain.
pub(crate) fn any_ancestor_blocks<'a>(
    non_finalized_state: &'a NonFinalizedState,
    db: &'a ZebraDb,
    hash: block::Hash,
) -> Iter<'a, Block> {
    Iter {
        non_finalized_state,
        db,
        state: IterState::NonFinalizedOrFinalized(hash),
        iterable: PhantomData,
    }
}
