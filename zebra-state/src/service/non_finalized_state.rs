//! Non-finalized chain state management as defined by [RFC0005]
//!
//! [RFC0005]: https://zebra.zfnd.org/dev/rfcs/0005-state-updates.html

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    mem,
    path::{Path, PathBuf},
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
    constants::{
        MAX_BLOCK_REORG_HEIGHT, MAX_INVALIDATED_BLOCKS, MAX_NON_FINALIZED_CHAIN_FORKS,
        MAX_NON_FINALIZED_CHAIN_FORKS_TEST_NETWORKS, MAX_NON_FINALIZED_INDEX_ENTRIES,
        MAX_NON_FINALIZED_INDEX_ENTRIES_TEST_NETWORKS, MIN_NON_FINALIZED_BLOCKS_OVER_INDEX_LIMIT,
    },
    error::ReconsiderError,
    request::{ContextuallyVerifiedBlock, FinalizableBlock},
    service::{
        check,
        finalized_state::{calculate_deferred_pool_balance_change, ZebraDb},
        InvalidateError,
    },
    SemanticallyVerifiedBlock, ValidateContextError, WatchReceiver,
};

mod backup;
mod chain;

#[cfg(test)]
pub(crate) use backup::MIN_DURATION_BETWEEN_BACKUP_UPDATES;

#[cfg(test)]
mod tests;

pub(crate) use chain::{Chain, SpendingTransactionId};

/// A chain whose tip is a block that passed contextual validation, before it is added to the
/// chain set.
#[derive(Debug)]
pub(crate) struct ValidatedChain {
    /// The chain that ends at the block.
    chain: Arc<Chain>,

    /// The block's parent, if it is a non-finalized block. A chain in the chain set that ends at
    /// the parent is replaced by `chain`.
    parent_hash: Option<block::Hash>,

    /// The block's height.
    height: block::Height,

    /// The block's hash.
    hash: block::Hash,
}

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

    /// Note commitment trees, and a mining candidate's chain history, computed before the commit
    /// of the block they belong to.
    ///
    /// The block write task sets this for one validation. Clones never carry it.
    pub(crate) precomputed_trees: Option<crate::service::precomputed_trees::PrecomputedTrees>,

    /// A private copy of the best chain with the latest validated block proposal on top.
    ///
    /// Proposal validation runs on a clone of this state, and leaves the chain it built here. A
    /// commit of the solved proposal rebinds that chain's tip to the solved block, instead of
    /// applying the block to its parent chain again. Clones share this channel, and only nodes
    /// that validate proposals keep a chain in it.
    prepared_chain: watch::Sender<Option<Arc<Chain>>>,

    // Configuration
    //
    /// The configured Zcash network.
    pub network: Network,

    /// The maximum [`Self::index_entry_count`] of all chains, if any.
    max_index_entries: Option<usize>,

    /// Blocks that were only in evicted chains, and haven't been taken by
    /// [`Self::take_evicted_block_hashes`] yet.
    evicted_block_hashes: Vec<block::Hash>,

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
            max_index_entries: self.max_index_entries,
            // Only the write task reports evicted blocks.
            evicted_block_hashes: Vec::new(),
            invalidated_blocks: self.invalidated_blocks.clone(),
            precomputed_trees: None,
            prepared_chain: self.prepared_chain.clone(),
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
            max_index_entries: Some(if network.is_a_test_network() {
                MAX_NON_FINALIZED_INDEX_ENTRIES_TEST_NETWORKS
            } else {
                MAX_NON_FINALIZED_INDEX_ENTRIES
            }),
            evicted_block_hashes: Vec::new(),
            invalidated_blocks: Default::default(),
            precomputed_trees: None,
            prepared_chain: watch::Sender::new(None),
            should_count_metrics: true,
            #[cfg(feature = "progress-bar")]
            chain_count_bar: None,
            #[cfg(feature = "progress-bar")]
            chain_fork_length_bars: Vec::new(),
        }
    }

    /// Writes the current non-finalized state to the backup directory at `backup_dir_path`.
    ///
    /// Reads the existing backup directory contents, writes any blocks that are in the
    /// non-finalized state but missing from the backup, and deletes any backup files that
    /// are no longer present in the non-finalized state.
    ///
    /// This method performs blocking I/O and should only be called from a blocking context.
    pub(crate) fn write_to_backup(&self, backup_dir_path: &Path) {
        let backup_blocks: HashMap<block::Hash, PathBuf> =
            backup::list_backup_dir_entries(backup_dir_path).collect();
        backup::update_non_finalized_state_backup(backup_dir_path, self, backup_blocks);
    }

    /// Accepts an optional path to the non-finalized state backup directory and a handle to the database.
    ///
    /// If a backup directory path is provided:
    /// - Creates a new backup directory at the provided path if none exists,
    /// - Restores non-finalized blocks from the backup directory, if any, and
    /// - Unless `skip_backup_task` is true, spawns a task that updates the non-finalized
    ///   backup cache with the latest non-finalized state sent to the returned watch channel.
    ///
    /// Returns the non-finalized state with a watch channel sender and receiver.
    pub async fn with_backup(
        self,
        backup_dir_path: Option<PathBuf>,
        finalized_state: &ZebraDb,
        should_restore_backup: bool,
        skip_backup_task: bool,
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

        if !should_restore_backup {
            tracing::info!(
                ?backup_dir_path,
                spawning_backup_task = !skip_backup_task,
                "not restoring non-finalized blocks from backup, any backed up blocks that are \
                 missing from the non-finalized state will be deleted"
            );
        } else if skip_backup_task {
            tracing::info!(
                ?backup_dir_path,
                "restoring non-finalized blocks from backup (sync write mode, backup task skipped)"
            );
        } else {
            tracing::info!(
                ?backup_dir_path,
                "restoring non-finalized blocks from backup and spawning backup task"
            );
        }

        let non_finalized_state = {
            let backup_dir_path = backup_dir_path.clone();
            let finalized_state = finalized_state.clone();
            tokio::task::spawn_blocking(move || {
                // Create a new backup directory if none exists
                std::fs::create_dir_all(&backup_dir_path)
                    .expect("failed to create non-finalized state backup directory");

                if should_restore_backup {
                    let mut restored =
                        backup::restore_backup(self, &backup_dir_path, &finalized_state);
                    // The state service's sent hashes are built from the restored chains, so
                    // blocks evicted while restoring were never sent. A notice for one of them
                    // would remove a later re-delivery of that block from the sent hashes.
                    restored.take_evicted_block_hashes();
                    restored
                } else {
                    self
                }
            })
            .await
            .expect("failed to join blocking task")
        };

        let (non_finalized_state, sender, receiver) = with_watch_channel(non_finalized_state);

        if !skip_backup_task {
            tokio::spawn(backup::run_backup_task(receiver.clone(), backup_dir_path));
        }

        if !non_finalized_state.is_chain_set_empty() {
            let num_blocks_restored = non_finalized_state
                .best_chain()
                .expect("must have best chain if chain set is not empty")
                .len();

            tracing::info!(
                ?num_blocks_restored,
                "restored blocks from non-finalized backup cache"
            );
        } else if should_restore_backup {
            tracing::info!("no blocks were restored from the non-finalized backup cache");
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
    /// then limit the number of tracked chains and the size of their indexes.
    fn insert_with<F>(&mut self, chain: Arc<Chain>, chain_filter: F)
    where
        F: FnOnce(&mut BTreeSet<Arc<Chain>>),
    {
        self.chain_set.insert(chain.clone());

        chain_filter(&mut self.chain_set);

        let max_forks = if self.network.is_a_test_network() {
            MAX_NON_FINALIZED_CHAIN_FORKS_TEST_NETWORKS
        } else {
            MAX_NON_FINALIZED_CHAIN_FORKS
        };
        loop {
            let index_entries = self.index_entry_count();
            let over_index_limit = self
                .max_index_entries
                .is_some_and(|max_index_entries| index_entries > max_index_entries);
            if self.chain_set.len() <= max_forks && !over_index_limit {
                break;
            }

            // Keep the inserted chain and the best chain, so late forks can grow. Evict the
            // lowest-work chain, and on a work tie the one whose tip was received first.
            let best_chain = self.chain_set.last().cloned();
            let Some(evicted) = self
                .chain_set
                .iter()
                .filter(|c| **c != chain && Some(*c) != best_chain.as_ref())
                .min_by(|a, b| {
                    let received_time = |c: &Chain| c.tip_block().map(|tip| tip.received_time);
                    a.partial_cumulative_work
                        .cmp(&b.partial_cumulative_work)
                        .then_with(|| received_time(a).cmp(&received_time(b)))
                        .then_with(|| a.cmp(b))
                })
                .cloned()
            else {
                // The fork limit allows more than two chains, so only the index limit can
                // leave just the best and inserted chains.
                break;
            };

            self.chain_set.remove(&evicted);
            if self.should_count_metrics() {
                metrics::counter!("state.memory.fork.evicted.count").increment(1);
            }

            // Chains contain all their non-finalized ancestors, so once a block is in another
            // chain, so are all the blocks below it.
            let evicted_blocks = evicted
                .blocks
                .values()
                .rev()
                .map(|block| block.hash)
                .take_while(|hash| !self.any_chain_contains(hash))
                .collect::<Vec<_>>();
            self.evicted_block_hashes.extend(evicted_blocks);

            tracing::warn!(
                tip = ?evicted.non_finalized_tip_hash(),
                height = ?evicted.non_finalized_tip_height(),
                max_forks,
                index_entries,
                max_index_entries = ?self.max_index_entries,
                "the fork limit evicted a chain: if the node stops advancing, restart it"
            );
        }

        self.update_metrics_bars();
    }

    /// Sets the maximum [`Self::index_entry_count`] of all chains.
    #[cfg(test)]
    pub(crate) fn set_max_index_entries(&mut self, max_index_entries: Option<usize>) {
        self.max_index_entries = max_index_entries;
    }

    /// Returns the hashes of blocks that were dropped from the non-finalized state because every
    /// chain containing them was evicted by the fork limits, and forgets them.
    ///
    /// The network can still extend those blocks, so the state service must be able to download
    /// and commit them again (#11133).
    pub(crate) fn take_evicted_block_hashes(&mut self) -> Vec<block::Hash> {
        let mut evicted = mem::take(&mut self.evicted_block_hashes);
        // `finalize()` re-inserts chains one at a time, so an eviction there can record
        // blocks that a chain re-inserted later still holds.
        evicted.retain(|hash| !self.any_chain_contains(hash));
        evicted
    }

    /// Insert `chain` into `self.chain_set`, then limit the number of tracked chains.
    fn insert(&mut self, chain: Arc<Chain>) {
        self.insert_with(chain, |_ignored_chain| { /* no filter */ })
    }

    /// Returns `true` if the best chain's root block should be finalized: when the best chain is
    /// longer than the reorg limit, or when the chains pass the index entry limit and the best
    /// chain is longer than [`MIN_NON_FINALIZED_BLOCKS_OVER_INDEX_LIMIT`].
    pub(crate) fn should_finalize_root(&self) -> bool {
        let Some(best_chain_len) = self.best_chain_len() else {
            return false;
        };

        best_chain_len > MAX_BLOCK_REORG_HEIGHT
            || (best_chain_len > MIN_NON_FINALIZED_BLOCKS_OVER_INDEX_LIMIT
                && self
                    .max_index_entries
                    .is_some_and(|max_index_entries| self.index_entry_count() > max_index_entries))
    }

    /// Finalize the lowest height block in the non-finalized portion of the best
    /// chain and update all side-chains to match.
    pub fn finalize(&mut self) -> FinalizableBlock {
        // Chain::cmp uses the partial cumulative work, the tip's receipt time, and the
        // hash of the tip block. None of these fields has interior mutability.
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

        // Side chains rebuild their maps from the best chain's, so they keep sharing them.
        let shared_best_chain = best_chain.clone();

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
            //
            // Popping the root separately from the best chain stops sharing its map paths.
            mut_side_chain.detach_index_entries_at(best_chain_root.height);
            let (side_chain_root, _treestate) = mut_side_chain.pop_root();
            assert_eq!(side_chain_root.hash, best_chain_root.hash);

            // Rebuilding costs about the entries the chains don't share, so wait until the chain
            // has removed that many separately since its last rebuild.
            if mut_side_chain
                .divergent_index_entries(&shared_best_chain)
                .is_some_and(|divergent| mut_side_chain.index_removals_since_rebuild >= divergent)
            {
                mut_side_chain.share_indexes_with(&shared_best_chain);
            }

            // add the chain back to `self.chain_set`
            if !side_chain.is_empty() {
                self.insert(side_chain);
            }
        }

        // Remove all invalidated_blocks at or below the finalized height
        self.invalidated_blocks
            .retain(|height, _blocks| *height >= best_chain_root.height);

        self.update_metrics_for_chains();

        // Add the treestate to the finalized block.
        FinalizableBlock::new(Arc::unwrap_or_clone(best_chain_root), root_treestate)
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
        let validated = self.validate_block(prepared, finalized_state)?;
        self.insert_validated(validated);

        Ok(())
    }

    /// Contextually validates `prepared` on top of an existing chain's tip or a newly forked
    /// chain, without changing the chain set.
    ///
    /// This doesn't run `check::initial_contextual_validity`, which the block write task runs
    /// first.
    ///
    /// Returns the chain with the block, for [`Self::insert_validated`].
    pub(crate) fn validate_block(
        &self,
        prepared: SemanticallyVerifiedBlock,
        finalized_state: &ZebraDb,
    ) -> Result<ValidatedChain, ValidateContextError> {
        let parent_hash = prepared.block.header.previous_block_hash;
        let (height, hash) = (prepared.height, prepared.hash);

        let parent_chain = self.parent_chain(parent_hash)?;

        // If the block is invalid, return the error,
        // and drop the cloned parent Arc, or newly created chain fork.
        let modified_chain = self.validate_and_commit(parent_chain, prepared, finalized_state)?;

        Ok(ValidatedChain {
            chain: modified_chain,
            parent_hash: Some(parent_hash),
            height,
            hash,
        })
    }

    /// Adds a chain from [`Self::validate_block`] or [`Self::validate_new_chain`] to the chain
    /// set.
    ///
    /// The chain set must not have changed since the chain was validated.
    pub(crate) fn insert_validated(&mut self, validated: ValidatedChain) {
        let ValidatedChain {
            chain,
            parent_hash,
            height,
            hash,
        } = validated;

        match parent_hash {
            // If the block is valid:
            // - add the new chain fork or updated chain to the set of recent chains
            // - remove the parent chain, if it was in the chain set
            //   (if it was a newly created fork, it won't be in the chain set)
            Some(parent_hash) => self.insert_with(chain, |chain_set| {
                chain_set.retain(|chain| chain.non_finalized_tip_hash() != parent_hash)
            }),
            // If the block is valid, add the new chain fork to the set of recent chains.
            None => self.insert(chain),
        }

        self.update_metrics_for_committed_block(height, hash);
    }

    /// Invalidate block with hash `block_hash` and all descendants from the non-finalized state. Insert
    /// the new chain into the chain_set and discard the previous.
    ///
    /// Returns an error without modifying the state if the block is the root of the only chain.
    #[allow(clippy::unwrap_in_result)]
    pub fn invalidate_block(&mut self, block_hash: Hash) -> Result<block::Hash, InvalidateError> {
        let Some(chain) = self.find_chain(|chain| chain.contains_block_hash(block_hash)) else {
            return Err(InvalidateError::BlockNotFound(block_hash));
        };

        let invalidated_blocks = if chain.non_finalized_root_hash() == block_hash {
            if self.chain_set.len() == 1 {
                return Err(InvalidateError::OnlyChainRoot(block_hash));
            }

            self.chain_set.remove(&chain);
            chain
                .blocks
                .values()
                .map(|block| block.as_ref().clone())
                .collect()
        } else {
            let (new_chain, invalidated_blocks) = chain
                .invalidate_block(block_hash)
                .expect("already checked that chain contains hash");

            // Add the new chain fork or updated chain to the set of recent chains, and
            // remove the chain containing the hash of the block from chain set
            self.insert_with(Arc::new(new_chain), |chain_set| {
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
        // Locate the record but keep it live until replay succeeds, so a
        // recoverable error can't lose it; it is `shift_remove`d atomically with
        // the insert below.
        let (height, invalidated_blocks) = self
            .invalidated_blocks
            .iter()
            .find_map(|(height, blocks)| {
                if blocks.first()?.hash == block_hash {
                    Some((*height, (**blocks).clone()))
                } else {
                    None
                }
            })
            .ok_or(ReconsiderError::MissingInvalidatedBlock(block_hash))?;

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
                finalized_state.note_commitment_trees_for_tip(),
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
                .map_err(ReconsiderError::ReplayFailed)?;
        }

        let (tip_height, tip_hash) = modified_chain.non_finalized_tip();

        // All fallible steps have succeeded; remove the invalidation record
        // atomically with installing the restored chain so a failed attempt
        // does not destroy the record.
        self.invalidated_blocks.shift_remove(&height);

        // Only track invalidated_blocks that are not yet finalized. Once blocks are finalized (below the best_chain_root_height)
        // we can discard the block.
        if let Some(best_chain_root_height) = finalized_state.finalized_tip_height() {
            self.invalidated_blocks
                .retain(|height, _blocks| *height >= best_chain_root_height);
        }

        self.insert_with(Arc::new(modified_chain), |chain_set| {
            chain_set.retain(|chain| chain.non_finalized_tip_hash() != root_parent_hash)
        });

        self.update_metrics_for_committed_block(tip_height, tip_hash);

        Ok(invalidated_block_hashes)
    }

    /// Commit block to the non-finalized state as a new chain where its parent
    /// is the finalized tip.
    #[tracing::instrument(level = "debug", skip(self, finalized_state, prepared))]
    pub fn commit_new_chain(
        &mut self,
        prepared: SemanticallyVerifiedBlock,
        finalized_state: &ZebraDb,
    ) -> Result<(), ValidateContextError> {
        let validated = self.validate_new_chain(prepared, finalized_state)?;
        self.insert_validated(validated);

        Ok(())
    }

    /// Contextually validates `prepared` as a new chain where its parent is the finalized tip,
    /// without changing the chain set.
    ///
    /// This doesn't run `check::initial_contextual_validity`, which the block write task runs
    /// first.
    ///
    /// Returns the chain with the block, for [`Self::insert_validated`].
    #[allow(clippy::unwrap_in_result)]
    pub(crate) fn validate_new_chain(
        &self,
        prepared: SemanticallyVerifiedBlock,
        finalized_state: &ZebraDb,
    ) -> Result<ValidatedChain, ValidateContextError> {
        let finalized_tip_height = finalized_state.finalized_tip_height();

        // TODO: fix tests that don't initialize the finalized state
        #[cfg(not(test))]
        let finalized_tip_height = finalized_tip_height.expect("finalized state contains blocks");
        #[cfg(test)]
        let finalized_tip_height = finalized_tip_height.unwrap_or(zebra_chain::block::Height(0));

        let chain = Chain::new(
            &self.network,
            finalized_tip_height,
            finalized_state.note_commitment_trees_for_tip(),
            finalized_state.history_tree(),
            finalized_state.finalized_value_pool(),
        );

        let (height, hash) = (prepared.height, prepared.hash);

        // If the block is invalid, return the error, and drop the newly created chain fork
        let chain = self.validate_and_commit(Arc::new(chain), prepared, finalized_state)?;

        Ok(ValidatedChain {
            chain,
            parent_hash: None,
            height,
            hash,
        })
    }

    /// Commits `contextual`, a block that was validated as a block proposal on its parent, and
    /// then solved, without repeating its contextual validation.
    ///
    /// If the chain that the proposal's validation built is still available, this rebinds its
    /// tip to the solved block. Otherwise, it applies the block to its parent chain.
    ///
    /// Returns the proposal's block if the solved block replaced it, so the caller can drop it
    /// after answering the commit.
    #[allow(clippy::unwrap_in_result)]
    pub(crate) fn commit_prevalidated_contextual(
        &mut self,
        contextual: ContextuallyVerifiedBlock,
        finalized_state: &ZebraDb,
    ) -> Result<Option<Arc<ContextuallyVerifiedBlock>>, ValidateContextError> {
        // Blocks are invalidated by their solved hashes, which proposals don't have.
        if self
            .invalidated_blocks
            .values()
            .any(|blocks| blocks.iter().any(|block| block.hash == contextual.hash))
        {
            return Err(ValidateContextError::BlockPreviouslyInvalidated {
                block_hash: contextual.hash,
            });
        }

        let parent_hash = contextual.block.header.previous_block_hash;
        let (height, hash) = (contextual.height, contextual.hash);

        let mut replaced_proposal = None;
        let chain = if parent_hash == finalized_state.finalized_tip_hash() {
            Chain::new(
                &self.network,
                finalized_state
                    .finalized_tip_height()
                    .expect("a validated proposal has a finalized parent"),
                finalized_state.note_commitment_trees_for_tip(),
                finalized_state.history_tree(),
                finalized_state.finalized_value_pool(),
            )
            .push(contextual)?
        } else {
            let parent_chain = self.parent_chain(parent_hash)?;

            match self.take_prepared_chain_for(&contextual, &parent_chain) {
                Some(mut prepared_chain) => {
                    replaced_proposal = Some(prepared_chain.rebind_tip(contextual)?);
                    prepared_chain
                }
                None => Arc::unwrap_or_clone(parent_chain).push(contextual)?,
            }
        };

        self.insert_with(Arc::new(chain), |chain_set| {
            chain_set.retain(|chain| chain.non_finalized_tip_hash() != parent_hash)
        });
        self.update_metrics_for_committed_block(height, hash);

        Ok(replaced_proposal)
    }

    /// Stores `chain`, which has a validated block proposal at its tip, for the commit of the
    /// solved proposal. Replaces the chain of any earlier proposal.
    pub(crate) fn store_prepared_chain(&self, chain: Arc<Chain>) {
        self.prepared_chain.send_replace(Some(chain));
    }

    /// Drops the prepared chain once the best chain is longer than it.
    ///
    /// A block that solved its proposal now wouldn't extend the best chain, and nothing else
    /// replaces a prepared chain on a node that has stopped validating proposals. The block
    /// write task calls this after its commits. Proposal validation runs on a copy of the
    /// non-finalized state, and doesn't.
    pub(crate) fn drop_passed_prepared_chain(&self) {
        let Some(best_tip_height) = self
            .best_chain()
            .map(|chain| chain.non_finalized_tip_height())
        else {
            return;
        };

        self.prepared_chain.send_if_modified(|prepared| {
            if prepared
                .as_ref()
                .is_some_and(|prepared| prepared.non_finalized_tip_height() < best_tip_height)
            {
                *prepared = None;
            }

            // Nothing waits for changes to the prepared chain.
            false
        });
    }

    /// Returns `true` if there is a prepared chain.
    #[cfg(test)]
    pub(crate) fn has_prepared_chain(&self) -> bool {
        self.prepared_chain.borrow().is_some()
    }

    /// Takes the prepared chain, if it is `parent_chain` with a proposal for `block` on top.
    ///
    /// The returned chain has the same blocks as `parent_chain` below its tip. Another prepared
    /// chain is left for the commit of its own proposal.
    fn take_prepared_chain_for(
        &self,
        block: &ContextuallyVerifiedBlock,
        parent_chain: &Chain,
    ) -> Option<Chain> {
        let mut prepared_chain = None;
        self.prepared_chain.send_if_modified(|prepared| {
            if prepared
                .as_ref()
                .is_some_and(|prepared| is_prepared_chain_for(prepared, block, parent_chain))
            {
                prepared_chain = prepared.take();
            }

            // Nothing waits for changes to the prepared chain.
            false
        });
        let mut prepared_chain = Arc::unwrap_or_clone(prepared_chain?);

        // The proposal was validated on a snapshot of the best chain, which can still have root
        // blocks that the block write task has finalized since then.
        while prepared_chain.non_finalized_root_height() < parent_chain.non_finalized_root_height()
        {
            // Popping the root separately from the chains in the chain set stops sharing its
            // map paths with them, as it does for a side chain when a block is finalized.
            prepared_chain.detach_index_entries_at(prepared_chain.non_finalized_root_height());
            prepared_chain.pop_root();
        }
        // Fork diagnostics aren't part of the chain's blocks.
        prepared_chain.last_fork_height = parent_chain.last_fork_height;

        prepared_chain.can_rebind_tip().then_some(prepared_chain)
    }

    /// Removes and returns the chain whose tip block is `tip`, if there is one.
    pub(crate) fn remove_chain_with_tip(&mut self, tip: block::Hash) -> Option<Arc<Chain>> {
        let chain = self.find_chain(|chain| chain.non_finalized_tip_hash() == tip)?;
        self.chain_set.remove(&chain);

        Some(chain)
    }

    /// Contextually validate `prepared` using `finalized_state`.
    /// If validation succeeds, push `prepared` onto `new_chain`.
    ///
    /// `new_chain` should start as a clone of the parent chain fork,
    /// or the finalized tip.
    #[tracing::instrument(
        level = "debug",
        skip(self, finalized_state, new_chain, prepared),
        fields(height = ?prepared.height, hash = %prepared.hash)
    )]
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
            &new_chain,
            &new_chain.spent_utxos,
            finalized_state,
        )?;

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

        let (height, block_hash) = (prepared.height, prepared.hash);
        let transaction_count = prepared.block.transactions.len();
        let spent_utxo_count = spent_utxos.len();
        let deferred_pool_balance_change = calculate_deferred_pool_balance_change(
            height,
            &self.network,
            new_chain.chain_value_pools,
        )
        .map_err(|subsidy_error| ValidateContextError::InvalidSubsidy {
            subsidy_error: subsidy_error.into(),
            height,
            block_hash,
        })?;

        let (contextual, block_fees) = ContextuallyVerifiedBlock::with_block_spent_utxos_and_fees(
            prepared,
            spent_utxos,
            deferred_pool_balance_change,
            &self.network,
            new_chain.chain_value_pools,
        )
        .map_err(|value_balance_error| {
            ValidateContextError::CalculateBlockChainValueChange {
                value_balance_error,
                height,
                block_hash,
                transaction_count,
                spent_utxo_count,
            }
        })?;

        // The semantic verifier defers parent-dependent payouts to this exact-parent check.
        if zebra_chain::parameters::subsidy::nsm_reissuance_is_active(height, &self.network) {
            check::nsm_subsidy_is_valid(
                &contextual,
                &self.network,
                new_chain.chain_value_pools,
                block_fees.expect("reissuance starts no earlier than NU7 fee accounting"),
            )?;
        }

        Self::validate_and_update_parallel(
            new_chain,
            contextual,
            sprout_final_treestates,
            self.precomputed_trees.as_ref(),
        )
    }

    /// Validate `contextual` and update `new_chain`, doing CPU-intensive work in parallel batches.
    #[allow(clippy::unwrap_in_result)]
    #[tracing::instrument(
        skip(new_chain, contextual, sprout_final_treestates, precomputed_trees),
        fields(height = ?contextual.height, hash = %contextual.hash)
    )]
    fn validate_and_update_parallel(
        new_chain: Arc<Chain>,
        contextual: ContextuallyVerifiedBlock,
        sprout_final_treestates: HashMap<sprout::tree::Root, Arc<sprout::tree::NoteCommitmentTree>>,
        precomputed_trees: Option<&crate::service::precomputed_trees::PrecomputedTrees>,
    ) -> Result<Arc<Chain>, ValidateContextError> {
        let mut block_commitment_result = None;
        let mut sprout_anchor_result = None;
        let mut chain_push_result = None;

        // Clone function arguments for different threads
        let block = contextual.block.clone();
        let network = new_chain.network();
        let history_tree = new_chain.history_block_commitment_tree();
        let commitment_is_checked = precomputed_trees
            .is_some_and(|trees| trees.commitment_is_checked(&block, &history_tree));

        let block2 = contextual.block.clone();
        let height = contextual.height;
        let transaction_hashes = contextual.transaction_hashes.clone();

        rayon::in_place_scope_fifo(|scope| {
            scope.spawn_fifo(|_scope| {
                block_commitment_result = Some(if commitment_is_checked {
                    // A mining candidate checked this block's header commitment against a parent
                    // history tree with the same root.
                    #[cfg(any(test, feature = "proptest-impl"))]
                    crate::service::precomputed_trees::SKIPPED_COMMITMENT_CHECKS
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                    Ok(())
                } else {
                    check::block_commitment_is_valid_for_chain_history(
                        block,
                        &network,
                        &history_tree,
                    )
                });
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
                chain_push_result = Some(
                    new_chain
                        .push_with_trees(contextual, precomputed_trees)
                        .map(Arc::new),
                );
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
        // and the non-finalized chain is further limited by the rollback window
        // (`MAX_BLOCK_REORG_HEIGHT`, currently 1000 blocks).
        Some(self.best_chain()?.blocks.len() as u32)
    }

    /// Returns the root height of the non-finalized state, if the non-finalized state is not empty.
    pub fn root_height(&self) -> Option<block::Height> {
        self.best_chain()
            .map(|chain| chain.non_finalized_root_height())
    }

    /// Returns the index entries in all chains, counting the entries of blocks that chains share
    /// once, and the entries that chains have stopped sharing.
    pub(crate) fn index_entry_count(&self) -> usize {
        let mut seen_blocks = HashSet::new();
        let mut entries = 0;
        for chain in &self.chain_set {
            entries += chain.detached_index_entry_count();
            for block in chain.blocks.values() {
                if seen_blocks.insert(Arc::as_ptr(block)) {
                    entries += chain.block_index_entry_count(block);
                }
            }
        }
        entries
    }

    /// Returns `true` if `hash` is contained in the non-finalized portion of any
    /// known chain.
    pub fn any_chain_contains(&self, hash: &block::Hash) -> bool {
        self.chain_set
            .iter()
            .rev()
            .any(|chain| chain.height_by_hash.contains_key(hash))
    }

    /// Returns the first chain satisfying the given predicate.
    ///
    /// If multiple chains satisfy the predicate, returns the chain with the highest difficulty.
    /// (Using the first-received, then tip block hash tie-breakers.)
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
        // Index entry counts are far below the integers f64 represents exactly.
        metrics::gauge!("state.memory.index.entries").set(self.index_entry_count() as f64);
        metrics::gauge!("state.memory.best.chain.index.entries").set(
            self.best_chain()
                .map_or(0, |chain| chain.index_entry_count()) as f64,
        );

        if let Some(best_chain) = self.best_chain() {
            let value_pools = best_chain.chain_value_pools;
            metrics::gauge!("zcash.pool.value.zatoshis", "name" => "transparent")
                .set(u64::from(value_pools.transparent_amount()) as f64);
            metrics::gauge!("zcash.pool.value.zatoshis", "name" => "sprout")
                .set(u64::from(value_pools.sprout_amount()) as f64);
            metrics::gauge!("zcash.pool.value.zatoshis", "name" => "sapling")
                .set(u64::from(value_pools.sapling_amount()) as f64);
            metrics::gauge!("zcash.pool.value.zatoshis", "name" => "orchard")
                .set(u64::from(value_pools.orchard_amount()) as f64);
            metrics::gauge!("zcash.pool.value.zatoshis", "name" => "ironwood")
                .set(u64::from(value_pools.ironwood_amount()) as f64);

            metrics::gauge!("zcash.pool.notes.created", "name" => "sprout")
                .set(best_chain.sprout_note_commitment_tree_for_tip().count() as f64);
            metrics::gauge!("zcash.pool.notes.created", "name" => "sapling")
                .set(best_chain.sapling_note_commitment_tree_for_tip().count() as f64);
            metrics::gauge!("zcash.pool.notes.created", "name" => "orchard")
                .set(best_chain.orchard_note_commitment_tree_for_tip().count() as f64);
            metrics::gauge!("zcash.pool.notes.created", "name" => "ironwood")
                .set(best_chain.ironwood_note_commitment_tree_for_tip().count() as f64);
        }
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

/// Returns `true` if `prepared` is `parent_chain` with a validated proposal for `block` on top.
///
/// A block's hash commits to its ancestors, so two chains with the same block at a height have
/// the same blocks below it. `prepared` can also have root blocks that `parent_chain` has
/// finalized, but not the other way around, because it was built from an earlier best chain.
fn is_prepared_chain_for(
    prepared: &Chain,
    block: &ContextuallyVerifiedBlock,
    parent_chain: &Chain,
) -> bool {
    if parent_chain.is_empty() {
        return false;
    }

    let Some(proposal) = prepared.tip_block() else {
        return false;
    };
    let parent_height = parent_chain.non_finalized_tip_height();

    same_block_except_proof_of_work(proposal, block)
        && parent_height.next().ok() == Some(prepared.non_finalized_tip_height())
        && prepared.hash_by_height(parent_height) == Some(parent_chain.non_finalized_tip_hash())
        && prepared.non_finalized_root_height() <= parent_chain.non_finalized_root_height()
}

/// Returns `true` if `proposal` and `block` have the same transactions, height, and header apart
/// from the nonce and Equihash solution.
///
/// Both blocks' transactions were validated against their headers: `proposal`'s by the state,
/// and `block`'s by the sender of [`Request::CommitContextuallyVerifiedBlock`]. A validated
/// header's Merkle root and block commitments cover the transactions and their authorizing data,
/// so this means the same block contents.
///
/// [`Request::CommitContextuallyVerifiedBlock`]: crate::Request::CommitContextuallyVerifiedBlock
fn same_block_except_proof_of_work(
    proposal: &ContextuallyVerifiedBlock,
    block: &ContextuallyVerifiedBlock,
) -> bool {
    let (proposal_header, header) = (&proposal.block.header, &block.block.header);

    proposal.height == block.height
        && proposal.transaction_hashes == block.transaction_hashes
        && proposal_header.version == header.version
        && proposal_header.previous_block_hash == header.previous_block_hash
        && proposal_header.merkle_root == header.merkle_root
        && proposal_header.commitment_bytes == header.commitment_bytes
        && proposal_header.time == header.time
        && proposal_header.difficulty_threshold == header.difficulty_threshold
}

impl Drop for NonFinalizedState {
    fn drop(&mut self) {
        self.disable_metrics();
    }
}
