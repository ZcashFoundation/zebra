//! Iterators for blocks in the non-finalized and finalized state.

use std::{marker::PhantomData, sync::Arc};

use zebra_chain::block::{self, Block, Height};

use crate::{
    service::{
        finalized_state::ZebraDb,
        non_finalized_state::{Chain, NonFinalizedState},
        read,
    },
    HashOrHeight,
};

/// Generic state chain iterator, which iterates by block height or hash.
/// Can be used for blocks, block headers, or any type indexed by [`HashOrHeight`].
///
/// Starts at any hash or height in any non-finalized or finalized chain,
/// and iterates in reverse height order. (Towards the genesis block.)
#[derive(Clone, Debug)]
pub(crate) struct Iter<Item: ChainItem> {
    /// The non-finalized chain fork we're iterating, if the iterator is in the non-finalized state.
    ///
    /// This is a cloned copy of a potentially out-of-date chain fork.
    pub(super) chain: Option<Arc<Chain>>,

    /// The finalized database we're iterating.
    ///
    /// This is the shared live database instance, which can concurrently write blocks.
    pub(super) db: ZebraDb,

    /// The height of the item which will be yielded by `Iterator::next()`.
    pub(super) height: Option<Height>,

    /// An internal marker type that tells the Rust type system what we're iterating.
    iterable: PhantomData<Item::Type>,
}

impl<Item> Iter<Item>
where
    Item: ChainItem,
{
    /// Returns an item by height, and updates the iterator's internal state to point to the
    /// previous height.
    fn yield_by_height(&mut self) -> Option<Item::Type> {
        let current_height = self.height?;

        // TODO:
        // Check if the root of the chain connects to the finalized state. Cloned chains can become
        // disconnected if they are concurrently pruned by a finalized block from another chain
        // fork. If that happens, the iterator is invalid and should stop returning items.
        //
        // Currently, we skip from the disconnected chain root to the previous height in the
        // finalized state, which is usually ok, but could cause consensus or light wallet bugs.
        let item = Item::read(self.chain.as_ref(), &self.db, current_height);

        // The iterator is finished if the current height is genesis.
        self.height = current_height.previous().ok();

        // Drop the chain if we've finished using it.
        if let Some(chain) = self.chain.as_ref() {
            if let Some(height) = self.height {
                if !chain.contains_block_height(height) {
                    std::mem::drop(self.chain.take());
                }
            } else {
                std::mem::drop(self.chain.take());
            }
        }

        item
    }
}

impl<Item> Iterator for Iter<Item>
where
    Item: ChainItem,
{
    type Item = Item::Type;

    fn next(&mut self) -> Option<Self::Item> {
        self.yield_by_height()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();
        (len, Some(len))
    }
}

impl<Item> ExactSizeIterator for Iter<Item>
where
    Item: ChainItem,
{
    fn len(&self) -> usize {
        // Add one to the height for the genesis block.
        //
        // TODO:
        // If the Item can skip heights, or return multiple items per block, we can't calculate
        // its length using the block height. For example, subtree end height iterators, or
        // transaction iterators.
        //
        // TODO:
        // Check if the root of the chain connects to the finalized state. If that happens, the
        // iterator is invalid and the length should be zero. See the comment in yield_by_height()
        // for details.
        self.height.map_or(0, |height| height.as_usize() + 1)
    }
}

// TODO:
// If the Item can return None before it gets to genesis, it is not fused. For example, subtree
// end height iterators.
impl<Item> std::iter::FusedIterator for Iter<Item> where Item: ChainItem {}

/// A trait that implements iteration for a specific chain type.
pub(crate) trait ChainItem {
    type Type;

    /// Read the `Type` at `height` from the non-finalized `chain` or finalized `db`.
    fn read(chain: Option<&Arc<Chain>>, db: &ZebraDb, height: Height) -> Option<Self::Type>;
}

// Block iteration

impl ChainItem for Block {
    type Type = Arc<Block>;

    fn read(chain: Option<&Arc<Chain>>, db: &ZebraDb, height: Height) -> Option<Self::Type> {
        read::block(chain, db, height.into())
    }
}

// Block header iteration

impl ChainItem for block::Header {
    type Type = Arc<block::Header>;

    fn read(chain: Option<&Arc<Chain>>, db: &ZebraDb, height: Height) -> Option<Self::Type> {
        read::block_header(chain, db, height.into())
    }
}

/// Returns a block iterator over the relevant chain containing `hash`,
/// in order from the largest height to genesis.
///
/// The block with `hash` is included in the iterator.
/// `hash` can come from any chain or `db`.
///
/// Use [`any_chain_ancestor_iter()`] in new code.
pub(crate) fn any_ancestor_blocks(
    non_finalized_state: &NonFinalizedState,
    db: &ZebraDb,
    hash: block::Hash,
) -> Iter<Block> {
    any_chain_ancestor_iter(non_finalized_state, db, hash)
}

/// Returns a generic chain item iterator over the relevant chain containing `hash`,
/// in order from the largest height to genesis.
///
/// The item with `hash` is included in the iterator.
/// `hash` can come from any chain or `db`.
pub(crate) fn any_chain_ancestor_iter<Item>(
    non_finalized_state: &NonFinalizedState,
    db: &ZebraDb,
    hash: block::Hash,
) -> Iter<Item>
where
    Item: ChainItem,
{
    // We need to look up the relevant chain, and the height for the hash.
    let chain = non_finalized_state.find_chain(|chain| chain.contains_block_hash(hash));
    let height = read::height_by_hash(chain.as_ref(), db, hash);

    Iter {
        chain,
        db: db.clone(),
        height,
        iterable: PhantomData,
    }
}

/// Returns a generic chain item iterator over a `chain` containing `hash_or_height`,
/// in order from the largest height to genesis.
///
/// The item with `hash_or_height` is included in the iterator.
/// `hash_or_height` must be in `chain` or `db`.
#[allow(dead_code)]
pub(crate) fn known_chain_ancestor_iter<Item>(
    chain: Option<Arc<Chain>>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Iter<Item>
where
    Item: ChainItem,
{
    // We need to look up the height for the hash.
    let height =
        hash_or_height.height_or_else(|hash| read::height_by_hash(chain.as_ref(), db, hash));

    Iter {
        chain,
        db: db.clone(),
        height,
        iterable: PhantomData,
    }
}
