//! Non-finalized chain state management as defined by [RFC0005]
//!
//! [RFC0005]: https://zebra.zfnd.org/dev/rfcs/0005-state-updates.html

use std::{
    collections::{BTreeSet, HashMap},
    mem,
    ops::Bound,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
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
        MAX_INVALIDATED_BLOCKS, MAX_NON_FINALIZED_CHAIN_FORKS,
        MAX_NON_FINALIZED_CHAIN_FORKS_TEST_NETWORKS,
    },
    error::ReconsiderError,
    request::{ContextuallyVerifiedBlock, FinalizableBlock},
    service::{
        check,
        finalized_state::{calculate_deferred_pool_balance_change, ZebraDb},
        InvalidateError,
    },
    MinedBlockNotifications, SemanticallyVerifiedBlock, ValidateContextError, WatchReceiver,
};

mod address_transfers;
mod backup;
mod chain;
mod created_utxos;

#[cfg(test)]
pub(crate) use backup::MIN_DURATION_BETWEEN_BACKUP_UPDATES;

#[cfg(test)]
mod tests;

use address_transfers::AddressTransfers;
pub(crate) use chain::{Chain, SpendingTransactionId};
pub(crate) use created_utxos::CreatedUtxos;

/// Each rejection requires an authenticated block with valid proof of work at its contextual
/// difficulty, unless the network disables proof of work. The hard cap is defense in depth:
/// capacity disables early relay and speculative mining until the covered heights are finalized,
/// rather than forgetting a recorded hash. Later rejections are not recorded, so their replays
/// are verified again.
pub(super) const MAX_EARLY_RELAY_REJECTIONS: usize = 4096;

#[derive(Debug, Default)]
pub(super) struct RejectedBlocks {
    hashes: HashMap<Hash, Height>,
    finalized_height: Option<Height>,
    full_until: Option<Height>,
}

impl RejectedBlocks {
    pub(super) fn contains(&self, hash: Hash) -> bool {
        self.hashes.contains_key(&hash)
    }

    pub(super) fn is_full(&self) -> bool {
        self.full_until.is_some()
    }

    /// Returns whether overflow requires withdrawing every early-relay candidate.
    pub(super) fn reject(&mut self, hash: Hash, height: Height) -> bool {
        if self
            .finalized_height
            .is_some_and(|finalized| height <= finalized)
        {
            return self.full_until.is_some();
        }
        if self.hashes.len() < MAX_EARLY_RELAY_REJECTIONS {
            self.hashes.entry(hash).or_insert(height);
        } else if !self.hashes.contains_key(&hash) {
            self.full_until = Some(self.full_until.map_or(height, |old| old.max(height)));
        }
        if self.hashes.len() == MAX_EARLY_RELAY_REJECTIONS && self.full_until.is_none() {
            self.full_until = self.hashes.values().copied().max();
        }
        self.full_until.is_some()
    }

    fn prune(&mut self, finalized: Height) -> bool {
        let was_full = self.is_full();
        self.hashes.retain(|_, height| *height > finalized);
        self.finalized_height = Some(finalized);
        if self.full_until.is_some_and(|height| height <= finalized) {
            self.full_until = None;
        }
        was_full && !self.is_full()
    }
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

    /// The height of the last block that the finalized state contains, but the best chain still
    /// has, so finalizing it didn't copy a chain shared with a published snapshot.
    ///
    /// A commit extending the best chain pops these blocks from the chain copy it makes anyway.
    /// Every other write pops them first, so chains are compared without finalized blocks.
    retained_finalized_height: Option<Height>,

    /// Blocks that have been invalidated in, and removed from, the non finalized
    /// state.
    invalidated_blocks: IndexMap<Height, Arc<Vec<ContextuallyVerifiedBlock>>>,

    /// Authenticated rejections, shared so concurrent verifiers immediately stop early relay.
    ///
    /// This is the only mutable part of otherwise immutable published chain snapshots. Entries
    /// are add-only between finalizations; only the writer prunes them when finalizing a height.
    /// Never hold this lock across an await, another lock, or I/O. Kept only in memory: after a
    /// restart, a replay is verified once before it is recorded again.
    pub(super) rejected_blocks: Arc<Mutex<RejectedBlocks>>,
    /// The chain that the last commit or finalization replaced, kept as a spare.
    ///
    /// Published chains are shared with read-state snapshots, so extending or finalizing one
    /// needs a private copy, and cloning a chain's indexes costs time proportional to the whole
    /// chain. Once readers release the replaced chain, replaying the blocks it misses turns it
    /// into a private copy of the current chain at a cost proportional to those blocks, so each
    /// commit swaps the two copies instead of cloning. Snapshots never carry it.
    spare: Option<Arc<Chain>>,

    /// A private copy of the best chain with the latest validated block proposal on top, shared
    /// by the proposal checks on snapshots and the writer.
    ///
    /// Proposal validation pushes each proposal onto this copy and leaves it there. A commit of
    /// the solved proposal then rebinds that tip to the solved block instead of pushing the
    /// block again, and retires the parent chain here for the next proposal. Any other commit or
    /// proposal replays the copy to the chain it needs first. Only nodes that validate proposals
    /// allocate it.
    prepared: Arc<Mutex<Option<Arc<Chain>>>>,

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
            retained_finalized_height: self.retained_finalized_height,
            network: self.network.clone(),
            invalidated_blocks: self.invalidated_blocks.clone(),
            rejected_blocks: self.rejected_blocks.clone(),
            // The spare is private to the writer: a clone would share it with readers.
            spare: None,
            prepared: self.prepared.clone(),
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
            retained_finalized_height: None,
            network: network.clone(),
            invalidated_blocks: Default::default(),
            rejected_blocks: Default::default(),
            spare: None,
            prepared: Default::default(),
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
                    backup::restore_backup(self, &backup_dir_path, &finalized_state)
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
            && self.retained_finalized_height == other.retained_finalized_height
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
        self.chain_set.insert(chain.clone());

        chain_filter(&mut self.chain_set);

        let max_forks = if self.network.is_a_test_network() {
            MAX_NON_FINALIZED_CHAIN_FORKS_TEST_NETWORKS
        } else {
            MAX_NON_FINALIZED_CHAIN_FORKS
        };
        while self.chain_set.len() > max_forks {
            // Keep the inserted chain and the best chain, so late forks can grow. Evict the
            // lowest-work chain, and on a work tie the one whose tip was received first.
            let best_chain = self.chain_set.last().cloned();
            let evicted = self
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
                .expect("the fork limit allows more than two chains")
                .clone();

            self.chain_set.remove(&evicted);
            tracing::warn!(
                tip = ?evicted.non_finalized_tip_hash(),
                height = ?evicted.non_finalized_tip_height(),
                max_forks,
                "the fork limit evicted a chain: if the node stops advancing, restart it"
            );
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
        let root_height = self.root_height().expect("there's at least one chain");
        let finalizable = self.finalize_keeping_best_chain(root_height);
        self.pop_finalized_roots();

        finalizable
    }

    /// Removes chains that conflict with the best chain's block being finalized at `height`.
    ///
    /// Run before publishing the next snapshot, so retention cannot leave discarded forks visible.
    /// Keep the returned chains until after acknowledgement to defer their index cleanup.
    pub(crate) fn prune_for_finalization(&mut self, height: Height) -> Vec<Arc<Chain>> {
        let hash = self
            .best_chain()
            .and_then(|chain| chain.hash_by_height(height))
            .expect("the best chain contains the block being finalized");
        let mut pruned = Vec::new();
        self.chain_set.retain(|chain| {
            if chain.contains_block_hash(hash) {
                true
            } else {
                pruned.push(chain.clone());
                false
            }
        });
        pruned
    }

    /// Reconciles follower chains with the finalized database.
    ///
    /// Drops chains that neither contain the finalized tip nor start directly above it, regardless
    /// of their work. The best chain keeps matching overlap until the next write, just like
    /// [`Self::finalize_keeping_best_chain`], avoiding a copy of the published chain.
    /// Returns whether the in-memory state changed and needs to be published.
    pub fn prune_finalized(&mut self, finalized_state: &ZebraDb) -> bool {
        let Some((height, hash)) = finalized_state.tip() else {
            return false;
        };
        let chain_count = self.chain_set.len();
        self.chain_set.retain(|chain| {
            let root = chain
                .blocks
                .first_key_value()
                .expect("stored chains are non-empty")
                .1;
            chain.non_finalized_tip_height() > height
                && (chain.hash_by_height(height) == Some(hash)
                    || (height.next().ok() == Some(root.height)
                        && root.block.header.previous_block_hash == hash))
        });
        let retained_height = self
            .root_height()
            .filter(|root| *root <= height)
            .map(|_| height);
        let mut changed = self.chain_set.len() != chain_count
            || self.retained_finalized_height != retained_height;
        self.retained_finalized_height = retained_height;
        if self.chain_set.len() > 1 {
            let best_chain = self
                .chain_set
                .pop_last()
                .expect("there are multiple chains");
            for mut chain in mem::take(&mut self.chain_set) {
                if chain.non_finalized_root_height() <= height {
                    let chain = Arc::make_mut(&mut chain);
                    while chain.non_finalized_root_height() <= height {
                        chain.pop_root();
                    }
                    changed = true;
                }
                self.chain_set.insert(chain);
            }
            self.chain_set.insert(best_chain);
        }
        if changed {
            self.update_metrics_for_chains();
            self.update_metrics_bars();
        }
        changed
    }

    /// Returns the best chain's block at `height` for the finalized state, and removes it from
    /// side chains like [`Self::finalize`].
    ///
    /// The best chain keeps the block, so a published snapshot sharing that chain isn't copied.
    /// The finalized state must contain the block before the next write, which removes it: a
    /// commit extending the best chain pops it from the chain copy that commit makes anyway.
    pub(crate) fn finalize_keeping_best_chain(&mut self, height: Height) -> FinalizableBlock {
        // Chain::cmp uses partial cumulative work, tip receipt time, and tip hash.
        // None of these fields has interior mutability.
        // (And when the tip block is dropped for a chain, the chain is also dropped.)
        #[allow(clippy::mutable_key_type)]
        let mut side_chains = mem::take(&mut self.chain_set);
        let best_chain = side_chains.pop_last().expect("there's at least one chain");
        let best_chain_block = best_chain
            .blocks
            .get(&height)
            .expect("the best chain contains the block being finalized")
            .clone();
        let treestate = best_chain
            .treestate(height.into())
            .expect("chains keep a treestate for each of their blocks");
        self.insert(best_chain);

        // for each remaining chain in side_chains
        for mut side_chain in side_chains.into_iter().rev() {
            if side_chain.non_finalized_root_hash() != best_chain_block.hash {
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
            assert_eq!(side_chain_root.hash, best_chain_block.hash);

            // add the chain back to `self.chain_set`
            if !side_chain.is_empty() {
                self.insert(side_chain);
            }
        }

        // Remove all invalidated_blocks at or below the finalized height
        self.invalidated_blocks
            .retain(|height, _blocks| *height >= best_chain_block.height);
        let rejection_capacity_cleared = self
            .rejected_blocks
            .lock()
            .expect("authenticated rejection lock is not poisoned")
            .prune(best_chain_block.height);
        if rejection_capacity_cleared {
            tracing::info!(
                finalized_height = ?best_chain_block.height,
                "early relay and speculative mining enabled after finalization freed rejection capacity"
            );
        }

        self.retained_finalized_height = Some(height);
        self.update_metrics_for_chains();

        // Add the treestate to the finalized block.
        FinalizableBlock::new(Arc::unwrap_or_clone(best_chain_block), treestate)
    }

    /// Pops the best chain blocks kept by [`Self::finalize_keeping_best_chain`].
    ///
    /// Copies the best chain if a published snapshot shares it.
    fn pop_finalized_roots(&mut self) {
        let Some(height) = self.retained_finalized_height.take() else {
            return;
        };
        let mut best_chain = self
            .chain_set
            .pop_last()
            .expect("finalized blocks were kept in the best chain");
        let chain = Arc::make_mut(&mut best_chain);
        while !chain.is_empty() && chain.non_finalized_root_height() <= height {
            chain.pop_root();
        }

        if !best_chain.is_empty() {
            self.insert(best_chain);
        }
        self.update_metrics_for_chains();
    }

    /// Pops finalized blocks from the best chain, unless `parent_hash` is its tip, so chain work
    /// stays comparable when another chain gets a child.
    fn pop_finalized_roots_unless_extending(&mut self, parent_hash: block::Hash) {
        if self.best_tip().map(|(_, hash)| hash) != Some(parent_hash) {
            self.pop_finalized_roots();
        }
    }

    /// Returns a private copy of `chain` to extend, without the blocks kept by
    /// [`Self::finalize_keeping_best_chain`].
    ///
    /// Replays the spare into the copy when it can, so a chain shared with read-state snapshots
    /// isn't cloned, and keeps a shared `chain` as the next spare: readers release it once they
    /// see the extension. A private chain, such as a new fork, is used directly.
    ///
    /// Callers must pass the best chain while it keeps finalized blocks, or a new empty chain.
    /// They clear `retained_finalized_height` once the extension replaces the best chain.
    fn private_copy(&mut self, chain: Arc<Chain>) -> Chain {
        let (mut copy, retired) = match self.take_spare(&chain) {
            Some(spare) => (spare, Some(chain)),
            None => match Arc::try_unwrap(chain) {
                Ok(private_chain) => (private_chain, None),
                Err(shared) => ((*shared).clone(), Some(shared)),
            },
        };
        if retired.is_some() {
            self.spare = retired;
        }
        if let Some(height) = self.retained_finalized_height.filter(|_| !copy.is_empty()) {
            while copy.non_finalized_root_height() <= height {
                copy.pop_root();
            }
        }
        copy
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

        self.pop_finalized_roots_unless_extending(parent_hash);
        let parent_chain = self.parent_chain(parent_hash)?;
        let new_chain = Arc::new(self.private_copy(parent_chain));

        // If the block is invalid, return the error,
        // and drop the cloned parent Arc, or newly created chain fork.
        let modified_chain = self.validate_and_commit(new_chain, prepared, finalized_state)?;

        // If the block is valid:
        // - add the new chain fork or updated chain to the set of recent chains
        // - remove the parent chain, if it was in the chain set
        //   (if it was a newly created fork, it won't be in the chain set)
        self.insert_with(modified_chain, |chain_set| {
            chain_set.retain(|chain| chain.non_finalized_tip_hash() != parent_hash)
        });
        self.retained_finalized_height = None;

        self.update_metrics_for_committed_block(height, hash);

        Ok(())
    }

    /// Apply an exact proposal verdict to its solved block and original parent.
    ///
    /// Normal chain application rebuilds indexes and the history leaf using the solved hash.
    /// Mined bodies are published only after checking current invalidation and resolving the parent.
    /// A changed wall clock can suppress early relay without changing the normal commit policy.
    pub(super) fn commit_prevalidated_contextual(
        &mut self,
        contextual: ContextuallyVerifiedBlock,
        finalized_state: &ZebraDb,
        notifications: Option<&MinedBlockNotifications>,
    ) -> Result<(), ValidateContextError> {
        // Local invalidation refers to solved hashes, which proposals do not bind.
        if self.is_block_invalidated(contextual.hash) {
            return Err(ValidateContextError::BlockPreviouslyInvalidated {
                block_hash: contextual.hash,
            });
        }
        let parent_hash = contextual.block.header.previous_block_hash;
        let (height, hash) = (contextual.height, contextual.hash);
        self.pop_finalized_roots_unless_extending(parent_hash);
        let parent_chain = if parent_hash == finalized_state.finalized_tip_hash() {
            Arc::new(Chain::new(
                &self.network,
                finalized_state
                    .finalized_tip_height()
                    .expect("a successful proposal has a finalized parent"),
                finalized_state.note_commitment_trees_for_tip(),
                finalized_state.history_tree(),
                finalized_state.finalized_value_pool(),
            ))
        } else {
            self.parent_chain(parent_hash)?
        };
        if let Some(notifications) = notifications {
            if contextual
                .block
                .header
                .time_is_valid_at(chrono::Utc::now(), &height, &hash)
                .is_ok()
            {
                let _ = notifications.validated.send(Some(contextual.block.clone()));
            }
        }
        // A solved proposal rebinds the chain its validation prepared, and retires the parent
        // chain for the next proposal. Any other block is pushed onto a private copy.
        if let Some(chain) = self.take_prepared_for(&contextual, &parent_chain) {
            self.insert_with(Arc::new(chain), |chains| {
                chains.retain(|chain| chain.non_finalized_tip_hash() != parent_hash)
            });
            *self.prepared() = Some(parent_chain);
            self.retained_finalized_height = None;
            self.update_metrics_for_committed_block(height, hash);
            return Ok(());
        }
        let chain = self.private_copy(parent_chain).push(contextual)?;
        self.insert_with(Arc::new(chain), |chains| {
            chains.retain(|chain| chain.non_finalized_tip_hash() != parent_hash)
        });
        self.retained_finalized_height = None;
        self.update_metrics_for_committed_block(height, hash);
        Ok(())
    }

    /// Locks the shared prepared chain.
    fn prepared(&self) -> std::sync::MutexGuard<'_, Option<Arc<Chain>>> {
        self.prepared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Returns the prepared chain with its proposal tip rebound to `block`, if the prepared chain
    /// is `parent_chain`, without the blocks kept by [`Self::finalize_keeping_best_chain`], with
    /// a proposal for `block` on top, and no reader shares it.
    ///
    /// Otherwise the prepared chain is left for the next proposal to replay.
    fn take_prepared_for(
        &mut self,
        block: &ContextuallyVerifiedBlock,
        parent_chain: &Chain,
    ) -> Option<Chain> {
        if parent_chain.is_empty() {
            return None;
        }
        // Proposal checks extend a private copy, which drops the blocks finalization kept.
        let root_height = self
            .retained_finalized_height
            .and_then(|height| height.next().ok())
            .map_or(parent_chain.non_finalized_root_height(), |height| {
                height.max(parent_chain.non_finalized_root_height())
            });
        let mut prepared = self.prepared();
        let candidate = prepared.as_ref()?;
        let proposal = candidate.tip_block()?;
        if Arc::strong_count(candidate) != 1
            || parent_chain.hash_by_height(root_height) != Some(candidate.non_finalized_root_hash())
            || candidate.non_finalized_tip_height()
                != parent_chain.non_finalized_tip_height().next().ok()?
            || !candidate.can_rebind_tip()
            || !same_block_except_proof_of_work(proposal, block)
        {
            return None;
        }
        let candidate = prepared.take()?;
        drop(prepared);
        let mut chain = Arc::unwrap_or_clone(candidate);
        match chain.rebind_tip(block.clone()) {
            Ok(()) => Some(chain),
            // The chain is unchanged, so the next proposal can still replay it.
            Err(_) => {
                *self.prepared() = Some(Arc::new(chain));
                None
            }
        }
    }

    /// Takes the prepared chain as the spare for a proposal check on this snapshot, waiting up
    /// to `deadline` for a reader or the writer to release it.
    pub(crate) fn take_prepared_as_spare(&mut self, deadline: Instant) {
        loop {
            let taken = {
                let mut prepared = self.prepared();
                match prepared.as_ref() {
                    None => return,
                    Some(chain) if Arc::strong_count(chain) == 1 => prepared.take(),
                    Some(_) => None,
                }
            };
            if taken.is_some() {
                self.spare = taken;
                return;
            }
            if Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Stores `chain` as the prepared chain, replacing any previous one.
    pub(crate) fn store_prepared(&self, chain: Chain) {
        *self.prepared() = Some(Arc::new(chain));
    }

    /// Replays the spare into a private copy of the best chain ahead of the next commit or
    /// finalization, so they don't pay for the replay. Returns whether a copy is ready.
    ///
    /// Readers release the retired chain shortly after the snapshot that replaced it is
    /// published, so this waits for them until `deadline`, unless `interrupted` reports that a
    /// block is waiting to be committed.
    pub fn prepare_spare(&mut self, deadline: Instant, interrupted: impl Fn() -> bool) -> bool {
        let Some(best_chain) = self.best_chain().cloned() else {
            return false;
        };
        if !self.spare_matches(&best_chain) {
            return false;
        }
        while self.spare_is_shared() {
            if Instant::now() >= deadline || interrupted() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        match self.take_spare(&best_chain) {
            Some(spare) => {
                self.spare = Some(Arc::new(spare));
                true
            }
            None => false,
        }
    }

    /// Returns `true` if the spare is a version of `target`, so replaying it can make a private
    /// copy of `target` once readers release it.
    fn spare_matches(&self, target: &Chain) -> bool {
        self.spare
            .as_ref()
            .is_some_and(|spare| chain_is_version_of(spare, target))
    }

    /// Returns `true` if a reader still shares the spare.
    fn spare_is_shared(&self) -> bool {
        self.spare
            .as_ref()
            .is_some_and(|spare| Arc::strong_count(spare) > 1)
    }

    /// Removes and returns the spare chain, if there is one.
    pub(crate) fn remove_spare(&mut self) -> Option<Arc<Chain>> {
        self.spare.take()
    }

    /// Removes and returns the chain whose tip is `tip`, if there is one.
    pub(crate) fn remove_chain_with_tip(&mut self, tip: block::Hash) -> Option<Arc<Chain>> {
        let chain = self.find_chain(|chain| chain.non_finalized_tip_hash() == tip)?;
        self.chain_set.remove(&chain);
        Some(chain)
    }

    /// Returns a private copy of `target` replayed from the spare chain, or `None` when the spare
    /// is missing, still shared with a reader, or not a version of `target`.
    ///
    /// Replaying pops the tip blocks `target` lacks, such as an unsolved proposal, pops the roots
    /// `target` has finalized, and pushes the blocks it has added, so it costs time proportional
    /// to those blocks rather than to the whole chain. A spare that can't be used is kept for a
    /// later commit, unless replaying it failed.
    fn take_spare(&mut self, target: &Chain) -> Option<Chain> {
        let spare = self.spare.as_ref()?;
        if spare.is_empty() {
            self.spare = None;
            return None;
        }
        if !chain_is_version_of(spare, target) {
            return None;
        }
        let mut spare = match Arc::try_unwrap(self.spare.take()?) {
            Ok(spare) => spare,
            Err(shared) => {
                self.spare = Some(shared);
                return None;
            }
        };
        while !target
            .height_by_hash
            .contains_key(&spare.non_finalized_tip_hash())
        {
            if spare.blocks.len() == 1 {
                return None;
            }
            spare.pop_tip();
        }
        while spare.non_finalized_root_height() < target.non_finalized_root_height() {
            if spare.blocks.len() == 1 {
                return None;
            }
            spare.pop_root();
        }
        if spare.non_finalized_root_hash() != target.non_finalized_root_hash() {
            return None;
        }
        let spare_tip_height = spare.non_finalized_tip_height();
        for block in target
            .blocks
            .range((Bound::Excluded(spare_tip_height), Bound::Unbounded))
            .map(|(_, block)| block)
        {
            // These blocks were accepted onto this chain, so replaying them can only fail if
            // the spare is not an earlier version of `target` after all.
            spare = spare.push(block.as_ref().clone()).ok()?;
        }
        // Fork diagnostics are not part of the replayed blocks.
        spare.last_fork_height = target.last_fork_height;
        Some(spare)
    }

    /// Invalidate block with hash `block_hash` and all descendants from the non-finalized state. Insert
    /// the new chain into the chain_set and discard the previous.
    ///
    /// Returns an error without removing any chains if the block is the root of every chain.
    #[allow(clippy::unwrap_in_result)]
    pub fn invalidate_block(&mut self, block_hash: Hash) -> Result<block::Hash, InvalidateError> {
        self.pop_finalized_roots();
        let Some(chain) = self.find_chain(|chain| chain.contains_block_hash(block_hash)) else {
            return Err(InvalidateError::BlockNotFound(block_hash));
        };

        let invalidated_blocks = if chain.non_finalized_root_hash() == block_hash {
            if self
                .chain_set
                .iter()
                .all(|chain| chain.contains_block_hash(block_hash))
            {
                return Err(InvalidateError::OnlyChainRoot(block_hash));
            }

            self.chain_set
                .retain(|chain| !chain.contains_block_hash(block_hash));
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
        self.pop_finalized_roots();
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
    #[allow(clippy::unwrap_in_result)]
    pub fn commit_new_chain(
        &mut self,
        prepared: SemanticallyVerifiedBlock,
        finalized_state: &ZebraDb,
    ) -> Result<(), ValidateContextError> {
        self.pop_finalized_roots();
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
        if self.is_block_invalidated(prepared.hash) {
            return Err(ValidateContextError::BlockPreviouslyInvalidated {
                block_hash: prepared.hash,
            });
        }

        // Reads from disk
        //
        // TODO: if these disk reads show up in profiles, run them in parallel, using std::thread::spawn()
        let spent_utxos = check::utxo::transparent_spend(
            &prepared,
            &new_chain.created_utxos,
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

        Self::validate_and_update_parallel(new_chain, contextual, sprout_final_treestates)
    }

    /// Validate `contextual` and update `new_chain`, doing CPU-intensive work in parallel batches.
    #[allow(clippy::unwrap_in_result)]
    #[tracing::instrument(
        skip(new_chain, contextual, sprout_final_treestates),
        fields(height = ?contextual.height, hash = %contextual.hash)
    )]
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
                    None,
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
        // and the non-finalized chain is further limited by the rollback window
        // (`MAX_BLOCK_REORG_HEIGHT`, currently 1000 blocks).
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

    /// Returns whether local invalidation excludes this block or one of its retained descendants.
    pub(super) fn is_block_invalidated(&self, hash: block::Hash) -> bool {
        self.invalidated_blocks
            .values()
            .any(|blocks| blocks.iter().any(|block| block.hash == hash))
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

impl Drop for NonFinalizedState {
    fn drop(&mut self) {
        self.disable_metrics();
    }
}

/// Returns `true` if `chain` is a version of `target`: both contain `target`'s root block, so
/// popping `chain`'s extra tips and roots and pushing `target`'s later blocks makes them equal.
fn chain_is_version_of(chain: &Chain, target: &Chain) -> bool {
    if chain.is_empty() || target.is_empty() {
        return false;
    }
    let root_height = target.non_finalized_root_height();
    chain
        .blocks
        .get(&root_height)
        .is_some_and(|block| block.hash == target.non_finalized_root_hash())
}

/// Returns `true` if `proposal` and `block` have the same transactions, height and header apart
/// from the nonce and Equihash solution. The header's commitment covers the transaction
/// authorizing data, so this means the same block contents.
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
