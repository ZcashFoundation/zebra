//! [`Chain`] implements a single non-finalized blockchain,
//! starting at the finalized tip.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    ops::{Deref, DerefMut, RangeInclusive},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use mset::MultiSet;
use tracing::instrument;

use zebra_chain::{
    amount::{Amount, NegativeAllowed, NonNegative},
    block::{self, Height},
    block_info::BlockInfo,
    history_tree::{HistoryTree, NonEmptyHistoryTree},
    orchard,
    parallel::tree::NoteCommitmentTrees,
    parameters::{Network, NetworkUpgrade},
    primitives::{zcash_history::Entry, Groth16Proof},
    sapling,
    serialization::ZcashSerialize as _,
    sprout,
    subtree::{NoteCommitmentSubtree, NoteCommitmentSubtreeData, NoteCommitmentSubtreeIndex},
    transaction::{
        self,
        Transaction::{self, *},
    },
    transparent,
    value_balance::ValueBalance,
    work::difficulty::PartialCumulativeWork,
};

use crate::{
    request::Treestate, service::check, ContextuallyVerifiedBlock, HashOrHeight, OutputLocation,
    TransactionLocation, ValidateContextError,
};

#[cfg(feature = "indexer")]
use crate::request::Spend;

use self::index::TransparentTransfers;

pub mod index;

/// A single non-finalized partial chain, from the child of the finalized tip,
/// to a non-finalized chain tip.
#[derive(Clone, Debug, Default)]
pub struct Chain {
    // Config
    //
    /// The configured network for this chain.
    network: Network,

    /// The internal state of this chain.
    inner: ChainInner,

    // Diagnostics
    //
    /// The last height this chain forked at. Diagnostics only.
    ///
    /// This field is only used for metrics. It is not consensus-critical, and it is not checked for
    /// equality.
    ///
    /// We keep the same last fork height in both sides of a clone, because every new block clones a
    /// chain, even if it's just growing that chain.
    ///
    /// # Note
    ///
    /// Most diagnostics are implemented on the `NonFinalizedState`, rather than each chain. Some
    /// diagnostics only use the best chain, and others need to modify the Chain state, but that's
    /// difficult with `Arc<Chain>`s.
    pub(super) last_fork_height: Option<Height>,
}

/// Spending transaction id type when the `indexer` feature is selected.
#[cfg(feature = "indexer")]
pub(crate) type SpendingTransactionId = transaction::Hash;

/// Spending transaction id type when the `indexer` feature is not selected.
#[cfg(not(feature = "indexer"))]
pub(crate) type SpendingTransactionId = ();

/// The internal state of [`Chain`].
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ChainInner {
    // Blocks, heights, hashes, and transaction locations
    //
    /// The contextually valid blocks which form this non-finalized partial chain, in height order.
    pub(crate) blocks: BTreeMap<block::Height, ContextuallyVerifiedBlock>,

    /// An index of block heights for each block hash in `blocks`.
    pub height_by_hash: HashMap<block::Hash, block::Height>,

    /// An index of [`TransactionLocation`]s for each transaction hash in `blocks`.
    pub tx_loc_by_hash: HashMap<transaction::Hash, TransactionLocation>,

    // Transparent outputs and spends
    //
    /// The [`transparent::Utxo`]s created by `blocks`.
    ///
    /// Note that these UTXOs may not be unspent.
    /// Outputs can be spent by later transactions or blocks in the chain.
    //
    // TODO: replace OutPoint with OutputLocation?
    pub(crate) created_utxos: HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
    /// The spending transaction ids by [`transparent::OutPoint`]s spent by `blocks`,
    /// including spent outputs created by earlier transactions or blocks in the chain.
    ///
    /// Note: Spending transaction ids are only tracked when the `indexer` feature is selected.
    pub(crate) spent_utxos: HashMap<transparent::OutPoint, SpendingTransactionId>,

    // Note commitment trees
    //
    /// The Sprout note commitment tree for each anchor.
    /// This is required for interstitial states.
    ///
    /// When a chain is forked from the finalized tip, also contains the finalized tip root.
    /// This extra root is removed when the first non-finalized block is committed.
    pub(crate) sprout_trees_by_anchor:
        HashMap<sprout::tree::Root, Arc<sprout::tree::NoteCommitmentTree>>,
    /// The Sprout note commitment tree for each height.
    ///
    /// When a chain is forked from the finalized tip, also contains the finalized tip tree.
    /// This extra tree is removed when the first non-finalized block is committed.
    pub(crate) sprout_trees_by_height:
        BTreeMap<block::Height, Arc<sprout::tree::NoteCommitmentTree>>,

    /// The Sapling note commitment tree for each height.
    ///
    /// When a chain is forked from the finalized tip, also contains the finalized tip tree.
    /// This extra tree is removed when the first non-finalized block is committed.
    pub(crate) sapling_trees_by_height:
        BTreeMap<block::Height, Arc<sapling::tree::NoteCommitmentTree>>,

    /// The Orchard note commitment tree for each height.
    ///
    /// When a chain is forked from the finalized tip, also contains the finalized tip tree.
    /// This extra tree is removed when the first non-finalized block is committed.
    pub(crate) orchard_trees_by_height:
        BTreeMap<block::Height, Arc<orchard::tree::NoteCommitmentTree>>,

    // History trees
    //
    /// The ZIP-221 history tree for each height, including all finalized blocks,
    /// and the non-finalized `blocks` below that height in this chain.
    ///
    /// When a chain is forked from the finalized tip, also contains the finalized tip tree.
    /// This extra tree is removed when the first non-finalized block is committed.
    pub(crate) history_trees_by_height: BTreeMap<block::Height, Arc<HistoryTree>>,

    pub(crate) history_nodes_by_height: BTreeMap<block::Height, Vec<Entry>>,

    // Anchors
    //
    /// The Sprout anchors created by `blocks`.
    ///
    /// When a chain is forked from the finalized tip, also contains the finalized tip root.
    /// This extra root is removed when the first non-finalized block is committed.
    pub(crate) sprout_anchors: MultiSet<sprout::tree::Root>,
    /// The Sprout anchors created by each block in `blocks`.
    ///
    /// When a chain is forked from the finalized tip, also contains the finalized tip root.
    /// This extra root is removed when the first non-finalized block is committed.
    pub(crate) sprout_anchors_by_height: BTreeMap<block::Height, sprout::tree::Root>,

    /// The Sapling anchors created by `blocks`.
    ///
    /// When a chain is forked from the finalized tip, also contains the finalized tip root.
    /// This extra root is removed when the first non-finalized block is committed.
    pub(crate) sapling_anchors: MultiSet<sapling::tree::Root>,
    /// The Sapling anchors created by each block in `blocks`.
    ///
    /// When a chain is forked from the finalized tip, also contains the finalized tip root.
    /// This extra root is removed when the first non-finalized block is committed.
    pub(crate) sapling_anchors_by_height: BTreeMap<block::Height, sapling::tree::Root>,
    /// A list of Sapling subtrees completed in the non-finalized state
    pub(crate) sapling_subtrees:
        BTreeMap<NoteCommitmentSubtreeIndex, NoteCommitmentSubtreeData<sapling::tree::Node>>,

    /// The Orchard anchors created by `blocks`.
    ///
    /// When a chain is forked from the finalized tip, also contains the finalized tip root.
    /// This extra root is removed when the first non-finalized block is committed.
    pub(crate) orchard_anchors: MultiSet<orchard::tree::Root>,
    /// The Orchard anchors created by each block in `blocks`.
    ///
    /// When a chain is forked from the finalized tip, also contains the finalized tip root.
    /// This extra root is removed when the first non-finalized block is committed.
    pub(crate) orchard_anchors_by_height: BTreeMap<block::Height, orchard::tree::Root>,
    /// A list of Orchard subtrees completed in the non-finalized state
    pub(crate) orchard_subtrees:
        BTreeMap<NoteCommitmentSubtreeIndex, NoteCommitmentSubtreeData<orchard::tree::Node>>,

    // Nullifiers
    //
    /// The Sprout nullifiers revealed by `blocks` and, if the `indexer` feature is selected,
    /// the id of the transaction that revealed them.
    pub(crate) sprout_nullifiers: HashMap<sprout::Nullifier, SpendingTransactionId>,
    /// The Sapling nullifiers revealed by `blocks` and, if the `indexer` feature is selected,
    /// the id of the transaction that revealed them.
    pub(crate) sapling_nullifiers: HashMap<sapling::Nullifier, SpendingTransactionId>,
    /// The Orchard nullifiers revealed by `blocks` and, if the `indexer` feature is selected,
    /// the id of the transaction that revealed them.
    pub(crate) orchard_nullifiers: HashMap<orchard::Nullifier, SpendingTransactionId>,

    // Transparent Transfers
    // TODO: move to the transparent section
    //
    /// Partial transparent address index data from `blocks`.
    pub(super) partial_transparent_transfers: HashMap<transparent::Address, TransparentTransfers>,

    // Chain Work
    //
    /// The cumulative work represented by `blocks`.
    ///
    /// Since the best chain is determined by the largest cumulative work,
    /// the work represented by finalized blocks can be ignored,
    /// because they are common to all non-finalized chains.
    pub(super) partial_cumulative_work: PartialCumulativeWork,

    // Chain Pools
    //
    /// The chain value pool balances of the tip of this [`Chain`], including the block value pool
    /// changes from all finalized blocks, and the non-finalized blocks in this chain.
    ///
    /// When a new chain is created from the finalized tip, it is initialized with the finalized tip
    /// chain value pool balances.
    pub(crate) chain_value_pools: ValueBalance<NonNegative>,
    /// The block info after the given block height.
    pub(crate) block_info_by_height: BTreeMap<block::Height, BlockInfo>,
}

impl Chain {
    /// Create a new Chain with the given finalized tip trees and network.
    pub(crate) fn new(
        network: &Network,
        finalized_tip_height: Height,
        sprout_note_commitment_tree: Arc<sprout::tree::NoteCommitmentTree>,
        sapling_note_commitment_tree: Arc<sapling::tree::NoteCommitmentTree>,
        orchard_note_commitment_tree: Arc<orchard::tree::NoteCommitmentTree>,
        history_tree: Arc<HistoryTree>,
        finalized_tip_chain_value_pools: ValueBalance<NonNegative>,
    ) -> Self {
        let inner = ChainInner {
            blocks: Default::default(),
            height_by_hash: Default::default(),
            tx_loc_by_hash: Default::default(),
            created_utxos: Default::default(),
            spent_utxos: Default::default(),
            sprout_anchors: MultiSet::new(),
            sprout_anchors_by_height: Default::default(),
            sprout_trees_by_anchor: Default::default(),
            sprout_trees_by_height: Default::default(),
            sapling_anchors: MultiSet::new(),
            sapling_anchors_by_height: Default::default(),
            sapling_trees_by_height: Default::default(),
            sapling_subtrees: Default::default(),
            orchard_anchors: MultiSet::new(),
            orchard_anchors_by_height: Default::default(),
            orchard_trees_by_height: Default::default(),
            orchard_subtrees: Default::default(),
            sprout_nullifiers: Default::default(),
            sapling_nullifiers: Default::default(),
            orchard_nullifiers: Default::default(),
            partial_transparent_transfers: Default::default(),
            partial_cumulative_work: Default::default(),
            history_trees_by_height: Default::default(),
            history_nodes_by_height: Default::default(),
            chain_value_pools: finalized_tip_chain_value_pools,
            block_info_by_height: Default::default(),
        };

        let mut chain = Self {
            network: network.clone(),
            inner,
            last_fork_height: None,
        };

        chain.add_sprout_tree_and_anchor(finalized_tip_height, sprout_note_commitment_tree);
        chain.add_sapling_tree_and_anchor(finalized_tip_height, sapling_note_commitment_tree);
        chain.add_orchard_tree_and_anchor(finalized_tip_height, orchard_note_commitment_tree);
        chain.add_history_tree(finalized_tip_height, history_tree);

        chain
    }

    /// Is the internal state of `self` the same as `other`?
    ///
    /// [`Chain`] has custom [`Eq`] and [`Ord`] implementations based on proof of work,
    /// which are used to select the best chain. So we can't derive [`Eq`] for [`Chain`].
    ///
    /// Unlike the custom trait impls, this method returns `true` if the entire internal state
    /// of two chains is equal.
    ///
    /// If the internal states are different, it returns `false`,
    /// even if the blocks in the two chains are equal.
    #[cfg(any(test, feature = "proptest-impl"))]
    pub fn eq_internal_state(&self, other: &Chain) -> bool {
        self.inner == other.inner
    }

    /// Returns the last fork height if that height is still in the non-finalized state.
    /// Otherwise, if that fork has been finalized, returns `None`.
    #[allow(dead_code)]
    pub fn recent_fork_height(&self) -> Option<Height> {
        self.last_fork_height
            .filter(|last| last >= &self.non_finalized_root_height())
    }

    /// Returns this chain fork's length, if its fork is still in the non-finalized state.
    /// Otherwise, if the fork has been finalized, returns `None`.
    #[allow(dead_code)]
    pub fn recent_fork_length(&self) -> Option<u32> {
        let fork_length = self.non_finalized_tip_height() - self.recent_fork_height()?;

        // If the fork is above the tip, it is invalid, so just return `None`
        // (Ignoring invalid data is ok because this is metrics-only code.)
        fork_length.try_into().ok()
    }

    /// Push a contextually valid non-finalized block into this chain as the new tip.
    ///
    /// If the block is invalid, drops this chain, and returns an error.
    ///
    /// Note: a [`ContextuallyVerifiedBlock`] isn't actually contextually valid until
    /// [`Self::update_chain_tip_with`] returns success.
    #[instrument(level = "debug", skip(self, block), fields(block = %block.block))]
    pub fn push(mut self, block: ContextuallyVerifiedBlock) -> Result<Chain, ValidateContextError> {
        // update cumulative data members
        self.update_chain_tip_with(&block)?;

        tracing::debug!(block = %block.block, "adding block to chain");
        self.blocks.insert(block.height, block);

        Ok(self)
    }

    /// Pops the lowest height block of the non-finalized portion of a chain,
    /// and returns it with its associated treestate.
    #[instrument(level = "debug", skip(self))]
    pub(crate) fn pop_root(&mut self) -> (ContextuallyVerifiedBlock, Treestate) {
        // Obtain the lowest height.
        let block_height = self.non_finalized_root_height();

        // Obtain the treestate associated with the block being finalized.
        let treestate = self
            .treestate(block_height.into())
            .expect("The treestate must be present for the root height.");

        if treestate.note_commitment_trees.sapling_subtree.is_some() {
            self.sapling_subtrees.pop_first();
        }

        if treestate.note_commitment_trees.orchard_subtree.is_some() {
            self.orchard_subtrees.pop_first();
        }

        // Remove the lowest height block from `self.blocks`.
        let block = self
            .blocks
            .remove(&block_height)
            .expect("only called while blocks is populated");

        // Update cumulative data members.
        self.revert_chain_with(&block, RevertPosition::Root);

        (block, treestate)
    }

    /// Returns the block at the provided height and all of its descendant blocks.
    pub fn child_blocks(&self, block_height: &block::Height) -> Vec<ContextuallyVerifiedBlock> {
        self.blocks
            .range(block_height..)
            .map(|(_h, b)| b.clone())
            .collect()
    }

    /// Returns a new chain without the invalidated block or its descendants.
    pub fn invalidate_block(
        &self,
        block_hash: block::Hash,
    ) -> Option<(Self, Vec<ContextuallyVerifiedBlock>)> {
        let block_height = self.height_by_hash(block_hash)?;
        let mut new_chain = self.fork(block_hash)?;
        new_chain.pop_tip();
        new_chain.last_fork_height = self.last_fork_height.min(Some(block_height));
        Some((new_chain, self.child_blocks(&block_height)))
    }

    /// Returns the height of the chain root.
    pub fn non_finalized_root_height(&self) -> block::Height {
        self.blocks
            .keys()
            .next()
            .cloned()
            .expect("only called while blocks is populated")
    }

    /// Fork and return a chain at the block with the given `fork_tip`, if it is part of this
    /// chain. Otherwise, if this chain does not contain `fork_tip`, returns `None`.
    pub fn fork(&self, fork_tip: block::Hash) -> Option<Self> {
        if !self.height_by_hash.contains_key(&fork_tip) {
            return None;
        }

        let mut forked = self.clone();

        // Revert blocks above the fork
        while forked.non_finalized_tip_hash() != fork_tip {
            forked.pop_tip();

            forked.last_fork_height = Some(forked.non_finalized_tip_height());
        }

        Some(forked)
    }

    /// Returns the [`Network`] for this chain.
    pub fn network(&self) -> Network {
        self.network.clone()
    }

    /// Returns the [`ContextuallyVerifiedBlock`] with [`block::Hash`] or
    /// [`Height`], if it exists in this chain.
    pub fn block(&self, hash_or_height: HashOrHeight) -> Option<&ContextuallyVerifiedBlock> {
        let height =
            hash_or_height.height_or_else(|hash| self.height_by_hash.get(&hash).cloned())?;

        self.blocks.get(&height)
    }

    /// Returns the [`Transaction`] with [`transaction::Hash`], if it exists in this chain.
    pub fn transaction(
        &self,
        hash: transaction::Hash,
    ) -> Option<(&Arc<Transaction>, block::Height, DateTime<Utc>)> {
        self.tx_loc_by_hash.get(&hash).map(|tx_loc| {
            (
                &self.blocks[&tx_loc.height].block.transactions[tx_loc.index.as_usize()],
                tx_loc.height,
                self.blocks[&tx_loc.height].block.header.time,
            )
        })
    }

    /// Returns the [`Transaction`] at [`TransactionLocation`], if it exists in this chain.
    #[allow(dead_code)]
    pub fn transaction_by_loc(&self, tx_loc: TransactionLocation) -> Option<&Arc<Transaction>> {
        self.blocks
            .get(&tx_loc.height)?
            .block
            .transactions
            .get(tx_loc.index.as_usize())
    }

    /// Returns the [`transaction::Hash`] for the transaction at [`TransactionLocation`],
    /// if it exists in this chain.
    #[allow(dead_code)]
    pub fn transaction_hash_by_loc(
        &self,
        tx_loc: TransactionLocation,
    ) -> Option<&transaction::Hash> {
        self.blocks
            .get(&tx_loc.height)?
            .transaction_hashes
            .get(tx_loc.index.as_usize())
    }

    /// Returns the [`transaction::Hash`]es in the block with `hash_or_height`,
    /// if it exists in this chain.
    ///
    /// Hashes are returned in block order.
    ///
    /// Returns `None` if the block is not found.
    pub fn transaction_hashes_for_block(
        &self,
        hash_or_height: HashOrHeight,
    ) -> Option<Arc<[transaction::Hash]>> {
        let transaction_hashes = self.block(hash_or_height)?.transaction_hashes.clone();

        Some(transaction_hashes)
    }

    /// Returns the [`block::Hash`] for `height`, if it exists in this chain.
    pub fn hash_by_height(&self, height: Height) -> Option<block::Hash> {
        let hash = self.blocks.get(&height)?.hash;

        Some(hash)
    }

    /// Returns the [`Height`] for `hash`, if it exists in this chain.
    pub fn height_by_hash(&self, hash: block::Hash) -> Option<Height> {
        self.height_by_hash.get(&hash).cloned()
    }

    /// Returns true is the chain contains the given block hash.
    /// Returns false otherwise.
    pub fn contains_block_hash(&self, hash: block::Hash) -> bool {
        self.height_by_hash.contains_key(&hash)
    }

    /// Returns true is the chain contains the given block height.
    /// Returns false otherwise.
    pub fn contains_block_height(&self, height: Height) -> bool {
        self.blocks.contains_key(&height)
    }

    /// Returns true is the chain contains the given block hash or height.
    /// Returns false otherwise.
    #[allow(dead_code)]
    pub fn contains_hash_or_height(&self, hash_or_height: impl Into<HashOrHeight>) -> bool {
        use HashOrHeight::*;

        let hash_or_height = hash_or_height.into();

        match hash_or_height {
            Hash(hash) => self.contains_block_hash(hash),
            Height(height) => self.contains_block_height(height),
        }
    }

    /// Returns the non-finalized tip block height and hash.
    pub fn non_finalized_tip(&self) -> (Height, block::Hash) {
        (
            self.non_finalized_tip_height(),
            self.non_finalized_tip_hash(),
        )
    }

    /// Returns the non-finalized tip block height, hash, and total pool value balances.
    pub fn non_finalized_tip_with_value_balance(
        &self,
    ) -> (Height, block::Hash, ValueBalance<NonNegative>) {
        (
            self.non_finalized_tip_height(),
            self.non_finalized_tip_hash(),
            self.chain_value_pools,
        )
    }

    /// Returns the total pool balance after the block specified by
    /// [`HashOrHeight`], if it exists in the non-finalized [`Chain`].
    pub fn block_info(&self, hash_or_height: HashOrHeight) -> Option<BlockInfo> {
        let height =
            hash_or_height.height_or_else(|hash| self.height_by_hash.get(&hash).cloned())?;

        self.block_info_by_height.get(&height).cloned()
    }

    /// Returns the Sprout note commitment tree of the tip of this [`Chain`],
    /// including all finalized notes, and the non-finalized notes in this chain.
    ///
    /// If the chain is empty, instead returns the tree of the finalized tip,
    /// which was supplied in [`Chain::new()`]
    ///
    /// # Panics
    ///
    /// If this chain has no sprout trees. (This should be impossible.)
    pub fn sprout_note_commitment_tree_for_tip(&self) -> Arc<sprout::tree::NoteCommitmentTree> {
        self.sprout_trees_by_height
            .last_key_value()
            .expect("only called while sprout_trees_by_height is populated")
            .1
            .clone()
    }

    /// Returns the Sprout [`NoteCommitmentTree`](sprout::tree::NoteCommitmentTree) specified by
    /// a [`HashOrHeight`], if it exists in the non-finalized [`Chain`].
    pub fn sprout_tree(
        &self,
        hash_or_height: HashOrHeight,
    ) -> Option<Arc<sprout::tree::NoteCommitmentTree>> {
        let height =
            hash_or_height.height_or_else(|hash| self.height_by_hash.get(&hash).cloned())?;

        self.sprout_trees_by_height
            .range(..=height)
            .next_back()
            .map(|(_height, tree)| tree.clone())
    }

    /// Adds the Sprout `tree` to the tree and anchor indexes at `height`.
    ///
    /// `height` can be either:
    ///
    /// - the height of a new block that has just been added to the chain tip, or
    /// - the finalized tip height—the height of the parent of the first block of a new chain.
    ///
    /// Stores only the first tree in each series of identical trees.
    ///
    /// # Panics
    ///
    /// - If there's a tree already stored at `height`.
    /// - If there's an anchor already stored at `height`.
    fn add_sprout_tree_and_anchor(
        &mut self,
        height: Height,
        tree: Arc<sprout::tree::NoteCommitmentTree>,
    ) {
        // Having updated all the note commitment trees and nullifier sets in
        // this block, the roots of the note commitment trees as of the last
        // transaction are the anchor treestates of this block.
        //
        // Use the previously cached root which was calculated in parallel.
        let anchor = tree.root();
        trace!(?height, ?anchor, "adding sprout tree");

        // Add the new tree only if:
        //
        // - it differs from the previous one, or
        // - there's no previous tree.
        if height.is_min()
            || self
                .sprout_tree(height.previous().expect("prev height").into())
                .is_none_or(|prev_tree| prev_tree != tree)
        {
            assert_eq!(
                self.sprout_trees_by_height.insert(height, tree.clone()),
                None,
                "incorrect overwrite of sprout tree: trees must be reverted then inserted",
            );
        }

        // Store the root.
        assert_eq!(
            self.sprout_anchors_by_height.insert(height, anchor),
            None,
            "incorrect overwrite of sprout anchor: anchors must be reverted then inserted",
        );

        // Multiple inserts are expected here,
        // because the anchors only change if a block has shielded transactions.
        self.sprout_anchors.insert(anchor);
        self.sprout_trees_by_anchor.insert(anchor, tree);
    }

    /// Removes the Sprout tree and anchor indexes at `height`.
    ///
    /// `height` can be at two different [`RevertPosition`]s in the chain:
    ///
    /// - a tip block above a chain fork—only the tree and anchor at that height are removed, or
    /// - a root block—all trees and anchors at and below that height are removed, including
    ///   temporary finalized tip trees.
    ///
    ///   # Panics
    ///
    ///  - If the anchor being removed is not present.
    ///  - If there is no tree at `height`.
    fn remove_sprout_tree_and_anchor(&mut self, position: RevertPosition, height: Height) {
        let (removed_heights, highest_removed_tree) = if position == RevertPosition::Root {
            (
                // Remove all trees and anchors at or below the removed block.
                // This makes sure the temporary trees from finalized tip forks are removed.
                self.sprout_anchors_by_height
                    .keys()
                    .cloned()
                    .filter(|index_height| *index_height <= height)
                    .collect(),
                // Cache the highest (rightmost) tree before its removal.
                self.sprout_tree(height.into()),
            )
        } else {
            // Just remove the reverted tip trees and anchors.
            // We don't need to cache the highest (rightmost) tree.
            (vec![height], None)
        };

        for height in &removed_heights {
            let anchor = self
                .sprout_anchors_by_height
                .remove(height)
                .expect("Sprout anchor must be present if block was added to chain");

            self.sprout_trees_by_height.remove(height);

            trace!(?height, ?position, ?anchor, "removing sprout tree");

            // Multiple removals are expected here,
            // because the anchors only change if a block has shielded transactions.
            assert!(
                self.sprout_anchors.remove(&anchor),
                "Sprout anchor must be present if block was added to chain"
            );
            if !self.sprout_anchors.contains(&anchor) {
                self.sprout_trees_by_anchor.remove(&anchor);
            }
        }

        // # Invariant
        //
        // The height following after the removed heights in a non-empty non-finalized state must
        // always have its tree.
        //
        // The loop above can violate the invariant, and if `position` is [`RevertPosition::Root`],
        // it will always violate the invariant. We restore the invariant by storing the highest
        // (rightmost) removed tree just above `height` if there is no tree at that height.
        if !self.is_empty() && height < self.non_finalized_tip_height() {
            let next_height = height
                .next()
                .expect("Zebra should never reach the max height in normal operation.");

            self.sprout_trees_by_height
                .entry(next_height)
                .or_insert_with(|| {
                    highest_removed_tree.expect("There should be a cached removed tree.")
                });
        }
    }

    /// Returns the Sapling note commitment tree of the tip of this [`Chain`],
    /// including all finalized notes, and the non-finalized notes in this chain.
    ///
    /// If the chain is empty, instead returns the tree of the finalized tip,
    /// which was supplied in [`Chain::new()`]
    ///
    /// # Panics
    ///
    /// If this chain has no sapling trees. (This should be impossible.)
    pub fn sapling_note_commitment_tree_for_tip(&self) -> Arc<sapling::tree::NoteCommitmentTree> {
        self.sapling_trees_by_height
            .last_key_value()
            .expect("only called while sapling_trees_by_height is populated")
            .1
            .clone()
    }

    /// Returns the Sapling [`NoteCommitmentTree`](sapling::tree::NoteCommitmentTree) specified
    /// by a [`HashOrHeight`], if it exists in the non-finalized [`Chain`].
    pub fn sapling_tree(
        &self,
        hash_or_height: HashOrHeight,
    ) -> Option<Arc<sapling::tree::NoteCommitmentTree>> {
        let height =
            hash_or_height.height_or_else(|hash| self.height_by_hash.get(&hash).cloned())?;

        self.sapling_trees_by_height
            .range(..=height)
            .next_back()
            .map(|(_height, tree)| tree.clone())
    }

    /// Returns the Sapling [`NoteCommitmentSubtree`] that was completed at a block with
    /// [`HashOrHeight`], if it exists in the non-finalized [`Chain`].
    ///
    /// # Concurrency
    ///
    /// This method should not be used to get subtrees in concurrent code by height,
    /// because the same heights in different chain forks can have different subtrees.
    pub fn sapling_subtree(
        &self,
        hash_or_height: HashOrHeight,
    ) -> Option<NoteCommitmentSubtree<sapling::tree::Node>> {
        let height =
            hash_or_height.height_or_else(|hash| self.height_by_hash.get(&hash).cloned())?;

        self.sapling_subtrees
            .iter()
            .find(|(_index, subtree)| subtree.end_height == height)
            .map(|(index, subtree)| subtree.with_index(*index))
    }

    /// Returns a list of Sapling [`NoteCommitmentSubtree`]s in the provided range.
    ///
    /// Unlike the finalized state and `ReadRequest::SaplingSubtrees`, the returned subtrees
    /// can start after `start_index`. These subtrees are continuous up to the tip.
    ///
    /// There is no API for retrieving single subtrees by index, because it can accidentally be
    /// used to create an inconsistent list of subtrees after concurrent non-finalized and
    /// finalized updates.
    pub fn sapling_subtrees_in_range(
        &self,
        range: impl std::ops::RangeBounds<NoteCommitmentSubtreeIndex>,
    ) -> BTreeMap<NoteCommitmentSubtreeIndex, NoteCommitmentSubtreeData<sapling::tree::Node>> {
        self.sapling_subtrees
            .range(range)
            .map(|(index, subtree)| (*index, *subtree))
            .collect()
    }

    /// Returns the Sapling [`NoteCommitmentSubtree`] if it was completed at the tip height.
    pub fn sapling_subtree_for_tip(&self) -> Option<NoteCommitmentSubtree<sapling::tree::Node>> {
        if !self.is_empty() {
            let tip = self.non_finalized_tip_height();
            self.sapling_subtree(tip.into())
        } else {
            None
        }
    }

    /// Adds the Sapling `tree` to the tree and anchor indexes at `height`.
    ///
    /// `height` can be either:
    ///
    /// - the height of a new block that has just been added to the chain tip, or
    /// - the finalized tip height—the height of the parent of the first block of a new chain.
    ///
    /// Stores only the first tree in each series of identical trees.
    ///
    /// # Panics
    ///
    /// - If there's a tree already stored at `height`.
    /// - If there's an anchor already stored at `height`.
    fn add_sapling_tree_and_anchor(
        &mut self,
        height: Height,
        tree: Arc<sapling::tree::NoteCommitmentTree>,
    ) {
        let anchor = tree.root();
        trace!(?height, ?anchor, "adding sapling tree");

        // Add the new tree only if:
        //
        // - it differs from the previous one, or
        // - there's no previous tree.
        if height.is_min()
            || self
                .sapling_tree(height.previous().expect("prev height").into())
                .is_none_or(|prev_tree| prev_tree != tree)
        {
            assert_eq!(
                self.sapling_trees_by_height.insert(height, tree),
                None,
                "incorrect overwrite of sapling tree: trees must be reverted then inserted",
            );
        }

        // Store the root.
        assert_eq!(
            self.sapling_anchors_by_height.insert(height, anchor),
            None,
            "incorrect overwrite of sapling anchor: anchors must be reverted then inserted",
        );

        // Multiple inserts are expected here,
        // because the anchors only change if a block has shielded transactions.
        self.sapling_anchors.insert(anchor);
    }

    /// Removes the Sapling tree and anchor indexes at `height`.
    ///
    /// `height` can be at two different [`RevertPosition`]s in the chain:
    ///
    /// - a tip block above a chain fork—only the tree and anchor at that height are removed, or
    /// - a root block—all trees and anchors at and below that height are removed, including
    ///   temporary finalized tip trees.
    ///
    ///   # Panics
    ///
    ///  - If the anchor being removed is not present.
    ///  - If there is no tree at `height`.
    fn remove_sapling_tree_and_anchor(&mut self, position: RevertPosition, height: Height) {
        let (removed_heights, highest_removed_tree) = if position == RevertPosition::Root {
            (
                // Remove all trees and anchors at or below the removed block.
                // This makes sure the temporary trees from finalized tip forks are removed.
                self.sapling_anchors_by_height
                    .keys()
                    .cloned()
                    .filter(|index_height| *index_height <= height)
                    .collect(),
                // Cache the highest (rightmost) tree before its removal.
                self.sapling_tree(height.into()),
            )
        } else {
            // Just remove the reverted tip trees and anchors.
            // We don't need to cache the highest (rightmost) tree.
            (vec![height], None)
        };

        for height in &removed_heights {
            let anchor = self
                .sapling_anchors_by_height
                .remove(height)
                .expect("Sapling anchor must be present if block was added to chain");

            self.sapling_trees_by_height.remove(height);

            trace!(?height, ?position, ?anchor, "removing sapling tree");

            // Multiple removals are expected here,
            // because the anchors only change if a block has shielded transactions.
            assert!(
                self.sapling_anchors.remove(&anchor),
                "Sapling anchor must be present if block was added to chain"
            );
        }

        // # Invariant
        //
        // The height following after the removed heights in a non-empty non-finalized state must
        // always have its tree.
        //
        // The loop above can violate the invariant, and if `position` is [`RevertPosition::Root`],
        // it will always violate the invariant. We restore the invariant by storing the highest
        // (rightmost) removed tree just above `height` if there is no tree at that height.
        if !self.is_empty() && height < self.non_finalized_tip_height() {
            let next_height = height
                .next()
                .expect("Zebra should never reach the max height in normal operation.");

            self.sapling_trees_by_height
                .entry(next_height)
                .or_insert_with(|| {
                    highest_removed_tree.expect("There should be a cached removed tree.")
                });
        }
    }

    /// Returns the Orchard note commitment tree of the tip of this [`Chain`],
    /// including all finalized notes, and the non-finalized notes in this chain.
    ///
    /// If the chain is empty, instead returns the tree of the finalized tip,
    /// which was supplied in [`Chain::new()`]
    ///
    /// # Panics
    ///
    /// If this chain has no orchard trees. (This should be impossible.)
    pub fn orchard_note_commitment_tree_for_tip(&self) -> Arc<orchard::tree::NoteCommitmentTree> {
        self.orchard_trees_by_height
            .last_key_value()
            .expect("only called while orchard_trees_by_height is populated")
            .1
            .clone()
    }

    /// Returns the Orchard
    /// [`NoteCommitmentTree`](orchard::tree::NoteCommitmentTree) specified by a
    /// [`HashOrHeight`], if it exists in the non-finalized [`Chain`].
    pub fn orchard_tree(
        &self,
        hash_or_height: HashOrHeight,
    ) -> Option<Arc<orchard::tree::NoteCommitmentTree>> {
        let height =
            hash_or_height.height_or_else(|hash| self.height_by_hash.get(&hash).cloned())?;

        self.orchard_trees_by_height
            .range(..=height)
            .next_back()
            .map(|(_height, tree)| tree.clone())
    }

    /// Returns the Orchard [`NoteCommitmentSubtree`] that was completed at a block with
    /// [`HashOrHeight`], if it exists in the non-finalized [`Chain`].
    ///
    /// # Concurrency
    ///
    /// This method should not be used to get subtrees in concurrent code by height,
    /// because the same heights in different chain forks can have different subtrees.
    pub fn orchard_subtree(
        &self,
        hash_or_height: HashOrHeight,
    ) -> Option<NoteCommitmentSubtree<orchard::tree::Node>> {
        let height =
            hash_or_height.height_or_else(|hash| self.height_by_hash.get(&hash).cloned())?;

        self.orchard_subtrees
            .iter()
            .find(|(_index, subtree)| subtree.end_height == height)
            .map(|(index, subtree)| subtree.with_index(*index))
    }

    /// Returns a list of Orchard [`NoteCommitmentSubtree`]s in the provided range.
    ///
    /// Unlike the finalized state and `ReadRequest::OrchardSubtrees`, the returned subtrees
    /// can start after `start_index`. These subtrees are continuous up to the tip.
    ///
    /// There is no API for retrieving single subtrees by index, because it can accidentally be
    /// used to create an inconsistent list of subtrees after concurrent non-finalized and
    /// finalized updates.
    pub fn orchard_subtrees_in_range(
        &self,
        range: impl std::ops::RangeBounds<NoteCommitmentSubtreeIndex>,
    ) -> BTreeMap<NoteCommitmentSubtreeIndex, NoteCommitmentSubtreeData<orchard::tree::Node>> {
        self.orchard_subtrees
            .range(range)
            .map(|(index, subtree)| (*index, *subtree))
            .collect()
    }

    /// Returns the Orchard [`NoteCommitmentSubtree`] if it was completed at the tip height.
    pub fn orchard_subtree_for_tip(&self) -> Option<NoteCommitmentSubtree<orchard::tree::Node>> {
        if !self.is_empty() {
            let tip = self.non_finalized_tip_height();
            self.orchard_subtree(tip.into())
        } else {
            None
        }
    }

    /// Adds the Orchard `tree` to the tree and anchor indexes at `height`.
    ///
    /// `height` can be either:
    ///
    /// - the height of a new block that has just been added to the chain tip, or
    /// - the finalized tip height—the height of the parent of the first block of a new chain.
    ///
    /// Stores only the first tree in each series of identical trees.
    ///
    /// # Panics
    ///
    /// - If there's a tree already stored at `height`.
    /// - If there's an anchor already stored at `height`.
    fn add_orchard_tree_and_anchor(
        &mut self,
        height: Height,
        tree: Arc<orchard::tree::NoteCommitmentTree>,
    ) {
        // Having updated all the note commitment trees and nullifier sets in
        // this block, the roots of the note commitment trees as of the last
        // transaction are the anchor treestates of this block.
        //
        // Use the previously cached root which was calculated in parallel.
        let anchor = tree.root();
        trace!(?height, ?anchor, "adding orchard tree");

        // Add the new tree only if:
        //
        // - it differs from the previous one, or
        // - there's no previous tree.
        if height.is_min()
            || self
                .orchard_tree(height.previous().expect("prev height").into())
                .is_none_or(|prev_tree| prev_tree != tree)
        {
            assert_eq!(
                self.orchard_trees_by_height.insert(height, tree),
                None,
                "incorrect overwrite of orchard tree: trees must be reverted then inserted",
            );
        }

        // Store the root.
        assert_eq!(
            self.orchard_anchors_by_height.insert(height, anchor),
            None,
            "incorrect overwrite of orchard anchor: anchors must be reverted then inserted",
        );

        // Multiple inserts are expected here,
        // because the anchors only change if a block has shielded transactions.
        self.orchard_anchors.insert(anchor);
    }

    /// Removes the Orchard tree and anchor indexes at `height`.
    ///
    /// `height` can be at two different [`RevertPosition`]s in the chain:
    ///
    /// - a tip block above a chain fork—only the tree and anchor at that height are removed, or
    /// - a root block—all trees and anchors at and below that height are removed, including
    ///   temporary finalized tip trees.
    ///
    ///   # Panics
    ///
    ///  - If the anchor being removed is not present.
    ///  - If there is no tree at `height`.
    fn remove_orchard_tree_and_anchor(&mut self, position: RevertPosition, height: Height) {
        let (removed_heights, highest_removed_tree) = if position == RevertPosition::Root {
            (
                // Remove all trees and anchors at or below the removed block.
                // This makes sure the temporary trees from finalized tip forks are removed.
                self.orchard_anchors_by_height
                    .keys()
                    .cloned()
                    .filter(|index_height| *index_height <= height)
                    .collect(),
                // Cache the highest (rightmost) tree before its removal.
                self.orchard_tree(height.into()),
            )
        } else {
            // Just remove the reverted tip trees and anchors.
            // We don't need to cache the highest (rightmost) tree.
            (vec![height], None)
        };

        for height in &removed_heights {
            let anchor = self
                .orchard_anchors_by_height
                .remove(height)
                .expect("Orchard anchor must be present if block was added to chain");

            self.orchard_trees_by_height.remove(height);

            trace!(?height, ?position, ?anchor, "removing orchard tree");

            // Multiple removals are expected here,
            // because the anchors only change if a block has shielded transactions.
            assert!(
                self.orchard_anchors.remove(&anchor),
                "Orchard anchor must be present if block was added to chain"
            );
        }

        // # Invariant
        //
        // The height following after the removed heights in a non-empty non-finalized state must
        // always have its tree.
        //
        // The loop above can violate the invariant, and if `position` is [`RevertPosition::Root`],
        // it will always violate the invariant. We restore the invariant by storing the highest
        // (rightmost) removed tree just above `height` if there is no tree at that height.
        if !self.is_empty() && height < self.non_finalized_tip_height() {
            let next_height = height
                .next()
                .expect("Zebra should never reach the max height in normal operation.");

            self.orchard_trees_by_height
                .entry(next_height)
                .or_insert_with(|| {
                    highest_removed_tree.expect("There should be a cached removed tree.")
                });
        }
    }

    /// Returns the History tree of the tip of this [`Chain`],
    /// including all finalized blocks, and the non-finalized blocks below the chain tip.
    ///
    /// If the chain is empty, instead returns the tree of the finalized tip,
    /// which was supplied in [`Chain::new()`]
    ///
    /// # Panics
    ///
    /// If this chain has no history trees. (This should be impossible.)
    pub fn history_block_commitment_tree(&self) -> Arc<HistoryTree> {
        self.history_trees_by_height
            .last_key_value()
            .expect("only called while history_trees_by_height is populated")
            .1
            .clone()
    }

    /// Returns the [`HistoryTree`] specified by a [`HashOrHeight`], if it
    /// exists in the non-finalized [`Chain`].
    pub fn history_tree(&self, hash_or_height: HashOrHeight) -> Option<Arc<HistoryTree>> {
        let height =
            hash_or_height.height_or_else(|hash| self.height_by_hash.get(&hash).cloned())?;

        self.history_trees_by_height.get(&height).cloned()
    }

    /// Add the History `tree` to the history tree index at `height`.
    ///
    /// `height` can be either:
    /// - the height of a new block that has just been added to the chain tip, or
    /// - the finalized tip height: the height of the parent of the first block of a new chain.
    fn add_history_tree(&mut self, height: Height, tree: Arc<HistoryTree>) {
        // The history tree commits to all the blocks before this block.
        //
        // Use the previously cached root which was calculated in parallel.
        trace!(?height, "adding history tree");

        assert_eq!(
            self.history_trees_by_height.insert(height, tree),
            None,
            "incorrect overwrite of history tree: trees must be reverted then inserted",
        );
    }

    /// Remove the History tree index at `height`.
    ///
    /// `height` can be at two different [`RevertPosition`]s in the chain:
    /// - a tip block above a chain fork: only that height is removed, or
    /// - a root block: all trees below that height are removed,
    ///   including temporary finalized tip trees.
    fn remove_history_tree(&mut self, position: RevertPosition, height: Height) {
        trace!(?height, ?position, "removing history tree");

        if position == RevertPosition::Root {
            // Remove all trees at or below the reverted root block.
            // This makes sure the temporary trees from finalized tip forks are removed.
            self.history_trees_by_height
                .retain(|index_height, _tree| *index_height > height);
        } else {
            // Just remove the reverted tip tree.
            self.history_trees_by_height
                .remove(&height)
                .expect("History tree must be present if block was added to chain");
        }
    }

    /// Returns the history tree node at the given index.
    ///
    /// Returns `None` if the chain network upgrade does not match the given one, if there is no history tree associated with the chain, or if the requested index does not exist.
    pub fn history_node(&self, upgrade: NetworkUpgrade, index: u32) -> Option<Entry> {
        if upgrade != NetworkUpgrade::current(&self.network(), self.non_finalized_tip_height()) {
            return None;
        }

        let history_tree = self.history_block_commitment_tree();
        if let Some(inner_tree) = Option::<NonEmptyHistoryTree>::clone(&history_tree) {
            let height = inner_tree.current_height();
            let entries = self.history_nodes_at_chain_tip()?;
            let node_count = history_tree.node_count_at(height)?;
            let start_index = node_count - entries.len() as u32;

            if index >= start_index && index < node_count {
                let entry = entries.get((index - start_index) as usize)?;
                Some(entry.clone())
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Returns the history nodes added by the block at the given hash or height, or `None` if they are not found (because either the block does not exist, or history nodes are not available for this chain)
    fn history_nodes_at_block(&self, hash_or_height: HashOrHeight) -> Option<Vec<Entry>> {
        let height = hash_or_height.height()?;

        self.history_nodes_by_height.get(&height).cloned()
    }

    /// Returns all the history nodes added by this chain, up to the tip block.
    ///
    /// Returns `None` if the chain has no history nodes.
    fn history_nodes_at_chain_tip(&self) -> Option<Vec<Entry>> {
        let mut result = Vec::<Entry>::new();

        if self.history_nodes_by_height.is_empty() {
            return None;
        }

        for (_key, mut entries) in self.history_nodes_by_height.clone() {
            result.append(&mut entries);
        }

        Some(result)
    }

    /// Add history nodes for the given block height to this chain.
    fn add_history_nodes(&mut self, height: Height, nodes: Vec<Entry>) {
        trace!(?height, "adding history nodes");

        assert_eq!(
            self.history_nodes_by_height.insert(height, nodes),
            None,
            "incorrect overwrite of history nodes: nodes should never be overwritten",
        );
    }

    /// Remove the history nodes at `height`.
    ///
    /// `height` can be at two different [`RevertPosition`]s in the chain:
    /// - a tip block above a chain fork: only that height is removed, or
    /// - a root block: all nodes below that height are removed.
    fn remove_history_nodes(&mut self, position: RevertPosition, height: Height) {
        trace!(?height, ?position, "removing history nodes");

        if position == RevertPosition::Root {
            self.history_nodes_by_height
                .retain(|index_height, _| *index_height > height);
        } else {
            self.history_nodes_by_height
                .remove(&height)
                .expect("History nodes must be present if block was added to chain");
        }
    }

    fn treestate(&self, hash_or_height: HashOrHeight) -> Option<Treestate> {
        let sprout_tree = self.sprout_tree(hash_or_height)?;
        let sapling_tree = self.sapling_tree(hash_or_height)?;
        let orchard_tree = self.orchard_tree(hash_or_height)?;
        let history_tree = self.history_tree(hash_or_height)?;
        let sapling_subtree = self.sapling_subtree(hash_or_height);
        let orchard_subtree = self.orchard_subtree(hash_or_height);
        let new_history_nodes = self.history_nodes_at_block(hash_or_height);

        Some(Treestate::new(
            sprout_tree,
            sapling_tree,
            orchard_tree,
            sapling_subtree,
            orchard_subtree,
            history_tree,
            new_history_nodes,
        ))
    }

    /// Returns the block hash of the tip block.
    pub fn non_finalized_tip_hash(&self) -> block::Hash {
        self.blocks
            .values()
            .next_back()
            .expect("only called while blocks is populated")
            .hash
    }

    /// Returns the non-finalized root block hash and height.
    #[allow(dead_code)]
    pub fn non_finalized_root(&self) -> (block::Hash, block::Height) {
        (
            self.non_finalized_root_hash(),
            self.non_finalized_root_height(),
        )
    }

    /// Returns the block hash of the non-finalized root block.
    pub fn non_finalized_root_hash(&self) -> block::Hash {
        self.blocks
            .values()
            .next()
            .expect("only called while blocks is populated")
            .hash
    }

    /// Returns the block hash of the `n`th block from the non-finalized root.
    ///
    /// This is the block at `non_finalized_root_height() + n`.
    #[allow(dead_code)]
    pub fn non_finalized_nth_hash(&self, n: usize) -> Option<block::Hash> {
        self.blocks.values().nth(n).map(|block| block.hash)
    }

    /// Remove the highest height block of the non-finalized portion of a chain.
    fn pop_tip(&mut self) {
        let block_height = self.non_finalized_tip_height();

        let block = self
            .blocks
            .remove(&block_height)
            .expect("only called while blocks is populated");

        assert!(
            !self.blocks.is_empty(),
            "Non-finalized chains must have at least one block to be valid"
        );

        self.revert_chain_with(&block, RevertPosition::Tip);
    }

    /// Return the non-finalized tip height for this chain.
    ///
    /// # Panics
    ///
    /// Panics if called while the chain is empty,
    /// or while the chain is updating its internal state with the first block.
    pub fn non_finalized_tip_height(&self) -> block::Height {
        self.max_block_height()
            .expect("only called while blocks is populated")
    }

    /// Return the non-finalized tip height for this chain,
    /// or `None` if `self.blocks` is empty.
    fn max_block_height(&self) -> Option<block::Height> {
        self.blocks.keys().next_back().cloned()
    }

    /// Return the non-finalized tip block for this chain,
    /// or `None` if `self.blocks` is empty.
    pub fn tip_block(&self) -> Option<&ContextuallyVerifiedBlock> {
        self.blocks.values().next_back()
    }

    /// Returns true if the non-finalized part of this chain is empty.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Returns the non-finalized length of this chain.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Returns the unspent transaction outputs (UTXOs) in this non-finalized chain.
    ///
    /// Callers should also check the finalized state for available UTXOs.
    /// If UTXOs remain unspent when a block is finalized, they are stored in the finalized state,
    /// and removed from the relevant chain(s).
    pub fn unspent_utxos(&self) -> HashMap<transparent::OutPoint, transparent::OrderedUtxo> {
        let mut unspent_utxos = self.created_utxos.clone();
        unspent_utxos.retain(|outpoint, _utxo| !self.spent_utxos.contains_key(outpoint));

        unspent_utxos
    }

    /// Returns the [`transparent::Utxo`] pointed to by the given
    /// [`transparent::OutPoint`] if it was created by this chain.
    ///
    /// UTXOs are returned regardless of whether they have been spent.
    pub fn created_utxo(&self, outpoint: &transparent::OutPoint) -> Option<transparent::Utxo> {
        self.created_utxos
            .get(outpoint)
            .map(|utxo| utxo.utxo.clone())
    }

    /// Returns the [`Hash`](transaction::Hash) of the transaction that spent an output at
    /// the provided [`transparent::OutPoint`] or revealed the provided nullifier, if it exists
    /// and is spent or revealed by this chain.
    #[cfg(feature = "indexer")]
    pub fn spending_transaction_hash(&self, spend: &Spend) -> Option<transaction::Hash> {
        match spend {
            Spend::OutPoint(outpoint) => self.spent_utxos.get(outpoint),
            Spend::Sprout(nullifier) => self.sprout_nullifiers.get(nullifier),
            Spend::Sapling(nullifier) => self.sapling_nullifiers.get(nullifier),
            Spend::Orchard(nullifier) => self.orchard_nullifiers.get(nullifier),
        }
        .cloned()
    }

    // Address index queries

    /// Returns the transparent transfers for `addresses` in this non-finalized chain.
    ///
    /// If none of the addresses have an address index, returns an empty iterator.
    ///
    /// # Correctness
    ///
    /// Callers should apply the returned indexes to the corresponding finalized state indexes.
    ///
    /// The combined result will only be correct if the chains match.
    /// The exact type of match varies by query.
    pub fn partial_transparent_indexes<'a>(
        &'a self,
        addresses: &'a HashSet<transparent::Address>,
    ) -> impl Iterator<Item = &'a TransparentTransfers> {
        addresses
            .iter()
            .flat_map(|address| self.partial_transparent_transfers.get(address))
    }

    /// Returns a tuple of the transparent balance change and the total received funds for
    /// `addresses` in this non-finalized chain.
    ///
    /// If the balance doesn't change for any of the addresses, returns zero.
    ///
    /// # Correctness
    ///
    /// Callers should apply this balance change to the finalized state balance for `addresses`.
    ///
    /// The total balance will only be correct if this partial chain matches the finalized state.
    /// Specifically, the root of this partial chain must be a child block of the finalized tip.
    pub fn partial_transparent_balance_change(
        &self,
        addresses: &HashSet<transparent::Address>,
    ) -> (Amount<NegativeAllowed>, u64) {
        let (balance, received) = self.partial_transparent_indexes(addresses).fold(
            (Ok(Amount::zero()), 0),
            |(balance, received), transfers| {
                let balance = balance + transfers.balance();
                (balance, received + transfers.received())
            },
        );

        (balance.expect("unexpected amount overflow"), received)
    }

    /// Returns the transparent UTXO changes for `addresses` in this non-finalized chain.
    ///
    /// If the UTXOs don't change for any of the addresses, returns empty lists.
    ///
    /// # Correctness
    ///
    /// Callers should apply these non-finalized UTXO changes to the finalized state UTXOs.
    ///
    /// The UTXOs will only be correct if the non-finalized chain matches or overlaps with
    /// the finalized state.
    ///
    /// Specifically, a block in the partial chain must be a child block of the finalized tip.
    /// (But the child block does not have to be the partial chain root.)
    pub fn partial_transparent_utxo_changes(
        &self,
        addresses: &HashSet<transparent::Address>,
    ) -> (
        BTreeMap<OutputLocation, transparent::Output>,
        BTreeSet<OutputLocation>,
    ) {
        let created_utxos = self
            .partial_transparent_indexes(addresses)
            .flat_map(|transfers| transfers.created_utxos())
            .map(|(out_loc, output)| (*out_loc, output.clone()))
            .collect();

        let spent_utxos = self
            .partial_transparent_indexes(addresses)
            .flat_map(|transfers| transfers.spent_utxos())
            .cloned()
            .collect();

        (created_utxos, spent_utxos)
    }

    /// Returns the [`transaction::Hash`]es used by `addresses` to receive or spend funds,
    /// in the non-finalized chain, filtered using the `query_height_range`.
    ///
    /// If none of the addresses receive or spend funds in this partial chain, returns an empty list.
    ///
    /// # Correctness
    ///
    /// Callers should combine these non-finalized transactions with the finalized state transactions.
    ///
    /// The transaction IDs will only be correct if the non-finalized chain matches or overlaps with
    /// the finalized state.
    ///
    /// Specifically, a block in the partial chain must be a child block of the finalized tip.
    /// (But the child block does not have to be the partial chain root.)
    ///
    /// This condition does not apply if there is only one address.
    /// Since address transactions are only appended by blocks,
    /// and the finalized state query reads them in order,
    /// it is impossible to get inconsistent transactions for a single address.
    pub fn partial_transparent_tx_ids(
        &self,
        addresses: &HashSet<transparent::Address>,
        query_height_range: RangeInclusive<Height>,
    ) -> BTreeMap<TransactionLocation, transaction::Hash> {
        self.partial_transparent_indexes(addresses)
            .flat_map(|transfers| {
                transfers.tx_ids(&self.tx_loc_by_hash, query_height_range.clone())
            })
            .collect()
    }

    /// Update the chain tip with the `contextually_valid` block,
    /// running note commitment tree updates in parallel with other updates.
    ///
    /// Used to implement `update_chain_tip_with::<ContextuallyVerifiedBlock>`.
    #[instrument(skip(self, contextually_valid), fields(block = %contextually_valid.block))]
    #[allow(clippy::unwrap_in_result)]
    fn update_chain_tip_with_block_parallel(
        &mut self,
        contextually_valid: &ContextuallyVerifiedBlock,
    ) -> Result<(), ValidateContextError> {
        let height = contextually_valid.height;

        // Prepare data for parallel execution
        let mut nct = NoteCommitmentTrees {
            sprout: self.sprout_note_commitment_tree_for_tip(),
            sapling: self.sapling_note_commitment_tree_for_tip(),
            sapling_subtree: self.sapling_subtree_for_tip(),
            orchard: self.orchard_note_commitment_tree_for_tip(),
            orchard_subtree: self.orchard_subtree_for_tip(),
        };

        let mut tree_result = None;
        let mut partial_result = None;

        // Run 4 tasks in parallel:
        // - sprout, sapling, and orchard tree updates and root calculations
        // - the rest of the Chain updates
        rayon::in_place_scope_fifo(|scope| {
            // Spawns a separate rayon task for each note commitment tree
            tree_result = Some(nct.update_trees_parallel(&contextually_valid.block.clone()));

            scope.spawn_fifo(|_scope| {
                partial_result =
                    Some(self.update_chain_tip_with_block_except_trees(contextually_valid));
            });
        });

        tree_result.expect("scope has already finished")?;
        partial_result.expect("scope has already finished")?;

        // Update the note commitment trees in the chain.
        self.add_sprout_tree_and_anchor(height, nct.sprout);
        self.add_sapling_tree_and_anchor(height, nct.sapling);
        self.add_orchard_tree_and_anchor(height, nct.orchard);

        if let Some(subtree) = nct.sapling_subtree {
            self.sapling_subtrees
                .insert(subtree.index, subtree.into_data());
        }
        if let Some(subtree) = nct.orchard_subtree {
            self.orchard_subtrees
                .insert(subtree.index, subtree.into_data());
        }

        let sapling_root = self.sapling_note_commitment_tree_for_tip().root();
        let orchard_root = self.orchard_note_commitment_tree_for_tip().root();

        // TODO: update the history trees in a rayon thread, if they show up in CPU profiles
        let mut history_tree = self.history_block_commitment_tree();
        let history_tree_mut = Arc::make_mut(&mut history_tree);
        let new_entries = history_tree_mut
            .push(
                &self.network,
                contextually_valid.block.clone(),
                &sapling_root,
                &orchard_root,
            )
            .map_err(Arc::new)?;

        // Add new entries to non finalized state
        // `new_entries` should never be `None` unless history trees are not available yet
        if let Some(new_nodes) = new_entries {
            self.add_history_nodes(height, new_nodes.clone());
        };

        self.add_history_tree(height, history_tree);

        Ok(())
    }

    /// Update the chain tip with the `contextually_valid` block,
    /// except for the note commitment and history tree updates.
    ///
    /// Used to implement `update_chain_tip_with::<ContextuallyVerifiedBlock>`.
    #[instrument(skip(self, contextually_valid), fields(block = %contextually_valid.block))]
    #[allow(clippy::unwrap_in_result)]
    fn update_chain_tip_with_block_except_trees(
        &mut self,
        contextually_valid: &ContextuallyVerifiedBlock,
    ) -> Result<(), ValidateContextError> {
        let (
            block,
            hash,
            height,
            new_outputs,
            spent_outputs,
            transaction_hashes,
            chain_value_pool_change,
        ) = (
            contextually_valid.block.as_ref(),
            contextually_valid.hash,
            contextually_valid.height,
            &contextually_valid.new_outputs,
            &contextually_valid.spent_outputs,
            &contextually_valid.transaction_hashes,
            &contextually_valid.chain_value_pool_change,
        );

        // add hash to height_by_hash
        let prior_height = self.height_by_hash.insert(hash, height);
        assert!(
            prior_height.is_none(),
            "block heights must be unique within a single chain"
        );

        // add work to partial cumulative work
        let block_work = block
            .header
            .difficulty_threshold
            .to_work()
            .expect("work has already been validated");
        self.partial_cumulative_work += block_work;

        // for each transaction in block
        for (transaction_index, (transaction, transaction_hash)) in block
            .transactions
            .iter()
            .zip(transaction_hashes.iter().cloned())
            .enumerate()
        {
            let (
                inputs,
                outputs,
                joinsplit_data,
                sapling_shielded_data_per_spend_anchor,
                sapling_shielded_data_shared_anchor,
                orchard_shielded_data,
            ) = match transaction.deref() {
                V4 {
                    inputs,
                    outputs,
                    joinsplit_data,
                    sapling_shielded_data,
                    ..
                } => (inputs, outputs, joinsplit_data, sapling_shielded_data, &None, &None),
                V5 {
                    inputs,
                    outputs,
                    sapling_shielded_data,
                    orchard_shielded_data,
                    ..
                } => (
                    inputs,
                    outputs,
                    &None,
                    &None,
                    sapling_shielded_data,
                    orchard_shielded_data,
                ),
                #[cfg(feature="tx_v6")]
                V6 {
                    inputs,
                    outputs,
                    sapling_shielded_data,
                    orchard_shielded_data,
                    ..
                } => (
                    inputs,
                    outputs,
                    &None,
                    &None,
                    sapling_shielded_data,
                    orchard_shielded_data,
                ),

                V1 { .. } | V2 { .. } | V3 { .. } => unreachable!(
                    "older transaction versions only exist in finalized blocks, because of the mandatory canopy checkpoint",
                ),
            };

            // add key `transaction.hash` and value `(height, tx_index)` to `tx_loc_by_hash`
            let transaction_location = TransactionLocation::from_usize(height, transaction_index);
            let prior_pair = self
                .tx_loc_by_hash
                .insert(transaction_hash, transaction_location);
            assert_eq!(
                prior_pair, None,
                "transactions must be unique within a single chain"
            );

            // add the utxos this produced
            self.update_chain_tip_with(&(outputs, &transaction_hash, new_outputs))?;
            // delete the utxos this consumed
            self.update_chain_tip_with(&(inputs, &transaction_hash, spent_outputs))?;

            // add the shielded data

            #[cfg(not(feature = "indexer"))]
            let transaction_hash = ();

            self.update_chain_tip_with(&(joinsplit_data, &transaction_hash))?;
            self.update_chain_tip_with(&(
                sapling_shielded_data_per_spend_anchor,
                &transaction_hash,
            ))?;
            self.update_chain_tip_with(&(sapling_shielded_data_shared_anchor, &transaction_hash))?;
            self.update_chain_tip_with(&(orchard_shielded_data, &transaction_hash))?;
        }

        // update the chain value pool balances
        let size = block.zcash_serialized_size();
        self.update_chain_tip_with(&(*chain_value_pool_change, height, size))?;

        Ok(())
    }
}

impl Deref for Chain {
    type Target = ChainInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Chain {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// The revert position being performed on a chain.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum RevertPosition {
    /// The chain root is being reverted via [`Chain::pop_root`], when a block
    /// is finalized.
    Root,

    /// The chain tip is being reverted via [`Chain::pop_tip`],
    /// when a chain is forked.
    Tip,
}

/// Helper trait to organize inverse operations done on the [`Chain`] type.
///
/// Used to overload update and revert methods, based on the type of the argument,
/// and the position of the removed block in the chain.
///
/// This trait was motivated by the length of the `push`, [`Chain::pop_root`],
/// and [`Chain::pop_tip`] functions, and fear that it would be easy to
/// introduce bugs when updating them, unless the code was reorganized to keep
/// related operations adjacent to each other.
pub(crate) trait UpdateWith<T> {
    /// When `T` is added to the chain tip,
    /// update [`Chain`] cumulative data members to add data that are derived from `T`.
    fn update_chain_tip_with(&mut self, _: &T) -> Result<(), ValidateContextError>;

    /// When `T` is removed from `position` in the chain,
    /// revert [`Chain`] cumulative data members to remove data that are derived from `T`.
    fn revert_chain_with(&mut self, _: &T, position: RevertPosition);
}

impl UpdateWith<ContextuallyVerifiedBlock> for Chain {
    #[instrument(skip(self, contextually_valid), fields(block = %contextually_valid.block))]
    #[allow(clippy::unwrap_in_result)]
    fn update_chain_tip_with(
        &mut self,
        contextually_valid: &ContextuallyVerifiedBlock,
    ) -> Result<(), ValidateContextError> {
        self.update_chain_tip_with_block_parallel(contextually_valid)
    }

    #[instrument(skip(self, contextually_valid), fields(block = %contextually_valid.block))]
    fn revert_chain_with(
        &mut self,
        contextually_valid: &ContextuallyVerifiedBlock,
        position: RevertPosition,
    ) {
        let (
            block,
            hash,
            height,
            new_outputs,
            spent_outputs,
            transaction_hashes,
            chain_value_pool_change,
        ) = (
            contextually_valid.block.as_ref(),
            contextually_valid.hash,
            contextually_valid.height,
            &contextually_valid.new_outputs,
            &contextually_valid.spent_outputs,
            &contextually_valid.transaction_hashes,
            &contextually_valid.chain_value_pool_change,
        );

        // remove the blocks hash from `height_by_hash`
        assert!(
            self.height_by_hash.remove(&hash).is_some(),
            "hash must be present if block was added to chain"
        );

        // TODO: move this to a Work or block header UpdateWith.revert...()?
        // remove work from partial_cumulative_work
        let block_work = block
            .header
            .difficulty_threshold
            .to_work()
            .expect("work has already been validated");
        self.partial_cumulative_work -= block_work;

        // for each transaction in block
        for (transaction, transaction_hash) in
            block.transactions.iter().zip(transaction_hashes.iter())
        {
            let (
                inputs,
                outputs,
                joinsplit_data,
                sapling_shielded_data_per_spend_anchor,
                sapling_shielded_data_shared_anchor,
                orchard_shielded_data,
            ) = match transaction.deref() {
                V4 {
                    inputs,
                    outputs,
                    joinsplit_data,
                    sapling_shielded_data,
                    ..
                } => (inputs, outputs, joinsplit_data, sapling_shielded_data, &None, &None),
                V5 {
                    inputs,
                    outputs,
                    sapling_shielded_data,
                    orchard_shielded_data,
                    ..
                } => (
                    inputs,
                    outputs,
                    &None,
                    &None,
                    sapling_shielded_data,
                    orchard_shielded_data,
                ),
                #[cfg(feature="tx_v6")]
                V6 {
                    inputs,
                    outputs,
                    sapling_shielded_data,
                    orchard_shielded_data,
                    ..
                } => (
                    inputs,
                    outputs,
                    &None,
                    &None,
                    sapling_shielded_data,
                    orchard_shielded_data,
                ),

                V1 { .. } | V2 { .. } | V3 { .. } => unreachable!(
                    "older transaction versions only exist in finalized blocks, because of the mandatory canopy checkpoint",
                ),
            };

            // remove the utxos this produced
            self.revert_chain_with(&(outputs, transaction_hash, new_outputs), position);
            // reset the utxos this consumed
            self.revert_chain_with(&(inputs, transaction_hash, spent_outputs), position);

            // TODO: move this to the history tree UpdateWith.revert...()?
            // remove `transaction.hash` from `tx_loc_by_hash`
            assert!(
                self.tx_loc_by_hash.remove(transaction_hash).is_some(),
                "transactions must be present if block was added to chain"
            );

            // remove the shielded data

            #[cfg(not(feature = "indexer"))]
            let transaction_hash = &();

            self.revert_chain_with(&(joinsplit_data, transaction_hash), position);
            self.revert_chain_with(
                &(sapling_shielded_data_per_spend_anchor, transaction_hash),
                position,
            );
            self.revert_chain_with(
                &(sapling_shielded_data_shared_anchor, transaction_hash),
                position,
            );
            self.revert_chain_with(&(orchard_shielded_data, transaction_hash), position);
        }

        // TODO: move these to the shielded UpdateWith.revert...()?
        self.remove_sprout_tree_and_anchor(position, height);
        self.remove_sapling_tree_and_anchor(position, height);
        self.remove_orchard_tree_and_anchor(position, height);

        // TODO: move this to the history tree UpdateWith.revert...()?
        self.remove_history_tree(position, height);

        self.remove_history_nodes(position, height);

        // revert the chain value pool balances, if needed
        // note that size is 0 because it isn't need for reverting
        self.revert_chain_with(&(*chain_value_pool_change, height, 0), position);
    }
}

// Created UTXOs
//
// TODO: replace arguments with a struct
impl
    UpdateWith<(
        // The outputs from a transaction in this block
        &Vec<transparent::Output>,
        // The hash of the transaction that the outputs are from
        &transaction::Hash,
        // The UTXOs for all outputs created by this transaction (or block)
        &HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
    )> for Chain
{
    #[allow(clippy::unwrap_in_result)]
    fn update_chain_tip_with(
        &mut self,
        &(created_outputs, creating_tx_hash, block_created_outputs): &(
            &Vec<transparent::Output>,
            &transaction::Hash,
            &HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
        ),
    ) -> Result<(), ValidateContextError> {
        for output_index in 0..created_outputs.len() {
            let outpoint = transparent::OutPoint {
                hash: *creating_tx_hash,
                index: output_index.try_into().expect("valid indexes fit in u32"),
            };
            let created_utxo = block_created_outputs
                .get(&outpoint)
                .expect("new_outputs contains all created UTXOs");

            // Update the chain's created UTXOs
            let previous_entry = self.created_utxos.insert(outpoint, created_utxo.clone());
            assert_eq!(
                previous_entry, None,
                "unexpected created output: duplicate update or duplicate UTXO",
            );

            // Update the address index with this UTXO
            if let Some(receiving_address) = created_utxo.utxo.output.address(&self.network) {
                let address_transfers = self
                    .partial_transparent_transfers
                    .entry(receiving_address)
                    .or_default();

                address_transfers.update_chain_tip_with(&(&outpoint, created_utxo))?;
            }
        }

        Ok(())
    }

    fn revert_chain_with(
        &mut self,
        &(created_outputs, creating_tx_hash, block_created_outputs): &(
            &Vec<transparent::Output>,
            &transaction::Hash,
            &HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
        ),
        position: RevertPosition,
    ) {
        for output_index in 0..created_outputs.len() {
            let outpoint = transparent::OutPoint {
                hash: *creating_tx_hash,
                index: output_index.try_into().expect("valid indexes fit in u32"),
            };
            let created_utxo = block_created_outputs
                .get(&outpoint)
                .expect("new_outputs contains all created UTXOs");

            // Revert the chain's created UTXOs
            let removed_entry = self.created_utxos.remove(&outpoint);
            assert!(
                removed_entry.is_some(),
                "unexpected revert of created output: duplicate revert or duplicate UTXO",
            );

            // Revert the address index for this UTXO
            if let Some(receiving_address) = created_utxo.utxo.output.address(&self.network) {
                let address_transfers = self
                    .partial_transparent_transfers
                    .get_mut(&receiving_address)
                    .expect("block has previously been applied to the chain");

                address_transfers.revert_chain_with(&(&outpoint, created_utxo), position);

                // Remove this transfer if it is now empty
                if address_transfers.is_empty() {
                    self.partial_transparent_transfers
                        .remove(&receiving_address);
                }
            }
        }
    }
}

// Transparent inputs
//
// TODO: replace arguments with a struct
impl
    UpdateWith<(
        // The inputs from a transaction in this block
        &Vec<transparent::Input>,
        // The hash of the transaction that the inputs are from
        // (not the transaction the spent output was created by)
        &transaction::Hash,
        // The outputs for all inputs spent in this transaction (or block)
        &HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
    )> for Chain
{
    fn update_chain_tip_with(
        &mut self,
        &(spending_inputs, spending_tx_hash, spent_outputs): &(
            &Vec<transparent::Input>,
            &transaction::Hash,
            &HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
        ),
    ) -> Result<(), ValidateContextError> {
        for spending_input in spending_inputs.iter() {
            let spent_outpoint = if let Some(spent_outpoint) = spending_input.outpoint() {
                spent_outpoint
            } else {
                continue;
            };

            #[cfg(feature = "indexer")]
            let insert_value = *spending_tx_hash;
            #[cfg(not(feature = "indexer"))]
            let insert_value = ();

            // Index the spent outpoint in the chain
            let was_spend_newly_inserted = self
                .spent_utxos
                .insert(spent_outpoint, insert_value)
                .is_none();
            assert!(
                was_spend_newly_inserted,
                "unexpected duplicate spent output: should be checked earlier"
            );

            // TODO: fix tests to supply correct spent outputs, then turn this into an expect()
            let spent_output = if let Some(spent_output) = spent_outputs.get(&spent_outpoint) {
                spent_output
            } else if !cfg!(test) {
                panic!("unexpected missing spent output: all spent outputs must be indexed");
            } else {
                continue;
            };

            // Index the spent output for the address
            if let Some(spending_address) = spent_output.utxo.output.address(&self.network) {
                let address_transfers = self
                    .partial_transparent_transfers
                    .entry(spending_address)
                    .or_default();

                address_transfers.update_chain_tip_with(&(
                    spending_input,
                    spending_tx_hash,
                    spent_output,
                ))?;
            }
        }

        Ok(())
    }

    fn revert_chain_with(
        &mut self,
        &(spending_inputs, spending_tx_hash, spent_outputs): &(
            &Vec<transparent::Input>,
            &transaction::Hash,
            &HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
        ),
        position: RevertPosition,
    ) {
        for spending_input in spending_inputs.iter() {
            let spent_outpoint = if let Some(spent_outpoint) = spending_input.outpoint() {
                spent_outpoint
            } else {
                continue;
            };

            // Revert the spent outpoint in the chain
            let was_spent_outpoint_removed = self.spent_utxos.remove(&spent_outpoint).is_some();
            assert!(
                was_spent_outpoint_removed,
                "spent_utxos must be present if block was added to chain"
            );

            // TODO: fix tests to supply correct spent outputs, then turn this into an expect()
            let spent_output = if let Some(spent_output) = spent_outputs.get(&spent_outpoint) {
                spent_output
            } else if !cfg!(test) {
                panic!(
                    "unexpected missing reverted spent output: all spent outputs must be indexed"
                );
            } else {
                continue;
            };

            // Revert the spent output for the address
            if let Some(receiving_address) = spent_output.utxo.output.address(&self.network) {
                let address_transfers = self
                    .partial_transparent_transfers
                    .get_mut(&receiving_address)
                    .expect("block has previously been applied to the chain");

                address_transfers
                    .revert_chain_with(&(spending_input, spending_tx_hash, spent_output), position);

                // Remove this transfer if it is now empty
                if address_transfers.is_empty() {
                    self.partial_transparent_transfers
                        .remove(&receiving_address);
                }
            }
        }
    }
}

impl
    UpdateWith<(
        &Option<transaction::JoinSplitData<Groth16Proof>>,
        &SpendingTransactionId,
    )> for Chain
{
    #[instrument(skip(self, joinsplit_data))]
    fn update_chain_tip_with(
        &mut self,
        &(joinsplit_data, revealing_tx_id): &(
            &Option<transaction::JoinSplitData<Groth16Proof>>,
            &SpendingTransactionId,
        ),
    ) -> Result<(), ValidateContextError> {
        if let Some(joinsplit_data) = joinsplit_data {
            // We do note commitment tree updates in parallel rayon threads.

            check::nullifier::add_to_non_finalized_chain_unique(
                &mut self.sprout_nullifiers,
                joinsplit_data.nullifiers(),
                *revealing_tx_id,
            )?;
        }
        Ok(())
    }

    /// # Panics
    ///
    /// Panics if any nullifier is missing from the chain when we try to remove it.
    ///
    /// See [`check::nullifier::remove_from_non_finalized_chain`] for details.
    #[instrument(skip(self, joinsplit_data))]
    fn revert_chain_with(
        &mut self,
        &(joinsplit_data, _revealing_tx_id): &(
            &Option<transaction::JoinSplitData<Groth16Proof>>,
            &SpendingTransactionId,
        ),
        _position: RevertPosition,
    ) {
        if let Some(joinsplit_data) = joinsplit_data {
            // Note commitments are removed from the Chain during a fork,
            // by removing trees above the fork height from the note commitment index.
            // This happens when reverting the block itself.

            check::nullifier::remove_from_non_finalized_chain(
                &mut self.sprout_nullifiers,
                joinsplit_data.nullifiers(),
            );
        }
    }
}

impl<AnchorV>
    UpdateWith<(
        &Option<sapling::ShieldedData<AnchorV>>,
        &SpendingTransactionId,
    )> for Chain
where
    AnchorV: sapling::AnchorVariant + Clone,
{
    #[instrument(skip(self, sapling_shielded_data))]
    fn update_chain_tip_with(
        &mut self,
        &(sapling_shielded_data, revealing_tx_id): &(
            &Option<sapling::ShieldedData<AnchorV>>,
            &SpendingTransactionId,
        ),
    ) -> Result<(), ValidateContextError> {
        if let Some(sapling_shielded_data) = sapling_shielded_data {
            // We do note commitment tree updates in parallel rayon threads.

            check::nullifier::add_to_non_finalized_chain_unique(
                &mut self.sapling_nullifiers,
                sapling_shielded_data.nullifiers(),
                *revealing_tx_id,
            )?;
        }
        Ok(())
    }

    /// # Panics
    ///
    /// Panics if any nullifier is missing from the chain when we try to remove it.
    ///
    /// See [`check::nullifier::remove_from_non_finalized_chain`] for details.
    #[instrument(skip(self, sapling_shielded_data))]
    fn revert_chain_with(
        &mut self,
        &(sapling_shielded_data, _revealing_tx_id): &(
            &Option<sapling::ShieldedData<AnchorV>>,
            &SpendingTransactionId,
        ),
        _position: RevertPosition,
    ) {
        if let Some(sapling_shielded_data) = sapling_shielded_data {
            // Note commitments are removed from the Chain during a fork,
            // by removing trees above the fork height from the note commitment index.
            // This happens when reverting the block itself.

            check::nullifier::remove_from_non_finalized_chain(
                &mut self.sapling_nullifiers,
                sapling_shielded_data.nullifiers(),
            );
        }
    }
}

impl UpdateWith<(&Option<orchard::ShieldedData>, &SpendingTransactionId)> for Chain {
    #[instrument(skip(self, orchard_shielded_data))]
    fn update_chain_tip_with(
        &mut self,
        &(orchard_shielded_data, revealing_tx_id): &(
            &Option<orchard::ShieldedData>,
            &SpendingTransactionId,
        ),
    ) -> Result<(), ValidateContextError> {
        if let Some(orchard_shielded_data) = orchard_shielded_data {
            // We do note commitment tree updates in parallel rayon threads.

            check::nullifier::add_to_non_finalized_chain_unique(
                &mut self.orchard_nullifiers,
                orchard_shielded_data.nullifiers(),
                *revealing_tx_id,
            )?;
        }
        Ok(())
    }

    /// # Panics
    ///
    /// Panics if any nullifier is missing from the chain when we try to remove it.
    ///
    /// See [`check::nullifier::remove_from_non_finalized_chain`] for details.
    #[instrument(skip(self, orchard_shielded_data))]
    fn revert_chain_with(
        &mut self,
        (orchard_shielded_data, _revealing_tx_id): &(
            &Option<orchard::ShieldedData>,
            &SpendingTransactionId,
        ),
        _position: RevertPosition,
    ) {
        if let Some(orchard_shielded_data) = orchard_shielded_data {
            // Note commitments are removed from the Chain during a fork,
            // by removing trees above the fork height from the note commitment index.
            // This happens when reverting the block itself.

            check::nullifier::remove_from_non_finalized_chain(
                &mut self.orchard_nullifiers,
                orchard_shielded_data.nullifiers(),
            );
        }
    }
}

impl UpdateWith<(ValueBalance<NegativeAllowed>, Height, usize)> for Chain {
    #[allow(clippy::unwrap_in_result)]
    fn update_chain_tip_with(
        &mut self,
        (block_value_pool_change, height, size): &(ValueBalance<NegativeAllowed>, Height, usize),
    ) -> Result<(), ValidateContextError> {
        match self
            .chain_value_pools
            .add_chain_value_pool_change(*block_value_pool_change)
        {
            Ok(chain_value_pools) => {
                self.chain_value_pools = chain_value_pools;
                self.block_info_by_height
                    .insert(*height, BlockInfo::new(chain_value_pools, *size as u32));
            }
            Err(value_balance_error) => Err(ValidateContextError::AddValuePool {
                value_balance_error,
                chain_value_pools: self.chain_value_pools,
                block_value_pool_change: *block_value_pool_change,
                height: Some(*height),
            })?,
        };

        Ok(())
    }

    /// Revert the chain state using a block chain value pool change.
    ///
    /// When forking from the tip, subtract the block's chain value pool change.
    ///
    /// When finalizing the root, leave the chain value pool balances unchanged.
    /// [`ChainInner::chain_value_pools`] tracks the chain value pools for all finalized blocks, and
    /// the non-finalized blocks in this chain. So finalizing the root doesn't change the set of
    /// blocks it tracks.
    ///
    /// # Panics
    ///
    /// Panics if the chain pool value balance is invalid after we subtract the block value pool
    /// change.
    fn revert_chain_with(
        &mut self,
        (block_value_pool_change, height, _size): &(ValueBalance<NegativeAllowed>, Height, usize),
        position: RevertPosition,
    ) {
        use std::ops::Neg;

        if position == RevertPosition::Tip {
            self.chain_value_pools = self
                .chain_value_pools
                .add_chain_value_pool_change(block_value_pool_change.neg())
                .expect("reverting the tip will leave the pools in a previously valid state");
        }
        self.block_info_by_height.remove(height);
    }
}

impl Ord for Chain {
    /// Chain order for the [`NonFinalizedState`][1]'s `chain_set`.
    ///
    /// Chains with higher cumulative Proof of Work are [`Ordering::Greater`],
    /// breaking ties using the tip block hash.
    ///
    /// Despite the consensus rules, Zebra uses the tip block hash as a
    /// tie-breaker. Zebra blocks are downloaded in parallel, so download
    /// timestamps may not be unique. (And Zebra currently doesn't track
    /// download times, because [`Block`](block::Block)s are immutable.)
    ///
    /// This departure from the consensus rules may delay network convergence,
    /// for as long as the greater hash belongs to the later mined block.
    /// But Zebra nodes should converge as soon as the tied work is broken.
    ///
    /// "At a given point in time, each full validator is aware of a set of candidate blocks.
    /// These form a tree rooted at the genesis block, where each node in the tree
    /// refers to its parent via the hashPrevBlock block header field.
    ///
    /// A path from the root toward the leaves of the tree consisting of a sequence
    /// of one or more valid blocks consistent with consensus rules,
    /// is called a valid block chain.
    ///
    /// In order to choose the best valid block chain in its view of the overall block tree,
    /// a node sums the work ... of all blocks in each valid block chain,
    /// and considers the valid block chain with greatest total work to be best.
    ///
    /// To break ties between leaf blocks, a node will prefer the block that it received first.
    ///
    /// The consensus protocol is designed to ensure that for any given block height,
    /// the vast majority of nodes should eventually agree on their best valid block chain
    /// up to that height."
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#blockchain>
    ///
    /// # Correctness
    ///
    /// `Chain::cmp` is used in a `BTreeSet`, so the fields accessed by `cmp` must not have
    /// interior mutability.
    ///
    /// # Panics
    ///
    /// If two chains compare equal.
    ///
    /// This panic enforces the [`NonFinalizedState::chain_set`][2] unique chain invariant.
    ///
    /// If the chain set contains duplicate chains, the non-finalized state might
    /// handle new blocks or block finalization incorrectly.
    ///
    /// [1]: super::NonFinalizedState
    /// [2]: super::NonFinalizedState::chain_set
    fn cmp(&self, other: &Self) -> Ordering {
        if self.partial_cumulative_work != other.partial_cumulative_work {
            self.partial_cumulative_work
                .cmp(&other.partial_cumulative_work)
        } else {
            let self_hash = self
                .blocks
                .values()
                .last()
                .expect("always at least 1 element")
                .hash;

            let other_hash = other
                .blocks
                .values()
                .last()
                .expect("always at least 1 element")
                .hash;

            // This comparison is a tie-breaker within the local node, so it does not need to
            // be consistent with the ordering on `ExpandedDifficulty` and `block::Hash`.
            match self_hash.0.cmp(&other_hash.0) {
                Ordering::Equal => unreachable!("Chain tip block hashes are always unique"),
                ordering => ordering,
            }
        }
    }
}

impl PartialOrd for Chain {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Chain {
    /// Chain equality for [`NonFinalizedState::chain_set`][1], using proof of
    /// work, then the tip block hash as a tie-breaker.
    ///
    /// # Panics
    ///
    /// If two chains compare equal.
    ///
    /// See [`Chain::cmp`] for details.
    ///
    /// [1]: super::NonFinalizedState::chain_set
    fn eq(&self, other: &Self) -> bool {
        self.partial_cmp(other) == Some(Ordering::Equal)
    }
}

impl Eq for Chain {}

#[cfg(test)]
impl Chain {
    /// Inserts the supplied Sapling note commitment subtree into the chain.
    pub(crate) fn insert_sapling_subtree(
        &mut self,
        subtree: NoteCommitmentSubtree<sapling::tree::Node>,
    ) {
        self.inner
            .sapling_subtrees
            .insert(subtree.index, subtree.into_data());
    }

    /// Inserts the supplied Orchard note commitment subtree into the chain.
    pub(crate) fn insert_orchard_subtree(
        &mut self,
        subtree: NoteCommitmentSubtree<orchard::tree::Node>,
    ) {
        self.inner
            .orchard_subtrees
            .insert(subtree.index, subtree.into_data());
    }
}
