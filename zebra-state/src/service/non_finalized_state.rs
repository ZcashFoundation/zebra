//! Non-finalized chain state management as defined by [RFC0005]
//!
//! [RFC0005]: https://zebra.zfnd.org/dev/rfcs/0005-state-updates.html

use std::{
    collections::{BTreeSet, HashMap},
    mem,
    path::PathBuf,
    sync::Arc,
};

use indexmap::IndexMap;
use tokio::sync::watch;
use zebra_chain::{
    block::{self, Block, Hash, Height},
    parameters::Network,
    sprout::{self},
    transparent,
};

use crate::{
    constants::{MAX_INVALIDATED_BLOCKS, MAX_NON_FINALIZED_CHAIN_FORKS},
    error::ReconsiderError,
    request::{ContextuallyVerifiedBlock, FinalizableBlock},
    service::{check, finalized_state::ZebraDb, InvalidateError},
    SemanticallyVerifiedBlock, ValidateContextError, WatchReceiver,
};

mod backup;
mod chain;

#[cfg(test)]
pub(crate) use backup::MIN_DURATION_BETWEEN_BACKUP_UPDATES;

#[cfg(test)]
mod tests;

pub(crate) use chain::{Chain, SpendingTransactionId};

/// The state of the chains in memory, including queued blocks.
///
/// Clones of the non-finalized state contain independent copies of the chains.
/// This is different from `FinalizedState::clone()`,
/// which returns a shared reference to the database.
///
/// Most chain data is clone-on-write using [`Arc`].
pub struct NonFinalizedState {
    // Chain Data
    //
    /// Verified, non-finalized chains, in ascending work order.
    ///
    /// The best chain is [`NonFinalizedState::best_chain()`], or `chain_iter().next()`.
    /// Using `chain_set.last()` or `chain_set.iter().next_back()` is deprecated,
    /// callers should migrate to `chain_iter().next()`.
    chain_set: BTreeSet<Arc<Chain>>,

    /// Blocks that have been invalidated in, and removed from, the non finalized
    /// state.
    invalidated_blocks: IndexMap<Height, Arc<Vec<ContextuallyVerifiedBlock>>>,

    // Configuration
    //
    /// The configured Zcash network.
    pub network: Network,

    // Diagnostics
    //
    /// Configures the non-finalized state to count metrics.
    ///
    /// Used for skipping metrics and progress bars when testing block proposals
    /// with a commit to a cloned non-finalized state.
    //
    // TODO: make this field private and set it via an argument to NonFinalizedState::new()
    should_count_metrics: bool,

    /// Number of chain forks transmitter.
    #[cfg(feature = "progress-bar")]
    chain_count_bar: Option<howudoin::Tx>,

    /// A chain fork length transmitter for each [`Chain`] in [`chain_set`](Self.chain_set).
    ///
    /// Because `chain_set` contains `Arc<Chain>`s, it is difficult to update the metrics state
    /// on each chain. ([`Arc`]s are read-only, and we don't want to clone them just for metrics.)
    #[cfg(feature = "progress-bar")]
    chain_fork_length_bars: Vec<howudoin::Tx>,
}

impl std::fmt::Debug for NonFinalizedState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut f = f.debug_struct("NonFinalizedState");

        f.field("chain_set", &self.chain_set)
            .field("network", &self.network);

        f.field("should_count_metrics", &self.should_count_metrics);

        f.finish()
    }
}

impl Clone for NonFinalizedState {
    fn clone(&self) -> Self {
        Self {
            chain_set: self.chain_set.clone(),
            network: self.network.clone(),
            invalidated_blocks: self.invalidated_blocks.clone(),
            should_count_metrics: self.should_count_metrics,
            // Don't track progress in clones.
            #[cfg(feature = "progress-bar")]
            chain_count_bar: None,
            #[cfg(feature = "progress-bar")]
            chain_fork_length_bars: Vec::new(),
        }
    }
}

impl NonFinalizedState {
    /// Returns a new non-finalized state for `network`.
    pub fn new(network: &Network) -> NonFinalizedState {
        NonFinalizedState {
            chain_set: Default::default(),
            network: network.clone(),
            invalidated_blocks: Default::default(),
            should_count_metrics: true,
            #[cfg(feature = "progress-bar")]
            chain_count_bar: None,
            #[cfg(feature = "progress-bar")]
            chain_fork_length_bars: Vec::new(),
        }
    }

    /// Accepts an optional path to the non-finalized state backup directory and a handle to the database.
    ///
    /// If a backup directory path is provided:
    /// - Creates a new backup directory at the provided path if none exists,
    /// - Restores non-finalized blocks from the backup directory, if any, and
    /// - Spawns a task that updates the non-finalized backup cache with
    ///   the latest non-finalized state sent to the returned watch channel.
    ///
    /// Returns the non-finalized state with a watch channel sender and receiver.
    pub async fn with_backup(
        self,
        backup_dir_path: Option<PathBuf>,
        finalized_state: &ZebraDb,
        should_restore_backup: bool,
    ) -> (
        Self,
        watch::Sender<NonFinalizedState>,
        WatchReceiver<NonFinalizedState>,
    ) {
        let with_watch_channel = |non_finalized_state: NonFinalizedState| {
            let (sender, receiver) = watch::channel(non_finalized_state.clone());
            (non_finalized_state, sender, WatchReceiver::new(receiver))
        };

        let Some(backup_dir_path) = backup_dir_path else {
            return with_watch_channel(self);
        };

        tracing::info!(
            ?backup_dir_path,
            "restoring non-finalized blocks from backup and spawning backup task"
        );

        let non_finalized_state = {
            let backup_dir_path = backup_dir_path.clone();
            let finalized_state = finalized_state.clone();
            tokio::task::spawn_blocking(move || {
                // Create a new backup directory if none exists
                std::fs::create_dir_all(&backup_dir_path)
                    .expect("failed to create non-finalized state backup directory");

                if should_restore_backup {
                    backup::restore_backup(self, &backup_dir_path, &finalized_state)
                } else {
                    self
                }
            })
            .await
            .expect("failed to join blocking task")
        };

        let (non_finalized_state, sender, receiver) = with_watch_channel(non_finalized_state);

        tokio::spawn(backup::run_backup_task(receiver.clone(), backup_dir_path));

        if !non_finalized_state.is_chain_set_empty() {
            let num_blocks_restored = non_finalized_state
                .best_chain()
                .expect("must have best chain if chain set is not empty")
                .len();

            tracing::info!(
                ?num_blocks_restored,
                "restored blocks from non-finalized backup cache"
            );
        }

        (non_finalized_state, sender, receiver)
    }

    /// Is the internal state of `self` the same as `other`?
    ///
    /// [`Chain`] has a custom [`Eq`] implementation based on proof of work,
    /// which is used to select the best chain. So we can't derive [`Eq`] for [`NonFinalizedState`].
    ///
    /// Unlike the custom trait impl, this method returns `true` if the entire internal state
    /// of two non-finalized states is equal.
    ///
    /// If the internal states are different, it returns `false`,
    /// even if the chains and blocks are equal.
    #[cfg(any(test, feature = "proptest-impl"))]
    #[allow(dead_code)]
    pub fn eq_internal_state(&self, other: &NonFinalizedState) -> bool {
        // this method must be updated every time a consensus-critical field is added to NonFinalizedState
        // (diagnostic fields can be ignored)

        self.chain_set.len() == other.chain_set.len()
            && self
                .chain_set
                .iter()
                .zip(other.chain_set.iter())
                .all(|(self_chain, other_chain)| self_chain.eq_internal_state(other_chain))
            && self.network == other.network
    }

    /// Returns an iterator over the non-finalized chains, with the best chain first.
    //
    // TODO: replace chain_set.iter().rev() with this method
    pub fn chain_iter(&self) -> impl Iterator<Item = &Arc<Chain>> {
        self.chain_set.iter().rev()
    }

    /// Insert `chain` into `self.chain_set`, apply `chain_filter` to the chains,
    /// then limit the number of tracked chains.
    fn insert_with<F>(&mut self, chain: Arc<Chain>, chain_filter: F)
    where
        F: FnOnce(&mut BTreeSet<Arc<Chain>>),
    {
        self.chain_set.insert(chain);

        chain_filter(&mut self.chain_set);

        while self.chain_set.len() > MAX_NON_FINALIZED_CHAIN_FORKS {
            // The first chain is the chain with the lowest work.
            self.chain_set.pop_first();
        }

        self.update_metrics_bars();
    }

    /// Insert `chain` into `self.chain_set`, then limit the number of tracked chains.
    fn insert(&mut self, chain: Arc<Chain>) {
        self.insert_with(chain, |_ignored_chain| { /* no filter */ })
    }

    /// Finalize the lowest height block in the non-finalized portion of the best
    /// chain and update all side-chains to match.
    pub fn finalize(&mut self) -> FinalizableBlock {
        // Chain::cmp uses the partial cumulative work, and the hash of the tip block.
        // Neither of these fields has interior mutability.
        // (And when the tip block is dropped for a chain, the chain is also dropped.)
        #[allow(clippy::mutable_key_type)]
        let chains = mem::take(&mut self.chain_set);
        let mut chains = chains.into_iter();

        // extract best chain
        let mut best_chain = chains.next_back().expect("there's at least one chain");

        // clone if required
        let mut_best_chain = Arc::make_mut(&mut best_chain);

        // extract the rest into side_chains so they can be mutated
        let side_chains = chains;

        // Pop the lowest height block from the best chain to be finalized, and
        // also obtain its associated treestate.
        let (best_chain_root, root_treestate) = mut_best_chain.pop_root();

        // add best_chain back to `self.chain_set`
        if !best_chain.is_empty() {
            self.insert(best_chain);
        }

        // for each remaining chain in side_chains
        for mut side_chain in side_chains.rev() {
            if side_chain.non_finalized_root_hash() != best_chain_root.hash {
                // If we popped the root, the chain would be empty or orphaned,
                // so just drop it now.
                drop(side_chain);

                continue;
            }

            // otherwise, the popped root block is the same as the finalizing block

            // clone if required
            let mut_side_chain = Arc::make_mut(&mut side_chain);

            // remove the first block from `chain`
            let (side_chain_root, _treestate) = mut_side_chain.pop_root();
            assert_eq!(side_chain_root.hash, best_chain_root.hash);

            // add the chain back to `self.chain_set`
            self.insert(side_chain);
        }

        // Remove all invalidated_blocks at or below the finalized height
        self.invalidated_blocks
            .retain(|height, _blocks| *height >= best_chain_root.height);

        self.update_metrics_for_chains();

        // Add the treestate to the finalized block.
        FinalizableBlock::new(best_chain_root, root_treestate)
    }

    /// Commit block to the non-finalized state, on top of:
    /// - an existing chain's tip, or
    /// - a newly forked chain.
    #[tracing::instrument(level = "debug", skip(self, finalized_state, prepared))]
    pub fn commit_block(
        &mut self,
        prepared: SemanticallyVerifiedBlock,
        finalized_state: &ZebraDb,
    ) -> Result<(), ValidateContextError> {
        let parent_hash = prepared.block.header.previous_block_hash;
        let (height, hash) = (prepared.height, prepared.hash);

        let parent_chain = self.parent_chain(parent_hash)?;

        // If the block is invalid, return the error,
        // and drop the cloned parent Arc, or newly created chain fork.
        let modified_chain = self.validate_and_commit(parent_chain, prepared, finalized_state)?;

        // If the block is valid:
        // - add the new chain fork or updated chain to the set of recent chains
        // - remove the parent chain, if it was in the chain set
        //   (if it was a newly created fork, it won't be in the chain set)
        self.insert_with(modified_chain, |chain_set| {
            chain_set.retain(|chain| chain.non_finalized_tip_hash() != parent_hash)
        });

        self.update_metrics_for_committed_block(height, hash);

        Ok(())
    }

    /// Invalidate block with hash `block_hash` and all descendants from the non-finalized state. Insert
    /// the new chain into the chain_set and discard the previous.
    #[allow(clippy::unwrap_in_result)]
    pub fn invalidate_block(&mut self, block_hash: Hash) -> Result<block::Hash, InvalidateError> {
        let Some(chain) = self.find_chain(|chain| chain.contains_block_hash(block_hash)) else {
            return Err(InvalidateError::BlockNotFound(block_hash));
        };

        let invalidated_blocks = if chain.non_finalized_root_hash() == block_hash {
            self.chain_set.remove(&chain);
            chain.blocks.values().cloned().collect()
        } else {
            let (new_chain, invalidated_blocks) = chain
                .invalidate_block(block_hash)
                .expect("already checked that chain contains hash");

            // Add the new chain fork or updated chain to the set of recent chains, and
            // remove the chain containing the hash of the block from chain set
            self.insert_with(Arc::new(new_chain.clone()), |chain_set| {
                chain_set.retain(|c| !c.contains_block_hash(block_hash))
            });

            invalidated_blocks
        };

        // TODO: Allow for invalidating multiple block hashes at a given height (#9552).
        self.invalidated_blocks.insert(
            invalidated_blocks
                .first()
                .expect("should not be empty")
                .clone()
                .height,
            Arc::new(invalidated_blocks),
        );

        while self.invalidated_blocks.len() > MAX_INVALIDATED_BLOCKS {
            self.invalidated_blocks.shift_remove_index(0);
        }

        self.update_metrics_for_chains();
        self.update_metrics_bars();

        Ok(block_hash)
    }

    /// Reconsiders a previously invalidated block and its descendants into the non-finalized state
    /// based on a block_hash. Reconsidered blocks are inserted into the previous chain and re-inserted
    /// into the chain_set.
    #[allow(clippy::unwrap_in_result)]
    pub fn reconsider_block(
        &mut self,
        block_hash: block::Hash,
        finalized_state: &ZebraDb,
    ) -> Result<Vec<block::Hash>, ReconsiderError> {
        // Get the invalidated blocks that were invalidated by the given block_hash
        let height = self
            .invalidated_blocks
            .iter()
            .find_map(|(height, blocks)| {
                if blocks.first()?.hash == block_hash {
                    Some(height)
                } else {
                    None
                }
            })
            .ok_or(ReconsiderError::MissingInvalidatedBlock(block_hash))?;

        let invalidated_blocks = Arc::unwrap_or_clone(
            self.invalidated_blocks
                .clone()
                .shift_remove(height)
                .ok_or(ReconsiderError::MissingInvalidatedBlock(block_hash))?,
        );

        let invalidated_block_hashes = invalidated_blocks
            .iter()
            .map(|block| block.hash)
            .collect::<Vec<_>>();

        // Find and fork the parent chain of the invalidated_root. Update the parent chain
        // with the invalidated_descendants
        let invalidated_root = invalidated_blocks
            .first()
            .ok_or(ReconsiderError::InvalidatedBlocksEmpty)?;

        let root_parent_hash = invalidated_root.block.header.previous_block_hash;

        // If the parent is the tip of the finalized_state we create a new chain and insert it
        // into the non finalized state
        let chain_result = if root_parent_hash == finalized_state.finalized_tip_hash() {
            let chain = Chain::new(
                &self.network,
                finalized_state
                    .finalized_tip_height()
                    .ok_or(ReconsiderError::ParentChainNotFound(block_hash))?,
                finalized_state.sprout_tree_for_tip(),
                finalized_state.sapling_tree_for_tip(),
                finalized_state.orchard_tree_for_tip(),
                finalized_state.history_tree(),
                finalized_state.finalized_value_pool(),
            );
            Arc::new(chain)
        } else {
            // The parent is not the finalized_tip and still exist in the NonFinalizedState
            // or else we return an error due to the parent not existing in the NonFinalizedState
            self.parent_chain(root_parent_hash)
                .map_err(|_| ReconsiderError::ParentChainNotFound(block_hash))?
        };

        let mut modified_chain = Arc::unwrap_or_clone(chain_result);
        for block in invalidated_blocks {
            modified_chain = modified_chain
                .push(block)
                .expect("previously invalidated block should be valid for chain");
        }

        let (height, hash) = modified_chain.non_finalized_tip();

        // Only track invalidated_blocks that are not yet finalized. Once blocks are finalized (below the best_chain_root_height)
        // we can discard the block.
        if let Some(best_chain_root_height) = finalized_state.finalized_tip_height() {
            self.invalidated_blocks
                .retain(|height, _blocks| *height >= best_chain_root_height);
        }

        self.insert_with(Arc::new(modified_chain), |chain_set| {
            chain_set.retain(|chain| chain.non_finalized_tip_hash() != root_parent_hash)
        });

        self.update_metrics_for_committed_block(height, hash);

        Ok(invalidated_block_hashes)
    }

    /// Commit block to the non-finalized state as a new chain where its parent
    /// is the finalized tip.
    #[tracing::instrument(level = "debug", skip(self, finalized_state, prepared))]
    #[allow(clippy::unwrap_in_result)]
    pub fn commit_new_chain(
        &mut self,
        prepared: SemanticallyVerifiedBlock,
        finalized_state: &ZebraDb,
    ) -> Result<(), ValidateContextError> {
        let finalized_tip_height = finalized_state.finalized_tip_height();

        // TODO: fix tests that don't initialize the finalized state
        #[cfg(not(test))]
        let finalized_tip_height = finalized_tip_height.expect("finalized state contains blocks");
        #[cfg(test)]
        let finalized_tip_height = finalized_tip_height.unwrap_or(zebra_chain::block::Height(0));

        let chain = Chain::new(
            &self.network,
            finalized_tip_height,
            finalized_state.sprout_tree_for_tip(),
            finalized_state.sapling_tree_for_tip(),
            finalized_state.orchard_tree_for_tip(),
            finalized_state.history_tree(),
            finalized_state.finalized_value_pool(),
        );

        let (height, hash) = (prepared.height, prepared.hash);

        // If the block is invalid, return the error, and drop the newly created chain fork
        let chain = self.validate_and_commit(Arc::new(chain), prepared, finalized_state)?;

        // If the block is valid, add the new chain fork to the set of recent chains.
        self.insert(chain);
        self.update_metrics_for_committed_block(height, hash);

        Ok(())
    }

    /// Contextually validate `prepared` using `finalized_state`.
    /// If validation succeeds, push `prepared` onto `new_chain`.
    ///
    /// `new_chain` should start as a clone of the parent chain fork,
    /// or the finalized tip.
    #[tracing::instrument(level = "debug", skip(self, finalized_state, new_chain))]
    fn validate_and_commit(
        &self,
        new_chain: Arc<Chain>,
        prepared: SemanticallyVerifiedBlock,
        finalized_state: &ZebraDb,
    ) -> Result<Arc<Chain>, ValidateContextError> {
        if self
            .invalidated_blocks
            .values()
            .any(|blocks| blocks.iter().any(|block| block.hash == prepared.hash))
        {
            return Err(ValidateContextError::BlockPreviouslyInvalidated {
                block_hash: prepared.hash,
            });
        }

        // Reads from disk
        //
        // TODO: if these disk reads show up in profiles, run them in parallel, using std::thread::spawn()
        let spent_utxos = check::utxo::transparent_spend(
            &prepared,
            &new_chain.unspent_utxos(),
            &new_chain.spent_utxos,
            finalized_state,
        )?;

        // ORCHARD: various orchard-related validation checks get called here, in validate_and_commit
        // Reads from disk
        check::anchors::block_sapling_orchard_anchors_refer_to_final_treestates(
            finalized_state,
            &new_chain,
            &prepared,
        )?;

        // Reads from disk
        let sprout_final_treestates = check::anchors::block_fetch_sprout_final_treestates(
            finalized_state,
            &new_chain,
            &prepared,
        );

        // Quick check that doesn't read from disk
        let contextual = ContextuallyVerifiedBlock::with_block_and_spent_utxos(
            prepared.clone(),
            spent_utxos.clone(),
        )
        .map_err(|value_balance_error| {
            ValidateContextError::CalculateBlockChainValueChange {
                value_balance_error,
                height: prepared.height,
                block_hash: prepared.hash,
                transaction_count: prepared.block.transactions.len(),
                spent_utxo_count: spent_utxos.len(),
            }
        })?;

        Self::validate_and_update_parallel(new_chain, contextual, sprout_final_treestates)
    }

    /// Validate `contextual` and update `new_chain`, doing CPU-intensive work in parallel batches.
    #[allow(clippy::unwrap_in_result)]
    #[tracing::instrument(skip(new_chain, sprout_final_treestates))]
    fn validate_and_update_parallel(
        new_chain: Arc<Chain>,
        contextual: ContextuallyVerifiedBlock,
        sprout_final_treestates: HashMap<sprout::tree::Root, Arc<sprout::tree::NoteCommitmentTree>>,
    ) -> Result<Arc<Chain>, ValidateContextError> {
        let mut block_commitment_result = None;
        let mut sprout_anchor_result = None;
        let mut chain_push_result = None;

        // Clone function arguments for different threads
        let block = contextual.block.clone();
        let network = new_chain.network();
        let history_tree = new_chain.history_block_commitment_tree();

        let block2 = contextual.block.clone();
        let height = contextual.height;
        let transaction_hashes = contextual.transaction_hashes.clone();

        rayon::in_place_scope_fifo(|scope| {
            scope.spawn_fifo(|_scope| {
                block_commitment_result = Some(check::block_commitment_is_valid_for_chain_history(
                    block,
                    &network,
                    &history_tree,
                ));
            });

            scope.spawn_fifo(|_scope| {
                sprout_anchor_result =
                    Some(check::anchors::block_sprout_anchors_refer_to_treestates(
                        sprout_final_treestates,
                        block2,
                        transaction_hashes,
                        height,
                    ));
            });

            // We're pretty sure the new block is valid,
            // so clone the inner chain if needed, then add the new block.
            //
            // Pushing a block onto a Chain can launch additional parallel batches.
            // TODO: should we pass _scope into Chain::push()?
            scope.spawn_fifo(|_scope| {
                // TODO: Replace with Arc::unwrap_or_clone() when it stabilises:
                // https://github.com/rust-lang/rust/issues/93610
                let new_chain = Arc::try_unwrap(new_chain)
                    .unwrap_or_else(|shared_chain| (*shared_chain).clone());
                chain_push_result = Some(new_chain.push(contextual).map(Arc::new));
            });
        });

        // Don't return the updated Chain unless all the parallel results were Ok
        block_commitment_result.expect("scope has finished")?;
        sprout_anchor_result.expect("scope has finished")?;

        chain_push_result.expect("scope has finished")
    }

    /// Returns the length of the non-finalized portion of the current best chain
    /// or `None` if the best chain has no blocks.
    pub fn best_chain_len(&self) -> Option<u32> {
        // This `as` can't overflow because the number of blocks in the chain is limited to i32::MAX,
        // and the non-finalized chain is further limited by the fork length (slightly over 100 blocks).
        Some(self.best_chain()?.blocks.len() as u32)
    }

    /// Returns the root height of the non-finalized state, if the non-finalized state is not empty.
    pub fn root_height(&self) -> Option<block::Height> {
        self.best_chain()
            .map(|chain| chain.non_finalized_root_height())
    }

    /// Returns `true` if `hash` is contained in the non-finalized portion of any
    /// known chain.
    #[allow(dead_code)]
    pub fn any_chain_contains(&self, hash: &block::Hash) -> bool {
        self.chain_set
            .iter()
            .rev()
            .any(|chain| chain.height_by_hash.contains_key(hash))
    }

    /// Returns the first chain satisfying the given predicate.
    ///
    /// If multiple chains satisfy the predicate, returns the chain with the highest difficulty.
    /// (Using the tip block hash tie-breaker.)
    pub fn find_chain<P>(&self, mut predicate: P) -> Option<Arc<Chain>>
    where
        P: FnMut(&Chain) -> bool,
    {
        // Reverse the iteration order, to find highest difficulty chains first.
        self.chain_set
            .iter()
            .rev()
            .find(|chain| predicate(chain))
            .cloned()
    }

    /// Returns the [`transparent::Utxo`] pointed to by the given
    /// [`transparent::OutPoint`] if it is present in any chain.
    ///
    /// UTXOs are returned regardless of whether they have been spent.
    pub fn any_utxo(&self, outpoint: &transparent::OutPoint) -> Option<transparent::Utxo> {
        self.chain_set
            .iter()
            .rev()
            .find_map(|chain| chain.created_utxo(outpoint))
    }

    /// Returns the `block` with the given hash in any chain.
    #[allow(dead_code)]
    pub fn any_block_by_hash(&self, hash: block::Hash) -> Option<Arc<Block>> {
        // This performs efficiently because the number of chains is limited to 10.
        for chain in self.chain_set.iter().rev() {
            if let Some(prepared) = chain
                .height_by_hash
                .get(&hash)
                .and_then(|height| chain.blocks.get(height))
            {
                return Some(prepared.block.clone());
            }
        }

        None
    }

    /// Returns the previous block hash for the given block hash in any chain.
    #[allow(dead_code)]
    pub fn any_prev_block_hash_for_hash(&self, hash: block::Hash) -> Option<block::Hash> {
        // This performs efficiently because the blocks are in memory.
        self.any_block_by_hash(hash)
            .map(|block| block.header.previous_block_hash)
    }

    /// Returns the hash for a given `block::Height` if it is present in the best chain.
    #[allow(dead_code)]
    pub fn best_hash(&self, height: block::Height) -> Option<block::Hash> {
        self.best_chain()?
            .blocks
            .get(&height)
            .map(|prepared| prepared.hash)
    }

    /// Returns the tip of the best chain.
    #[allow(dead_code)]
    pub fn best_tip(&self) -> Option<(block::Height, block::Hash)> {
        let best_chain = self.best_chain()?;
        let height = best_chain.non_finalized_tip_height();
        let hash = best_chain.non_finalized_tip_hash();

        Some((height, hash))
    }

    /// Returns the block at the tip of the best chain.
    #[allow(dead_code)]
    pub fn best_tip_block(&self) -> Option<&ContextuallyVerifiedBlock> {
        let best_chain = self.best_chain()?;

        best_chain.tip_block()
    }

    /// Returns the height of `hash` in the best chain.
    #[allow(dead_code)]
    pub fn best_height_by_hash(&self, hash: block::Hash) -> Option<block::Height> {
        let best_chain = self.best_chain()?;
        let height = *best_chain.height_by_hash.get(&hash)?;
        Some(height)
    }

    /// Returns the height of `hash` in any chain.
    #[allow(dead_code)]
    pub fn any_height_by_hash(&self, hash: block::Hash) -> Option<block::Height> {
        for chain in self.chain_set.iter().rev() {
            if let Some(height) = chain.height_by_hash.get(&hash) {
                return Some(*height);
            }
        }

        None
    }

    /// Returns `true` if the best chain contains `sprout_nullifier`.
    #[cfg(any(test, feature = "proptest-impl"))]
    #[allow(dead_code)]
    pub fn best_contains_sprout_nullifier(&self, sprout_nullifier: &sprout::Nullifier) -> bool {
        self.best_chain()
            .map(|best_chain| best_chain.sprout_nullifiers.contains_key(sprout_nullifier))
            .unwrap_or(false)
    }

    /// Returns `true` if the best chain contains `sapling_nullifier`.
    #[cfg(any(test, feature = "proptest-impl"))]
    #[allow(dead_code)]
    pub fn best_contains_sapling_nullifier(
        &self,
        sapling_nullifier: &zebra_chain::sapling::Nullifier,
    ) -> bool {
        self.best_chain()
            .map(|best_chain| {
                best_chain
                    .sapling_nullifiers
                    .contains_key(sapling_nullifier)
            })
            .unwrap_or(false)
    }

    /// Returns `true` if the best chain contains `orchard_nullifier`.
    #[cfg(any(test, feature = "proptest-impl"))]
    #[allow(dead_code)]
    pub fn best_contains_orchard_nullifier(
        &self,
        orchard_nullifier: &zebra_chain::orchard::Nullifier,
    ) -> bool {
        self.best_chain()
            .map(|best_chain| {
                best_chain
                    .orchard_nullifiers
                    .contains_key(orchard_nullifier)
            })
            .unwrap_or(false)
    }

    /// Return the non-finalized portion of the current best chain.
    pub fn best_chain(&self) -> Option<&Arc<Chain>> {
        self.chain_iter().next()
    }

    /// Return the number of chains.
    pub fn chain_count(&self) -> usize {
        self.chain_set.len()
    }

    /// Returns true if this [`NonFinalizedState`] contains no chains.
    pub fn is_chain_set_empty(&self) -> bool {
        self.chain_count() == 0
    }

    /// Return the invalidated blocks.
    pub fn invalidated_blocks(&self) -> IndexMap<Height, Arc<Vec<ContextuallyVerifiedBlock>>> {
        self.invalidated_blocks.clone()
    }

    /// Return the chain whose tip block hash is `parent_hash`.
    ///
    /// The chain can be an existing chain in the non-finalized state, or a freshly
    /// created fork.
    fn parent_chain(&self, parent_hash: block::Hash) -> Result<Arc<Chain>, ValidateContextError> {
        match self.find_chain(|chain| chain.non_finalized_tip_hash() == parent_hash) {
            // Clone the existing Arc<Chain> in the non-finalized state
            Some(chain) => Ok(chain.clone()),
            // Create a new fork
            None => {
                // Check the lowest difficulty chains first,
                // because the fork could be closer to their tip.
                let fork_chain = self
                    .chain_set
                    .iter()
                    .rev()
                    .find_map(|chain| chain.fork(parent_hash))
                    .ok_or(ValidateContextError::NotReadyToBeCommitted)?;

                Ok(Arc::new(fork_chain))
            }
        }
    }

    /// Should this `NonFinalizedState` instance track metrics and progress bars?
    fn should_count_metrics(&self) -> bool {
        self.should_count_metrics
    }

    /// Update the metrics after `block` is committed
    fn update_metrics_for_committed_block(&self, height: block::Height, hash: block::Hash) {
        if !self.should_count_metrics() {
            return;
        }

        metrics::counter!("state.memory.committed.block.count").increment(1);
        metrics::gauge!("state.memory.committed.block.height").set(height.0 as f64);

        if self
            .best_chain()
            .expect("metrics are only updated after initialization")
            .non_finalized_tip_hash()
            == hash
        {
            metrics::counter!("state.memory.best.committed.block.count").increment(1);
            metrics::gauge!("state.memory.best.committed.block.height").set(height.0 as f64);
        }

        self.update_metrics_for_chains();
    }

    /// Update the metrics after `self.chain_set` is modified
    fn update_metrics_for_chains(&self) {
        if !self.should_count_metrics() {
            return;
        }

        metrics::gauge!("state.memory.chain.count").set(self.chain_set.len() as f64);
        metrics::gauge!("state.memory.best.chain.length",)
            .set(self.best_chain_len().unwrap_or_default() as f64);
    }

    /// Update the progress bars after any chain is modified.
    /// This includes both chain forks and committed blocks.
    fn update_metrics_bars(&mut self) {
        // TODO: make chain_count_bar interior mutable, move to update_metrics_for_committed_block()

        if !self.should_count_metrics() {
            #[allow(clippy::needless_return)]
            return;
        }

        #[cfg(feature = "progress-bar")]
        {
            use std::cmp::Ordering::*;

            if matches!(howudoin::cancelled(), Some(true)) {
                self.disable_metrics();
                return;
            }

            // Update the chain count bar
            if self.chain_count_bar.is_none() {
                self.chain_count_bar = Some(howudoin::new_root().label("Chain Forks"));
            }

            let chain_count_bar = self
                .chain_count_bar
                .as_ref()
                .expect("just initialized if missing");
            let finalized_tip_height = self
                .best_chain()
                .map(|chain| chain.non_finalized_root_height().0 - 1);

            chain_count_bar.set_pos(u64::try_from(self.chain_count()).expect("fits in u64"));
            // .set_len(u64::try_from(MAX_NON_FINALIZED_CHAIN_FORKS).expect("fits in u64"));

            if let Some(finalized_tip_height) = finalized_tip_height {
                chain_count_bar.desc(format!("Finalized Root {finalized_tip_height}"));
            }

            // Update each chain length bar, creating or deleting bars as needed
            let prev_length_bars = self.chain_fork_length_bars.len();

            match self.chain_count().cmp(&prev_length_bars) {
                Greater => self
                    .chain_fork_length_bars
                    .resize_with(self.chain_count(), || {
                        howudoin::new_with_parent(chain_count_bar.id())
                    }),
                Less => {
                    let redundant_bars = self.chain_fork_length_bars.split_off(self.chain_count());
                    for bar in redundant_bars {
                        bar.close();
                    }
                }
                Equal => {}
            }

            // It doesn't matter what chain the bar was previously used for,
            // because we update everything based on the latest chain in that position.
            for (chain_length_bar, chain) in
                std::iter::zip(self.chain_fork_length_bars.iter(), self.chain_iter())
            {
                let fork_height = chain
                    .last_fork_height
                    .unwrap_or_else(|| chain.non_finalized_tip_height())
                    .0;

                // We need to initialize and set all the values of the bar here, because:
                // - the bar might have been newly created, or
                // - the chain this bar was previously assigned to might have changed position.
                chain_length_bar
                    .label(format!("Fork {fork_height}"))
                    .set_pos(u64::try_from(chain.len()).expect("fits in u64"));
                // TODO: should this be MAX_BLOCK_REORG_HEIGHT?
                // .set_len(u64::from(
                //     zebra_chain::transparent::MIN_TRANSPARENT_COINBASE_MATURITY,
                // ));

                // TODO: store work in the finalized state for each height (#7109),
                //       and show the full chain work here, like `zcashd` (#7110)
                //
                // For now, we don't show any work here, see the deleted code in PR #7087.
                let mut desc = String::new();

                if let Some(recent_fork_height) = chain.recent_fork_height() {
                    let recent_fork_length = chain
                        .recent_fork_length()
                        .expect("just checked recent fork height");

                    let mut plural = "s";
                    if recent_fork_length == 1 {
                        plural = "";
                    }

                    desc.push_str(&format!(
                        " at {recent_fork_height:?} + {recent_fork_length} block{plural}"
                    ));
                }

                chain_length_bar.desc(desc);
            }
        }
    }

    /// Stop tracking metrics for this non-finalized state and all its chains.
    pub fn disable_metrics(&mut self) {
        self.should_count_metrics = false;

        #[cfg(feature = "progress-bar")]
        {
            let count_bar = self.chain_count_bar.take().into_iter();
            let fork_bars = self.chain_fork_length_bars.drain(..);
            count_bar.chain(fork_bars).for_each(howudoin::Tx::close);
        }
    }
}

impl Drop for NonFinalizedState {
    fn drop(&mut self) {
        self.disable_metrics();
    }
}
